//! wgpu terminal renderer: a glyph atlas + one instanced quad per cell, ported
//! from the web `singleCanvasRenderer.ts`. `TermGpu` is surface-agnostic — it
//! draws into a *provided* render pass, so it works both inside a winit window
//! surface (`Renderer`, the raw spike) and inside an Iced `shader` widget.

use std::collections::HashMap;
use std::sync::Arc;

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

    font_name: String,
    regular: (Vec<u8>, u32),
    bold_face: Option<(Vec<u8>, u32)>,
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
    glyphs: HashMap<(char, bool), Glyph>,
    next_slot: u32,
    color_next: u32,
    per_row: u32,
    atlas_dirty: bool,
    color_dirty: bool,

    scratch: Vec<f32>,
    count: u32,
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
        spec: &crate::font::FontSpec,
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
            font_name: spec.name.clone(), regular: spec.regular.clone(), bold_face: spec.bold.clone(),
            em_px, scale, built_pts, cell_w, cell_h, baseline,
            is_srgb: format.is_srgb(),
            atlas_cpu, color_atlas_cpu, glyphs: HashMap::new(), next_slot: 2, color_next: 0,
            per_row, atlas_dirty: true, color_dirty: true,
            scratch: Vec::new(), count: 0,
        }
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

    /// Reserve `cells` horizontally-contiguous slots in the colour atlas (a wide
    /// emoji needs 2), never straddling a row wrap.
    fn alloc_color(&mut self, cells: u32) -> u32 {
        let col = self.color_next % self.per_row;
        if col + cells > self.per_row {
            self.color_next += self.per_row - col; // skip the row's tail
        }
        let slot = self.color_next;
        self.color_next += cells;
        slot
    }

    /// Resolve `ch` (regular/bold) to a cached glyph: a programmatic block/box glyph
    /// or a rasterised mono glyph in the R8 atlas, or a colour glyph (emoji) in the
    /// RGBA atlas spanning `wide_hint ? 2 : 1` cells.
    fn slot_for(&mut self, ch: char, bold: bool, wide_hint: bool) -> Glyph {
        if ch == ' ' || ch == '\0' {
            return Glyph { slot: SLOT_BLANK, color: false, cells: 1 };
        }
        if let Some(&g) = self.glyphs.get(&(ch, bold)) {
            return g;
        }
        let cp = ch as u32;
        // The next free mono slot (only consumed if we actually draw a mono glyph).
        let mslot = self.next_slot;
        let ox = (mslot % self.per_row) * self.cell_w;
        let oy = (mslot / self.per_row) * self.cell_h;
        // Block Elements + Box Drawing are drawn programmatically with consistent
        // stroke centres so lines AND corners tile seamlessly — what the web's
        // canvas renderer and GPU terminals (Alacritty/Kitty/WezTerm) do.
        // Font-rendering them leaves sub-pixel gaps and, for rounded corners
        // Menlo lacks, mismatched glyphs from a fallback font.
        if draw_block_glyph(&mut self.atlas_cpu, cp, ox, oy, self.cell_w, self.cell_h)
            || draw_box_glyph(&mut self.atlas_cpu, cp, ox, oy, self.cell_w, self.cell_h)
        {
            self.next_slot += 1;
            self.atlas_dirty = true;
            let g = Glyph { slot: mslot, color: false, cells: 1 };
            self.glyphs.insert((ch, bold), g);
            return g;
        }
        // Pick the bold face when we carry one (swash path); otherwise pass the
        // regular bytes and let the rasteriser synthesise bold (CoreText).
        let (data, index) = match (bold, &self.bold_face) {
            (true, Some(b)) => (b.0.as_slice(), b.1),
            _ => (self.regular.0.as_slice(), self.regular.1),
        };
        // Also hand over the bold-face bytes regardless of weight: the DirectWrite
        // path loads them into a real bold IDWriteFontFace so bold renders the bundled
        // bold (not a synthesised faux-bold). Other platforms ignore this.
        let bold_data = self.bold_face.as_ref().map(|(b, _)| b.as_slice());
        let raster =
            crate::raster::rasterize(&self.font_name, data, index, bold_data, self.em_px, ch, bold);
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
                self.color_dirty = true;
                Glyph { slot, color: true, cells }
            }
            Some(bmp) => {
                // Scale oversized fallback symbols to fit the cell — WINDOWS ONLY.
                // On macOS the fallback glyphs already fit, and pixel-rounding can
                // leave an ordinary glyph's ink ~1px past the rounded cell width,
                // which would wrongly trigger a rescale + recenter and mangle normal
                // text. Cascadia's metrics on Windows fit, so only real symbols trip it.
                #[cfg(target_os = "windows")]
                let bmp = fit_to_box(bmp, self.cell_w, self.cell_h, self.baseline);
                blit_glyph(&mut self.atlas_cpu, &bmp, self.baseline, self.cell_w, self.cell_h, ox, oy);
                self.next_slot += 1;
                self.atlas_dirty = true;
                Glyph { slot: mslot, color: false, cells: 1 }
            }
            // No glyph anywhere → blank (don't consume the mono slot).
            None => Glyph { slot: SLOT_BLANK, color: false, cells: 1 },
        };
        self.glyphs.insert((ch, bold), g);
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

        self.scratch.clear();
        for (row, col, c, fg, bg, bold, wide) in &cells {
            let g = self.slot_for(*c, *bold, *wide);
            let (u, v) = self.uv(g.slot);
            let kind = if g.color { 1.0 } else { 0.0 };
            self.scratch.extend_from_slice(&[
                *col as f32 * cw, *row as f32 * ch, u, v,
                fg[0], fg[1], fg[2], bg[0], bg[1], bg[2],
                kind, g.cells as f32,
            ]);
        }
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

        if self.atlas_dirty {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.atlas_cpu,
                wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(ATLAS), rows_per_image: Some(ATLAS) },
                wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            );
            self.atlas_dirty = false;
        }
        if self.color_dirty {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.color_atlas_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &self.color_atlas_cpu,
                wgpu::ImageDataLayout { offset: 0, bytes_per_row: Some(ATLAS * 4), rows_per_image: Some(ATLAS) },
                wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            );
            self.color_dirty = false;
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

        let gpu = TermGpu::new(&device, format, spec, scale);
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

/// Scale an oversized glyph down to fit `box_w`×`box_h`, centered, instead of letting
/// the blit clip the overflow. Fallback-font symbols Cascadia Mono lacks (e.g. `✻`
/// mono, or `⏵` as a Segoe UI Emoji colour glyph) are drawn near full-em and overflow
/// the narrow cell on Windows — clipping cut their edges/tips off. Handles both mono
/// (1 byte/px) and colour (RGBA) coverage. No-op when the glyph already fits.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
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
    if !bmp.color && bmp.height <= box_h && bmp.width > box_w && bmp.width <= box_w + 3 {
        let left = ((box_w as f32 - bmp.width as f32) / 2.0).round() as i32; // negative → clipped
        let top = base - ((box_h as f32 - bmp.height as f32) / 2.0).round() as i32;
        return GlyphBitmap { left, top, ..bmp };
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

/// Bilinear-downscale an 8-bit coverage bitmap from `sw`×`sh` to `dw`×`dh`.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
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
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
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

    #[test]
    fn wide_mono_symbol_centered_not_shrunk() {
        // ✻-like: 9px wide in a 7px cell, fits in height. Keep the full 9×10 (centered,
        // left<0 so the blit clips the ~1px overhang) instead of downscaling to ~7×8 —
        // matching Windows Terminal's full-size rendering. (A far-wider glyph, e.g. the
        // 14×14 in oversized_glyph_scaled_into_cell_keeping_aspect, still scales down.)
        let out = fit_to_box(glyph(9, 10), 7, 14, 11.0);
        assert_eq!((out.width, out.height), (9, 10)); // not shrunk
        assert_eq!(out.left, -1); // (7-9)/2 → 1px clipped each side
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
