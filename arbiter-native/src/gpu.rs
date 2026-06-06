//! wgpu single-surface terminal renderer: a glyph atlas + one instanced quad
//! per cell. Direct port of the web `singleCanvasRenderer.ts` design — except
//! it draws into the window's own GPU surface (no transparent overlay, no
//! webview compositor). One instance = one cell; the shader does
//! mix(bg, fg, glyphCoverage).

use std::collections::HashMap;
use std::sync::Arc;

use ab_glyph::{Font, FontVec, ScaleFont};
use wgpu::util::DeviceExt;
use winit::window::Window;

const ATLAS: u32 = 1024;
const SLOT_SOLID: u32 = 0; // fully-covered cell (block cursor)
const SLOT_BLANK: u32 = 1; // empty coverage (bg-only cells)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    canvas: [f32; 2],
    cell: [f32; 2],
    glyph: [f32; 2],
    _pad: [f32; 2],
}

const SHADER: &str = r#"
struct Uniforms { canvas: vec2<f32>, cell: vec2<f32>, glyph: vec2<f32> };
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var atlas: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) fg: vec3<f32>,
  @location(2) bg: vec3<f32>,
};

@vertex
fn vs(
  @location(0) corner: vec2<f32>,
  @location(1) pos: vec2<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) fg: vec3<f32>,
  @location(4) bg: vec3<f32>,
) -> VsOut {
  let px = pos + corner * u.cell;
  let clip = vec2<f32>((px.x / u.canvas.x) * 2.0 - 1.0, 1.0 - (px.y / u.canvas.y) * 2.0);
  var out: VsOut;
  out.clip = vec4<f32>(clip, 0.0, 1.0);
  out.uv = uv + corner * u.glyph;
  out.fg = fg;
  out.bg = bg;
  return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
  let a = textureSample(atlas, samp, in.uv).r;
  return vec4<f32>(mix(in.bg, in.fg, a), 1.0);
}
"#;

pub struct Renderer {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    quad_vb: wgpu::Buffer,
    inst_vb: wgpu::Buffer,
    inst_cap: u64, // capacity in instances
    uniform_buf: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    atlas_tex: wgpu::Texture,

    // CPU glyph atlas.
    font: FontVec,
    px: f32,
    pub cell_w: u32,
    pub cell_h: u32,
    ascent: f32,
    atlas_cpu: Vec<u8>,
    glyphs: HashMap<char, u32>,
    next_slot: u32,
    per_row: u32,
    atlas_dirty: bool,

    // Reusable per-frame instance scratch (10 f32 per instance).
    scratch: Vec<f32>,
}

impl Renderer {
    pub async fn new(window: Arc<Window>, font_bytes: Vec<u8>, font_index: u32, scale: f32) -> Self {
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
        // Prefer a non-sRGB 8-bit format so our 0..1 colours aren't gamma-shifted.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo, // vsync
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        // ── Glyph atlas (CPU) ────────────────────────────────────────────────
        let font = FontVec::try_from_vec_and_index(font_bytes, font_index).expect("load font");
        let px = (14.0 * scale).round().max(8.0);
        let scaled = font.as_scaled(px);
        let advance = scaled.h_advance(font.glyph_id('M'));
        let cell_w = advance.ceil().max(1.0) as u32;
        let ascent = scaled.ascent();
        let cell_h = (scaled.ascent() - scaled.descent() + scaled.line_gap()).ceil().max(1.0) as u32;
        let per_row = (ATLAS / cell_w).max(1);
        let mut atlas_cpu = vec![0u8; (ATLAS * ATLAS) as usize];
        // Slot 0 = solid block (cursor).
        fill_slot(&mut atlas_cpu, SLOT_SOLID, per_row, cell_w, cell_h, 255);
        // Slot 1 stays blank.

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

        // ── Pipeline ─────────────────────────────────────────────────────────
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("term-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
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
                compilation_options: Default::default(),
                buffers: &[
                    // quad corners (per-vertex)
                    wgpu::VertexBufferLayout {
                        array_stride: 2 * 4,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2],
                    },
                    // per-instance: pos(2) uv(2) fg(3) bg(3) = 10 f32
                    wgpu::VertexBufferLayout {
                        array_stride: 10 * 4,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            1 => Float32x2, 2 => Float32x2, 3 => Float32x3, 4 => Float32x3
                        ],
                    },
                ],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs",
                compilation_options: Default::default(),
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
            cache: None,
        });

        let quad_vb = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad"),
            contents: bytemuck::cast_slice(&[0.0f32, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0]),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let inst_cap = 8192u64;
        let inst_vb = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("inst"),
            size: inst_cap * 10 * 4,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            device, queue, surface, config, pipeline, quad_vb, inst_vb, inst_cap,
            uniform_buf, bind_group, atlas_tex,
            font, px, cell_w, cell_h, ascent,
            atlas_cpu, glyphs: HashMap::new(), next_slot: 2, per_row, atlas_dirty: true,
            scratch: Vec::new(),
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    /// Map a char to an atlas slot, rasterising it on first use.
    fn slot_for(&mut self, ch: char) -> u32 {
        if ch == ' ' || ch == '\0' {
            return SLOT_BLANK;
        }
        if let Some(&s) = self.glyphs.get(&ch) {
            return s;
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        self.glyphs.insert(ch, slot);
        let col = slot % self.per_row;
        let row = slot / self.per_row;
        let ox = col * self.cell_w;
        let oy = row * self.cell_h;
        rasterize_into(
            &self.font, &mut self.atlas_cpu, self.px, self.ascent,
            self.cell_w, self.cell_h, ox, oy, ch,
        );
        self.atlas_dirty = true;
        slot
    }

    fn uv(&self, slot: u32) -> (f32, f32) {
        let col = slot % self.per_row;
        let row = slot / self.per_row;
        ((col * self.cell_w) as f32 / ATLAS as f32, (row * self.cell_h) as f32 / ATLAS as f32)
    }

    /// Build the instance list from the terminal grid and draw one frame.
    pub fn render(&mut self, term: &crate::term::VtTerm) {
        let cw = self.cell_w as f32;
        let ch = self.cell_h as f32;
        let default_bg = term.default_bg();

        // Pre-resolve every glyph slot we need (mutates the atlas), so the
        // instance-building borrow of `self.scratch` doesn't clash with `slot_for`.
        let (cur_row, cur_col, cur_vis) = term.cursor();
        let mut cells: Vec<(usize, usize, u32, [f32; 3], [f32; 3])> = Vec::new();
        term.for_each_cell(|row, col, c, fg, bg| {
            let blank = (c == ' ' || c == '\0') && bg == default_bg;
            if blank {
                return;
            }
            cells.push((row, col, 0, fg, bg)); // slot filled below
            let _ = c;
        });
        // Resolve slots (second pass — needs &mut self).
        {
            let mut i = 0;
            term.for_each_cell(|_row, _col, c, fg, bg| {
                let blank = (c == ' ' || c == '\0') && bg == default_bg;
                if blank {
                    return;
                }
                let s = self.slot_for(c);
                if i < cells.len() {
                    cells[i].2 = s;
                }
                i += 1;
                let _ = (fg, bg);
            });
        }

        // Pack instances.
        self.scratch.clear();
        for (row, col, slot, fg, bg) in &cells {
            let (u, v) = self.uv(*slot);
            self.scratch.extend_from_slice(&[
                *col as f32 * cw, *row as f32 * ch, u, v,
                fg[0], fg[1], fg[2], bg[0], bg[1], bg[2],
            ]);
        }
        if cur_vis {
            let (u, v) = self.uv(SLOT_SOLID);
            let cur = [0.76f32, 0.54, 0.14]; // amber block
            self.scratch.extend_from_slice(&[
                cur_col as f32 * cw, cur_row as f32 * ch, u, v,
                cur[0], cur[1], cur[2], cur[0], cur[1], cur[2],
            ]);
        }
        let count = (self.scratch.len() / 10) as u32;

        // Upload atlas if new glyphs were added.
        if self.atlas_dirty {
            self.queue.write_texture(
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

        // Grow the instance buffer if needed.
        if count as u64 > self.inst_cap {
            self.inst_cap = (count as u64).next_power_of_two();
            self.inst_vb = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("inst"),
                size: self.inst_cap * 10 * 4,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        self.queue.write_buffer(&self.inst_vb, 0, bytemuck::cast_slice(&self.scratch));

        // Uniforms.
        let u = Uniforms {
            canvas: [self.config.width as f32, self.config.height as f32],
            cell: [cw, ch],
            glyph: [cw / ATLAS as f32, ch / ATLAS as f32],
            _pad: [0.0, 0.0],
        };
        self.queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));

        // Draw.
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
                            r: default_bg[0] as f64,
                            g: default_bg[1] as f64,
                            b: default_bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if count > 0 {
                rp.set_pipeline(&self.pipeline);
                rp.set_bind_group(0, &self.bind_group, &[]);
                rp.set_vertex_buffer(0, self.quad_vb.slice(..));
                rp.set_vertex_buffer(1, self.inst_vb.slice(..));
                rp.draw(0..4, 0..count);
            }
        }
        self.queue.submit([enc.finish()]);
        frame.present();
    }
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

#[allow(clippy::too_many_arguments)]
fn rasterize_into(
    font: &FontVec,
    atlas: &mut [u8],
    px: f32,
    ascent: f32,
    cell_w: u32,
    cell_h: u32,
    ox: u32,
    oy: u32,
    ch: char,
) {
    let glyph = font
        .glyph_id(ch)
        .with_scale_and_position(px, ab_glyph::point(0.0, ascent));
    if let Some(outlined) = font.outline_glyph(glyph) {
        let b = outlined.px_bounds();
        outlined.draw(|gx, gy, c| {
            let xx = b.min.x as i32 + gx as i32;
            let yy = b.min.y as i32 + gy as i32;
            if xx >= 0 && (xx as u32) < cell_w && yy >= 0 && (yy as u32) < cell_h {
                let ax = ox + xx as u32;
                let ay = oy + yy as u32;
                let idx = (ay * ATLAS + ax) as usize;
                let cov = (c * 255.0) as u8;
                if cov > atlas[idx] {
                    atlas[idx] = cov;
                }
            }
        });
    }
}
