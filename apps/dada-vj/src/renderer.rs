//! wgpu (Metal) renderer.
//!
//! Két célfelületet támogat:
//! * **Surface** — ablak/projektor kimenet (swapchain),
//! * **Offscreen** — textúra (layer FX-lánchoz és headless benchmarkhoz).
//!
//! A kettő ugyanazt a pipeline-t és shadert használja, így az offscreen mérés
//! pontosan azt a költséget adja, amit élőben a rétegek jelentenek.
//!
//! GPU-idő méréshez timestamp query-t használunk (ha a driver támogatja).

use anyhow::{Context, Result};
use dada_core::BlendMode;
use dada_fx::{blend_index, blend_state};
use std::sync::Arc;

/// A négy támogatott blend mód sorrendje.
pub const BLEND_MODES: [BlendMode; 4] = [
    BlendMode::Normal,
    BlendMode::Add,
    BlendMode::Multiply,
    BlendMode::Screen,
];

/// Négy pipeline építése a blend módokhoz.
fn build_mode_pipelines(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    make: impl Fn(wgpu::BlendState) -> wgpu::RenderPipeline,
) -> [wgpu::RenderPipeline; 4] {
    [
        make(blend_state(BLEND_MODES[0])),
        make(blend_state(BLEND_MODES[1])),
        make(blend_state(BLEND_MODES[2])),
        make(blend_state(BLEND_MODES[3])),
    ]
}
use wgpu::util::DeviceExt;
use winit::window::Window;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    resolution: [f32; 2],
    time: f32,
    beat: f32,
}

enum Target {
    Surface {
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    },
    Offscreen {
        _texture: wgpu::Texture,
        view: wgpu::TextureView,
        size: (u32, u32),
    },
}

/// GPU-idő mérése timestamp query-vel.
struct Timers {
    query_set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
    /// Üres blit művelet, ami arra kényszeríti a Metal backendet, hogy a függő
    /// timestamp-mintákat *ott* rögzítse, ne a parancssor végén.
    flush: wgpu::Buffer,
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    proc_pipelines: [wgpu::RenderPipeline; 4],
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    target: Target,
    format: wgpu::TextureFormat,
    adapter_name: String,
    timers: Option<Timers>,
    timestamp_period_ns: f32,
    video_bgl: wgpu::BindGroupLayout,
    video_sampler: wgpu::Sampler,
    video_pipelines: [wgpu::RenderPipeline; 4],
    video_params: wgpu::Buffer,
    preview: Option<PreviewTarget>,
}

impl Renderer {
    pub async fn new_surface(window: Arc<Window>, vsync: bool) -> Result<Self> {
        let size = window.inner_size();
        let width = size.width.max(1);
        let height = size.height.max(1);

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..Default::default()
        });
        let surface = instance
            .create_surface(window.clone())
            .context("surface létrehozása")?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("nem találtam megfelelő GPU adaptert")?;

        let info = adapter.get_info();
        let adapter_name = format!("{} ({:?}, {:?})", info.name, info.device_type, info.backend);
        log::info!("GPU: {adapter_name}");

        let features = requested_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("dada-device"),
                    required_features: features,
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .context("device létrehozása")?;
        let timers = create_timers(&device, features);
        let timestamp_period_ns = queue.get_timestamp_period();

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        log::info!(
            "surface: formátum={format:?} present={:?} alpha={:?}",
            caps.present_modes,
            caps.alpha_modes
        );

        let proc_pipelines = create_proc_pipelines(&device, format);
        let (video_bgl, video_sampler, video_pipelines, video_params) =
            create_video_resources(&device, format);
        let (uniform_buffer, bind_group) = create_uniforms(&device, width, height);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width,
            height,
            present_mode: if vsync {
                wgpu::PresentMode::AutoVsync
            } else {
                wgpu::PresentMode::AutoNoVsync
            },
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        Ok(Self {
            device,
            queue,
            proc_pipelines,
            bind_group,
            uniform_buffer,
            target: Target::Surface { surface, config },
            format,
            adapter_name,
            timers,
            timestamp_period_ns,
            video_bgl,
            video_sampler,
            video_pipelines,
            video_params,
            preview: None,
        })
    }

    /// Ablak nélküli renderelés: layer FX-hez és benchmarkhoz.
    pub async fn new_offscreen(
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<Self> {
        let width = width.max(1);
        let height = height.max(1);
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .context("nem találtam megfelelő GPU adaptert")?;
        let info = adapter.get_info();
        let adapter_name = format!("{} ({:?}, {:?})", info.name, info.device_type, info.backend);
        let features = requested_features(&adapter);
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("dada-device-offscreen"),
                    required_features: features,
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .context("device létrehozása")?;
        let timers = create_timers(&device, features);
        let timestamp_period_ns = queue.get_timestamp_period();

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let proc_pipelines = create_proc_pipelines(&device, format);
        let (video_bgl, video_sampler, video_pipelines, video_params) =
            create_video_resources(&device, format);
        let (uniform_buffer, bind_group) = create_uniforms(&device, width, height);

        Ok(Self {
            device,
            queue,
            proc_pipelines,
            bind_group,
            uniform_buffer,
            target: Target::Offscreen {
                _texture: texture,
                view,
                size: (width, height),
            },
            format,
            adapter_name,
            timers,
            timestamp_period_ns,
            video_bgl,
            video_sampler,
            video_pipelines,
            video_params,
            preview: None,
        })
    }

    pub fn adapter_name(&self) -> &str {
        &self.adapter_name
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Egy 16 bájtos layer-params puffer létrehozása (opacity).
    pub fn create_layer_params(&self) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer-params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Bind group egy videó-frame textúrához (textúra + sampler + opacity).
    pub fn make_video_bind_group(
        &self,
        view: &wgpu::TextureView,
        params: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("video-frame"),
            layout: &self.video_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.video_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params.as_entire_binding(),
                },
            ],
        })
    }

    /// A videó-réteg pipeline-ja a kért blend módhoz.
    pub fn video_pipeline(&self, mode: BlendMode) -> &wgpu::RenderPipeline {
        &self.video_pipelines[blend_index(mode)]
    }

    pub fn video_bgl(&self) -> &wgpu::BindGroupLayout {
        &self.video_bgl
    }

    pub fn video_sampler(&self) -> &wgpu::Sampler {
        &self.video_sampler
    }

    /// A procedurális teszt-minta pipeline-ja.
    pub fn proc_pipeline(&self, mode: BlendMode) -> &wgpu::RenderPipeline {
        &self.proc_pipelines[blend_index(mode)]
    }

    pub fn size(&self) -> (u32, u32) {
        match &self.target {
            Target::Surface { config, .. } => (config.width, config.height),
            Target::Offscreen { size, .. } => *size,
        }
    }

    pub fn gpu_timing_available(&self) -> bool {
        self.timers.is_some()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if let Target::Surface { surface, config } = &mut self.target {
            config.width = width;
            config.height = height;
            surface.configure(&self.device, config);
        }
    }

    pub fn reconfigure(&mut self) {
        if let Target::Surface { surface, config } = &self.target {
            log::warn!("surface újrakonfigurálása");
            surface.configure(&self.device, config);
        }
    }

    /// Egy frame renderelése. Surface esetén presentál, offscreen esetén nem.
    pub fn render(
        &mut self,
        time: f32,
        beat: f32,
        layers: &[LayerRef],
    ) -> Result<(), wgpu::SurfaceError> {
        match &self.target {
            Target::Surface { surface, config } => {
                let frame = surface.get_current_texture()?;
                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder = self
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("frame"),
                    });
                self.draw(&mut encoder, &view, (config.width, config.height), time, beat, layers);
                self.queue.submit(Some(encoder.finish()));
                frame.present();
                Ok(())
            }
            Target::Offscreen { view, size, .. } => {
                let mut encoder =
                    self.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("frame-offscreen"),
                        });
                self.draw(&mut encoder, view, *size, time, beat, layers);
                self.queue.submit(Some(encoder.finish()));
                Ok(())
            }
        }
    }

    /// Offscreen render GPU-idő méréssel. Ok(None), ha nincs timestamp támogatás.
    pub fn render_timed(
        &self,
        time: f32,
        beat: f32,
        _layers: u32,
    ) -> Result<Option<f64>, wgpu::SurfaceError> {
        let (Target::Offscreen { view, size, .. }, Some(timers)) = (&self.target, &self.timers)
        else {
            return Ok(None);
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-timed"),
            });
        // 1. minta: itt. A Metal backend a függő mintákat a következő blit
        // encoder végén rögzíti, ezért egy üres blittel kikényszerítjük.
        encoder.write_timestamp(&timers.query_set, 0);
        encoder.clear_buffer(&timers.flush, 0, Some(4));
        self.draw(&mut encoder, view, *size, time, beat, &[]);
        // 2. minta: a render pass után.
        encoder.write_timestamp(&timers.query_set, 1);
        encoder.resolve_query_set(&timers.query_set, 0..2, &timers.resolve, 0);
        encoder.copy_buffer_to_buffer(&timers.resolve, 0, &timers.readback, 0, 16);
        self.queue.submit(Some(encoder.finish()));

        let slice = timers.readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        let _ = rx.recv();
        let gpu_ms = {
            let data = slice.get_mapped_range();
            let t0 = u64::from_le_bytes(data[0..8].try_into().unwrap());
            let t1 = u64::from_le_bytes(data[8..16].try_into().unwrap());
            drop(data);
            t1.wrapping_sub(t0) as f64 * self.timestamp_period_ns as f64 / 1e6
        };
        timers.readback.unmap();
        Ok(Some(gpu_ms))
    }

    fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        size: (u32, u32),
        time: f32,
        beat: f32,
        layers: &[LayerRef],
    ) {
        let uniforms = Uniforms {
            resolution: [size.0 as f32, size.1 as f32],
            time,
            beat,
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("main-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.01,
                        g: 0.01,
                        b: 0.02,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        if layers.is_empty() {
            // Nincs réteg: procedurális teszt-minta.
            pass.set_pipeline(&self.proc_pipelines[0]);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        } else {
            // A rétegek alulról felfelé, mindegyik a saját blend módjával.
            for layer in layers {
                pass.set_pipeline(&layer.pipeline);
                pass.set_bind_group(0, &layer.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
    }

    /// Az offscreen cél közepéről olvas vissza pixeleket (automatikus validációhoz).
    pub fn read_back_center(&self, cols: u32, rows: u32) -> Option<Vec<u8>> {
        let Target::Offscreen { _texture, size, .. } = &self.target else {
            return None;
        };
        let (width, height) = *size;
        let cols = cols.min(width);
        let rows = rows.min(height);
        let bytes_per_row = ((cols * 4).div_ceil(256)) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: (bytes_per_row * rows) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: _texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: (width - cols) / 2,
                    y: (height - rows) / 2,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(rows),
                },
            },
            wgpu::Extent3d {
                width: cols,
                height: rows,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range().to_vec();
        buffer.unmap();
        Some(data)
    }

    /// Előnézet készítése: a réteg kicsinyített renderelése, majd CPU-ra olvasása.
    ///
    /// A kicsinyített textúrába renderelünk (nem a nagy képet olvassuk vissza),
    /// így a readback költsége ~30x kisebb, és a shaderek ugyanazok.
    pub fn capture_preview(
        &mut self,
        layers: &[LayerRef],
        time: f32,
        beat: f32,
    ) -> Option<Vec<u8>> {
        let (pw, ph) = (640u32, 360u32);
        if self.preview.is_none() {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("preview"),
                size: wgpu::Extent3d {
                    width: pw,
                    height: ph,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bytes_per_row = ((pw * 4).div_ceil(256)) * 256;
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("preview-readback"),
                size: (bytes_per_row * ph) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.preview = Some(PreviewTarget {
                texture,
                view,
                size: (pw, ph),
                buffer,
                bytes_per_row,
                pixels: vec![0u8; (bytes_per_row * ph) as usize],
            });
        }

        // Kivesszük a self-ből, hogy a draw hívás közben ne legyen ütköző kölcsönzés.
        let mut preview = self.preview.take()?;
        let size = preview.size;
        let bytes_per_row = preview.bytes_per_row;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("preview"),
            });
        self.draw(&mut encoder, &preview.view, size, time, beat, layers);
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &preview.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &preview.buffer,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(size.1),
                },
            },
            wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit(Some(encoder.finish()));

        let slice = preview.buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::Maintain::Wait);
        let _ = rx.recv();
        {
            let data = slice.get_mapped_range();
            preview.pixels.copy_from_slice(&data);
        }
        preview.buffer.unmap();

        // BGRA -> RGB, soronként (a padding kihagyásával).
        let mut rgb = Vec::with_capacity((size.0 * size.1 * 3) as usize);
        for y in 0..size.1 {
            let row = (y * bytes_per_row) as usize;
            for x in 0..size.0 {
                let o = row + (x * 4) as usize;
                rgb.push(preview.pixels[o + 2]);
                rgb.push(preview.pixels[o + 1]);
                rgb.push(preview.pixels[o]);
            }
        }
        self.preview = Some(preview);
        Some(rgb)
    }

    /// Megvárja a GPU-t (benchmarkhoz).
    pub fn wait_idle(&self) {
        self.device.poll(wgpu::Maintain::Wait);
    }
}

fn requested_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let supported = adapter.features();
    let mut f = wgpu::Features::empty();
    if supported.contains(wgpu::Features::TIMESTAMP_QUERY) {
        f |= wgpu::Features::TIMESTAMP_QUERY;
    }
    if supported.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS) {
        f |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    }
    if supported.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES) {
        f |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
    }
    f
}

fn create_timers(device: &wgpu::Device, features: wgpu::Features) -> Option<Timers> {
    if !features.contains(wgpu::Features::TIMESTAMP_QUERY) {
        return None;
    }
    let query_set = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("gpu-timers"),
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("timer-resolve"),
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("timer-readback"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let flush = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("timer-flush"),
        size: 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    Some(Timers {
        query_set,
        resolve,
        readback,
        flush,
    })
}

/// Előnézet-cél: kicsinyített, CPU-ra visszaolvasható textúra.
struct PreviewTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    buffer: wgpu::Buffer,
    bytes_per_row: u32,
    pixels: Vec<u8>,
}

/// Egy egyedi réteg referenciája (a pipeline és a bind group a hívóé).
///
/// Ugyanezt használja az audio-vizuál és az ISF shader-réteg is.
/// Egy réteg a kompozitáláshoz: kész pipeline + bind group.
///
/// Tulajdonolt handle-ök (Arc-szerűek), így a hívó szabadon összeállíthatja
/// a réteglistát anélkül, hogy a renderer-t kölcsönözné.
#[derive(Clone)]
pub struct LayerRef {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group: wgpu::BindGroup,
}

/// A videó-réteg pipeline-ja: egy textúra + lineáris sampler.
fn create_video_resources(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> (
    wgpu::BindGroupLayout,
    wgpu::Sampler,
    [wgpu::RenderPipeline; 4],
    wgpu::Buffer,
) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("video-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
    });
    let video_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("video-bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let video_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("video-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::FilterMode::Nearest,
        ..Default::default()
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("video-pipeline-layout"),
        bind_group_layouts: &[&video_bgl],
        push_constant_ranges: &[],
    });
    let video_pipelines = build_mode_pipelines(device, format, |blend| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_video"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_video"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
    });
    let video_params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("video-params"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    (video_bgl, video_sampler, video_pipelines, video_params)
}

fn create_proc_pipelines(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
) -> [wgpu::RenderPipeline; 4] {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("spike-a-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("uniforms-layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("pipeline-layout"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });
    build_mode_pipelines(device, format, |blend| {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("spike-a-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(blend),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        })
    })
}

fn create_uniforms(
    device: &wgpu::Device,
    width: u32,
    height: u32,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("uniforms"),
        contents: bytemuck::bytes_of(&Uniforms {
            resolution: [width as f32, height as f32],
            time: 0.0,
            beat: 0.0,
        }),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("uniforms-layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("uniforms"),
        layout: &bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buffer.as_entire_binding(),
        }],
    });
    (uniform_buffer, bind_group)
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("adapter", &self.adapter_name)
            .field("format", &self.format)
            .finish()
    }
}
