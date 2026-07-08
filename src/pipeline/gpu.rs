//! Owns all wgpu resources for the raw processing pipeline and streams the
//! result directly into egui via `egui_wgpu::CallbackTrait`. [1, 2]
//!
//! Data flow, all on GPU:
//!   raw_tex (R32Float, uploaded once per image)
//!     -> rcd_demosaic.wgsl: 8-pass RCD demosaic (runs once in new())
//!     -> rgb_a_texture (Rgba32Float, cached demosaic result)
//!     -> pipeline.wgsl: WB, color matrix, exposure, tonemap, OETF
//!     -> out_tex (Rgba8Unorm, rewritten every time params change)
//!     -> sampled directly by egui's renderer

use crate::pipeline::exposure::GpuParams;
use crate::pipeline::raw_loader::LoadedRaw;
use anyhow::{anyhow, Result};
use eframe::egui;
use eframe::egui_wgpu;
use eframe::wgpu;

pub struct RawGpuPipeline {
    // --- Per-frame pipeline (WB + color matrix + exposure + tonemap) ---
    compute_pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,

    // --- Raw sensor texture (uploaded once) ---
    raw_texture: wgpu::Texture,
    _raw_texture_view: wgpu::TextureView,

    // --- Demosaic pipelines + resources ---
    common_bgl: wgpu::BindGroupLayout,
    common_bg: wgpu::BindGroup,
    green_bgl: wgpu::BindGroupLayout,
    green_bg: wgpu::BindGroup,
    rb_bgl: wgpu::BindGroupLayout,
    rb_bg: wgpu::BindGroup,
    rb_green_bg: wgpu::BindGroup,

    vh_pipeline: wgpu::ComputePipeline,
    lpf_pipeline: wgpu::ComputePipeline,
    green_pipeline: wgpu::ComputePipeline,
    pq_pipeline: wgpu::ComputePipeline,
    rb_rb_pipeline: wgpu::ComputePipeline,
    rb_green_pipeline: wgpu::ComputePipeline,
    color_smooth_pipeline: wgpu::ComputePipeline,
    chroma_refine_pipeline: wgpu::ComputePipeline,

    // --- Intermediate / output textures ---
    vh_dir_texture: wgpu::Texture,
    lpf_texture: wgpu::Texture,
    pq_dir_texture: wgpu::Texture,
    rgb_a_texture: wgpu::Texture,
    rgb_b_texture: wgpu::Texture,
    demosaiced_texture_view: wgpu::TextureView,

    // --- Display output ---
    out_texture: wgpu::Texture,
    out_texture_view: wgpu::TextureView,

    pub width: u32,
    pub height: u32,
    pub egui_texture_id: egui::TextureId,
}

impl RawGpuPipeline {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        raw: &LoadedRaw,
        params: &GpuParams,
    ) -> Result<Self> {
        let extent = wgpu::Extent3d {
            width: raw.width,
            height: raw.height,
            depth_or_array_layers: 1,
        };

        // ============================================================
        // 1. Raw sensor texture (uploaded once)
        // ============================================================
        let raw_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw::raw_texture"),
            size: extent,
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
            extent,
        );
        let raw_texture_view = raw_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // ============================================================
        // 2. Intermediate demosaic textures
        // ============================================================
        let vh_dir_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw::vh_dir_texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let lpf_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw::lpf_texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let pq_dir_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw::pq_dir_texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        });
        let rgb_a_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw::rgb_a_texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let rgb_b_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw::rgb_b_texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        // Pass 8 outputs back into rgb_a_texture, so demosaiced_texture_view references rgb_a.
        let demosaiced_texture_view =
            rgb_a_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("auraw::uniform_buffer"),
            size: std::mem::size_of::<GpuParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Initialize uniform buffer immediately so the demosaic passes
        // have valid width/height/cfa_pattern.
        queue.write_buffer(&uniform_buffer, 0, bytemuck::bytes_of(params));

        let vh_view = vh_dir_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let lpf_view = lpf_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let pq_view = pq_dir_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let rgb_a_view = rgb_a_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let rgb_b_view = rgb_b_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // ============================================================
        // 3. Demosaic bind group layouts + bind groups
        // ============================================================
        let common_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("auraw::demosaic_common_bgl"),
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
                        access: wgpu::StorageTextureAccess::ReadWrite,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::ReadWrite,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::ReadWrite,
                        format: wgpu::TextureFormat::R32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

        let green_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("auraw::demosaic_green_bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba32Float,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            }],
        });

        let rb_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("auraw::demosaic_rb_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba32Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

        let common_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("auraw::demosaic_common_bg"),
            layout: &common_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&raw_texture_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&vh_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(&lpf_view) },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&pq_view) },
            ],
        });

        let green_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("auraw::demosaic_green_bg"),
            layout: &green_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&rgb_a_view) },
            ],
        });

        let rb_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("auraw::demosaic_rb_bg"),
            layout: &rb_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&rgb_a_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&rgb_b_view) },
            ],
        });

        let rb_green_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("auraw::demosaic_rb_green_bg"),
            layout: &rb_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&rgb_b_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&rgb_a_view) },
            ],
        });

        // ============================================================
        // 4. Demosaic shader + 8 pipelines
        // ============================================================
        let demosaic_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("auraw::demosaic_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/rcd_demosaic.wgsl").into()),
        });

        let pl_common = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("auraw::demosaic_common_pl"),
            bind_group_layouts: &[Some(&common_bgl)],
            immediate_size: 0,
        });

        let pl_green = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("auraw::demosaic_green_pl"),
            bind_group_layouts: &[Some(&common_bgl), Some(&green_bgl)],
            immediate_size: 0,
        });

        let pl_rb = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("auraw::demosaic_rb_pl"),
            bind_group_layouts: &[Some(&common_bgl), None, Some(&rb_bgl)],
            immediate_size: 0,
        });

        let mk_pipeline = |entry: &str, layout: &wgpu::PipelineLayout| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(&format!("auraw::demosaic::{entry}")),
                layout: Some(layout),
                module: &demosaic_shader,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };

        let vh_pipeline = mk_pipeline("vh_discrimination", &pl_common);
        let lpf_pipeline = mk_pipeline("lpf", &pl_common);
        let green_pipeline = mk_pipeline("green_fill", &pl_green);
        let pq_pipeline = mk_pipeline("pq_discrimination", &pl_common);
        let rb_rb_pipeline = mk_pipeline("rb_at_rb_sites", &pl_rb);
        let rb_green_pipeline = mk_pipeline("rb_at_green_sites", &pl_rb);
        let color_smooth_pipeline = mk_pipeline("color_smooth", &pl_rb);
        let chroma_refine_pipeline = mk_pipeline("chroma_refine", &pl_rb);

        // ============================================================
        // 5. Per-frame pipeline (now reads demosaiced_tex, not raw_tex)
        // ============================================================
        let per_frame_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("auraw::pipeline_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/pipeline.wgsl").into()),
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
            immediate_size: 0,
        });

        let compute_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("auraw::compute_pipeline"),
            layout: Some(&pipeline_layout),
            module: &per_frame_shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });

        // ============================================================
        // 6. Output texture
        // ============================================================
        let out_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw::out_texture"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let out_texture_view = out_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // ============================================================
        // 7. Per-frame bind group (binding 1 is now demosaiced_texture_view)
        // ============================================================
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("auraw::bind_group"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniform_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&demosaiced_texture_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&out_texture_view) },
            ],
        });

        let egui_texture_id = renderer.register_native_texture(
            device,
            &out_texture_view,
            wgpu::FilterMode::Linear,
        );

        let pipeline = Self {
            compute_pipeline,
            bind_group_layout,
            uniform_buffer,
            bind_group,
            raw_texture,
            _raw_texture_view: raw_texture_view,
            common_bgl,
            common_bg,
            green_bgl,
            green_bg,
            rb_bgl,
            rb_bg,
            rb_green_bg,
            vh_pipeline,
            lpf_pipeline,
            green_pipeline,
            pq_pipeline,
            rb_rb_pipeline,
            rb_green_pipeline,
            color_smooth_pipeline,
            chroma_refine_pipeline,
            vh_dir_texture,
            lpf_texture,
            pq_dir_texture,
            rgb_a_texture,
            rgb_b_texture,
            demosaiced_texture_view,
            out_texture,
            out_texture_view,
            width: raw.width,
            height: raw.height,
            egui_texture_id,
        };

        // ============================================================
        // 8. Run the demosaic + initial per-frame render
        // ============================================================
        pipeline.demosaic(device, queue);
        pipeline.recompute(queue, device, params);

        Ok(pipeline)
    }

    /// Dispatch all 8 RCD demosaic passes. Called once in `new()` right
    /// after the raw texture is uploaded. The result lives in
    /// `rgb_a_texture` (via Pass 8) and is cached for the lifetime of the
    /// pipeline.
    pub fn demosaic(&self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw::demosaic_encoder"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("auraw::demosaic_compute_pass"),
                timestamp_writes: None,
            });

            let wg_x = self.width.div_ceil(8);
            let wg_y = self.height.div_ceil(8);

            // Pass 1: VH discrimination (Group 0)
            pass.set_pipeline(&self.vh_pipeline);
            pass.set_bind_group(0, &self.common_bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);

            // Pass 2: LPF (Group 0)
            pass.set_pipeline(&self.lpf_pipeline);
            pass.set_bind_group(0, &self.common_bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);

            // Pass 3: Green fill (Group 0 + Group 1 -> writes to rgb_a)
            pass.set_pipeline(&self.green_pipeline);
            pass.set_bind_group(0, &self.common_bg, &[]);
            pass.set_bind_group(1, &self.green_bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);

            // Pass 4: PQ discrimination (Group 0)
            pass.set_pipeline(&self.pq_pipeline);
            pass.set_bind_group(0, &self.common_bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);

            // Pass 5: R/B at R/B sites (Group 0 + Group 2 -> reads rgb_a, writes rgb_b)
            pass.set_pipeline(&self.rb_rb_pipeline);
            pass.set_bind_group(0, &self.common_bg, &[]);
            pass.set_bind_group(2, &self.rb_bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);

            // Pass 6: R/B at green sites (Group 0 + Group 2 -> reads rgb_b, writes rgb_a)
            pass.set_pipeline(&self.rb_green_pipeline);
            pass.set_bind_group(0, &self.common_bg, &[]);
            pass.set_bind_group(2, &self.rb_green_bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);

            // Pass 7: Color smoothing (Group 0 + Group 2 -> reads rgb_a, writes rgb_b)
            pass.set_pipeline(&self.color_smooth_pipeline);
            pass.set_bind_group(0, &self.common_bg, &[]);
            pass.set_bind_group(2, &self.rb_bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);

            // Pass 8: Residual Chroma Refinement (Group 0 + Group 2 -> reads rgb_b, writes rgb_a)
            pass.set_pipeline(&self.chroma_refine_pipeline);
            pass.set_bind_group(0, &self.common_bg, &[]);
            pass.set_bind_group(2, &self.rb_green_bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);
        }
        queue.submit(Some(encoder.finish()));
    }

    /// Push new parameters and dispatch the per-frame compute shader.
    /// Reads the cached demosaic from rgb_a_texture; only WB / color
    /// matrix / exposure / tonemap / OETF re-run.
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

pub fn wgpu_handles_from_frame<'a>(
    frame: &'a eframe::Frame,
) -> Result<(&'a wgpu::Device, &'a wgpu::Queue)> {
    let state = frame
        .wgpu_render_state()
        .ok_or_else(|| anyhow!("eframe is not running with the wgpu backend"))?;
    Ok((&state.device, &state.queue))
}