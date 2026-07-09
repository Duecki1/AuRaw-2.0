use crate::pipeline::{ExposureParams, LoadedRaw};
use anyhow::{anyhow, Result};
use bytemuck::{Pod, Zeroable};
use eframe::{egui, egui_wgpu, wgpu};
use wgpu::util::DeviceExt;

const SHADER_P1: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/highlights.wgsl"),
    "\n",
    include_str!("../shaders/basic_adjustments.wgsl"),
    "\n",
    include_str!("../shaders/tonemap.wgsl"),
    "\n",
    include_str!("../shaders/pass1.wgsl")
);

const SHADER_P2: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/highlights.wgsl"),
    "\n",
    include_str!("../shaders/basic_adjustments.wgsl"),
    "\n",
    include_str!("../shaders/tonemap.wgsl"),
    "\n",
    include_str!("../shaders/pass2.wgsl")
);

const SHADER_P3: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/highlights.wgsl"),
    "\n",
    include_str!("../shaders/basic_adjustments.wgsl"),
    "\n",
    include_str!("../shaders/tonemap.wgsl"),
    "\n",
    include_str!("../shaders/pass3.wgsl")
);

const SHADER_P4: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/highlights.wgsl"),
    "\n",
    include_str!("../shaders/basic_adjustments.wgsl"),
    "\n",
    include_str!("../shaders/tonemap.wgsl"),
    "\n",
    include_str!("../shaders/pass4.wgsl")
);

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GpuParams {
    black: f32,
    exposure: f32,
    hlcompr: f32,
    hlcomprthresh: f32,
    contrast: f32,
    middle_grey: f32,
    brightness: f32,
    saturation: f32,
    vibrance: f32,
    clip: f32,
    filmic_white: f32,
    filmic_black: f32,
    wb: [f32; 4],
    cam_to_srgb_0: [f32; 4],
    cam_to_srgb_1: [f32; 4],
    cam_to_srgb_2: [f32; 4],
    black_levels: [f32; 4],
    white_levels: [f32; 4],
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

impl GpuParams {
    pub fn new(exposure: &ExposureParams, raw: &LoadedRaw) -> Self {
        Self {
            black: exposure.black,
            exposure: exposure.exposure,
            hlcompr: exposure.hlcompr,
            hlcomprthresh: exposure.hlcomprthresh,
            contrast: exposure.contrast,
            middle_grey: exposure.middle_grey,
            brightness: exposure.brightness,
            saturation: exposure.saturation,
            vibrance: exposure.vibrance,
            clip: exposure.clip,
            filmic_white: exposure.filmic_white,
            filmic_black: exposure.filmic_black,
            wb: raw.wb_coeffs,
            cam_to_srgb_0: raw.cam_to_srgb[0],
            cam_to_srgb_1: raw.cam_to_srgb[1],
            cam_to_srgb_2: raw.cam_to_srgb[2],
            black_levels: raw.black_levels,
            white_levels: raw.white_levels,
            width: raw.width,
            height: raw.height,
            _pad0: 0,
            _pad1: 0,
        }
    }
}

struct Pass {
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
}

pub struct RawGpuPipeline {
    pub egui_texture_id: egui::TextureId,
    pub width: u32,
    pub height: u32,
    params_buffer: wgpu::Buffer,
    passes: [Pass; 4],
    _raw_texture: wgpu::Texture,
    _color_texture: wgpu::Texture,
    _tex1: wgpu::Texture,
    _tex2: wgpu::Texture,
    _tex3: wgpu::Texture,
    _out_texture: wgpu::Texture,
    _out_view: wgpu::TextureView,
}

impl RawGpuPipeline {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        raw: &LoadedRaw,
        params: &GpuParams,
    ) -> Result<Self> {
        validate_raw(raw)?;

        let raw_texture = create_raw_texture(device, queue, raw);
        let color_texture = create_color_texture(device, queue, raw);
        let size = texture_size(raw.width, raw.height);

        let out_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw output texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });

        let tex1 = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw tex1"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba16Float],
        });

        let tex2 = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw tex2"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba16Float],
        });

        let tex3 = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw tex3"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba16Float],
        });

        let out_view = out_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let tex1_view = tex1.create_view(&wgpu::TextureViewDescriptor::default());
        let tex2_view = tex2.create_view(&wgpu::TextureViewDescriptor::default());
        let tex3_view = tex3.create_view(&wgpu::TextureViewDescriptor::default());
        let raw_view = raw_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("auraw params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let common_entries = [
            buffer_entry(0),
            texture_entry(1, wgpu::TextureSampleType::Uint),
            texture_entry(2, wgpu::TextureSampleType::Uint),
        ];

        let bgl1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl1"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                storage_texture_entry(3, wgpu::TextureFormat::Rgba16Float, wgpu::StorageTextureAccess::WriteOnly),
            ],
        });

        let bgl2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl2"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                texture_entry(4, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(5, wgpu::TextureFormat::Rgba16Float, wgpu::StorageTextureAccess::WriteOnly),
            ],
        });

        let bgl3 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl3"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                texture_entry(6, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(7, wgpu::TextureFormat::Rgba16Float, wgpu::StorageTextureAccess::WriteOnly),
            ],
        });

        let bgl4 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl4"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                texture_entry(4, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(6, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(8, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(9, wgpu::TextureFormat::Rgba8Unorm, wgpu::StorageTextureAccess::WriteOnly),
            ],
        });

        let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg1"),
            layout: &bgl1,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&raw_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&color_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&tex1_view) },
            ],
        });

        let bg2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg2"),
            layout: &bgl2,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&raw_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&color_view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&tex1_view) },
                wgpu::BindGroupEntry { binding: 5, resource: wgpu::BindingResource::TextureView(&tex2_view) },
            ],
        });

        let bg3 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg3"),
            layout: &bgl3,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&raw_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&color_view) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&tex2_view) },
                wgpu::BindGroupEntry { binding: 7, resource: wgpu::BindingResource::TextureView(&tex3_view) },
            ],
        });

        let bg4 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg4"),
            layout: &bgl4,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&raw_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&color_view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&tex1_view) },
                wgpu::BindGroupEntry { binding: 6, resource: wgpu::BindingResource::TextureView(&tex2_view) },
                wgpu::BindGroupEntry { binding: 8, resource: wgpu::BindingResource::TextureView(&tex3_view) },
                wgpu::BindGroupEntry { binding: 9, resource: wgpu::BindingResource::TextureView(&out_view) },
            ],
        });

        let make_pipeline = |source: &str, entry: &str, bgl: &wgpu::BindGroupLayout| -> wgpu::ComputePipeline {
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(entry),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
            let pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&format!("pll_{}", entry)),
                bind_group_layouts: &[Some(bgl)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pll),
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };

        let passes = [
            Pass { pipeline: make_pipeline(SHADER_P1, "pass1_vh_lpf", &bgl1), bind_group: bg1 },
            Pass { pipeline: make_pipeline(SHADER_P2, "pass2_green_pq", &bgl2), bind_group: bg2 },
            Pass { pipeline: make_pipeline(SHADER_P3, "pass3_rb_opposite", &bgl3), bind_group: bg3 },
            Pass { pipeline: make_pipeline(SHADER_P4, "pass4_rb_green_output", &bgl4), bind_group: bg4 },
        ];

        let egui_texture_id =
            renderer.register_native_texture(device, &out_view, wgpu::FilterMode::Linear);

        let pipeline = Self {
            egui_texture_id,
            width: raw.width,
            height: raw.height,
            params_buffer,
            passes,
            _raw_texture: raw_texture,
            _color_texture: color_texture,
            _tex1: tex1,
            _tex2: tex2,
            _tex3: tex3,
            _out_texture: out_texture,
            _out_view: out_view,
        };
        pipeline.recompute(queue, device, params);
        Ok(pipeline)
    }

    pub fn recompute(&self, queue: &wgpu::Queue, device: &wgpu::Device, params: &GpuParams) {
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(params));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw recompute encoder"),
        });
        
        let wg_x = self.width.div_ceil(8);
        let wg_y = self.height.div_ceil(8);

        for i in 0..4 {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(&format!("auraw pass {}", i+1)),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.passes[i].pipeline);
            pass.set_bind_group(0, &self.passes[i].bind_group, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);
        }
        
        queue.submit(Some(encoder.finish()));
    }
}

fn buffer_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

fn storage_texture_entry(binding: u32, format: wgpu::TextureFormat, access: wgpu::StorageTextureAccess) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn validate_raw(raw: &LoadedRaw) -> Result<()> {
    let pixels = raw
        .width
        .checked_mul(raw.height)
        .ok_or_else(|| anyhow!("raw dimensions overflow"))? as usize;
    if raw.raw_pixels.len() != pixels {
        return Err(anyhow!(
            "raw pixel count mismatch: got {}, expected {}",
            raw.raw_pixels.len(),
            pixels
        ));
    }
    if raw.color_indices.len() != pixels {
        return Err(anyhow!(
            "CFA index count mismatch: got {}, expected {}",
            raw.color_indices.len(),
            pixels
        ));
    }
    Ok(())
}

fn texture_entry(binding: u32, sample_type: wgpu::TextureSampleType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn create_raw_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: &LoadedRaw,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("auraw raw mosaic"),
        size: texture_size(raw.width, raw.height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::R16Uint],
    });
    queue.write_texture(
        copy_texture(&texture),
        bytemuck::cast_slice(&raw.raw_pixels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(raw.width * 2),
            rows_per_image: Some(raw.height),
        },
        texture_size(raw.width, raw.height),
    );
    texture
}

fn create_color_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: &LoadedRaw,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("auraw CFA color indices"),
        size: texture_size(raw.width, raw.height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::R8Uint],
    });
    queue.write_texture(
        copy_texture(&texture),
        &raw.color_indices,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(raw.width),
            rows_per_image: Some(raw.height),
        },
        texture_size(raw.width, raw.height),
    );
    texture
}

fn copy_texture(texture: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    }
}

fn texture_size(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}