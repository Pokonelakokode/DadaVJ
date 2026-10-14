//! Audio-vizuál: a hang elemzési eredménye GPU-textúrákban + spektrogram shader.
//!
//! Három textúra megy fel a GPU-ra minden frame-ben:
//!   * waveform:   1024x1  R32Float  (-1..1)
//!   * spectrum:    512x1  R32Float  (0..1, log frekvencia-tengely)
//!   * spectrogram: 512x256 R32Float (idő x frekvencia, gördülő)
//!
//! A spektrogram csak akkor megy fel újra, ha valóban készült új oszlop.

use bytemuck::{Pod, Zeroable};
use dada_core::BlendMode;
use dada_fx::{blend_index, blend_state};
use dada_audio::{
    AnalyzerSnapshot, BANDS, SPECTROGRAM_COLS, SPECTROGRAM_ROWS, SPECTRUM_BINS, WAVEFORM_SAMPLES,
};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct AudioUniforms {
    resolution: [f32; 4],
    info: [f32; 4],
    bands: [f32; 4],
    bands2: [f32; 4],
}

/// Egy renderelhető audio-réteg: pipeline + bind group.
pub struct AudioVisual {
    pipelines: [wgpu::RenderPipeline; 4],
    /// A layer átlátszósága (0..1).
    pub opacity: f32,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    waveform_tex: wgpu::Texture,
    spectrum_tex: wgpu::Texture,
    spectrogram_tex: wgpu::Texture,
    last_write_row: Option<usize>,
    pub width: u32,
    pub height: u32,
    pub updates: u64,
}

impl AudioVisual {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let make_tex = |label: &str, w: u32, h: u32| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R32Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let waveform_tex = make_tex("audio-waveform", WAVEFORM_SAMPLES as u32, 1);
        let spectrum_tex = make_tex("audio-spectrum", SPECTRUM_BINS as u32, 1);
        // A textúra 256 (frekvencia) x 512 (idő) — így egy új szelet egy sor.
        let spectrogram_tex =
            make_tex("audio-spectrogram", SPECTROGRAM_ROWS as u32, SPECTROGRAM_COLS as u32);

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("audio-uniforms"),
            size: std::mem::size_of::<AudioUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("audio-bgl"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
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

        let waveform_view = waveform_tex.create_view(&Default::default());
        let spectrum_view = spectrum_tex.create_view(&Default::default());
        let spectrogram_view = spectrogram_tex.create_view(&Default::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("audio-bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&spectrogram_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&waveform_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&spectrum_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform_buffer.as_entire_binding(),
                },
            ],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("audio-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("audio-pipeline-layout"),
            bind_group_layouts: &[&bgl],
            push_constant_ranges: &[],
        });
        let modes = [
            BlendMode::Normal,
            BlendMode::Add,
            BlendMode::Multiply,
            BlendMode::Screen,
        ];
        let make = |mode: BlendMode| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("audio-pipeline"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_audio"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_audio"),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(blend_state(mode)),
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
        };
        let pipelines = [
            make(modes[0]),
            make(modes[1]),
            make(modes[2]),
            make(modes[3]),
        ];

        Self {
            pipelines,
            opacity: 1.0,
            bind_group,
            uniform_buffer,
            waveform_tex,
            spectrum_tex,
            spectrogram_tex,
            last_write_row: None,
            width,
            height,
            updates: 0,
        }
    }

    pub fn pipeline(&self, mode: BlendMode) -> &wgpu::RenderPipeline {
        &self.pipelines[blend_index(mode)]
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    /// Az elemzési pillanatkép feltöltése a GPU-textúrákba.
    pub fn update(
        &mut self,
        queue: &wgpu::Queue,
        snapshot: &AnalyzerSnapshot,
        time: f32,
        width: u32,
        height: u32,
    ) {
        self.width = width;
        self.height = height;
        self.updates += 1;

        let st = &snapshot.state;
        write_1d(queue, &self.waveform_tex, &st.waveform, WAVEFORM_SAMPLES);
        write_1d(queue, &self.spectrum_tex, &st.spectrum, SPECTRUM_BINS);

        // A spektrogram csak új sor esetén megy fel, és csak 1 KB (256 pixel).
        let row = snapshot.spectrogram_row_index;
        if self.last_write_row != Some(row) && snapshot.spectrogram_row.len() >= SPECTROGRAM_ROWS {
            queue.write_texture(
                wgpu::ImageCopyTexture {
                    texture: &self.spectrogram_tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: row as u32,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(&snapshot.spectrogram_row[..SPECTROGRAM_ROWS]),
                wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some((SPECTROGRAM_ROWS * 4) as u32),
                    rows_per_image: Some(1),
                },
                wgpu::Extent3d {
                    width: SPECTROGRAM_ROWS as u32,
                    height: 1,
                    depth_or_array_layers: 1,
                },
            );
            self.last_write_row = Some(row);
        }

        let mut bands = [0.0f32; 4];
        let mut bands2 = [0.0f32; 4];
        for i in 0..BANDS {
            if i < 4 {
                bands[i] = st.band_env[i];
            } else {
                bands2[i - 4] = st.band_env[i];
            }
        }
        bands2[2] = st.flux;
        bands2[3] = if st.onset { 1.0 } else { 0.0 };
        let uniforms = AudioUniforms {
            resolution: [width as f32, height as f32, self.opacity, 0.0],
            info: [row as f32, time, st.rms, st.peak],
            bands,
            bands2,
        };
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn write_1d(queue: &wgpu::Queue, texture: &wgpu::Texture, data: &[f32], len: usize) {
    let n = data.len().min(len);
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(&data[..n]),
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some((len * 4) as u32),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: len as u32,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
}
