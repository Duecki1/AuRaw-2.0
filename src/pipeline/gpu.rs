//! Owns all wgpu resources for the raw processing pipeline and streams the
//! result directly into egui via `egui_wgpu::CallbackTrait`.
//!
//! Data flow, all on GPU:
//!   raw_tex (R32Float, uploaded once per image)
//!     -> compute shader (pipeline.wgsl): demosaic, WB, color matrix, exposure
//!     -> out_tex (Rgba8Unorm, storage texture, rewritten every time params change)
//!     -> sampled directly by egui's renderer as a normal texture (registered
//!        via `egui_wgpu::Renderer::register_native_texture`)
//!
//! There is no `Queue::write_texture` readback, no `image` crate buffer, no
//! CPU-side pixel loop anywhere in the live-preview path. `image`/`bytemuck`
//! in Cargo.toml are only used for export/thumbnailing, not for preview.

use crate::pipeline::exposure::GpuParams;
use crate::pipeline::raw_loader::LoadedRaw;
use anyhow::{anyhow, Result};
use eframe::egui;
use eframe::egui_wgpu;
use eframe::wgpu;

/// Everything needed to render the current raw image at the current
/// exposure settings. Lives for as long as an image is open.
pub struct RawGpuPipeline {
    compute_pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,

    raw_texture: wgpu::Texture,
    raw_texture_view: wgpu::TextureView,

    out_texture: wgpu::Texture,
    out_texture_view: wgpu::TextureView,

    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,

    pub width: u32,
    pub height: u32,

    /// egui texture id for the output, so `ui.image()` can display it directly.
    pub egui_texture_id: egui::TextureId,
}

impl RawGpuPipeline {
    /// Build the pipeline and upload raw sensor data. Call once per opened image.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        raw: &LoadedRaw,
    ) -> Result<Self> {
        let shader_src = include_str!("../shaders/pipeline.wgsl");
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("auraw::pipeline_shader"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("auraw::bind_group_layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

      let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("auraw::pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0, // <-- ADD THIS FIELD
        });

        let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("auraw::compute_pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // --- raw sensor texture (uploaded once) ---
        let raw_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw::raw_texture"),
            size: wgpu::Extent3d {
                width: raw.width,
                height: raw.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &raw_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&raw.raw_pixels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(raw.width * 4),
                rows_per_image: Some(raw.height),
            },
            wgpu::Extent3d {
                width: raw.width,
                height: raw.height,
                depth_or_array_layers: 1,
            },
        );
        let raw_texture_view = raw_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // --- output texture (rewritten on every param change) ---
        let out_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw::out_texture"),
            size: wgpu::Extent3d {
                width: raw.width,
                height: raw.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let out_texture_view = out_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("auraw::uniform_buffer"),
            size: std::mem::size_of::<GpuParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("auraw::bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&raw_texture_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&out_texture_view),
                },
            ],
        });

        // Register the output texture with egui so ui.image() can draw it
        // straight from GPU memory — this is the "no CPU" hookup.
        let egui_texture_id = renderer.register_native_texture(
            device,
            &out_texture_view,
            wgpu::FilterMode::Linear,
        );

        Ok(Self {
            compute_pipeline,
            bind_group_layout,
            raw_texture,
            raw_texture_view,
            out_texture,
            out_texture_view,
            uniform_buffer,
            bind_group,
            width: raw.width,
            height: raw.height,
            egui_texture_id,
        })
    }

    /// Push new parameters and dispatch the compute shader. Call this
    /// whenever a slider moves — it re-runs the whole demosaic+exposure
    /// pass on GPU. At preview resolutions this is comfortably sub-frame.
    pub fn recompute(&self, queue: &wgpu::Queue, device: &wgpu::Device, params: &GpuParams) {
        queue.write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(params));

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw::recompute_encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("auraw::compute_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.compute_pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            let wg_x = self.width.div_ceil(8);
            let wg_y = self.height.div_ceil(8);
            pass.dispatch_workgroups(wg_x, wg_y, 1);
        }
        queue.submit(Some(encoder.finish()));
    }

    /// Re-register the output texture with egui, e.g. after a device loss.
    /// Not needed in normal operation — kept for completeness.
    #[allow(dead_code)]
    pub fn reregister(&mut self, device: &wgpu::Device, renderer: &mut egui_wgpu::Renderer) {
        renderer.free_texture(&self.egui_texture_id);
        self.egui_texture_id = renderer.register_native_texture(
            device,
            &self.out_texture_view,
            wgpu::FilterMode::Linear,
        );
    }
}

/// Helper to fetch the wgpu device/queue/renderer out of eframe's
/// `RenderState`. Centralized here so callers don't repeat the unwrap logic.
pub fn wgpu_handles_from_frame<'a>(
    frame: &'a eframe::Frame,
) -> Result<(&'a wgpu::Device, &'a wgpu::Queue)> {
    let state = frame
        .wgpu_render_state()
        .ok_or_else(|| anyhow!("eframe is not running with the wgpu backend"))?;
    Ok((&state.device, &state.queue))
}