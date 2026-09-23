//! wgpu terminal renderer: a glyph atlas + one instanced quad per cell, ported
//! from the web `singleCanvasRenderer.ts`. `TermGpu` is surface-agnostic — it
//! draws into a *provided* render pass, so it works both inside a winit window
//! surface (`Renderer`, the raw spike) and inside an Iced `shader` widget.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ab_glyph::{Font, FontVec, ScaleFont};
use wgpu::util::DeviceExt;
use winit::window::Window;

use crate::raster::GlyphBitmap;
use crate::term::VtTerm;

const ATLAS: u32 = 1024;
const SLOT_SOLID: u32 = 0; // fully-covered cell (block cursor)
const SLOT_BLANK: u32 = 1; // empty coverage (bg-only cells)

/// Terminal type metrics — matched to the web (which the webview renders with
/// `line-height: normal`): a 12px em (now user-configurable via
/// [`crate::term::font_px`]), and a cell height equal to the font's natural line
/// box (ascent − descent + line_gap), which for Menlo is ~14px. The point size is
/// multiplied by the DPR (`scale`) and rasterised at that resolution so text is
/// crisp on retina/HiDPI. LINE_HEIGHT is an extra leading multiplier on top of the
/// natural box (1.0 = match the web exactly).
const LINE_HEIGHT: f32 = 1.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    canvas: [f32; 2],
    cell: [f32; 2],
    glyph: [f32; 2],
    srgb: f32,
    // >0.5 = composite text with Windows Terminal's DirectWrite grayscale gamma-
    // correction (gamma-1.8 alpha correction; Windows). 0.0 = linear-space blend
    // (fuller, the macOS look). Was the alignment pad.
    gamma_blend: f32,
}

const SHADER: &str = r#"
struct Uniforms { canvas: vec2<f32>, cell: vec2<f32>, glyph: vec2<f32>, srgb: f32, gamma_blend: f32 };
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var catlas: texture_2d<f32>;

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) fg: vec3<f32>,
  @location(2) bg: vec3<f32>,
  @location(3) kind: f32,
};

@vertex
fn vs(
  @location(0) corner: vec2<f32>,
  @location(1) pos: vec2<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) fg: vec3<f32>,
  @location(4) bg: vec3<f32>,
  @location(5) flags: vec2<f32>,  // x = kind (0 mono, 1 colour), y = cells wide
) -> VsOut {
  let wide = flags.y;
  let px = pos + corner * vec2<f32>(u.cell.x * wide, u.cell.y);
  let clip = vec2<f32>((px.x / u.canvas.x) * 2.0 - 1.0, 1.0 - (px.y / u.canvas.y) * 2.0);
  var out: VsOut;
  out.clip = vec4<f32>(clip, 0.0, 1.0);
  out.uv = uv + corner * vec2<f32>(u.glyph.x * wide, u.glyph.y);
  out.fg = fg;
  out.bg = bg;
  out.kind = flags.x;
  return out;
}

// Antialiasing is blended in LINEAR space (gamma-correct), which makes the
// edge/partial-coverage pixels fuller — the smooth look macOS terminals
// (iTerm2/Terminal.app) have. The web blends in gamma space, which is flatter
// and thinner. fg/bg are sRGB, so decode → blend → re-encode.
fn to_linear(c: vec3<f32>) -> vec3<f32> {
  let lo = c / 12.92;
  let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
  return select(lo, hi, c > vec3<f32>(0.04045));
}
fn to_srgb(c: vec3<f32>) -> vec3<f32> {
  let lo = c * 12.92;
  let hi = 1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055;
  return select(lo, hi, c > vec3<f32>(0.0031308));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  if (u.gamma_blend > 0.5) {
    // Windows: composite in gamma (sRGB) space, but run the RAW glyph coverage through
    // Windows Terminal's DirectWrite grayscale gamma-correction first (ported verbatim
    // from WT's dwrite_helpers.hlsl / shader_ps.hlsl). This is the gamma-1.8 alpha-
    // correction polynomial DirectWrite uses for grayscale AA: it makes light-on-dark
    // text as full + legible as WT — fuller than the naive blend (which read hazy/thin)
    // without the heaviness of the full-linear blend. fg/bg (+ colour texels) are sRGB.
    var p: vec3<f32>;
    if (in.kind > 0.5) {
      // Colour glyph (emoji): straight-alpha sRGB over the cell bg, no text gamma.
      let s = textureSample(catlas, samp, in.uv);
      p = mix(in.bg, s.rgb, s.a);
    } else {
      let cov = textureSample(atlas, samp, in.uv).r;
      // DWrite_GrayscaleBlend: gamma-1.8 ratios + grayscale enhanced contrast 1.0.
      let g = vec4<f32>(0.148054421, -0.894594550, 1.47590804, -0.324668258);
      // Light-on-dark contrast adjustment (× grayscale enhanced contrast 1.0); 0 for
      // white text, ramps up as the fg darkens. Then EnhanceContrast on the coverage.
      let k = clamp(dot(in.fg, vec3<f32>(0.30, 0.59, 0.11) * -4.0) + 3.0, 0.0, 1.0);
      let intensity = dot(in.fg, vec3<f32>(0.25, 0.5, 0.25));
      let c = cov * (k + 1.0) / (cov * k + 1.0);
      // ApplyAlphaCorrection: the gamma-correct coverage to composite in sRGB space.
      let a = c + c * (1.0 - c) * ((g.x * intensity + g.y) * c + (g.z * intensity + g.w));
      p = mix(in.bg, in.fg, a);
    }
    // p is the desired sRGB pixel: a non-sRGB target stores it as-is; an sRGB
    // target re-encodes on write, so hand it the linear form.
    var out = p;
    if (u.srgb > 0.5) { out = to_linear(p); }
    return vec4<f32>(out, 1.0);
  }
  // Linear-space blend: fuller edges, the macOS look.
  var col: vec3<f32>;
  if (in.kind > 0.5) {
    // Colour glyph (emoji): straight-alpha sRGB RGBA composited over the cell bg.
    let s = textureSample(catlas, samp, in.uv);
    col = mix(to_linear(in.bg), to_linear(s.rgb), s.a);
  } else {
    // Mono glyph: single-channel coverage tinted with the fg colour.
    let a = textureSample(atlas, samp, in.uv).r;
    col = mix(to_linear(in.bg), to_linear(in.fg), a);
  }
  // An sRGB target re-encodes on write, so hand it linear; a non-sRGB target
  // (the raw spike) needs us to encode to sRGB ourselves.
  var out = col;
  if (u.srgb < 0.5) { out = to_srgb(col); }
  return vec4<f32>(out, 1.0);
}
"#;

/// A cached glyph: its atlas slot, whether it lives in the colour atlas (emoji),
/// and how many cells wide it spans (2 for a wide colour glyph).
#[derive(Clone, Copy)]
struct Glyph {
    slot: u32,
    color: bool,
    cells: u32,
}

/// Everything a frame depends on besides the atlas: the grid's generation, the cursor
/// (its visibility changes on a timer, not a grid mutation), the background setting, and
/// the canvas size. `prepare` rebuilds only when this differs from the last frame drawn.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FrameKey {
    generation: u64,
    cursor: (usize, usize, bool),
    bg: [u32; 3],
    canvas: (u32, u32),
}

/// Set when Arbiter put `WGPU_BACKEND` in its own environment for iced, rather than
/// inheriting it. Panes then leave it out (`Session::spawn`): a wgpu program started in
/// one, Arbiter itself under `cargo run` included, would take it for the user's choice.
pub static BACKEND_ENV_IS_OURS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Windows: the one wgpu backend to ask for (`WGPU_BACKEND`), decided before iced starts.
/// Pinning one keeps iced from initialising all three (DX12, Vulkan, OpenGL), tens of MB
/// of driver for nothing. DX12 first: it is Windows' own API, every driver has it, and
/// Vulkan flickers on Intel graphics. It used to draw an unmaximised window soft; the
/// vendored wgpu-hal presents 1:1 now (vendor/wgpu-hal/ARBITER-FORK.md). DX12 always lists
/// WARP, the software rasteriser, so only a hardware adapter counts; Vulkan next, and with
/// neither, WARP with a notice. `preferred` Vulkan only swaps the first two. The probe
/// enumerates adapters synchronously and drops the instances again at once.
/// The flag is false when a forced backend had no hardware adapter and the other was used.
#[cfg(windows)]
pub fn windows_backend(preferred: crate::persist::GraphicsBackend) -> (&'static str, bool) {
    use crate::persist::GraphicsBackend;
    let has_hardware = |backends: wgpu::Backends| {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor { backends, ..Default::default() });
        instance
            .enumerate_adapters(backends)
            .iter()
            .any(|a| a.get_info().device_type != wgpu::DeviceType::Cpu)
    };
    let dx12 = (wgpu::Backends::DX12, "dx12");
    let vulkan = (wgpu::Backends::VULKAN, "vulkan");
    let order = if preferred == GraphicsBackend::Vulkan { [vulkan, dx12] } else { [dx12, vulkan] };
    for (i, (backends, name)) in order.into_iter().enumerate() {
        if has_hardware(backends) {
            return (name, i == 0 || preferred == GraphicsBackend::Auto);
        }
    }
    warn_no_gpu();
    ("dx12", preferred != GraphicsBackend::Vulkan)
}

// On its own thread so the window opens behind it rather than after it.
#[cfg(windows)]
fn warn_no_gpu() {
    std::thread::spawn(|| {
        use windows::core::HSTRING;
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
        let text = HSTRING::from(
            "Arbiter found no graphics driver for DirectX 12 or Vulkan and is drawing in \
             software, which is slow.\n\n\
             Installing the graphics card's current driver fixes this.",
        );
        let caption = HSTRING::from("Arbiter");
        unsafe { MessageBoxW(HWND::default(), &text, &caption, MB_OK | MB_ICONWARNING) };
    });
}

/// Surface-agnostic renderer: pipeline + glyph atlas + instance buffer.
pub struct TermGpu {
    pipeline: wgpu::RenderPipeline,
    quad_vb: wgpu::Buffer,
    inst_vb: wgpu::Buffer,
    inst_cap: u64,
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    atlas_tex: wgpu::Texture,
    /// Separate RGBA atlas for colour glyphs (emoji); the mono `atlas_tex` (R8) and
    /// its writers are untouched, so normal text rendering is unaffected.
    color_atlas_tex: wgpu::Texture,

    /// Shared with every other pane's renderer: the font bytes are read, never owned here.
    spec: Arc<crate::font::FontSpec>,
    em_px: f32,
    scale: f32,
    /// Font size (points) this renderer was built with — compared against
    /// [`crate::term::font_px`] so the host can rebuild on a size change.
    built_pts: u32,
    pub cell_w: u32,
    pub cell_h: u32,
    baseline: f32,
    is_srgb: bool,
    atlas_cpu: Vec<u8>,
    color_atlas_cpu: Vec<u8>,
    /// Cached glyphs by (char, bold, drawn as a two-cell icon): the same icon exists at
    /// text size and at two-cell size, depending on what follows it (see `prepare`).
    glyphs: HashMap<(char, bool, bool), Glyph>,
    next_slot: u32,
    color_next: u32,
    per_row: u32,
    /// Atlas regions (x, y, w, h) drawn since the last upload; each goes up on its own,
    /// not the whole 1 MB or 4 MB texture. A whole-atlas entry means a fresh or flushed atlas.
    dirty: Vec<[u32; 4]>,
    color_dirty: Vec<[u32; 4]>,
    /// Set when an atlas was flushed while a frame was being built: every slot resolved
    /// before the flush is stale, so `prepare` builds the frame once more.
    flushed: bool,
    last_frame: Option<FrameKey>,

    scratch: Vec<f32>,
    count: u32,
    /// When `prepare` last rebuilt the frame, None before the first. The host keeps a frame
    /// mid-burst, and this bounds for how long (see `frame_age`).
    prepared_at: Option<Instant>,
}

/// The ab_glyph `PxScale` that renders `font`'s em square at `em_px` pixels.
/// ab_glyph sizes text by its ascent..descent height, not the em square like CSS
/// `Npx`, so passing a raw px gives glyphs ~15% too small. Scaling by
/// em / (ascent − descent + line_gap) corrects it to match the web's canvas.
fn abglyph_scale(font: &FontVec, em_px: f32) -> f32 {
    let upm = font.units_per_em().unwrap_or(1000.0);
    let h_units = font.ascent_unscaled() - font.descent_unscaled() + font.line_gap_unscaled();
    em_px * h_units / upm
}

/// Cell size (device px) for a font at a given scale — so callers (e.g. the
/// Iced shell) can map a window size to cols/rows without constructing a GPU.
pub fn measure_cell(font_bytes: &[u8], font_index: u32, scale: f32) -> (u32, u32) {
    let font = FontVec::try_from_vec_and_index(font_bytes.to_vec(), font_index).expect("load font");
    let em_px = (crate::term::font_px() as f32 * scale).round().max(8.0);
    let px = abglyph_scale(&font, em_px);
    let s = font.as_scaled(px);
    let w = s.h_advance(font.glyph_id('M')).round().max(1.0) as u32;
    let line = s.ascent() - s.descent() + s.line_gap();
    let h = (line * LINE_HEIGHT).ceil().max(1.0) as u32;
    (w, h)
}

impl TermGpu {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        spec: Arc<crate::font::FontSpec>,
        scale: f32,
    ) -> Self {
        // Glyph atlas (CPU). ab_glyph is used only for metrics (cell size +
        // baseline), from the regular face; glyphs are rasterised by the platform
        // engine (see `crate::raster`). Bold/regular share metrics (monospace).
        let font = FontVec::try_from_vec_and_index(spec.regular.0.clone(), spec.regular.1).expect("load font");
        let built_pts = crate::term::font_px();
        let em_px = (built_pts as f32 * scale).round().max(8.0);
        let px = abglyph_scale(&font, em_px);
        let scaled = font.as_scaled(px);
        // Cell width = the rounded glyph advance, matching the web's per-character
        // spacing (ceil would add ~1px between every character).
        let cell_w = scaled.h_advance(font.glyph_id('M')).round().max(1.0) as u32;
        // Cell height = the font's natural line box, matching the web's
        // `line-height: normal` (≈14px for Menlo at a 12px em). Without this the
        // rows are too tight and tall content (the Claude box) comes out short.
        let line = scaled.ascent() - scaled.descent() + scaled.line_gap();
        let cell_h = (line * LINE_HEIGHT).ceil().max(1.0) as u32;
        // Baseline = ascent from the cell top (plus any extra leading split
        // evenly), so glyphs sit on the baseline like the web.
        let baseline = scaled.ascent() + (cell_h as f32 - line) / 2.0;
        let per_row = (ATLAS / cell_w).max(1);
        let mut atlas_cpu = vec![0u8; (ATLAS * ATLAS) as usize];
        fill_slot(&mut atlas_cpu, SLOT_SOLID, per_row, cell_w, cell_h, 255);

        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // Colour glyph atlas (RGBA, zero = transparent). Same cell grid as the mono
        // atlas so the uv maths are shared; sampled only for colour-glyph instances.
        let color_atlas_cpu = vec![0u8; (ATLAS * ATLAS * 4) as usize];
        let color_atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("color-atlas"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let color_atlas_view = color_atlas_tex.create_view(&wgpu::TextureViewDescriptor::default());

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("term-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&atlas_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&color_atlas_view) },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pl"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("pipe"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs",
                buffers: &[
                    wgpu::VertexBufferLayout {
                        array_stride: 2 * 4,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                    },
                    wgpu::VertexBufferLayout {
                        array_stride: 12 * 4,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            1 => Float32x2, 2 => Float32x2, 3 => Float32x3, 4 => Float32x3, 5 => Float32x2
                        ],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
        });

        let quad_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad"),
            contents: bytemuck::cast_slice(&[0.0f32, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0]),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let inst_cap = 8192u64;
        let inst_vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("inst"),
            size: inst_cap * 12 * 4,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline, quad_vb, inst_vb, inst_cap, uniform_buf, bind_group, atlas_tex, color_atlas_tex,
            spec,
            em_px, scale, built_pts, cell_w, cell_h, baseline,
            is_srgb: format.is_srgb(),
            atlas_cpu, color_atlas_cpu, glyphs: HashMap::new(), next_slot: 2, color_next: 0,
            per_row,
            dirty: vec![[0, 0, ATLAS, ATLAS]],
            color_dirty: vec![[0, 0, ATLAS, ATLAS]],
            flushed: false,
            last_frame: None,
            scratch: Vec::new(), count: 0, prepared_at: None,
        }
    }

    /// Slots in an atlas: whole rows of whole cells.
    fn capacity(&self) -> u32 {
        self.per_row * (ATLAS / self.cell_h).max(1)
    }

    /// The mono atlas is full: start it over. Every mono glyph rasterises again the next
    /// time it is drawn, and the frame under construction is rebuilt (see `prepare`) so
    /// no instance keeps pointing at a recycled slot. Rare (thousands of distinct glyphs
    /// in one pane) and cheap next to the alternative, which was indexing past the atlas.
    fn flush_mono(&mut self) {
        self.glyphs.retain(|_, g| g.color);
        self.atlas_cpu.fill(0);
        fill_slot(&mut self.atlas_cpu, SLOT_SOLID, self.per_row, self.cell_w, self.cell_h, 255);
        self.next_slot = 2;
        self.dirty.clear();
        self.dirty.push([0, 0, ATLAS, ATLAS]);
        self.flushed = true;
    }

    fn flush_color(&mut self) {
        self.glyphs.retain(|_, g| !g.color);
        self.color_atlas_cpu.fill(0);
        self.color_next = 0;
        self.color_dirty.clear();
        self.color_dirty.push([0, 0, ATLAS, ATLAS]);
        self.flushed = true;
    }

    /// The display scale this renderer was built for. The host rebuilds the
    /// renderer when the window moves to a display with a different scale, so the
    /// font px / cell size track the new DPI (otherwise text halves/doubles).
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The font size (points) this renderer was built with. The host rebuilds the
    /// renderer when the Settings font size changes (like a DPI change), so the cell
    /// size + PTY grid track the new size.
    pub fn built_pts(&self) -> u32 {
        self.built_pts
    }

    /// How long ago `prepare` last rebuilt the frame, None before the first. The host
    /// keeps the previous frame while a burst of output is still landing in the grid,
    /// and bounds that by this age (see `session::WakeHold::frame_due`).
    pub fn frame_age(&self) -> Option<Duration> {
        self.prepared_at.map(|at| at.elapsed())
    }

    /// Reserve `cells` horizontally-contiguous slots in the colour atlas (a wide
    /// emoji needs 2), never straddling a row wrap.
    fn alloc_color(&mut self, cells: u32) -> u32 {
        let col = self.color_next % self.per_row;
        if col + cells > self.per_row {
            self.color_next += self.per_row - col; // skip the row's tail
        }
        if self.color_next + cells > self.capacity() {
            self.flush_color();
        }
        let slot = self.color_next;
        self.color_next += cells;
        slot
    }

    /// Reserve `cells` horizontally-contiguous slots in the mono atlas (a two-cell icon
    /// needs 2), never straddling a row wrap.
    fn alloc_mono(&mut self, cells: u32) -> u32 {
        let col = self.next_slot % self.per_row;
        if col + cells > self.per_row {
            self.next_slot += self.per_row - col; // skip the row's tail
        }
        if self.next_slot + cells > self.capacity() {
            self.flush_mono();
        }
        let slot = self.next_slot;
        self.next_slot += cells;
        slot
    }

    /// Resolve `ch` (regular/bold) to a cached glyph: a programmatic block/box glyph
    /// or a rasterised mono glyph in the R8 atlas, or a colour glyph (emoji) in the
    /// RGBA atlas spanning `wide_hint ? 2 : 1` cells. `icon2` draws an icon larger
    /// across two cells (see `prepare`).
    fn slot_for(&mut self, ch: char, bold: bool, wide_hint: bool, icon2: bool) -> Glyph {
        if ch == ' ' || ch == '\0' {
            return Glyph { slot: SLOT_BLANK, color: false, cells: 1 };
        }
        if let Some(&g) = self.glyphs.get(&(ch, bold, icon2)) {
            return g;
        }
        let cp = ch as u32;
        // The next free mono slot (only consumed if we actually draw a mono glyph), with
        // room made for it first.
        if self.next_slot + 1 > self.capacity() {
            self.flush_mono();
        }
        let mslot = self.next_slot;
        let ox = (mslot % self.per_row) * self.cell_w;
        let oy = (mslot / self.per_row) * self.cell_h;
        // Block Elements, Box Drawing and Powerline's straight separators are drawn
        // programmatically with consistent stroke centres so lines AND corners tile
        // seamlessly: what the web's canvas renderer and GPU terminals
        // (Alacritty/Kitty/WezTerm) do. Font-rendering them leaves sub-pixel gaps and,
        // for rounded corners Menlo lacks, mismatched glyphs from a fallback font.
        if draw_block_glyph(&mut self.atlas_cpu, cp, ox, oy, self.cell_w, self.cell_h)
            || draw_box_glyph(&mut self.atlas_cpu, cp, ox, oy, self.cell_w, self.cell_h)
            || draw_powerline_glyph(&mut self.atlas_cpu, cp, ox, oy, self.cell_w, self.cell_h)
        {
            self.next_slot += 1;
            self.dirty.push([ox, oy, self.cell_w, self.cell_h]);
            let g = Glyph { slot: mslot, color: false, cells: 1 };
            self.glyphs.insert((ch, bold, icon2), g);
            return g;
        }
        // Pick the bold face when we carry one (swash path); otherwise pass the
        // regular bytes and let the rasteriser synthesise bold (CoreText).
        let (data, index) = match (bold, &self.spec.bold) {
            (true, Some(b)) => (b.0.as_slice(), b.1),
            _ => (self.spec.regular.0.as_slice(), self.spec.regular.1),
        };
        // Also hand over the bold-face bytes regardless of weight: the DirectWrite
        // path loads them into a real bold IDWriteFontFace so bold renders the bundled
        // bold (not a synthesised faux-bold). Other platforms ignore this.
        let bold_data = self.spec.bold.as_ref().map(|(b, _)| b.as_slice());
        let raster = crate::raster::rasterize(
            &self.spec.name, data, index, bold_data, self.em_px, ch, bold, wide_hint, icon2,
        );
        // Diagnostic: ARBITER_GLYPH_DEBUG logs how non-ASCII symbols (e.g. ✻ U+273B,
        // ⏵ U+23F5) rasterise — mono vs colour, size + bearing vs the cell, and the
        // width flag — so glyph-fit issues can be seen instead of guessed. Fires once
        // per glyph (the cache returns earlier on repeats); silent without the env var.
        if cp >= 0x2300 && std::env::var_os("ARBITER_GLYPH_DEBUG").is_some() {
            match &raster {
                Some(b) => eprintln!(
                    "[glyph] U+{:04X} {:?} raster={}x{}@({},{}) color={} | cell={}x{} baseline={:.1} wide_hint={}",
                    cp, ch, b.width, b.height, b.left, b.top, b.color,
                    self.cell_w, self.cell_h, self.baseline, wide_hint,
                ),
                None => eprintln!(
                    "[glyph] U+{:04X} {:?} NO-GLYPH | cell={}x{} wide_hint={}",
                    cp, ch, self.cell_w, self.cell_h, wide_hint,
                ),
            }
        }
        let g = match raster {
            Some(bmp) if bmp.color => {
                // Colour glyph → RGBA atlas. Emoji are double-width, so span 2 cells.
                let cells = if wide_hint { 2 } else { 1 };
                // Windows: scale a colour glyph that overflows its region to fit (e.g.
                // ⏵, drawn as a Segoe UI Emoji glyph wider than its one cell, was
                // clipped on the right). No-op when it already fits → emoji unchanged.
                #[cfg(target_os = "windows")]
                let bmp = fit_to_box(bmp, cells * self.cell_w, self.cell_h, self.baseline);
                let slot = self.alloc_color(cells);
                let cox = (slot % self.per_row) * self.cell_w;
                let coy = (slot / self.per_row) * self.cell_h;
                blit_color(
                    &mut self.color_atlas_cpu, &bmp, self.baseline, cells * self.cell_w, self.cell_h, cox, coy,
                );
                self.color_dirty.push([cox, coy, cells * self.cell_w, self.cell_h]);
                Glyph { slot, color: true, cells }
            }
            Some(bmp) if icon2 => {
                // A two-cell icon (see `prepare`): drawn at `ICON_EM_SCALE`, fitted into
                // the pair of cells (every platform: it is oversized by design), seated on
                // the baseline, in two adjacent mono slots. The pre-reserved `mslot` is
                // simply not used.
                let bmp = fit_to_box(bmp, 2 * self.cell_w, self.cell_h, self.baseline);
                let bmp = seat_on_baseline(bmp, self.baseline);
                let slot = self.alloc_mono(2);
                let ox = (slot % self.per_row) * self.cell_w;
                let oy = (slot / self.per_row) * self.cell_h;
                blit_glyph(&mut self.atlas_cpu, &bmp, self.baseline, 2 * self.cell_w, self.cell_h, ox, oy);
                self.dirty.push([ox, oy, 2 * self.cell_w, self.cell_h]);
                Glyph { slot, color: false, cells: 2 }
            }
            Some(bmp) => {
                let bmp = if crate::raster::is_powerline_separator(ch) {
                    stretch_to_box(bmp, self.cell_w, self.cell_h, self.baseline)
                } else if fits_into_cell(ch) {
                    fit_to_box(bmp, self.cell_w, self.cell_h, self.baseline)
                } else {
                    bmp
                };
                blit_glyph(&mut self.atlas_cpu, &bmp, self.baseline, self.cell_w, self.cell_h, ox, oy);
                self.next_slot += 1;
                self.dirty.push([ox, oy, self.cell_w, self.cell_h]);
                Glyph { slot: mslot, color: false, cells: 1 }
            }
            // No glyph anywhere → blank (don't consume the mono slot).
            None => Glyph { slot: SLOT_BLANK, color: false, cells: 1 },
        };
        self.glyphs.insert((ch, bold, icon2), g);
        g
    }

    fn uv(&self, slot: u32) -> (f32, f32) {
        let col = slot % self.per_row;
        let row = slot / self.per_row;
        ((col * self.cell_w) as f32 / ATLAS as f32, (row * self.cell_h) as f32 / ATLAS as f32)
    }

    /// Build the instance list from the grid + upload atlas/buffers/uniforms.
    /// `canvas_w/h` are the draw area in physical px.
    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, term: &VtTerm, canvas_w: u32, canvas_h: u32) {
        let cw = self.cell_w as f32;
        let ch = self.cell_h as f32;
        let default_bg = term.default_bg();
        let (cur_row, cur_col, cur_vis) = term.cursor();
        // Nothing that reaches the screen has changed since the last frame built here:
        // keep the instance buffer as it is. Any output in any pane used to re-walk every
        // visible grid and re-upload it, at 60 fps while Claude worked.
        let key = FrameKey {
            generation: term.generation(),
            cursor: (cur_row, cur_col, cur_vis),
            bg: default_bg.map(f32::to_bits),
            canvas: (canvas_w, canvas_h),
        };
        if self.last_frame == Some(key) {
            return;
        }

        // Selection highlight bg (VS Code blue, matches the web's #264f78).
        const SEL_BG: [f32; 3] = [0x26 as f32 / 255.0, 0x4f as f32 / 255.0, 0x78 as f32 / 255.0];
        // Find-match highlights: amber for other matches, brighter for the current.
        const FIND_BG: [f32; 3] = [0x4a as f32 / 255.0, 0x3f as f32 / 255.0, 0x1a as f32 / 255.0];
        const FIND_CUR_BG: [f32; 3] = [0x8a as f32 / 255.0, 0x6d as f32 / 255.0, 0x1f as f32 / 255.0];
        // Detected http(s) links recolour their glyphs (web `#58a6ff`).
        const LINK_FG: [f32; 3] = [0x58 as f32 / 255.0, 0xa6 as f32 / 255.0, 0xff as f32 / 255.0];
        // Collect drawable cells, then resolve glyph slots (needs &mut self).
        let mut cells: Vec<(usize, usize, char, [f32; 3], [f32; 3], bool, bool)> = Vec::new();
        term.for_each_cell(|row, col, c, fg, bg, bold, wide, selected, hit, link| {
            let cell_fg = if link { LINK_FG } else { fg };
            let cell_bg = if selected {
                SEL_BG
            } else if hit == 2 {
                FIND_CUR_BG
            } else if hit == 1 {
                FIND_BG
            } else {
                bg
            };
            // Draw selected / highlighted cells even when blank; otherwise skip
            // empty default-bg cells.
            if selected || hit > 0 || !((c == ' ' || c == '\0') && bg == default_bg) {
                cells.push((row, col, c, cell_fg, cell_bg, bold, wide));
            }
        });

        // An icon (a Private Use Area glyph, which the symbols font supplies shrunk to one
        // cell) gets the cell after it too when that one is blank with the same
        // background, as Windows Terminal and WezTerm let icons overflow: drawn larger
        // across both cells, the blank's own quad dropped so it cannot paint over the
        // right half. The cells are in row order and blank default-background cells were
        // not collected, so the neighbour is either the next entry or such a blank.
        let cols = term.size().0;
        let mut mode = vec![0u8; cells.len()]; // 0 as is, 1 two-cell icon, 2 dropped blank
        for i in 0..cells.len() {
            let (row, col, c, _, bg, _, wide) = cells[i];
            if wide || col + 1 >= cols || !crate::raster::is_icon(c) {
                continue;
            }
            let neighbour = cells.get(i + 1).filter(|&&(r, k, ..)| r == row && k == col + 1);
            let next_blank = match neighbour {
                Some(&(_, _, nc, _, nbg, _, _)) => (nc == ' ' || nc == '\0') && nbg == bg,
                None => bg == default_bg,
            };
            if next_blank {
                mode[i] = 1;
                if neighbour.is_some() {
                    mode[i + 1] = 2;
                }
            }
        }

        // Built twice at most: an atlas flush mid-frame (see `flush_mono`) invalidates the
        // slots resolved before it, and the second pass resolves them all afresh.
        for _attempt in 0..2 {
            self.scratch.clear();
            self.flushed = false;
            for (i, (row, col, c, fg, bg, bold, wide)) in cells.iter().enumerate() {
                if mode[i] == 2 {
                    continue;
                }
                let g = self.slot_for(*c, *bold, *wide, mode[i] == 1);
                let (u, v) = self.uv(g.slot);
                let kind = if g.color { 1.0 } else { 0.0 };
                self.scratch.extend_from_slice(&[
                    *col as f32 * cw, *row as f32 * ch, u, v,
                    fg[0], fg[1], fg[2], bg[0], bg[1], bg[2],
                    kind, g.cells as f32,
                ]);
            }
            if !self.flushed {
                break;
            }
        }
        self.flushed = false;
        if cur_vis {
            let (u, v) = self.uv(SLOT_SOLID);
            let cur = [0.8f32, 0.8, 0.85]; // #ccccd9 block, matches the web cursor
            self.scratch.extend_from_slice(&[
                cur_col as f32 * cw, cur_row as f32 * ch, u, v,
                cur[0], cur[1], cur[2], cur[0], cur[1], cur[2],
                0.0, 1.0,
            ]);
        }
        self.count = (self.scratch.len() / 12) as u32;
        self.prepared_at = Some(Instant::now());

        // Upload the regions drawn since the last frame, each from its place in the CPU
        // copy: the data slice is the whole atlas and the layout's offset and row pitch
        // address the rectangle, so no repacking is needed. `write_texture` does not
        // require aligned row pitches (only buffer-to-texture copies do), and ours are
        // whole atlas rows anyway.
        for [x, y, w, h] in self.dirty.drain(..) {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &self.atlas_cpu,
                wgpu::ImageDataLayout {
                    offset: (y * ATLAS + x) as u64,
                    bytes_per_row: Some(ATLAS),
                    rows_per_image: None,
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        for [x, y, w, h] in self.color_dirty.drain(..) {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.color_atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x, y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &self.color_atlas_cpu,
                wgpu::ImageDataLayout {
                    offset: ((y * ATLAS + x) * 4) as u64,
                    bytes_per_row: Some(ATLAS * 4),
                    rows_per_image: None,
                },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }

        if self.count as u64 > self.inst_cap {
            self.inst_cap = (self.count as u64).next_power_of_two();
            self.inst_vb = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("inst"),
                size: self.inst_cap * 12 * 4,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.inst_vb, 0, bytemuck::cast_slice(&self.scratch));

        let u = Uniforms {
            canvas: [canvas_w.max(1) as f32, canvas_h.max(1) as f32],
            cell: [cw, ch],
            glyph: [cw / ATLAS as f32, ch / ATLAS as f32],
            srgb: if self.is_srgb { 1.0 } else { 0.0 },
            // Windows (> 0.5): composite text with Windows Terminal's DirectWrite
            // grayscale gamma-correction (see the fragment shader). macOS (0.0): the
            // fuller linear-space blend that matches iTerm2/Terminal.app.
            gamma_blend: if cfg!(target_os = "windows") { 1.0 } else { 0.0 },
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
        self.last_frame = Some(key);
    }

    /// Draw into a pass. The caller owns the pass + viewport/scissor.
    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.quad_vb.slice(..));
        pass.set_vertex_buffer(1, self.inst_vb.slice(..));
        pass.draw(0..4, 0..self.count);
    }
}

/// Thin surface-owning wrapper (the raw winit spike). Iced uses `TermGpu` directly.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    gpu: TermGpu,
}

impl Renderer {
    pub async fn new(window: Arc<Window>, spec: &crate::font::FontSpec, scale: f32) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let surface = instance.create_surface(window).expect("create_surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .expect("request_adapter");
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default(), None)
            .await
            .expect("request_device");

        let caps = surface.get_capabilities(&adapter);
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let gpu = TermGpu::new(&device, format, Arc::new(spec.clone()), scale);
        Self { surface, device, queue, config, gpu }
    }

    pub fn cell_w(&self) -> u32 { self.gpu.cell_w }
    pub fn cell_h(&self) -> u32 { self.gpu.cell_h }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    pub fn render(&mut self, term: &VtTerm) {
        self.gpu.prepare(&self.device, &self.queue, term, self.config.width, self.config.height);
        let bg = term.default_bg();
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut rp = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("rp"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            self.gpu.draw(&mut rp);
        }
        self.queue.submit([enc.finish()]);
        frame.present();
    }
}

/// Fill a sub-rect of a cell (cell-relative x/y), clamped to the cell, with a
/// coverage value. Used by the block/box glyph drawers.
fn fill_rect(atlas: &mut [u8], ox: u32, oy: u32, rx: u32, ry: u32, rw: u32, rh: u32, cell_w: u32, cell_h: u32, val: u8) {
    let x1 = (rx + rw).min(cell_w);
    let y1 = (ry + rh).min(cell_h);
    let mut yy = ry;
    while yy < y1 {
        let mut xx = rx;
        while xx < x1 {
            atlas[((oy + yy) * ATLAS + (ox + xx)) as usize] = val;
            xx += 1;
        }
        yy += 1;
    }
}

/// Block Elements (U+2580–U+259F) as exact filled rectangles, ported from the
/// web's `drawBlockGlyph`. Returns true if `cp` was handled.
fn draw_block_glyph(atlas: &mut [u8], cp: u32, ox: u32, oy: u32, w: u32, h: u32) -> bool {
    if !(0x2580..=0x259f).contains(&cp) {
        return false;
    }
    let wf = w as f32;
    let hf = h as f32;
    let r = |v: f32| v.round() as u32;
    let hx = r(wf / 2.0);
    let hy = r(hf / 2.0);
    // Lower partials ▁▂▃▅▆▇ keep the bottom `h - y` band; upper-fraction y.
    let lower = |frac: f32| -> (u32, u32) {
        let y = r(hf * frac);
        (y, h - y)
    };
    let mut fill = |rx: u32, ry: u32, rw: u32, rh: u32, val: u8| fill_rect(atlas, ox, oy, rx, ry, rw, rh, w, h, val);
    match cp {
        0x2588 => fill(0, 0, w, h, 255),                 // █ full
        0x2580 => fill(0, 0, w, hy, 255),                // ▀ upper half
        0x2584 => fill(0, hy, w, h - hy, 255),           // ▄ lower half
        0x258c => fill(0, 0, hx, h, 255),                // ▌ left half
        0x2590 => fill(hx, 0, w - hx, h, 255),           // ▐ right half
        0x2581 => { let (y, rh) = lower(7.0 / 8.0); fill(0, y, w, rh, 255) } // ▁
        0x2582 => { let (y, rh) = lower(6.0 / 8.0); fill(0, y, w, rh, 255) }
        0x2583 => { let (y, rh) = lower(5.0 / 8.0); fill(0, y, w, rh, 255) }
        0x2585 => { let (y, rh) = lower(3.0 / 8.0); fill(0, y, w, rh, 255) }
        0x2586 => { let (y, rh) = lower(2.0 / 8.0); fill(0, y, w, rh, 255) }
        0x2587 => { let (y, rh) = lower(1.0 / 8.0); fill(0, y, w, rh, 255) } // ▇
        0x2589 => fill(0, 0, r(wf * 7.0 / 8.0), h, 255), // ▉
        0x258a => fill(0, 0, r(wf * 6.0 / 8.0), h, 255),
        0x258b => fill(0, 0, r(wf * 5.0 / 8.0), h, 255),
        0x258d => fill(0, 0, r(wf * 3.0 / 8.0), h, 255),
        0x258e => fill(0, 0, r(wf * 2.0 / 8.0), h, 255),
        0x258f => fill(0, 0, r(wf / 8.0), h, 255),       // ▏
        0x2594 => fill(0, 0, w, r(hf / 8.0), 255),       // ▔ upper 1/8
        0x2595 => { let x = r(wf * 7.0 / 8.0); fill(x, 0, w - x, h, 255) } // ▕ right 1/8
        0x2591 => fill(0, 0, w, h, 64),                  // ░ 25%
        0x2592 => fill(0, 0, w, h, 128),                 // ▒ 50%
        0x2593 => fill(0, 0, w, h, 191),                 // ▓ 75%
        0x2596 => fill(0, hy, hx, h - hy, 255),          // ▖
        0x2597 => fill(hx, hy, w - hx, h - hy, 255),     // ▗
        0x2598 => fill(0, 0, hx, hy, 255),               // ▘
        0x2599 => { fill(0, 0, hx, hy, 255); fill(0, hy, w, h - hy, 255) } // ▙
        0x259a => { fill(0, 0, hx, hy, 255); fill(hx, hy, w - hx, h - hy, 255) } // ▚
        0x259b => { fill(0, 0, w, hy, 255); fill(0, hy, hx, h - hy, 255) } // ▛
        0x259c => { fill(0, 0, w, hy, 255); fill(hx, hy, w - hx, h - hy, 255) } // ▜
        0x259d => fill(hx, 0, w - hx, hy, 255),          // ▝
        0x259e => { fill(hx, 0, w - hx, hy, 255); fill(0, hy, hx, h - hy, 255) } // ▞
        0x259f => { fill(hx, 0, w - hx, hy, 255); fill(0, hy, w, h - hy, 255) } // ▟
        _ => return false,
    }
    true
}

/// Direction bitmask (1=left 2=right 4=up 8=down) for Box Drawing chars; heavy
/// and double variants are treated as light. Ported from the web's `BOX_DIRS`.
/// Used to know which edges a glyph's strokes should reach when closing gaps.
fn box_dirs(cp: u32) -> Option<u8> {
    Some(match cp {
        0x2500 | 0x2501 => 1 | 2,
        0x2502 | 0x2503 => 4 | 8,
        0x250c | 0x250f => 2 | 8,
        0x2510 | 0x2513 => 1 | 8,
        0x2514 | 0x2517 => 2 | 4,
        0x2518 | 0x251b => 1 | 4,
        0x251c | 0x2523 => 4 | 8 | 2,
        0x2524 | 0x252b => 4 | 8 | 1,
        0x252c | 0x2533 => 1 | 2 | 8,
        0x2534 | 0x253b => 1 | 2 | 4,
        0x253c | 0x254b => 1 | 2 | 4 | 8,
        0x2574 => 1,
        0x2575 => 4,
        0x2576 => 2,
        0x2577 => 8,
        0x256d => 2 | 8,
        0x256e => 1 | 8,
        0x256f => 1 | 4,
        0x2570 => 2 | 4,
        0x2550 => 1 | 2,
        0x2551 => 4 | 8,
        0x2554 => 2 | 8,
        0x2557 => 1 | 8,
        0x255a => 2 | 4,
        0x255d => 1 | 4,
        0x2560 => 4 | 8 | 2,
        0x2563 => 4 | 8 | 1,
        0x2566 => 1 | 2 | 8,
        0x2569 => 1 | 2 | 4,
        0x256c => 1 | 2 | 4 | 8,
        _ => return None,
    })
}

/// Box Drawing (U+2500–U+257F) as line segments from the cell centre to its
/// edges, ported from the web's `drawBoxGlyph`. Returns true if handled. Drawing
/// programmatically (vs the font) guarantees seamless tiling and aligned corners.
fn draw_box_glyph(atlas: &mut [u8], cp: u32, ox: u32, oy: u32, w: u32, h: u32) -> bool {
    let Some(m) = box_dirs(cp) else { return false };
    let r = |v: f32| v.round() as u32;
    let t = (r(h as f32 / 10.0)).max(1); // stroke thickness
    let ht = t / 2;
    let mid_x = r(w as f32 / 2.0);
    let mid_y = r(h as f32 / 2.0);
    let ty = mid_y.saturating_sub(ht);
    let tx = mid_x.saturating_sub(ht);
    let mut fill = |rx: u32, ry: u32, rw: u32, rh: u32| fill_rect(atlas, ox, oy, rx, ry, rw, rh, w, h, 255);
    if m & 1 != 0 {
        fill(0, ty, mid_x + ht, t); // left → centre
    }
    if m & 2 != 0 {
        fill(tx, ty, w - tx, t); // centre → right
    }
    if m & 4 != 0 {
        fill(tx, 0, t, mid_y + ht); // up → centre
    }
    if m & 8 != 0 {
        fill(tx, ty, t, h - ty); // centre → down
    }
    true
}

/// Powerline's four straight separators (U+E0B0..=U+E0B3) drawn to the exact cell, like box
/// drawing: a segment edge has to meet the cell's top, bottom and side with no gap, and a
/// font glyph scaled into the cell never quite does (the bundled symbols font draws them
/// wider than a cell and a hair taller, so fitting the width left a third of the height
/// empty). Bit 0 of the code point picks the thin chevron over the filled triangle, bit 1
/// the left-pointing mirror. 4x4 supersampled so the diagonals are smooth. Returns true if
/// `cp` was handled.
fn draw_powerline_glyph(atlas: &mut [u8], cp: u32, ox: u32, oy: u32, w: u32, h: u32) -> bool {
    if !(0xE0B0..=0xE0B3).contains(&cp) || w == 0 || h == 0 {
        return false;
    }
    let (wf, hf) = (w as f32, h as f32);
    let half_stroke = (hf / 10.0).round().max(1.0) / 2.0; // box drawing's stroke
    // Distance to the right-pointing chevron (0,0) → (wf, hf/2) → (0, hf). Folding y about
    // the middle maps both arms onto the one segment (0, hf/2) → (wf, 0).
    let chevron_distance = |x: f32, y: f32| -> f32 {
        let y = (y - hf / 2.0).abs();
        let (dx, dy) = (wf, -hf / 2.0);
        let u = ((x * dx + (y - hf / 2.0) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((x - u * dx).powi(2) + (y - (hf / 2.0 + u * dy)).powi(2)).sqrt()
    };
    let inside = |x: f32, y: f32| -> bool {
        let x = if cp & 2 != 0 { wf - x } else { x };
        if cp & 1 == 0 {
            x / wf + (2.0 * y / hf - 1.0).abs() <= 1.0
        } else {
            chevron_distance(x, y) <= half_stroke
        }
    };
    const N: u32 = 4;
    for py in 0..h {
        for px in 0..w {
            let hits = (0..N * N)
                .filter(|i| {
                    let x = px as f32 + ((i % N) as f32 + 0.5) / N as f32;
                    let y = py as f32 + ((i / N) as f32 + 0.5) / N as f32;
                    inside(x, y)
                })
                .count() as u32;
            atlas[((oy + py) * ATLAS + (ox + px)) as usize] = (hits * 255 / (N * N)) as u8;
        }
    }
    true
}

fn fill_slot(atlas: &mut [u8], slot: u32, per_row: u32, cell_w: u32, cell_h: u32, value: u8) {
    let ox = (slot % per_row) * cell_w;
    let oy = (slot / per_row) * cell_h;
    for y in 0..cell_h {
        for x in 0..cell_w {
            atlas[((oy + y) * ATLAS + (ox + x)) as usize] = value;
        }
    }
}

/// Blit a rasterised glyph into the atlas cell at (ox, oy): the bitmap's top is
/// placed `top` px above `baseline` and its left column at `left` (cell-relative),
/// clipped to the cell. Coverage is max-combined (cell starts blank).
fn blit_glyph(atlas: &mut [u8], bmp: &GlyphBitmap, baseline: f32, cell_w: u32, cell_h: u32, ox: u32, oy: u32) {
    let bw = bmp.width as i32;
    let bh = bmp.height as i32;
    let base_x = bmp.left;
    let base_y = baseline.round() as i32 - bmp.top;
    for gy in 0..bh {
        let cy = base_y + gy;
        if cy < 0 || cy >= cell_h as i32 {
            continue;
        }
        for gx in 0..bw {
            let cx = base_x + gx;
            if cx < 0 || cx >= cell_w as i32 {
                continue;
            }
            let cov = bmp.coverage[(gy * bw + gx) as usize];
            let idx = ((oy + cy as u32) * ATLAS + (ox + cx as u32)) as usize;
            if cov > atlas[idx] {
                atlas[idx] = cov;
            }
        }
    }
}

/// Move a glyph so its ink stands on the baseline like a capital letter. Icon fonts centre
/// their icons on the x-height, hanging two or three pixels under the baseline, which next
/// to a line of text reads as the icon sagging; seated, a status line's icons share the
/// ascender-to-baseline band with the letters. An icon taller than the ascent keeps its
/// top at the cell top and hangs below by the difference instead.
fn seat_on_baseline(bmp: GlyphBitmap, baseline: f32) -> GlyphBitmap {
    let ascent_rows = baseline.round().max(0.0) as u32;
    GlyphBitmap { top: bmp.height.min(ascent_rows) as i32, ..bmp }
}

/// Whether a one-cell glyph is scaled into the cell (`fit_to_box`) instead of being blitted
/// and clipped.
///
/// Windows fits every glyph: Cascadia Mono's own metrics fit the cell, so only a fallback
/// symbol ever trips it. Elsewhere only a Private Use Area glyph is fitted: the bundled
/// symbols font draws icons and Powerline separators at near full-em, half again the cell's
/// width (a 13px tag, an 11px separator, in a 7px Menlo cell), and the clip cut them in
/// half. Ordinary macOS glyphs must NOT be fitted: pixel rounding leaves an `M`'s ink ~1px
/// past the rounded cell width, and rescaling + recentering that mangles normal text.
fn fits_into_cell(ch: char) -> bool {
    cfg!(target_os = "windows") || crate::raster::is_pua(ch)
}

/// Stretch a Powerline separator to exactly the cell, each axis on its own. It is a segment
/// edge: any gap between it and the cell's top, bottom or side shows as a notch in the
/// segment colour, and the uniform `fit_to_box` leaves one (the bundled symbols font draws
/// the separators wider than a cell and a little taller, so fitting the width left a third
/// of the height empty). A patched Nerd Font sizes them to the cell the same way. The
/// straight four never get here (`draw_powerline_glyph`).
fn stretch_to_box(bmp: GlyphBitmap, box_w: u32, box_h: u32, baseline: f32) -> GlyphBitmap {
    if bmp.width == 0 || bmp.height == 0 {
        return bmp;
    }
    let coverage = if bmp.color {
        resample_rgba(&bmp.coverage, bmp.width, bmp.height, box_w, box_h)
    } else {
        resample_coverage(&bmp.coverage, bmp.width, bmp.height, box_w, box_h)
    };
    GlyphBitmap { left: 0, top: baseline.round() as i32, width: box_w, height: box_h, coverage, color: bmp.color }
}

/// Scale an oversized glyph down to fit `box_w`×`box_h`, centered, instead of letting
/// the blit clip the overflow. Fallback-font symbols Cascadia Mono lacks (e.g. `✻`
/// mono, or `⏵` as a Segoe UI Emoji colour glyph) are drawn near full-em and overflow
/// the narrow cell on Windows — clipping cut their edges/tips off. Handles both mono
/// (1 byte/px) and colour (RGBA) coverage. No-op when the glyph already fits.
fn fit_to_box(bmp: GlyphBitmap, box_w: u32, box_h: u32, baseline: f32) -> GlyphBitmap {
    let base = baseline.round() as i32;
    let top_y = base - bmp.top; // glyph's top edge vs the cell top (= blit's base_y)
    let h_over = bmp.left < 0 || bmp.left + bmp.width as i32 > box_w as i32;
    let v_over = top_y < 0 || top_y + bmp.height as i32 > box_h as i32;
    let size_over = bmp.width > box_w || bmp.height > box_h;

    if !size_over {
        if !h_over && !v_over {
            return bmp; // fits as-is
        }
        // Fits by SIZE but a skewed bearing pushes part past the cell edge (e.g. ⏵,
        // whose fallback glyph carries a large left bearing → its tip was clipped).
        // Recenter the overflowing axis only; no scaling → no quality loss.
        let left =
            if h_over { ((box_w as f32 - bmp.width as f32) / 2.0).round() as i32 } else { bmp.left };
        let top = if v_over {
            base - ((box_h as f32 - bmp.height as f32) / 2.0).round() as i32
        } else {
            bmp.top
        };
        return GlyphBitmap { left, top, ..bmp };
    }

    // A MONO fallback symbol only SLIGHTLY wider than the narrow cell, and no taller
    // (e.g. ✻ ~9px wide in a 7px cell): center it and let the blit clip the ~1px overhang,
    // keeping FULL height. Downscaling to the cell width (below) would shrink it well under
    // its natural size — Windows Terminal renders these at full size and lets them overflow.
    // Capped at +3px so a genuinely oversized glyph (or a colour emoji) still scales down
    // rather than losing big chunks to the clip; taller-than-cell glyphs scale down too.
    // And only where the clip takes spoke tips: a filled shape (⏺ from Segoe UI Symbol is
    // wider than the cell too) would come out with its sides cut off, a square.
    if !bmp.color && bmp.height <= box_h && bmp.width > box_w && bmp.width <= box_w + 3 {
        let left = ((box_w as f32 - bmp.width as f32) / 2.0).round() as i32; // negative → clipped
        if !clip_cuts_solid_ink(&bmp, (-left) as u32, box_w) {
            let top = base - ((box_h as f32 - bmp.height as f32) / 2.0).round() as i32;
            return GlyphBitmap { left, top, ..bmp };
        }
    }

    // Oversized: scale down to fit, centered. The blit draws the top at
    // `baseline - top`, so back `top` out from the desired offset from the box top.
    let s = (box_w as f32 / bmp.width as f32).min(box_h as f32 / bmp.height as f32);
    let nw = ((bmp.width as f32 * s).round() as u32).max(1);
    let nh = ((bmp.height as f32 * s).round() as u32).max(1);
    let coverage = if bmp.color {
        resample_rgba(&bmp.coverage, bmp.width, bmp.height, nw, nh)
    } else {
        resample_coverage(&bmp.coverage, bmp.width, bmp.height, nw, nh)
    };
    let left = ((box_w as f32 - nw as f32) / 2.0).round() as i32;
    let top = base - ((box_h as f32 - nh as f32) / 2.0).round() as i32;
    GlyphBitmap { left, top, width: nw, height: nh, coverage, color: bmp.color }
}

/// Whether showing only `box_w` columns of a mono glyph, starting at column `skip`, would
/// cut through solid ink: a surviving edge column that is mostly inked reads as a straight
/// edge, which is how a filled circle turns into a square. Thin spoke tips (✻) pass. Only
/// the sides actually clipped are examined.
fn clip_cuts_solid_ink(bmp: &GlyphBitmap, skip: u32, box_w: u32) -> bool {
    /// Inked share of a column at which its cut edge becomes a visible straight line.
    const SOLID: f32 = 0.4;
    if bmp.height == 0 || bmp.width == 0 {
        return false;
    }
    let column_ink = |x: u32| -> f32 {
        let sum: u32 = (0..bmp.height).map(|y| bmp.coverage[(y * bmp.width + x) as usize] as u32).sum();
        sum as f32 / (255.0 * bmp.height as f32)
    };
    let last = skip + box_w;
    (skip > 0 && column_ink(skip.min(bmp.width - 1)) > SOLID)
        || (last < bmp.width && column_ink(last - 1) > SOLID)
}

/// Bilinear-resample an 8-bit coverage bitmap from `sw`×`sh` to `dw`×`dh`.
fn resample_coverage(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh) as usize];
    let sample = |x: u32, y: u32| src[(y * sw + x) as usize] as f32;
    for dy in 0..dh {
        let sy = ((dy as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).max(0.0);
        let y0 = (sy.floor() as u32).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let fy = sy - y0 as f32;
        for dx in 0..dw {
            let sx = ((dx as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).max(0.0);
            let x0 = (sx.floor() as u32).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let fx = sx - x0 as f32;
            let t = sample(x0, y0) * (1.0 - fx) + sample(x1, y0) * fx;
            let b = sample(x0, y1) * (1.0 - fx) + sample(x1, y1) * fx;
            out[(dy * dw + dx) as usize] = (t * (1.0 - fy) + b * fy).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Bilinear-downscale a straight-alpha RGBA bitmap (4 bytes/px) from `sw`×`sh` to
/// `dw`×`dh` — the colour-glyph counterpart of [`resample_coverage`].
fn resample_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    let sample = |x: u32, y: u32, c: usize| src[((y * sw + x) * 4) as usize + c] as f32;
    for dy in 0..dh {
        let sy = ((dy as f32 + 0.5) * sh as f32 / dh as f32 - 0.5).max(0.0);
        let y0 = (sy.floor() as u32).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let fy = sy - y0 as f32;
        for dx in 0..dw {
            let sx = ((dx as f32 + 0.5) * sw as f32 / dw as f32 - 0.5).max(0.0);
            let x0 = (sx.floor() as u32).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let fx = sx - x0 as f32;
            let d = ((dy * dw + dx) * 4) as usize;
            for c in 0..4 {
                let t = sample(x0, y0, c) * (1.0 - fx) + sample(x1, y0, c) * fx;
                let b = sample(x0, y1, c) * (1.0 - fx) + sample(x1, y1, c) * fx;
                out[d + c] = (t * (1.0 - fy) + b * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Blit a colour (RGBA) glyph into the colour atlas at (ox, oy), baseline-aligned
/// like [`blit_glyph`], clipped to a `region_w`×`cell_h` box (2 cells wide for an
/// emoji). `bmp.coverage` is straight-alpha RGBA, 4 bytes/px.
fn blit_color(atlas: &mut [u8], bmp: &GlyphBitmap, baseline: f32, region_w: u32, cell_h: u32, ox: u32, oy: u32) {
    let bw = bmp.width as i32;
    let bh = bmp.height as i32;
    let base_x = bmp.left;
    let base_y = baseline.round() as i32 - bmp.top;
    for gy in 0..bh {
        let cy = base_y + gy;
        if cy < 0 || cy >= cell_h as i32 {
            continue;
        }
        for gx in 0..bw {
            let cx = base_x + gx;
            if cx < 0 || cx >= region_w as i32 {
                continue;
            }
            let s = ((gy * bw + gx) * 4) as usize;
            let d = (((oy + cy as u32) * ATLAS + (ox + cx as u32)) * 4) as usize;
            atlas[d..d + 4].copy_from_slice(&bmp.coverage[s..s + 4]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{fit_to_box, resample_coverage};
    use crate::raster::GlyphBitmap;

    fn glyph(w: u32, h: u32) -> GlyphBitmap {
        GlyphBitmap { left: 0, top: h as i32, width: w, height: h, coverage: vec![200u8; (w * h) as usize], color: false }
    }

    #[test]
    fn oversized_glyph_scaled_into_cell_keeping_aspect() {
        // A 14×14 symbol in an 8×16 cell: width-constrained → 8×8, centered, no clip.
        let out = fit_to_box(glyph(14, 14), 8, 16, 13.0);
        assert!(out.width <= 8 && out.height <= 16, "fits: {}x{}", out.width, out.height);
        assert_eq!((out.width, out.height), (8, 8));
        assert_eq!(out.coverage.len(), 64);
        assert_eq!(out.left, 0); // (8-8)/2
    }

    #[test]
    fn fitting_glyph_left_untouched() {
        let out = fit_to_box(glyph(6, 12), 8, 16, 13.0);
        assert_eq!((out.width, out.height), (6, 12));
    }

    #[test]
    fn position_overflow_recentered_without_scaling() {
        // ⏵-like: fits by size (5≤7, 8≤14) but a left bearing of 4 pushes the right
        // tip past the 7-wide cell (4+5=9). Recenter horizontally, no scaling.
        let bmp = GlyphBitmap { left: 4, top: 8, width: 5, height: 8, coverage: vec![200u8; 5 * 8], color: false };
        let out = fit_to_box(bmp, 7, 14, 11.0);
        assert_eq!((out.width, out.height), (5, 8)); // unchanged — no rescale
        assert_eq!(out.left, 1); // (7-5)/2, centered
        assert_eq!(out.top, 8); // fits vertically → untouched
    }

    /// A ✻-like cross: one full row and one full column, so the outer columns carry only
    /// a spoke tip.
    fn spoked(w: u32, h: u32) -> GlyphBitmap {
        let mut coverage = vec![0u8; (w * h) as usize];
        for x in 0..w {
            coverage[((h / 2) * w + x) as usize] = 220;
        }
        for y in 0..h {
            coverage[(y * w + w / 2) as usize] = 220;
        }
        GlyphBitmap { left: 0, top: h as i32, width: w, height: h, coverage, color: false }
    }

    #[test]
    fn wide_mono_symbol_centered_not_shrunk() {
        // ✻-like: 9px wide in a 7px cell, fits in height. Keep the full 9×10 (centered,
        // left<0 so the blit clips the ~1px overhang) instead of downscaling to ~7×8 —
        // matching Windows Terminal's full-size rendering. (A far-wider glyph, e.g. the
        // 14×14 in oversized_glyph_scaled_into_cell_keeping_aspect, still scales down.)
        let out = fit_to_box(spoked(9, 10), 7, 14, 11.0);
        assert_eq!((out.width, out.height), (9, 10)); // not shrunk
        assert_eq!(out.left, -1); // (7-9)/2 → 1px clipped each side
    }

    // ⏺-like: a filled shape 9px wide in a 7px cell. Clipping would cut straight through
    // its sides and leave a square, so it scales down instead.
    #[test]
    fn wide_filled_symbol_is_scaled_not_clipped() {
        let out = fit_to_box(glyph(9, 10), 7, 14, 11.0);
        assert_eq!(out.width, 7, "fits the cell width");
        assert!(out.left >= 0, "nothing clipped");
    }

    #[test]
    fn oversized_color_glyph_scaled_and_stays_rgba() {
        // A 16×16 colour glyph in a 1-cell (8×16) region → 8×8 RGBA, centered.
        let bmp = GlyphBitmap { left: 0, top: 16, width: 16, height: 16, coverage: vec![180u8; 16 * 16 * 4], color: true };
        let out = fit_to_box(bmp, 8, 16, 13.0);
        assert_eq!((out.width, out.height), (8, 8));
        assert!(out.color);
        assert_eq!(out.coverage.len(), 8 * 8 * 4);
    }

    #[test]
    fn resample_uniform_is_uniform_and_sized() {
        let out = resample_coverage(&vec![100u8; 16], 4, 4, 2, 2);
        assert_eq!(out.len(), 4);
        assert!(out.iter().all(|&v| v == 100));
    }

    #[test]
    fn resample_single_row_does_not_panic() {
        let out = resample_coverage(&[10, 250], 2, 1, 1, 1);
        assert_eq!(out.len(), 1);
    }
}

#[cfg(test)]
mod powerline_tests {
    use super::*;

    /// The cell `draw_powerline_glyph` draws for `cp`, row-major.
    fn cell(cp: u32, w: u32, h: u32) -> Vec<u8> {
        let mut atlas = vec![0u8; (ATLAS * ATLAS) as usize];
        assert!(draw_powerline_glyph(&mut atlas, cp, 0, 0, w, h), "U+{cp:04X} handled");
        let atlas = &atlas;
        (0..h).flat_map(|y| (0..w).map(move |x| atlas[(y * ATLAS + x) as usize])).collect()
    }

    // The filled right-pointing separator: its base is the cell's left edge at full height,
    // its tip meets the right edge at mid height, and the right corners stay empty. The
    // left-pointing one is the mirror.
    #[test]
    fn filled_triangle_spans_the_cell() {
        let (w, h) = (8u32, 16u32);
        let px = cell(0xE0B0, w, h);
        let at = |x: u32, y: u32| px[(y * w + x) as usize];
        assert!((1..h - 1).all(|y| at(0, y) == 255), "base column solid");
        assert!((0..w).all(|x| at(x, h / 2) > 0), "mid row inked to the right edge");
        assert_eq!((at(w - 1, 0), at(w - 1, h - 1)), (0, 0), "right corners empty");
        let mirrored = cell(0xE0B2, w, h);
        assert!((1..h - 1).all(|y| mirrored[(y * w + w - 1) as usize] == 255), "left-pointing: base on the right");
        assert_eq!(mirrored[0], 0, "and its top-left corner empty");
    }

    // The thin right-pointing separator is a stroke from the top-left corner to the right
    // edge's middle and back to the bottom-left corner: the cell's centre and the middle
    // of its left edge stay clear, and it carries less ink than the filled one.
    #[test]
    fn thin_chevron_is_a_stroke_not_a_fill() {
        let (w, h) = (8u32, 16u32);
        let px = cell(0xE0B1, w, h);
        let at = |x: u32, y: u32| px[(y * w + x) as usize];
        assert!(at(0, 0) > 0 && at(0, h - 1) > 0, "starts and ends at the left corners");
        assert!(at(w - 1, h / 2) > 0 || at(w - 1, h / 2 - 1) > 0, "reaches the right edge at mid height");
        assert_eq!((at(w / 2, h / 2), at(0, h / 2)), (0, 0), "centre and left middle clear");
        let ink: u32 = px.iter().map(|&c| c as u32).sum();
        let filled: u32 = cell(0xE0B0, w, h).iter().map(|&c| c as u32).sum();
        assert!(ink < filled, "thin: {ink} of ink against the filled {filled}");
        assert!(!draw_powerline_glyph(&mut vec![0u8; (ATLAS * ATLAS) as usize], 0xE0B4, 0, 0, w, h), "rounded: not drawn here");
    }

    // Any other separator (a rounded one, say) is stretched to exactly the cell, each axis
    // on its own: the bundled font's 11x15 glyph in a 7x14 cell becomes 7x14 at the cell's
    // top-left, where uniform fitting had left a third of the height empty.
    #[test]
    fn other_separators_are_stretched_to_the_cell() {
        let bmp = GlyphBitmap { left: -2, top: 12, width: 11, height: 15, coverage: vec![255; 11 * 15], color: false };
        let out = stretch_to_box(bmp, 7, 14, 11.0);
        assert_eq!((out.width, out.height, out.left, out.top), (7, 14, 0, 11));
        assert!(out.coverage.iter().all(|&c| c == 255), "solid stays solid");
        use crate::raster::{is_icon, is_powerline_separator};
        assert!(is_powerline_separator('\u{E0B4}') && is_powerline_separator('\u{E0D7}'));
        assert!(!is_powerline_separator('\u{E0A0}'), "the branch symbol is an ordinary glyph");
        assert!(!is_icon('\u{E0B4}'), "and a separator never takes a second cell");
    }
}

#[cfg(test)]
mod icon_fit_tests {
    use super::*;

    fn icon(w: u32, h: u32) -> GlyphBitmap {
        GlyphBitmap { left: 1, top: 12, width: w, height: h, coverage: vec![255; (w * h) as usize], color: false }
    }

    // A status-line icon at the text size (a 14x14 tag; the cell is 9x19, baseline 15)
    // is squeezed into one cell but kept whole in two: that is all the two-cell path does.
    #[test]
    fn two_cells_keep_an_icon_at_its_natural_size_where_one_shrinks_it() {
        let one = fit_to_box(icon(14, 14), 9, 19, 15.0);
        assert!(one.width <= 9 && one.height < 14, "one cell: {}x{}", one.width, one.height);
        let two = fit_to_box(icon(14, 14), 18, 19, 15.0);
        assert_eq!((two.width, two.height), (14, 14), "two cells: untouched");
        assert_eq!(two.left, 1, "and left where the font put it, so the gap stays on the right");
    }

    // An icon drawn hanging under the baseline (top 10 of height 12: two rows below) is
    // seated so its bottom row is the last row above the baseline; one taller than the
    // ascent keeps its top at the cell top instead.
    #[test]
    fn a_two_cell_icon_is_seated_on_the_baseline() {
        let hanging = GlyphBitmap { top: 10, ..icon(13, 12) };
        assert_eq!(seat_on_baseline(hanging, 15.05).top, 12);
        let tall = GlyphBitmap { top: 13, ..icon(14, 17) };
        assert_eq!(seat_on_baseline(tall, 15.05).top, 15, "hangs 2 below rather than poking above the cell");
    }

    // An icon that did NOT get a second cell (the next cell holds text) is fitted into the
    // one it has on every platform, not left to the blit's clip: from the bundled symbols
    // font it is drawn near full-em, about twice a Menlo cell's width, and clipping showed
    // half a glyph. Powerline's separators, which never take a second cell, likewise.
    #[test]
    fn a_one_cell_icon_is_fitted_everywhere_but_ordinary_text_is_not() {
        for ch in ['\u{F02B}', '\u{E0B0}'] {
            assert!(fits_into_cell(ch), "U+{:04X}", ch as u32);
        }
        assert_eq!(fits_into_cell('M'), cfg!(target_os = "windows"), "ordinary text");

        // A 13x13 tag and an 11x15 separator, measured at Menlo 12px: cell 7x14, baseline 11.
        for (w, h) in [(13u32, 13u32), (11, 15)] {
            let out = fit_to_box(icon(w, h), 7, 14, 11.0);
            assert!(out.width <= 7 && out.height <= 14, "{w}x{h} came out {}x{}", out.width, out.height);
            assert!(out.left >= 0, "and inside the cell, not clipped: left {}", out.left);
        }
    }
}

