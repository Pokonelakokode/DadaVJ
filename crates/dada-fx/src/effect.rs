//! ISF shader -> wgpu pipeline (uniformokkal, hot-reload-dal).

use anyhow::Result;
use dada_core::BlendMode;

use crate::blend::{blend_index, blend_state};
use crate::isf::IsfShader;

/// A teljes képernyős háromszög, ami a fragment shadernek átadja a normált
/// koordinátát a 0. számon (ez az ISF isf_FragNormCoord megfelelője).
const VERTEX_WGSL: &str = r#"
struct VOut {
    @builtin(position) pos : vec4<f32>,
    @location(0) uv : vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi : u32) -> VOut {
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0)
    );
    let q = p[vi];
    var o : VOut;
    o.pos = vec4<f32>(q, 0.0, 1.0);
    o.uv = vec2<f32>((q.x + 1.0) * 0.5, 1.0 - (q.y + 1.0) * 0.5);
    return o;
}
"#;

/// Egy renderelhető ISF effekt: pipeline + uniform puffer + paraméterértékek.
pub struct Effect {
    pipelines: [wgpu::RenderPipeline; 4],
    bind_group: wgpu::BindGroup,
    bgl: wgpu::BindGroupLayout,
    /// Képi bemenetek (binding 1/2, 3/4, ...): a saját sampler + a nézet.
    image_sampler: wgpu::Sampler,
    image_views: Vec<wgpu::TextureView>,
    /// Az 1x1-es tartalék textúra (ha nincs bekötött bemenet).
    _dummy: Option<wgpu::Texture>,
    uniform_buffer: wgpu::Buffer,
    pub shader: IsfShader,
    pub values: Vec<[f32; 4]>,
    pub frames: u32,
    /// A layer átlátszósága (0..1), a shader alpha-jába épül.
    pub opacity: f32,
    format: wgpu::TextureFormat,
}

impl Effect {
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        shader: IsfShader,
    ) -> Result<Self> {
        let values: Vec<[f32; 4]> = shader.params.iter().map(|p| p.default).collect();
        let size = shader.uniform_size.max(64);

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("effect-uniforms"),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = build_bgl(device, shader.images.len());
        let image_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("effect-image-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // Ha a shader kér képet, kell egy tartalék (1x1 fekete), különben a
        // bind group nem hozható létre a bemenet bekötése előtt.
        let mut dummy = None;
        let mut image_views = Vec::new();
        for _ in 0..shader.images.len() {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("effect-dummy-input"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            image_views.push(tex.create_view(&wgpu::TextureViewDescriptor::default()));
            dummy = Some(tex);
        }

        let bind_group = build_bg(
            device,
            &bgl,
            &uniform_buffer,
            &image_views,
            &image_sampler,
        );

        let pipelines = build_pipelines(device, format, &shader.wgsl_fragment, &bgl)?;

        Ok(Self {
            pipelines,
            bind_group,
            bgl,
            image_sampler,
            image_views,
            _dummy: dummy,
            uniform_buffer,
            shader,
            values,
            frames: 0,
            opacity: 1.0,
            format,
        })
    }

    /// Újratöltés fájlból (hot-reload). Hibánál a régi pipeline marad.
    pub fn reload(&mut self, device: &wgpu::Device) -> Result<()> {
        let shader = IsfShader::load(&self.shader.path)?;
        let values: Vec<[f32; 4]> = shader
            .params
            .iter()
            .map(|p| {
                // A korábbi értékeket próbáljuk megtartani név szerint.
                self.shader
                    .params
                    .iter()
                    .position(|old| old.name == p.name)
                    .and_then(|i| self.values.get(i).copied())
                    .unwrap_or(p.default)
            })
            .collect();
        let bgl_layout = self.pipelines[0].get_bind_group_layout(0);
        let pipelines = build_pipelines(device, self.format, &shader.wgsl_fragment, &bgl_layout)?;
        let size = shader.uniform_size.max(64);
        if size != self.uniform_buffer.size() {
            anyhow::bail!(
                "a uniform puffer mérete megváltozott ({} -> {}) — indítsd újra",
                self.uniform_buffer.size(),
                size
            );
        }
        self.pipelines = pipelines;
        self.shader = shader;
        self.values = values;
        self.frames = 0;
        Ok(())
    }

    pub fn pipeline(&self, mode: BlendMode) -> &wgpu::RenderPipeline {
        &self.pipelines[blend_index(mode)]
    }

    /// Képi bemenet bekötése (az ISF inputImage). A bind group újraépül.
    pub fn set_input(&mut self, device: &wgpu::Device, view: &wgpu::TextureView) {
        if self.shader.images.is_empty() {
            return;
        }
        self.image_views = vec![view.clone(); self.shader.images.len()];
        self.bind_group = build_bg(
            device,
            &self.bgl,
            &self.uniform_buffer,
            &self.image_views,
            &self.image_sampler,
        );
    }

    /// Vannak-e képi bemenetei a shadernek.
    pub fn has_inputs(&self) -> bool {
        !self.shader.images.is_empty()
    }

    pub fn bind_group(&self) -> &wgpu::BindGroup {
        &self.bind_group
    }

    pub fn set_param(&mut self, index: usize, value: [f32; 4]) {
        if let Some(v) = self.values.get_mut(index) {
            *v = value;
        }
    }

    /// Az uniform puffer feltöltése és a frame-számláló léptetése.
    pub fn update(
        &mut self,
        queue: &wgpu::Queue,
        resolution: [f32; 2],
        time: f32,
        delta: f32,
    ) {
        let mut bytes = vec![0u8; self.uniform_buffer.size() as usize];
        self.shader.write_uniform_block(
            &mut bytes,
            &self.values,
            (self.frames as f32, 0.0, self.opacity),
            resolution,
            time,
            delta,
        );
        queue.write_buffer(&self.uniform_buffer, 0, &bytes);
        self.frames += 1;
    }
}

/// A négy blend módhoz négy pipeline (a blend a pipeline állapota).
/// A bind group layout: 0 = uniform, 1/2 = 1. kép, 3/4 = 2. kép, ...
fn build_bgl(device: &wgpu::Device, image_count: usize) -> wgpu::BindGroupLayout {
    let mut entries = vec![wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }];
    for i in 0..image_count {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 1 + i as u32 * 2,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 2 + i as u32 * 2,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });
    }
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("effect-bgl"),
        entries: &entries,
    })
}

fn build_bg(
    device: &wgpu::Device,
    bgl: &wgpu::BindGroupLayout,
    uniform: &wgpu::Buffer,
    views: &[wgpu::TextureView],
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let mut entries = vec![wgpu::BindGroupEntry {
        binding: 0,
        resource: uniform.as_entire_binding(),
    }];
    for (i, view) in views.iter().enumerate() {
        entries.push(wgpu::BindGroupEntry {
            binding: 1 + i as u32 * 2,
            resource: wgpu::BindingResource::TextureView(view),
        });
        entries.push(wgpu::BindGroupEntry {
            binding: 2 + i as u32 * 2,
            resource: wgpu::BindingResource::Sampler(sampler),
        });
    }
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("effect-bg"),
        layout: bgl,
        entries: &entries,
    })
}

fn build_pipelines(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    fragment_wgsl: &str,
    bgl: &wgpu::BindGroupLayout,
) -> Result<[wgpu::RenderPipeline; 4]> {
    let modes = [
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
    ];
    Ok([
        build_pipeline(device, format, fragment_wgsl, bgl, blend_state(modes[0]))?,
        build_pipeline(device, format, fragment_wgsl, bgl, blend_state(modes[1]))?,
        build_pipeline(device, format, fragment_wgsl, bgl, blend_state(modes[2]))?,
        build_pipeline(device, format, fragment_wgsl, bgl, blend_state(modes[3]))?,
    ])
}

fn build_pipeline(
    device: &wgpu::Device,
    format: wgpu::TextureFormat,
    fragment_wgsl: &str,
    bgl: &wgpu::BindGroupLayout,
    blend: wgpu::BlendState,
) -> Result<wgpu::RenderPipeline> {
    let vs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("isf-vertex"),
        source: wgpu::ShaderSource::Wgsl(VERTEX_WGSL.into()),
    });
    let fs = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("isf-fragment"),
        source: wgpu::ShaderSource::Wgsl(fragment_wgsl.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("isf-pipeline-layout"),
        bind_group_layouts: &[bgl],
        push_constant_ranges: &[],
    });
    Ok(device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("isf-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &vs,
            entry_point: Some("vs_main"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &fs,
            entry_point: Some("main"),
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
    }))
}
