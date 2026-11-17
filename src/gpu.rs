//! wgpu renderer: one pipeline, one glyph atlas, one instanced draw call per frame.
//! Every primitive is a quad instance: solid/rounded rect, alpha-mask glyph, or color glyph.
use std::sync::Arc;
use winit::window::Window;

/// Glyph atlas side, in texels, at start. When it fills up, `Fonts` drops every cache, doubles
/// it (up to `ATLAS_MAX`) and the frame is redrawn: small by default, big only when needed (CJK,
/// emoji, large zoom), and never thrashing.
pub const ATLAS: u32 = 1024;
pub const ATLAS_MAX: u32 = 4096;

/// `Inst::misc[0]` kinds: solid (optionally rounded) rect, alpha-mask glyph, color glyph (emoji).
pub const RECT: f32 = 0.0;
pub const MASK: f32 = 1.0;
pub const COLOR: f32 = 2.0;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Inst {
    pub rect: [f32; 4],  // x, y, w, h (physical px)
    pub uv: [f32; 4],    // atlas x, y, w, h (texels)
    pub color: [f32; 4], // rgba 0..1 (sRGB)
    pub misc: [f32; 4],  // kind, corner radius, -, -
}

const SHADER: &str = r#"
struct U { screen: vec2<f32>, atlas: vec2<f32> };
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct In {
  @location(0) rect: vec4<f32>,
  @location(1) uv: vec4<f32>,
  @location(2) color: vec4<f32>,
  @location(3) misc: vec4<f32>,
};
struct Out {
  @builtin(position) pos: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) color: vec4<f32>,
  @location(2) local: vec2<f32>,
  @location(3) @interpolate(flat) misc: vec4<f32>,
  @location(4) @interpolate(flat) size: vec2<f32>,
};

@vertex fn vs(@builtin(vertex_index) vi: u32, i: In) -> Out {
  let c = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
  let p = i.rect.xy + c * i.rect.zw;
  var o: Out;
  o.pos = vec4<f32>(p.x / u.screen.x * 2.0 - 1.0, 1.0 - p.y / u.screen.y * 2.0, 0.0, 1.0);
  o.uv = (i.uv.xy + c * i.uv.zw) / u.atlas;
  o.color = i.color;
  o.local = c * i.rect.zw;
  o.misc = i.misc;
  o.size = i.rect.zw;
  return o;
}

@fragment fn fs(o: Out) -> @location(0) vec4<f32> {
  let kind = o.misc.x;
  if (kind < 0.5) {
    var a = o.color.a;
    let r = o.misc.y;
    if (r > 0.0) {
      let h = o.size * 0.5;
      let q = abs(o.local - h) - h + vec2<f32>(r);
      let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
      a = a * clamp(0.5 - d, 0.0, 1.0);
    }
    return vec4<f32>(o.color.rgb * a, a);
  }
  let t = textureSampleLevel(tex, samp, o.uv, 0.0);
  if (kind < 1.5) {
    let a = t.a * o.color.a;
    return vec4<f32>(o.color.rgb * a, a);
  }
  return vec4<f32>(t.rgb * t.a, t.a);
}
"#;

/// The window surface plus the single pipeline, atlas and instance buffer every frame draws with.
pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind: wgpu::BindGroup,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    atlas: wgpu::Texture,
    atlas_size: u32,
    inst: wgpu::Buffer,
    inst_cap: usize,
}

impl Gpu {
    pub fn new(window: Arc<Window>) -> Self {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor { backends: wgpu::Backends::PRIMARY, ..Default::default() });
        let surface = instance.create_surface(window.clone()).expect("surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("no GPU adapter");
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
        let caps = surface.get_capabilities(&adapter);
        // Blend in sRGB space (like native terminals): pick a non-sRGB surface format.
        let format = caps.formats.iter().copied().find(|f| !f.is_srgb()).unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 1,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let atlas = Self::atlas_texture(&device, ATLAS);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor::default()); // nearest: glyphs sit on whole pixels
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniform"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
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
        let bind = Self::bind_group(&device, &bgl, &uniform, &atlas, &sampler);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: None, source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[&bgl], push_constant_ranges: &[] });
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Inst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attrs,
                }],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let inst_cap = 4096;
        let inst = Self::inst_buffer(&device, inst_cap);
        Gpu { surface, device, queue, config, pipeline, bind, bgl, sampler, uniform, atlas, atlas_size: ATLAS, inst, inst_cap }
    }

    fn atlas_texture(device: &wgpu::Device, size: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas"),
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    fn bind_group(device: &wgpu::Device, bgl: &wgpu::BindGroupLayout, uniform: &wgpu::Buffer, atlas: &wgpu::Texture, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
        let view = atlas.create_view(&Default::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        })
    }

    /// Replace the atlas with an empty one of `size`² texels (its glyphs were dropped too).
    pub fn set_atlas_size(&mut self, size: u32) {
        if size == self.atlas_size {
            return;
        }
        self.atlas = Self::atlas_texture(&self.device, size);
        self.bind = Self::bind_group(&self.device, &self.bgl, &self.uniform, &self.atlas, &self.sampler);
        self.atlas_size = size;
    }

    fn inst_buffer(device: &wgpu::Device, n: usize) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (n * std::mem::size_of::<Inst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    /// Copy an RGBA image into the atlas.
    pub fn upload(&self, x: u32, y: u32, w: u32, h: u32, rgba: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &self.atlas, mip_level: 0, origin: wgpu::Origin3d { x, y, z: 0 }, aspect: wgpu::TextureAspect::All },
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }

    pub fn render(&mut self, insts: &[Inst], clear: [f32; 3]) {
        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(_) => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let enc = self.encode(&view, insts, clear);
        self.queue.submit([enc.finish()]);
        frame.present();
    }

    /// Render offscreen and read back as top-down BGRA/RGBA rows (debug: `pmux snapshot`).
    pub fn snapshot(&mut self, insts: &[Inst], clear: [f32; 3]) -> (u32, u32, Vec<u8>, bool) {
        let (w, h) = (self.config.width, self.config.height);
        let tex = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("snapshot"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.config.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let mut enc = self.encode(&view, insts, clear);
        let row = (w * 4).div_ceil(256) * 256;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        let data = slice.get_mapped_range();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            let s = (y * row) as usize;
            out.extend_from_slice(&data[s..s + (w * 4) as usize]);
        }
        let bgra = matches!(self.config.format, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb);
        (w, h, out, bgra)
    }

    fn encode(&mut self, view: &wgpu::TextureView, insts: &[Inst], clear: [f32; 3]) -> wgpu::CommandEncoder {
        if insts.len() > self.inst_cap {
            self.inst_cap = insts.len().next_power_of_two();
            self.inst = Self::inst_buffer(&self.device, self.inst_cap);
        }
        let u = [self.config.width as f32, self.config.height as f32, self.atlas_size as f32, self.atlas_size as f32];
        self.queue.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&u));
        if !insts.is_empty() {
            self.queue.write_buffer(&self.inst, 0, bytemuck::cast_slice(insts));
        }
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r: clear[0] as f64, g: clear[1] as f64, b: clear[2] as f64, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            if !insts.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind, &[]);
                pass.set_vertex_buffer(0, self.inst.slice(..));
                pass.draw(0..4, 0..insts.len() as u32);
            }
        }
        enc
    }
}

/// Minimal 32-bit BMP writer (no image crate needed).
pub fn write_bmp(path: &str, w: u32, h: u32, px: &[u8], bgra: bool) -> std::io::Result<()> {
    let size = 54 + w * h * 4;
    let mut f = Vec::with_capacity(size as usize);
    f.extend_from_slice(b"BM");
    for v in [size, 0, 54, 40] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.extend_from_slice(&(w as i32).to_le_bytes());
    f.extend_from_slice(&(-(h as i32)).to_le_bytes()); // top-down
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&32u16.to_le_bytes());
    for v in [0u32, w * h * 4, 2835, 2835, 0, 0] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    for p in px.chunks(4) {
        if bgra {
            f.extend_from_slice(&[p[0], p[1], p[2], 255]);
        } else {
            f.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
    }
    std::fs::write(path, f)
}
