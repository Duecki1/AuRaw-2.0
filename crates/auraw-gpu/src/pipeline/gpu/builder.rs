//! Focused construction phases for [`RawGpuPipeline::new_internal`](super::RawGpuPipeline).
//!
//! Each item in this module owns exactly one phase of pipeline construction:
//! derived geometry, GPU surfaces (textures/views/sampler), buffers, bind
//! group layouts, bind groups, shader loading, and compute-pass assembly.
//! The orchestrator in [`super`] calls them in the original inline order so
//! allocation sequence, wgpu object labels, pass ordering, and program-index
//! bookkeeping remain byte-for-byte identical to the previous monolithic
//! constructor.

use anyhow::{anyhow, Context, Result};
use std::sync::Arc;

use super::*;

pub(super) struct DerivedGeometry {
    pub(super) size: wgpu::Extent3d,
    pub(super) tone_size: wgpu::Extent3d,
    pub(super) work_format: wgpu::TextureFormat,
    pub(super) demosaic_format: wgpu::TextureFormat,
    pub(super) highlight_work_format: wgpu::TextureFormat,
    pub(super) tone_format: wgpu::TextureFormat,
    pub(super) image_workgroups: [u32; 3],
    pub(super) tone_workgroups: [u32; 3],
    pub(super) mask_atlas_edge: u32,
    pub(super) mask_layer_capacity: usize,
}

pub(super) fn compute_derived_geometry(
    raw: &LoadedRaw,
    params: &GpuParams,
    quality: ProcessingQuality,
    config: RawGpuPipelineConfig,
) -> DerivedGeometry {
    let size = texture_size(raw.width, raw.height);
    let work_format = processing_work_format(quality);
    let demosaic_format = work_format;
    let highlight_work_format = work_format;
    let tone_scale = tone_analysis_scale();
    let tone_size = texture_size(
        raw.width.div_ceil(tone_scale),
        raw.height.div_ceil(tone_scale),
    );
    let tone_format = tone_guide_format();
    let image_workgroups = dispatch_for_extent(raw.width, raw.height);
    let tone_workgroups = dispatch_for_extent(tone_size.width, tone_size.height);

    // A full-frame mask atlas cannot add spatial detail beyond the image
    // it masks. Capping it to the current proxy avoids reserving a 2048²
    // texture for every layer of an 800px preview (and, importantly, for
    // the tiny startup prewarm pipeline). Explicit detail/export atlases
    // keep their caller-selected resolution.
    let mask_atlas_edge = config
        .mask_atlas_edge_override
        .unwrap_or_else(|| interactive_mask_atlas_edge(raw.width, raw.height))
        .clamp(64, export_mask_atlas_edge_limit());
    let mask_layer_capacity = if config.mask_atlas_edge_override.is_some() {
        // Viewport detail and export both use explicit atlas sizes and can
        // allocate exactly the layers they will sample. This is what makes
        // a dense cropped detail atlas affordable alongside the main
        // preview; the ordinary full-frame interactive pipeline keeps all
        // 32 slots so adding common masks remains instant.
        (params.scene_tone.mask_counts[0] as usize).clamp(1, MAX_LOCAL_MASKS)
    } else {
        MAX_LOCAL_MASKS
    };

    DerivedGeometry {
        size,
        tone_size,
        work_format,
        demosaic_format,
        highlight_work_format,
        tone_format,
        image_workgroups,
        tone_workgroups,
        mask_atlas_edge,
        mask_layer_capacity,
    }
}

/// Every GPU texture, texture view, and sampler one pipeline instance owns.
pub(super) struct PipelineSurfaces {
    pub(super) raw_texture: wgpu::Texture,
    pub(super) color_texture: wgpu::Texture,
    pub(super) black_texture: wgpu::Texture,
    pub(super) reconstructed_raw_texture: wgpu::Texture,
    pub(super) highlight_work_a: wgpu::Texture,
    pub(super) highlight_work_b: wgpu::Texture,
    pub(super) scene_texture: wgpu::Texture,
    pub(super) display_linear_texture: wgpu::Texture,
    pub(super) out_texture: wgpu::Texture,
    pub(super) tex1: wgpu::Texture,
    pub(super) tex2: wgpu::Texture,
    pub(super) tone_guide_a: wgpu::Texture,
    pub(super) tone_guide_b: wgpu::Texture,
    pub(super) mask_texture: wgpu::Texture,
    pub(super) light_rays_mask_texture: wgpu::Texture,
    pub(super) out_view: wgpu::TextureView,
    pub(super) display_linear_view: wgpu::TextureView,
    pub(super) reconstructed_raw_view: wgpu::TextureView,
    pub(super) highlight_work_a_view: wgpu::TextureView,
    pub(super) highlight_work_b_view: wgpu::TextureView,
    pub(super) scene_view: wgpu::TextureView,
    pub(super) tex1_view: wgpu::TextureView,
    pub(super) tex2_view: wgpu::TextureView,
    pub(super) tone_guide_a_view: wgpu::TextureView,
    pub(super) tone_guide_b_view: wgpu::TextureView,
    pub(super) raw_view: wgpu::TextureView,
    pub(super) color_view: wgpu::TextureView,
    pub(super) black_view: wgpu::TextureView,
    pub(super) mask_view: wgpu::TextureView,
    pub(super) light_rays_mask_view: wgpu::TextureView,
    pub(super) mask_sampler: wgpu::Sampler,
}

/// Creates all textures, views, and the local-mask sampler. Returns whether an
/// AI-denoised scene-linear raster was uploaded into the scene texture.
pub(super) fn create_pipeline_surfaces(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: &LoadedRaw,
    ai_cfa: Option<&[u16]>,
    geometry: &DerivedGeometry,
) -> Result<(PipelineSurfaces, bool)> {
    let DerivedGeometry {
        size,
        tone_size,
        work_format,
        demosaic_format,
        highlight_work_format,
        tone_format,
        mask_atlas_edge,
        mask_layer_capacity,
        ..
    } = *geometry;

    let raw_texture = create_raw_texture(
        device,
        queue,
        raw,
        ai_cfa.unwrap_or(raw.raw_pixels.as_slice()),
    );
    let color_texture = create_color_texture(device, queue, raw);
    let black_texture = create_black_texture(device, queue, raw);

    // All demosaic stages sample this canonical reconstructed CFA.
    let reconstructed_raw_texture = create_processing_texture(
        device,
        size,
        wgpu::TextureFormat::R32Float,
        wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
        "auraw reconstructed raw CFA",
    );

    // Shared demosaic work surfaces. Bayer dual mode and X-Trans reuse
    // these after highlight reconstruction has written the canonical CFA.
    let highlight_work_a = create_float_work_texture(
        device,
        size,
        highlight_work_format,
        "auraw highlight work A",
    );
    let highlight_work_b = create_float_work_texture(
        device,
        size,
        highlight_work_format,
        "auraw highlight work B",
    );

    // Preserve a scene-linear camera-RGB result between demosaic and the
    // display pass. This lets local controls read true
    // RGB neighbourhoods instead of raw Bayer samples.
    let scene_texture = create_demosaic_texture(
        device,
        size,
        demosaic_format,
        "auraw scene-linear camera RGB",
    );
    let has_ai_scene = upload_ai_scene_texture(queue, &scene_texture, demosaic_format, raw)?;

    // The final creative result is tone-mapped into display-linear Rec.2020
    // before any output transfer function is applied. Export reads this
    // surface so resizing happens after demosaic/tone processing and before
    // sRGB encoding.
    let display_linear_texture =
        create_demosaic_texture(device, size, work_format, "auraw display-linear Rec.2020");

    let out_texture = create_processing_texture(
        device,
        size,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        "auraw output texture",
    );

    let tex1 = create_demosaic_texture(device, size, demosaic_format, "auraw tex1");
    let tex2 = create_demosaic_texture(device, size, demosaic_format, "auraw tex2");
    let tone_guide_a = create_tone_guide_texture(
        device,
        tone_size,
        tone_format,
        "auraw adaptive tone guide A",
    );
    let tone_guide_b = create_tone_guide_texture(
        device,
        tone_size,
        tone_format,
        "auraw adaptive tone guide B",
    );
    // The ordinary full-frame interactive pipeline reserves all 32 layers
    // so masks can be added without rebuilding it. Explicit-edge detail and
    // export pipelines allocate only the layers they actually sample.
    let mask_texture = create_processing_texture_array(
        device,
        mask_atlas_edge,
        mask_atlas_edge,
        mask_layer_capacity as u32,
        wgpu::TextureFormat::R16Float,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        "auraw normalized local-mask atlas",
    );
    // Light Rays needs the same full-image emission field in the fit
    // preview, zoom-detail crops, and independently processed export tiles.
    // A compact dedicated atlas is sufficient because shafts intentionally
    // integrate and soften their source over a long distance.
    let light_rays_mask_texture = create_processing_texture_array(
        device,
        LIGHT_RAYS_MASK_ATLAS_EDGE,
        LIGHT_RAYS_MASK_ATLAS_EDGE,
        mask_layer_capacity as u32,
        wgpu::TextureFormat::R16Float,
        wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        "auraw full-image Light Rays emission atlas",
    );
    // Do not upload an all-zero atlas here. With 32 supported layers that would
    // create a very large temporary CPU allocation. Every active layer is uploaded
    // before the first recompute, and shaders never sample layers beyond mask_counts.x.

    let out_view = default_texture_view(&out_texture);
    let display_linear_view = default_texture_view(&display_linear_texture);
    let reconstructed_raw_view = default_texture_view(&reconstructed_raw_texture);
    let highlight_work_a_view = default_texture_view(&highlight_work_a);
    let highlight_work_b_view = default_texture_view(&highlight_work_b);
    let scene_view = default_texture_view(&scene_texture);
    let tex1_view = default_texture_view(&tex1);
    let tex2_view = default_texture_view(&tex2);
    let tone_guide_a_view = default_texture_view(&tone_guide_a);
    let tone_guide_b_view = default_texture_view(&tone_guide_b);
    let raw_view = default_texture_view(&raw_texture);
    let color_view = default_texture_view(&color_texture);
    let black_view = default_texture_view(&black_texture);
    let mask_view = mask_texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("auraw local-mask array view"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let light_rays_mask_view = light_rays_mask_texture.create_view(&wgpu::TextureViewDescriptor {
        label: Some("auraw full-image Light Rays mask array view"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let mask_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("auraw local-mask linear sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    });

    Ok((
        PipelineSurfaces {
            raw_texture,
            color_texture,
            black_texture,
            reconstructed_raw_texture,
            highlight_work_a,
            highlight_work_b,
            scene_texture,
            display_linear_texture,
            out_texture,
            tex1,
            tex2,
            tone_guide_a,
            tone_guide_b,
            mask_texture,
            light_rays_mask_texture,
            out_view,
            display_linear_view,
            reconstructed_raw_view,
            highlight_work_a_view,
            highlight_work_b_view,
            scene_view,
            tex1_view,
            tex2_view,
            tone_guide_a_view,
            tone_guide_b_view,
            raw_view,
            color_view,
            black_view,
            mask_view,
            light_rays_mask_view,
            mask_sampler,
        },
        has_ai_scene,
    ))
}

/// Every persistent GPU buffer one pipeline instance owns, plus the validated
/// profile-buffer accounting values used by later uploads.
pub(super) struct PipelineBuffers {
    pub(super) profile_buffer: wgpu::Buffer,
    pub(super) output_lut_offset_bytes: u64,
    pub(super) camera_uniforms_buffer: wgpu::Buffer,
    pub(super) scene_tone_uniforms_buffer: wgpu::Buffer,
    pub(super) effects_uniforms_buffer: wgpu::Buffer,
    pub(super) mask_data_buffer: wgpu::Buffer,
    pub(super) tone_histogram_buffer: wgpu::Buffer,
    pub(super) tone_stats_buffer: wgpu::Buffer,
}

pub(super) fn create_pipeline_buffers(
    device: &wgpu::Device,
    params: &GpuParams,
    profile_words: &[[f32; 4]],
    output_lut_word_offset: u32,
) -> PipelineBuffers {
    let output_lut_offset_bytes =
        u64::from(output_lut_word_offset) * std::mem::size_of::<[f32; 4]>() as u64;
    let profile_buffer = create_initialized_buffer(
        device,
        "auraw DCP and ICC profile LUTs",
        bytemuck::cast_slice(profile_words),
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    );

    let camera_uniforms_buffer = create_initialized_buffer(
        device,
        "auraw camera uniforms",
        params.camera_bytes(),
        wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    );
    let scene_tone_uniforms_buffer = create_initialized_buffer(
        device,
        "auraw scene-tone uniforms",
        params.scene_tone_bytes(),
        wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    );
    let effects_uniforms_buffer = create_initialized_buffer(
        device,
        "auraw effects uniforms",
        params.effects_bytes(),
        wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    );
    let mask_data_buffer = create_initialized_buffer(
        device,
        "auraw local-mask data",
        params.mask_data_bytes(),
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    );

    let tone_histogram_buffer = create_gpu_buffer(
        device,
        "auraw tone histogram",
        256 * std::mem::size_of::<u32>() as u64,
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
    );
    let tone_stats_buffer = create_gpu_buffer(
        device,
        "auraw tone statistics",
        TONE_STATS_SIZE_BYTES,
        wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
    );

    PipelineBuffers {
        profile_buffer,
        output_lut_offset_bytes,
        camera_uniforms_buffer,
        scene_tone_uniforms_buffer,
        effects_uniforms_buffer,
        mask_data_buffer,
        tone_histogram_buffer,
        tone_stats_buffer,
    }
}

/// Every group-0 bind group layout of the pass graph, plus the shared
/// scene-tone/effects layouts bound as groups 1 and 2 by every dispatch.
pub(super) struct BindGroupLayouts {
    pub(super) bgl_scene_tone: wgpu::BindGroupLayout,
    pub(super) bgl_effects: wgpu::BindGroupLayout,
    pub(super) bgl_highlights: wgpu::BindGroupLayout,
    pub(super) bgl1: wgpu::BindGroupLayout,
    pub(super) bgl2: wgpu::BindGroupLayout,
    pub(super) bgl3: wgpu::BindGroupLayout,
    pub(super) bgl_dual_green: wgpu::BindGroupLayout,
    pub(super) bgl_dual_rgb: wgpu::BindGroupLayout,
    pub(super) bgl4: wgpu::BindGroupLayout,
    pub(super) bgl_xtrans_derivatives: wgpu::BindGroupLayout,
    pub(super) bgl_xtrans_homogeneity: wgpu::BindGroupLayout,
    pub(super) bgl_xtrans_accumulate: wgpu::BindGroupLayout,
    pub(super) bgl_xtrans_finish: wgpu::BindGroupLayout,
    pub(super) bgl_color_denoise: wgpu::BindGroupLayout,
    pub(super) bgl_tone_prepare: wgpu::BindGroupLayout,
    pub(super) bgl_tone_blur: wgpu::BindGroupLayout,
    pub(super) bgl_tone_reduce: wgpu::BindGroupLayout,
    pub(super) bgl_adjust_prepare: wgpu::BindGroupLayout,
    pub(super) bgl_adjust_tone: wgpu::BindGroupLayout,
    pub(super) bgl_adjust_effects: wgpu::BindGroupLayout,
    pub(super) bgl_mask_blur: wgpu::BindGroupLayout,
    pub(super) bgl_glow_prepare: wgpu::BindGroupLayout,
    pub(super) bgl_glow_blur: wgpu::BindGroupLayout,
    pub(super) bgl_adjust_creative: wgpu::BindGroupLayout,
    pub(super) bgl_adjust_render: wgpu::BindGroupLayout,
}

pub(super) fn create_bind_group_layouts(
    device: &wgpu::Device,
    program_template: Option<&RawGpuProgramTemplate>,
    cfa_kind: CfaKind,
    demosaic_format: wgpu::TextureFormat,
    work_format: wgpu::TextureFormat,
    tone_format: wgpu::TextureFormat,
) -> BindGroupLayouts {
    let common_entries = [
        buffer_entry(0),
        texture_entry(1, wgpu::TextureSampleType::Uint),
        texture_entry(2, wgpu::TextureSampleType::Uint),
        texture_entry(19, wgpu::TextureSampleType::Float { filterable: false }),
    ];

    // Groups 1 and 2 are intentionally identical for every compute
    // pipeline. Reusing them across passes isolates scene-tone and effects
    // updates from the camera/raw resource bind groups in group 0.
    let bgl_scene_tone = program_template
        .map(|template| template.pipelines[0].get_bind_group_layout(1))
        .unwrap_or_else(|| {
            create_bind_group_layout(device, "bgl scene-tone uniforms", &[buffer_entry(0)])
        });
    let bgl_effects = program_template
        .map(|template| template.pipelines[0].get_bind_group_layout(2))
        .unwrap_or_else(|| {
            create_bind_group_layout(device, "bgl effects uniforms", &[buffer_entry(0)])
        });

    let demosaic_start_for_programs = 1;
    let demosaic_high_pass_count = match cfa_kind {
        CfaKind::Bayer => 3,
        CfaKind::XTrans => 7,
    };
    let dual_green_for_programs = demosaic_start_for_programs + demosaic_high_pass_count;
    let dual_rgb_for_programs = dual_green_for_programs + 1;
    let demosaic_finish_for_programs = dual_rgb_for_programs + 1;
    let color_denoise_for_programs = demosaic_finish_for_programs + 1;
    let tone_prepare_for_programs = color_denoise_for_programs + COLOR_DENOISE_ENTRY_POINTS.len();
    let adjustment_prepare_for_programs = tone_prepare_for_programs + 4;
    let reused_layout = |pass_index: usize| {
        program_template.map(|template| template.pipelines[pass_index].get_bind_group_layout(0))
    };

    let bgl_highlights = reused_layout(0).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl highlights",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                storage_texture_entry(
                    3,
                    wgpu::TextureFormat::R32Float,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
            ],
        )
    });

    let bgl1 = reused_layout(demosaic_start_for_programs).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl1",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(4, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl2 = reused_layout(demosaic_start_for_programs + 1).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl2",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(5, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(6, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl3 = reused_layout(demosaic_start_for_programs + 2).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl3",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(7, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(8, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl_dual_green = reused_layout(dual_green_for_programs).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl dual demosaic green",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(20, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl_dual_rgb = reused_layout(dual_rgb_for_programs).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl dual demosaic rgb",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(21, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(22, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl4 = (matches!(cfa_kind, CfaKind::Bayer)
        .then(|| reused_layout(demosaic_finish_for_programs))
        .flatten())
    .unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl4",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(7, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(9, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(23, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(10, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    // X-Trans Markesteijn-3 uses the two highlight work textures as
    // derivative scratch after highlight reconstruction has finalized.
    // This retains the reference eight-direction homogeneity stages without
    // allocating eight full-resolution RGB candidate images.
    let bgl_xtrans_derivatives = (matches!(cfa_kind, CfaKind::XTrans)
        .then(|| reused_layout(demosaic_start_for_programs + 4))
        .flatten())
    .unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl X-Trans derivatives",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(9, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(20, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
                storage_texture_entry(21, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl_xtrans_homogeneity = (matches!(cfa_kind, CfaKind::XTrans)
        .then(|| reused_layout(demosaic_start_for_programs + 5))
        .flatten())
    .unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl X-Trans homogeneity",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(27, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(28, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(24, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
                storage_texture_entry(25, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl_xtrans_accumulate = (matches!(cfa_kind, CfaKind::XTrans)
        .then(|| reused_layout(demosaic_start_for_programs + 6))
        .flatten())
    .unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl X-Trans accumulate",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(9, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(29, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(30, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(26, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl_xtrans_finish = (matches!(cfa_kind, CfaKind::XTrans)
        .then(|| reused_layout(demosaic_finish_for_programs))
        .flatten())
    .unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl X-Trans finish",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(26, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(23, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(10, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl_color_denoise = reused_layout(color_denoise_for_programs).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl multiscale color denoise",
            &[
                buffer_entry(0),
                storage_texture_entry(10, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
                texture_entry(11, wgpu::TextureSampleType::Float { filterable: false }),
            ],
        )
    });

    let bgl_tone_prepare = reused_layout(tone_prepare_for_programs).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl tone prepare",
            &[
                buffer_entry(0),
                texture_entry(11, wgpu::TextureSampleType::Float { filterable: false }),
                storage_buffer_entry(15, false),
                storage_buffer_entry(20, true),
                storage_texture_entry(18, tone_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl_tone_blur = reused_layout(tone_prepare_for_programs + 1).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl tone guide blur",
            &[
                buffer_entry(0),
                texture_entry(17, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(18, tone_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl_tone_reduce = reused_layout(tone_prepare_for_programs + 3).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl tone histogram reduction",
            &[
                storage_buffer_entry(15, false),
                storage_buffer_entry(16, false),
            ],
        )
    });

    let bgl_adjust_prepare = reused_layout(adjustment_prepare_for_programs).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl scene preparation",
            &[
                common_entries[0],
                common_entries[1],
                common_entries[2],
                common_entries[3],
                texture_entry(11, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(21, work_format, wgpu::StorageTextureAccess::WriteOnly),
                storage_buffer_entry(16, true),
                texture_entry(17, wgpu::TextureSampleType::Float { filterable: false }),
                storage_buffer_entry(20, true),
                texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
                sampler_entry(28),
                storage_buffer_entry(33, true),
            ],
        )
    });

    let bgl_adjust_tone = reused_layout(adjustment_prepare_for_programs + 1).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl scene tone edits",
            &[
                buffer_entry(0),
                texture_entry(22, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(23, work_format, wgpu::StorageTextureAccess::WriteOnly),
                storage_buffer_entry(16, true),
                texture_entry(17, wgpu::TextureSampleType::Float { filterable: false }),
                storage_buffer_entry(20, true),
                texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
                sampler_entry(28),
                storage_buffer_entry(33, true),
            ],
        )
    });

    let bgl_adjust_effects =
        reused_layout(adjustment_prepare_for_programs + 3).unwrap_or_else(|| {
            create_bind_group_layout(
                device,
                "bgl scene presence and color",
                &[
                    buffer_entry(0),
                    texture_entry(22, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(23, work_format, wgpu::StorageTextureAccess::WriteOnly),
                    storage_buffer_entry(16, true),
                    texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
                    sampler_entry(28),
                    storage_buffer_entry(33, true),
                ],
            )
        });

    let bgl_mask_blur = reused_layout(adjustment_prepare_for_programs + 5).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl mask Blur diffusion",
            &[
                buffer_entry(0),
                texture_entry(24, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(25, work_format, wgpu::StorageTextureAccess::WriteOnly),
                texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
                sampler_entry(28),
                storage_buffer_entry(33, true),
            ],
        )
    });

    let bgl_glow_prepare =
        reused_layout(adjustment_prepare_for_programs + 10).unwrap_or_else(|| {
            create_bind_group_layout(
                device,
                "bgl Glow source extraction",
                &[
                    buffer_entry(0),
                    texture_entry(24, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(31, work_format, wgpu::StorageTextureAccess::WriteOnly),
                    texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
                    sampler_entry(28),
                    storage_buffer_entry(33, true),
                ],
            )
        });

    let bgl_glow_blur = reused_layout(adjustment_prepare_for_programs + 11).unwrap_or_else(|| {
        create_bind_group_layout(
            device,
            "bgl Glow diffusion",
            &[
                buffer_entry(0),
                texture_entry(30, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(31, work_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        )
    });

    let bgl_adjust_creative =
        reused_layout(adjustment_prepare_for_programs + 16).unwrap_or_else(|| {
            create_bind_group_layout(
                device,
                "bgl creative glow",
                &[
                    buffer_entry(0),
                    texture_entry(24, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(25, work_format, wgpu::StorageTextureAccess::WriteOnly),
                    texture_entry(30, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
                    sampler_entry(28),
                    storage_buffer_entry(33, true),
                    texture_array_entry(34, wgpu::TextureSampleType::Float { filterable: true }),
                ],
            )
        });

    let bgl_adjust_render = create_bind_group_layout(
        device,
        "bgl scene look view and output",
        &[
            buffer_entry(0),
            storage_texture_entry(
                12,
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::StorageTextureAccess::WriteOnly,
            ),
            texture_entry(26, wgpu::TextureSampleType::Float { filterable: false }),
            // The final DCP/view shoulder reads the cached scene percentiles
            // to choose a headroom-aware highlight knee. Keep binding 16 in
            // this entry point's layout even though earlier adjustment passes
            // already bind the same tone-statistics buffer independently.
            storage_buffer_entry(16, true),
            storage_buffer_entry(20, true),
            texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
            sampler_entry(28),
            storage_texture_entry(29, work_format, wgpu::StorageTextureAccess::WriteOnly),
            storage_buffer_entry(33, true),
        ],
    );
    let bgl_adjust_render =
        reused_layout(adjustment_prepare_for_programs + 17).unwrap_or(bgl_adjust_render);

    BindGroupLayouts {
        bgl_scene_tone,
        bgl_effects,
        bgl_highlights,
        bgl1,
        bgl2,
        bgl3,
        bgl_dual_green,
        bgl_dual_rgb,
        bgl4,
        bgl_xtrans_derivatives,
        bgl_xtrans_homogeneity,
        bgl_xtrans_accumulate,
        bgl_xtrans_finish,
        bgl_color_denoise,
        bgl_tone_prepare,
        bgl_tone_blur,
        bgl_tone_reduce,
        bgl_adjust_prepare,
        bgl_adjust_tone,
        bgl_adjust_effects,
        bgl_mask_blur,
        bgl_glow_prepare,
        bgl_glow_blur,
        bgl_adjust_creative,
        bgl_adjust_render,
    }
}

/// Every group-0 bind group of the pass graph, plus the shared scene-tone and
/// effects bind groups dispatched as groups 1 and 2 with every pass.
pub(super) struct BindGroups {
    pub(super) scene_tone_bind_group: wgpu::BindGroup,
    pub(super) effects_bind_group: wgpu::BindGroup,
    pub(super) bg_highlights: wgpu::BindGroup,
    pub(super) bg1: wgpu::BindGroup,
    pub(super) bg2: wgpu::BindGroup,
    pub(super) bg3: wgpu::BindGroup,
    pub(super) bg_dual_green: wgpu::BindGroup,
    pub(super) bg_dual_rgb: wgpu::BindGroup,
    pub(super) bg4: wgpu::BindGroup,
    pub(super) bg_xtrans_derivatives: wgpu::BindGroup,
    pub(super) bg_xtrans_homogeneity: wgpu::BindGroup,
    pub(super) bg_xtrans_accumulate: wgpu::BindGroup,
    pub(super) bg_xtrans_finish: wgpu::BindGroup,
    pub(super) bg_color_denoise: [wgpu::BindGroup; 6],
    pub(super) bg_tone_prepare: wgpu::BindGroup,
    pub(super) bg_tone_horizontal: wgpu::BindGroup,
    pub(super) bg_tone_vertical: wgpu::BindGroup,
    pub(super) bg_tone_reduce: wgpu::BindGroup,
    pub(super) bg_adjust_prepare: wgpu::BindGroup,
    pub(super) bg_adjust_tone: wgpu::BindGroup,
    pub(super) bg_adjust_local_tone: wgpu::BindGroup,
    pub(super) bg_adjust_effects: wgpu::BindGroup,
    pub(super) bg_adjust_effects_copy: wgpu::BindGroup,
    pub(super) bg_mask_blur_0: wgpu::BindGroup,
    pub(super) bg_mask_blur_1: wgpu::BindGroup,
    pub(super) bg_mask_blur_2: wgpu::BindGroup,
    pub(super) bg_mask_blur_3: wgpu::BindGroup,
    pub(super) bg_mask_blur_4: wgpu::BindGroup,
    pub(super) bg_glow_prepare: wgpu::BindGroup,
    pub(super) bg_glow_blur_0: wgpu::BindGroup,
    pub(super) bg_glow_blur_1: wgpu::BindGroup,
    pub(super) bg_glow_blur_2: wgpu::BindGroup,
    pub(super) bg_glow_blur_3: wgpu::BindGroup,
    pub(super) bg_glow_blur_4: wgpu::BindGroup,
    pub(super) bg_glow_prepare_after_blur: wgpu::BindGroup,
    pub(super) bg_glow_blur_after_blur_0: wgpu::BindGroup,
    pub(super) bg_glow_blur_after_blur_1: wgpu::BindGroup,
    pub(super) bg_glow_blur_after_blur_2: wgpu::BindGroup,
    pub(super) bg_glow_blur_after_blur_3: wgpu::BindGroup,
    pub(super) bg_glow_blur_after_blur_4: wgpu::BindGroup,
    pub(super) bg_adjust_creative: wgpu::BindGroup,
    pub(super) bg_adjust_creative_after_blur: wgpu::BindGroup,
    pub(super) bg_adjust_render: wgpu::BindGroup,
    pub(super) bg_adjust_render_after_blur: wgpu::BindGroup,
}

pub(super) fn create_bind_groups(
    device: &wgpu::Device,
    layouts: &BindGroupLayouts,
    buffers: &PipelineBuffers,
    surfaces: &PipelineSurfaces,
    cfa_kind: CfaKind,
) -> BindGroups {
    let BindGroupLayouts {
        bgl_scene_tone,
        bgl_effects,
        bgl_highlights,
        bgl1,
        bgl2,
        bgl3,
        bgl_dual_green,
        bgl_dual_rgb,
        bgl4,
        bgl_xtrans_derivatives,
        bgl_xtrans_homogeneity,
        bgl_xtrans_accumulate,
        bgl_xtrans_finish,
        bgl_color_denoise,
        bgl_tone_prepare,
        bgl_tone_blur,
        bgl_tone_reduce,
        bgl_adjust_prepare,
        bgl_adjust_tone,
        bgl_adjust_effects,
        bgl_mask_blur,
        bgl_glow_prepare,
        bgl_glow_blur,
        bgl_adjust_creative,
        bgl_adjust_render,
    } = layouts;
    let PipelineBuffers {
        camera_uniforms_buffer,
        scene_tone_uniforms_buffer,
        effects_uniforms_buffer,
        mask_data_buffer,
        tone_histogram_buffer,
        tone_stats_buffer,
        profile_buffer,
        ..
    } = buffers;
    let PipelineSurfaces {
        out_view,
        display_linear_view,
        reconstructed_raw_view,
        highlight_work_a_view,
        highlight_work_b_view,
        scene_view,
        tex1_view,
        tex2_view,
        tone_guide_a_view,
        tone_guide_b_view,
        raw_view,
        color_view,
        black_view,
        mask_view,
        light_rays_mask_view,
        mask_sampler,
        ..
    } = surfaces;

    let scene_tone_bind_group = create_bind_group(
        device,
        "bg scene-tone uniforms",
        bgl_scene_tone,
        &[buffer_binding(0, scene_tone_uniforms_buffer)],
    );
    let effects_bind_group = create_bind_group(
        device,
        "bg effects uniforms",
        bgl_effects,
        &[buffer_binding(0, effects_uniforms_buffer)],
    );

    let bg_highlights = create_bind_group(
        device,
        "bg highlight reconstruction",
        bgl_highlights,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
        ],
    );

    let bg1 = create_bind_group(
        device,
        "bg1",
        bgl1,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(4, tex1_view),
        ],
    );

    let bg2 = create_bind_group(
        device,
        "bg2",
        bgl2,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(5, tex1_view),
            texture_binding(6, tex2_view),
        ],
    );

    let bg3 = create_bind_group(
        device,
        "bg3",
        bgl3,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(7, tex2_view),
            texture_binding(8, tex1_view),
        ],
    );

    let (dual_green_view, dual_low_view) = match cfa_kind {
        CfaKind::Bayer => (highlight_work_a_view, highlight_work_b_view),
        CfaKind::XTrans => (tex1_view, tex2_view),
    };

    let bg_dual_green = create_bind_group(
        device,
        "bg dual demosaic green",
        bgl_dual_green,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(20, dual_green_view),
        ],
    );

    let bg_dual_rgb = create_bind_group(
        device,
        "bg dual demosaic rgb",
        bgl_dual_rgb,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(21, dual_green_view),
            texture_binding(22, dual_low_view),
        ],
    );

    let bg4 = create_bind_group(
        device,
        "bg4",
        bgl4,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(7, tex2_view),
            texture_binding(9, tex1_view),
            texture_binding(23, dual_low_view),
            texture_binding(10, scene_view),
        ],
    );

    let bg_xtrans_derivatives = create_bind_group(
        device,
        "bg X-Trans derivatives",
        bgl_xtrans_derivatives,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(9, tex2_view),
            texture_binding(20, highlight_work_a_view),
            texture_binding(21, highlight_work_b_view),
        ],
    );

    let bg_xtrans_homogeneity = create_bind_group(
        device,
        "bg X-Trans homogeneity",
        bgl_xtrans_homogeneity,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(27, highlight_work_a_view),
            texture_binding(28, highlight_work_b_view),
            texture_binding(24, tex1_view),
            texture_binding(25, scene_view),
        ],
    );

    let bg_xtrans_accumulate = create_bind_group(
        device,
        "bg X-Trans accumulate",
        bgl_xtrans_accumulate,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(9, tex2_view),
            texture_binding(29, tex1_view),
            texture_binding(30, scene_view),
            texture_binding(26, highlight_work_a_view),
        ],
    );

    let bg_xtrans_finish = create_bind_group(
        device,
        "bg X-Trans finish",
        bgl_xtrans_finish,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(3, reconstructed_raw_view),
            texture_binding(26, highlight_work_a_view),
            texture_binding(23, dual_low_view),
            texture_binding(10, scene_view),
        ],
    );

    let make_color_denoise_bind_group =
        |label: &str, read_view: &wgpu::TextureView, write_view: &wgpu::TextureView| {
            create_bind_group(
                device,
                label,
                bgl_color_denoise,
                &[
                    buffer_binding(0, camera_uniforms_buffer),
                    texture_binding(10, write_view),
                    texture_binding(11, read_view),
                ],
            )
        };
    // Six passes end back in scene_texture. Disabled Fast/Balanced scales
    // are explicit copies so every quality setting has identical parity.
    let bg_color_denoise = [
        make_color_denoise_bind_group("bg color denoise scale 1", scene_view, tex1_view),
        make_color_denoise_bind_group("bg color denoise scale 2", tex1_view, tex2_view),
        make_color_denoise_bind_group("bg color denoise scale 4", tex2_view, tex1_view),
        make_color_denoise_bind_group("bg color denoise scale 8", tex1_view, tex2_view),
        make_color_denoise_bind_group("bg color denoise scale 16", tex2_view, tex1_view),
        make_color_denoise_bind_group("bg color denoise scale 32", tex1_view, scene_view),
    ];

    let bg_tone_prepare = create_bind_group(
        device,
        "bg tone prepare",
        bgl_tone_prepare,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(11, scene_view),
            buffer_binding(15, tone_histogram_buffer),
            buffer_binding(20, profile_buffer),
            texture_binding(18, tone_guide_a_view),
        ],
    );

    let make_tone_blur_bind_group =
        |label: &str, read_view: &wgpu::TextureView, write_view: &wgpu::TextureView| {
            create_bind_group(
                device,
                label,
                bgl_tone_blur,
                &[
                    buffer_binding(0, camera_uniforms_buffer),
                    texture_binding(17, read_view),
                    texture_binding(18, write_view),
                ],
            )
        };
    let bg_tone_horizontal = make_tone_blur_bind_group(
        "bg tone guide horizontal",
        tone_guide_a_view,
        tone_guide_b_view,
    );
    let bg_tone_vertical = make_tone_blur_bind_group(
        "bg tone guide vertical",
        tone_guide_b_view,
        tone_guide_a_view,
    );

    let bg_tone_reduce = create_bind_group(
        device,
        "bg tone histogram reduction",
        bgl_tone_reduce,
        &[
            buffer_binding(15, tone_histogram_buffer),
            buffer_binding(16, tone_stats_buffer),
        ],
    );

    let bg_adjust_prepare = create_bind_group(
        device,
        "bg adjustment preparation",
        bgl_adjust_prepare,
        &[
            buffer_binding(0, camera_uniforms_buffer),
            texture_binding(1, raw_view),
            texture_binding(2, color_view),
            texture_binding(19, black_view),
            texture_binding(11, scene_view),
            texture_binding(21, tex1_view),
            buffer_binding(16, tone_stats_buffer),
            texture_binding(17, tone_guide_a_view),
            buffer_binding(20, profile_buffer),
            texture_binding(27, mask_view),
            sampler_binding(28, mask_sampler),
            buffer_binding(33, mask_data_buffer),
        ],
    );

    let make_adjust_tone_bind_group =
        |label: &str, input: &wgpu::TextureView, output: &wgpu::TextureView| {
            create_bind_group(
                device,
                label,
                bgl_adjust_tone,
                &[
                    buffer_binding(0, camera_uniforms_buffer),
                    texture_binding(22, input),
                    texture_binding(23, output),
                    buffer_binding(16, tone_stats_buffer),
                    texture_binding(17, tone_guide_a_view),
                    buffer_binding(20, profile_buffer),
                    texture_binding(27, mask_view),
                    sampler_binding(28, mask_sampler),
                    buffer_binding(33, mask_data_buffer),
                ],
            )
        };
    let bg_adjust_tone = make_adjust_tone_bind_group("bg scene tone edits", tex1_view, tex2_view);
    let bg_adjust_local_tone =
        make_adjust_tone_bind_group("bg local scene tone edits", tex2_view, tex1_view);

    let make_adjust_effects_bind_group =
        |label: &str, input: &wgpu::TextureView, output: &wgpu::TextureView| {
            create_bind_group(
                device,
                label,
                bgl_adjust_effects,
                &[
                    buffer_binding(0, camera_uniforms_buffer),
                    texture_binding(22, input),
                    texture_binding(23, output),
                    buffer_binding(16, tone_stats_buffer),
                    texture_binding(27, mask_view),
                    sampler_binding(28, mask_sampler),
                    buffer_binding(33, mask_data_buffer),
                ],
            )
        };
    let bg_adjust_effects =
        make_adjust_effects_bind_group("bg scene presence and color", tex1_view, tex2_view);
    let bg_adjust_effects_copy =
        make_adjust_effects_bind_group("bg scene effects copy", tex2_view, tex1_view);

    // Mask Blur diffuses the completed local-effects image through five
    // adjacent scales. It ends in tex2 and keeps tex1 available until the
    // chain has fully incorporated mask coverage and Amount.
    let make_mask_blur_bind_group =
        |label: &str, read_view: &wgpu::TextureView, write_view: &wgpu::TextureView| {
            create_bind_group(
                device,
                label,
                bgl_mask_blur,
                &[
                    buffer_binding(0, camera_uniforms_buffer),
                    texture_binding(24, read_view),
                    texture_binding(25, write_view),
                    texture_binding(27, mask_view),
                    sampler_binding(28, mask_sampler),
                    buffer_binding(33, mask_data_buffer),
                ],
            )
        };
    let bg_mask_blur_0 =
        make_mask_blur_bind_group("bg mask Blur diffusion 0", tex1_view, tex2_view);
    let bg_mask_blur_1 =
        make_mask_blur_bind_group("bg mask Blur diffusion 1", tex2_view, display_linear_view);
    let bg_mask_blur_2 =
        make_mask_blur_bind_group("bg mask Blur diffusion 2", display_linear_view, tex2_view);
    let bg_mask_blur_3 =
        make_mask_blur_bind_group("bg mask Blur diffusion 3", tex2_view, display_linear_view);
    let bg_mask_blur_4 =
        make_mask_blur_bind_group("bg mask Blur diffusion 4", display_linear_view, tex2_view);

    // Glow is extracted from the completed local-effects image in tex1.
    // Five adjacent B3-spline diffusion stages then ping-pong through tex2
    // and the display-linear surface. The latter is safe scratch here: the
    // final render overwrites it only after the creative composite.
    let make_glow_prepare_bind_group =
        |label: &str, source: &wgpu::TextureView, extracted: &wgpu::TextureView| {
            create_bind_group(
                device,
                label,
                bgl_glow_prepare,
                &[
                    buffer_binding(0, camera_uniforms_buffer),
                    texture_binding(24, source),
                    texture_binding(31, extracted),
                    texture_binding(27, mask_view),
                    sampler_binding(28, mask_sampler),
                    buffer_binding(33, mask_data_buffer),
                ],
            )
        };
    let bg_glow_prepare =
        make_glow_prepare_bind_group("bg Glow source extraction", tex1_view, tex2_view);

    let make_glow_blur_bind_group =
        |label: &str, read_view: &wgpu::TextureView, write_view: &wgpu::TextureView| {
            create_bind_group(
                device,
                label,
                bgl_glow_blur,
                &[
                    buffer_binding(0, camera_uniforms_buffer),
                    texture_binding(30, read_view),
                    texture_binding(31, write_view),
                ],
            )
        };
    let bg_glow_blur_0 =
        make_glow_blur_bind_group("bg Glow diffusion 0", tex2_view, display_linear_view);
    let bg_glow_blur_1 =
        make_glow_blur_bind_group("bg Glow diffusion 1", display_linear_view, tex2_view);
    let bg_glow_blur_2 =
        make_glow_blur_bind_group("bg Glow diffusion 2", tex2_view, display_linear_view);
    let bg_glow_blur_3 =
        make_glow_blur_bind_group("bg Glow diffusion 3", display_linear_view, tex2_view);
    let bg_glow_blur_4 =
        make_glow_blur_bind_group("bg Glow diffusion 4", tex2_view, display_linear_view);

    // When mask Blur ran first, tex2 contains the new creative base. Glow
    // uses tex1/display-linear as scratch so that base remains available
    // for the final creative composite.
    let bg_glow_prepare_after_blur = make_glow_prepare_bind_group(
        "bg Glow source extraction after mask Blur",
        tex2_view,
        tex1_view,
    );
    let bg_glow_blur_after_blur_0 = make_glow_blur_bind_group(
        "bg Glow diffusion after mask Blur 0",
        tex1_view,
        display_linear_view,
    );
    let bg_glow_blur_after_blur_1 = make_glow_blur_bind_group(
        "bg Glow diffusion after mask Blur 1",
        display_linear_view,
        tex1_view,
    );
    let bg_glow_blur_after_blur_2 = make_glow_blur_bind_group(
        "bg Glow diffusion after mask Blur 2",
        tex1_view,
        display_linear_view,
    );
    let bg_glow_blur_after_blur_3 = make_glow_blur_bind_group(
        "bg Glow diffusion after mask Blur 3",
        display_linear_view,
        tex1_view,
    );
    let bg_glow_blur_after_blur_4 = make_glow_blur_bind_group(
        "bg Glow diffusion after mask Blur 4",
        tex1_view,
        display_linear_view,
    );

    // The creative pass keeps the untouched local-effects result in tex1,
    // composites the final Glow diffusion from display_linear and writes
    // the result into tex2. The post-crop vignette is applied later in the
    // always-dispatched display-linear view pass.
    let make_adjust_creative_bind_group =
        |label: &str, input: &wgpu::TextureView, output: &wgpu::TextureView| {
            create_bind_group(
                device,
                label,
                bgl_adjust_creative,
                &[
                    buffer_binding(0, camera_uniforms_buffer),
                    texture_binding(24, input),
                    texture_binding(25, output),
                    texture_binding(30, display_linear_view),
                    texture_binding(27, mask_view),
                    sampler_binding(28, mask_sampler),
                    buffer_binding(33, mask_data_buffer),
                    texture_binding(34, light_rays_mask_view),
                ],
            )
        };
    let bg_adjust_creative =
        make_adjust_creative_bind_group("bg creative glow", tex1_view, tex2_view);
    let bg_adjust_creative_after_blur = make_adjust_creative_bind_group(
        "bg creative effects after mask Blur",
        tex2_view,
        tex1_view,
    );

    let make_adjust_render_bind_group = |label: &str, source: &wgpu::TextureView| {
        create_bind_group(
            device,
            label,
            bgl_adjust_render,
            &[
                buffer_binding(0, camera_uniforms_buffer),
                texture_binding(12, out_view),
                texture_binding(26, source),
                buffer_binding(16, tone_stats_buffer),
                buffer_binding(20, profile_buffer),
                texture_binding(27, mask_view),
                sampler_binding(28, mask_sampler),
                texture_binding(29, display_linear_view),
                buffer_binding(33, mask_data_buffer),
            ],
        )
    };
    let bg_adjust_render =
        make_adjust_render_bind_group("bg scene look view and output", tex2_view);
    let bg_adjust_render_after_blur =
        make_adjust_render_bind_group("bg scene look view and output after mask Blur", tex1_view);

    BindGroups {
        scene_tone_bind_group,
        effects_bind_group,
        bg_highlights,
        bg1,
        bg2,
        bg3,
        bg_dual_green,
        bg_dual_rgb,
        bg4,
        bg_xtrans_derivatives,
        bg_xtrans_homogeneity,
        bg_xtrans_accumulate,
        bg_xtrans_finish,
        bg_color_denoise,
        bg_tone_prepare,
        bg_tone_horizontal,
        bg_tone_vertical,
        bg_tone_reduce,
        bg_adjust_prepare,
        bg_adjust_tone,
        bg_adjust_local_tone,
        bg_adjust_effects,
        bg_adjust_effects_copy,
        bg_mask_blur_0,
        bg_mask_blur_1,
        bg_mask_blur_2,
        bg_mask_blur_3,
        bg_mask_blur_4,
        bg_glow_prepare,
        bg_glow_blur_0,
        bg_glow_blur_1,
        bg_glow_blur_2,
        bg_glow_blur_3,
        bg_glow_blur_4,
        bg_glow_prepare_after_blur,
        bg_glow_blur_after_blur_0,
        bg_glow_blur_after_blur_1,
        bg_glow_blur_after_blur_2,
        bg_glow_blur_after_blur_3,
        bg_glow_blur_after_blur_4,
        bg_adjust_creative,
        bg_adjust_creative_after_blur,
        bg_adjust_render,
        bg_adjust_render_after_blur,
    }
}

/// Format-specialized WGSL sources and their compiled modules. Every field is
/// `None` when a reusable program template supplies already-compiled
/// pipelines, mirroring the original lazy `load_shader` behaviour.
pub(super) struct ShaderSet {
    pub(super) highlight_module: Option<wgpu::ShaderModule>,
    pub(super) bayer_rcd_p1_module: Option<wgpu::ShaderModule>,
    pub(super) bayer_rcd_p2_module: Option<wgpu::ShaderModule>,
    pub(super) bayer_rcd_p3_module: Option<wgpu::ShaderModule>,
    pub(super) bayer_rcd_p4_module: Option<wgpu::ShaderModule>,
    pub(super) dual_demosaic_module: Option<wgpu::ShaderModule>,
    pub(super) xtrans_demosaic_module: Option<wgpu::ShaderModule>,
    pub(super) xtrans_finish_module: Option<wgpu::ShaderModule>,
    pub(super) color_denoise_module: Option<wgpu::ShaderModule>,
    pub(super) tone_analysis_module: Option<wgpu::ShaderModule>,
    pub(super) scene_adjustments_module: Option<wgpu::ShaderModule>,
    pub(super) creative_effects_module: Option<wgpu::ShaderModule>,
    pub(super) view_transform_module: Option<wgpu::ShaderModule>,
}

pub(super) fn load_shader_set(
    device: &wgpu::Device,
    has_program_template: bool,
    demosaic_format: wgpu::TextureFormat,
    work_format: wgpu::TextureFormat,
) -> Result<ShaderSet> {
    // Storage texture declarations are format-specific in the demosaic and
    // scene shaders. Highlight reconstruction writes its fixed R32F CFA.
    let bayer_rcd_p1 = work_shader_source(SHADER_BAYER_RCD_P1, demosaic_format)
        .context("specialize Bayer RCD pass 1 work format")?;
    let bayer_rcd_p2 = work_shader_source(SHADER_BAYER_RCD_P2, demosaic_format)
        .context("specialize Bayer RCD pass 2 work format")?;
    let bayer_rcd_p3 = work_shader_source(SHADER_BAYER_RCD_P3, demosaic_format)
        .context("specialize Bayer RCD pass 3 work format")?;
    let bayer_rcd_p4 = work_shader_source(SHADER_BAYER_RCD_P4, demosaic_format)
        .context("specialize Bayer RCD pass 4 work format")?;
    let dual_demosaic = work_shader_source(SHADER_DUAL_DEMOSAIC, demosaic_format)
        .context("specialize dual-demosaic work format")?;
    let xtrans_demosaic = work_shader_source(SHADER_XTRANS_DEMOSAIC, demosaic_format)
        .context("specialize grouped X-Trans demosaic work format")?;
    let xtrans_finish = work_shader_source(SHADER_XTRANS_FINISH, demosaic_format)
        .context("specialize X-Trans finish work format")?;
    let color_denoise_shader = work_shader_source(SHADER_COLOR_DENOISE, demosaic_format)
        .context("specialize multiscale color denoise work format")?;
    let scene_adjustments_shader = work_shader_source(SHADER_SCENE_ADJUSTMENTS, work_format)
        .context("specialize scene-adjustments shader work format")?;

    let mut shader_manager = (!has_program_template)
        .then(|| ShaderManager::new(work_format))
        .transpose()
        .context("initialize WGSL shader composer")?;
    let mut create_shader =
        |label: &'static str, source: &str, file_name: &str| -> Result<wgpu::ShaderModule> {
            shader_manager
                .as_mut()
                .expect("shader manager exists without a program template")
                .create_shader_module(device, label, source, file_name)
        };
    // One validated Naga module per WGSL entrypoint source. Entry-point
    // pipelines below share these modules instead of recompiling the same
    // source for every pass.
    let mut load_shader = |label: &'static str, source: &str, file_name: &str| {
        if has_program_template {
            Ok(None)
        } else {
            create_shader(label, source, file_name).map(Some)
        }
    };
    let highlight_module = load_shader(
        "auraw highlight module",
        SHADER_HIGHLIGHTS,
        "highlights.wgsl",
    )?;
    let bayer_rcd_p1_module = load_shader(
        "auraw Bayer RCD pass 1",
        bayer_rcd_p1.as_ref(),
        "pass1.wgsl",
    )?;
    let bayer_rcd_p2_module = load_shader(
        "auraw Bayer RCD pass 2",
        bayer_rcd_p2.as_ref(),
        "pass2.wgsl",
    )?;
    let bayer_rcd_p3_module = load_shader(
        "auraw Bayer RCD pass 3",
        bayer_rcd_p3.as_ref(),
        "pass3.wgsl",
    )?;
    let bayer_rcd_p4_module = load_shader(
        "auraw Bayer RCD pass 4",
        bayer_rcd_p4.as_ref(),
        "pass4.wgsl",
    )?;
    let dual_demosaic_module = load_shader(
        "auraw robust dual demosaic",
        dual_demosaic.as_ref(),
        "dual_demosaic.wgsl",
    )?;
    let xtrans_demosaic_module = load_shader(
        "auraw grouped X-Trans demosaic",
        xtrans_demosaic.as_ref(),
        "xtrans_demosaic.wgsl",
    )?;
    let xtrans_finish_module = load_shader(
        "auraw X-Trans finish",
        xtrans_finish.as_ref(),
        "xtrans_finish.wgsl",
    )?;
    let color_denoise_module = load_shader(
        "auraw multiscale color denoise",
        color_denoise_shader.as_ref(),
        "color_denoise.wgsl",
    )?;
    let tone_analysis_module = load_shader(
        "auraw tone analysis",
        SHADER_TONE_ANALYSIS,
        "tone_analysis.wgsl",
    )?;
    let scene_adjustments_module = load_shader(
        "auraw scene adjustments",
        scene_adjustments_shader.as_ref(),
        "scene_adjustments.wgsl",
    )?;
    let creative_effects_module = load_shader(
        "auraw creative effects",
        SHADER_CREATIVE_EFFECTS,
        "creative_effects.wgsl",
    )?;
    let view_transform_module = load_shader(
        "auraw view transform",
        SHADER_VIEW_TRANSFORM,
        "view_transform.wgsl",
    )?;

    Ok(ShaderSet {
        highlight_module,
        bayer_rcd_p1_module,
        bayer_rcd_p2_module,
        bayer_rcd_p3_module,
        bayer_rcd_p4_module,
        dual_demosaic_module,
        xtrans_demosaic_module,
        xtrans_finish_module,
        color_denoise_module,
        tone_analysis_module,
        scene_adjustments_module,
        creative_effects_module,
        view_transform_module,
    })
}

/// Pass-graph index bookkeeping stored on the finished pipeline so stage
/// encoders can dispatch exact sub-ranges without rebuilding bind groups.
#[derive(Clone, Copy, Debug)]
pub(super) struct StageIndices {
    pub(super) tone_prepare_pass_index: usize,
    pub(super) tone_reduce_pass_index: usize,
    pub(super) tone_stage_end: usize,
    pub(super) demosaic_start_index: usize,
    pub(super) demosaic_dual_start_index: usize,
    pub(super) demosaic_dual_end_index: usize,
    pub(super) demosaic_finish_index: usize,
    pub(super) color_denoise_start_index: usize,
    pub(super) color_denoise_end_index: usize,
    pub(super) adjustment_prepare_pass_index: usize,
    pub(super) adjustment_tone_pass_index: usize,
    pub(super) adjustment_effects_pass_index: usize,
    pub(super) mask_blur_start_index: usize,
    pub(super) mask_blur_end_index: usize,
    pub(super) glow_prepare_pass_index: usize,
    pub(super) glow_blur_start_index: usize,
    pub(super) glow_blur_end_index: usize,
    pub(super) adjustment_creative_pass_index: usize,
    pub(super) adjustment_render_pass_index: usize,
}

pub(super) struct AssembledPasses {
    pub(super) passes: Vec<Pass>,
    pub(super) post_blur_glow_passes: Vec<Pass>,
    pub(super) post_blur_creative_pass: Pass,
    pub(super) post_blur_render_pass: Pass,
    pub(super) indices: StageIndices,
}

/// Clones already-compiled pipelines from a program template or compiles new
/// ones, assigning each pass a monotonically increasing program index that
/// matches the template's pass order exactly.
struct PassAssembler<'a> {
    device: &'a wgpu::Device,
    program_template: Option<&'a RawGpuProgramTemplate>,
    pipeline_cache: Option<&'a Arc<PersistentGpuPipelineCache>>,
    bgl_scene_tone: &'a wgpu::BindGroupLayout,
    bgl_effects: &'a wgpu::BindGroupLayout,
    next_program_index: usize,
}

impl PassAssembler<'_> {
    fn make_pass(
        &mut self,
        shader: Option<&wgpu::ShaderModule>,
        entry: &str,
        bgl: &wgpu::BindGroupLayout,
        bind_group: wgpu::BindGroup,
        workgroups: [u32; 3],
    ) -> Pass {
        let program_index = self.next_program_index;
        self.next_program_index += 1;
        let pipeline = if let Some(template) = self.program_template {
            template.pipelines[program_index].clone()
        } else {
            let shader = shader.expect("shader module exists without a program template");
            let pll = self
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(&format!("pll_{}", entry)),
                    bind_group_layouts: &[
                        Some(bgl),
                        Some(self.bgl_scene_tone),
                        Some(self.bgl_effects),
                    ],
                    immediate_size: 0,
                });
            create_compute_pipeline(
                self.device,
                entry,
                &pll,
                shader,
                entry,
                self.pipeline_cache.map(|cache| cache.raw()),
            )
        };
        Pass {
            pipeline,
            bind_group,
            workgroups,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn assemble_passes(
    device: &wgpu::Device,
    program_template: Option<&RawGpuProgramTemplate>,
    pipeline_cache: Option<&Arc<PersistentGpuPipelineCache>>,
    layouts: &BindGroupLayouts,
    groups: &BindGroups,
    shaders: &ShaderSet,
    cfa_kind: CfaKind,
    image_workgroups: [u32; 3],
    tone_workgroups: [u32; 3],
) -> Result<AssembledPasses> {
    let BindGroupLayouts {
        bgl_scene_tone,
        bgl_effects,
        bgl_highlights,
        bgl1,
        bgl2,
        bgl3,
        bgl_dual_green,
        bgl_dual_rgb,
        bgl4,
        bgl_xtrans_derivatives,
        bgl_xtrans_homogeneity,
        bgl_xtrans_accumulate,
        bgl_xtrans_finish,
        bgl_color_denoise,
        bgl_tone_prepare,
        bgl_tone_blur,
        bgl_tone_reduce,
        bgl_adjust_prepare,
        bgl_adjust_tone,
        bgl_adjust_effects,
        bgl_mask_blur,
        bgl_glow_prepare,
        bgl_glow_blur,
        bgl_adjust_creative,
        bgl_adjust_render,
    } = layouts;
    let BindGroups {
        bg_highlights,
        bg1,
        bg2,
        bg3,
        bg_dual_green,
        bg_dual_rgb,
        bg4,
        bg_xtrans_derivatives,
        bg_xtrans_homogeneity,
        bg_xtrans_accumulate,
        bg_xtrans_finish,
        bg_color_denoise,
        bg_tone_prepare,
        bg_tone_horizontal,
        bg_tone_vertical,
        bg_tone_reduce,
        bg_adjust_prepare,
        bg_adjust_tone,
        bg_adjust_local_tone,
        bg_adjust_effects,
        bg_adjust_effects_copy,
        bg_mask_blur_0,
        bg_mask_blur_1,
        bg_mask_blur_2,
        bg_mask_blur_3,
        bg_mask_blur_4,
        bg_glow_prepare,
        bg_glow_blur_0,
        bg_glow_blur_1,
        bg_glow_blur_2,
        bg_glow_blur_3,
        bg_glow_blur_4,
        bg_glow_prepare_after_blur,
        bg_glow_blur_after_blur_0,
        bg_glow_blur_after_blur_1,
        bg_glow_blur_after_blur_2,
        bg_glow_blur_after_blur_3,
        bg_glow_blur_after_blur_4,
        bg_adjust_creative,
        bg_adjust_creative_after_blur,
        bg_adjust_render,
        bg_adjust_render_after_blur,
        ..
    } = groups;
    let ShaderSet {
        highlight_module,
        bayer_rcd_p1_module,
        bayer_rcd_p2_module,
        bayer_rcd_p3_module,
        bayer_rcd_p4_module,
        dual_demosaic_module,
        xtrans_demosaic_module,
        xtrans_finish_module,
        color_denoise_module,
        tone_analysis_module,
        scene_adjustments_module,
        creative_effects_module,
        view_transform_module,
    } = shaders;

    let mut assembler = PassAssembler {
        device,
        program_template,
        pipeline_cache,
        bgl_scene_tone,
        bgl_effects,
        next_program_index: 0,
    };
    let single_workgroup = [1, 1, 1];

    let mut passes = Vec::with_capacity(expected_pass_count(cfa_kind));

    // Reconstruct clipped photosites before every demosaic path.
    passes.push(assembler.make_pass(
        highlight_module.as_ref(),
        "highlight_reconstruct",
        bgl_highlights,
        bg_highlights.clone(),
        image_workgroups,
    ));

    let demosaic_start_index = passes.len();
    // Build the high-detail reference first. The robust low-frequency
    // branch is represented by two real full-frame buffers, but its two
    // dispatches are skipped at encode time unless Dual mode is selected.
    match cfa_kind {
        CfaKind::Bayer => passes.extend([
            assembler.make_pass(
                bayer_rcd_p1_module.as_ref(),
                "bayer_rcd_directional",
                bgl1,
                bg1.clone(),
                image_workgroups,
            ),
            assembler.make_pass(
                bayer_rcd_p2_module.as_ref(),
                "bayer_rcd_green",
                bgl2,
                bg2.clone(),
                image_workgroups,
            ),
            assembler.make_pass(
                bayer_rcd_p3_module.as_ref(),
                "bayer_rcd_chroma",
                bgl3,
                bg3.clone(),
                image_workgroups,
            ),
        ]),
        CfaKind::XTrans => passes.extend([
            assembler.make_pass(
                xtrans_demosaic_module.as_ref(),
                "xtrans_seed",
                bgl1,
                bg1.clone(),
                image_workgroups,
            ),
            assembler.make_pass(
                xtrans_demosaic_module.as_ref(),
                "xtrans_markesteijn_pass1",
                bgl2,
                bg2.clone(),
                image_workgroups,
            ),
            assembler.make_pass(
                xtrans_demosaic_module.as_ref(),
                "xtrans_markesteijn_pass2",
                bgl3,
                bg3.clone(),
                image_workgroups,
            ),
            assembler.make_pass(
                xtrans_demosaic_module.as_ref(),
                "xtrans_markesteijn_pass3",
                bgl2,
                bg2.clone(),
                image_workgroups,
            ),
            assembler.make_pass(
                xtrans_demosaic_module.as_ref(),
                "xtrans_markesteijn_derivatives",
                bgl_xtrans_derivatives,
                bg_xtrans_derivatives.clone(),
                image_workgroups,
            ),
            assembler.make_pass(
                xtrans_demosaic_module.as_ref(),
                "xtrans_markesteijn_homogeneity",
                bgl_xtrans_homogeneity,
                bg_xtrans_homogeneity.clone(),
                image_workgroups,
            ),
            assembler.make_pass(
                xtrans_demosaic_module.as_ref(),
                "xtrans_markesteijn_accumulate",
                bgl_xtrans_accumulate,
                bg_xtrans_accumulate.clone(),
                image_workgroups,
            ),
        ]),
    }

    let demosaic_dual_start_index = passes.len();
    passes.extend([
        assembler.make_pass(
            dual_demosaic_module.as_ref(),
            "dual_green_reconstruct",
            bgl_dual_green,
            bg_dual_green.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            dual_demosaic_module.as_ref(),
            "dual_rgb_reconstruct",
            bgl_dual_rgb,
            bg_dual_rgb.clone(),
            image_workgroups,
        ),
    ]);
    let demosaic_dual_end_index = passes.len();

    let demosaic_finish_index = passes.len();
    match cfa_kind {
        CfaKind::Bayer => passes.push(assembler.make_pass(
            bayer_rcd_p4_module.as_ref(),
            "bayer_rcd_output",
            bgl4,
            bg4.clone(),
            image_workgroups,
        )),
        CfaKind::XTrans => passes.push(assembler.make_pass(
            xtrans_finish_module.as_ref(),
            "xtrans_demosaic_finish",
            bgl_xtrans_finish,
            bg_xtrans_finish.clone(),
            image_workgroups,
        )),
    }

    let color_denoise_start_index = passes.len();
    for (entry, bind_group) in COLOR_DENOISE_ENTRY_POINTS
        .iter()
        .zip(bg_color_denoise.iter())
    {
        passes.push(assembler.make_pass(
            color_denoise_module.as_ref(),
            entry,
            bgl_color_denoise,
            bind_group.clone(),
            image_workgroups,
        ));
    }
    let color_denoise_end_index = passes.len();

    // Analyze the unexposed scene at reduced resolution. The guide is
    // bilateral and the histogram reduction emits robust tonal anchors.
    // recompute() clears the histogram immediately before this pass.
    let tone_prepare_pass_index = passes.len();
    passes.extend([
        assembler.make_pass(
            tone_analysis_module.as_ref(),
            "tone_guide_prepare",
            bgl_tone_prepare,
            bg_tone_prepare.clone(),
            tone_workgroups,
        ),
        assembler.make_pass(
            tone_analysis_module.as_ref(),
            "tone_guide_horizontal",
            bgl_tone_blur,
            bg_tone_horizontal.clone(),
            tone_workgroups,
        ),
        assembler.make_pass(
            tone_analysis_module.as_ref(),
            "tone_guide_vertical",
            bgl_tone_blur,
            bg_tone_vertical.clone(),
            tone_workgroups,
        ),
        assembler.make_pass(
            tone_analysis_module.as_ref(),
            "tone_reduce_histogram",
            bgl_tone_reduce,
            bg_tone_reduce.clone(),
            single_workgroup,
        ),
    ]);

    let tone_reduce_pass_index = tone_prepare_pass_index + 3;
    let tone_stage_end = passes.len();
    let adjustment_prepare_pass_index = passes.len();
    let adjustment_tone_pass_index = adjustment_prepare_pass_index + 1;
    let adjustment_effects_pass_index = adjustment_prepare_pass_index + 3;
    let mask_blur_start_index = adjustment_prepare_pass_index + 5;
    let mask_blur_end_index = mask_blur_start_index + 5;
    let glow_prepare_pass_index = mask_blur_end_index;
    let glow_blur_start_index = glow_prepare_pass_index + 1;
    let glow_blur_end_index = glow_blur_start_index + 5;
    let adjustment_creative_pass_index = glow_blur_end_index;
    let adjustment_render_pass_index = adjustment_creative_pass_index + 1;

    passes.extend([
        assembler.make_pass(
            scene_adjustments_module.as_ref(),
            "prepare_scene_node",
            bgl_adjust_prepare,
            bg_adjust_prepare.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            scene_adjustments_module.as_ref(),
            "apply_scene_tone_node",
            bgl_adjust_tone,
            bg_adjust_tone.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            scene_adjustments_module.as_ref(),
            "apply_local_scene_tone_node",
            bgl_adjust_tone,
            bg_adjust_local_tone.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "apply_scene_effects_node",
            bgl_adjust_effects,
            bg_adjust_effects.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "copy_scene_effects_node",
            bgl_adjust_effects,
            bg_adjust_effects_copy.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_mask_blur_0",
            bgl_mask_blur,
            bg_mask_blur_0.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_mask_blur_1",
            bgl_mask_blur,
            bg_mask_blur_1.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_mask_blur_2",
            bgl_mask_blur,
            bg_mask_blur_2.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_mask_blur_3",
            bgl_mask_blur,
            bg_mask_blur_3.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_mask_blur_4",
            bgl_mask_blur,
            bg_mask_blur_4.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "prepare_glow_source",
            bgl_glow_prepare,
            bg_glow_prepare.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_glow_0",
            bgl_glow_blur,
            bg_glow_blur_0.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_glow_1",
            bgl_glow_blur,
            bg_glow_blur_1.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_glow_2",
            bgl_glow_blur,
            bg_glow_blur_2.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_glow_3",
            bgl_glow_blur,
            bg_glow_blur_3.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "diffuse_glow_4",
            bgl_glow_blur,
            bg_glow_blur_4.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            creative_effects_module.as_ref(),
            "apply_creative_effects",
            bgl_adjust_creative,
            bg_adjust_creative.clone(),
            image_workgroups,
        ),
        assembler.make_pass(
            view_transform_module.as_ref(),
            "apply_view_node",
            bgl_adjust_render,
            bg_adjust_render.clone(),
            image_workgroups,
        ),
    ]);

    // These variants reuse the same compiled pipelines with bind groups
    // that follow tex2 as the creative base after mask Blur. Keeping them
    // outside `passes` avoids duplicating programs in reusable templates.
    let post_blur_glow_passes = vec![
        Pass {
            pipeline: passes[glow_prepare_pass_index].pipeline.clone(),
            bind_group: bg_glow_prepare_after_blur.clone(),
            workgroups: image_workgroups,
        },
        Pass {
            pipeline: passes[glow_blur_start_index].pipeline.clone(),
            bind_group: bg_glow_blur_after_blur_0.clone(),
            workgroups: image_workgroups,
        },
        Pass {
            pipeline: passes[glow_blur_start_index + 1].pipeline.clone(),
            bind_group: bg_glow_blur_after_blur_1.clone(),
            workgroups: image_workgroups,
        },
        Pass {
            pipeline: passes[glow_blur_start_index + 2].pipeline.clone(),
            bind_group: bg_glow_blur_after_blur_2.clone(),
            workgroups: image_workgroups,
        },
        Pass {
            pipeline: passes[glow_blur_start_index + 3].pipeline.clone(),
            bind_group: bg_glow_blur_after_blur_3.clone(),
            workgroups: image_workgroups,
        },
        Pass {
            pipeline: passes[glow_blur_start_index + 4].pipeline.clone(),
            bind_group: bg_glow_blur_after_blur_4.clone(),
            workgroups: image_workgroups,
        },
    ];
    let post_blur_creative_pass = Pass {
        pipeline: passes[adjustment_creative_pass_index].pipeline.clone(),
        bind_group: bg_adjust_creative_after_blur.clone(),
        workgroups: image_workgroups,
    };
    let post_blur_render_pass = Pass {
        pipeline: passes[adjustment_render_pass_index].pipeline.clone(),
        bind_group: bg_adjust_render_after_blur.clone(),
        workgroups: image_workgroups,
    };

    let expected_programs = expected_pass_count(cfa_kind);
    if assembler.next_program_index != expected_programs || passes.len() != expected_programs {
        return Err(anyhow!(
            "GPU render-plan mismatch for {:?}: built {} passes and consumed {} programs; expected {}",
            cfa_kind,
            passes.len(),
            assembler.next_program_index,
            expected_programs,
        ));
    }

    Ok(AssembledPasses {
        passes,
        post_blur_glow_passes,
        post_blur_creative_pass,
        post_blur_render_pass,
        indices: StageIndices {
            tone_prepare_pass_index,
            tone_reduce_pass_index,
            tone_stage_end,
            demosaic_start_index,
            demosaic_dual_start_index,
            demosaic_dual_end_index,
            demosaic_finish_index,
            color_denoise_start_index,
            color_denoise_end_index,
            adjustment_prepare_pass_index,
            adjustment_tone_pass_index,
            adjustment_effects_pass_index,
            mask_blur_start_index,
            mask_blur_end_index,
            glow_prepare_pass_index,
            glow_blur_start_index,
            glow_blur_end_index,
            adjustment_creative_pass_index,
            adjustment_render_pass_index,
        },
    })
}
