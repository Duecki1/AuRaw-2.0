use crate::pipeline::{CfaKind, ExposureParams, LoadedRaw};
use anyhow::{anyhow, Result};
use bytemuck::{Pod, Zeroable};
use eframe::{egui, egui_wgpu, wgpu};
use std::borrow::Cow;
use wgpu::util::DeviceExt;

const SHADER_HIGHLIGHTS: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/highlights.wgsl"),
    "\n",
    include_str!("../shaders/highlight_lch_pass.wgsl")
);

// Ordered coarse-to-fine multiscale reconstruction stages. The quality value
// in highlight_options.y enables a subset inside the shader, while disabled
// stages still copy their input so ping-pong parity remains deterministic.
const HIGHLIGHT_GUIDED_ENTRY_POINTS: [&str; 11] = [
    "highlight_guided_16_a",
    "highlight_guided_8_a",
    "highlight_guided_4_a",
    "highlight_guided_2_a",
    "highlight_guided_1_a",
    "highlight_guided_4_b",
    "highlight_guided_2_b",
    "highlight_guided_1_b",
    "highlight_guided_2_c",
    "highlight_guided_1_c",
    "highlight_guided_1_d",
];

const SHADER_BAYER_RCD_P1: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/pass1.wgsl")
);

const SHADER_BAYER_RCD_P2: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/pass2.wgsl")
);

const SHADER_BAYER_RCD_P3: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/pass3.wgsl")
);

const SHADER_BAYER_RCD_P4: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/pass4.wgsl")
);

const SHADER_XTRANS_P1: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/xtrans_pass1.wgsl")
);

const SHADER_XTRANS_P2: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/xtrans_pass2.wgsl")
);

const SHADER_XTRANS_P3: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/xtrans_pass3.wgsl")
);

const SHADER_XTRANS_P4: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/xtrans_pass4.wgsl")
);

const SHADER_ADJUSTMENTS: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/basic_adjustments.wgsl"),
    "\n",
    include_str!("../shaders/tonemap.wgsl"),
    "\n",
    include_str!("../shaders/adjustments.wgsl")
);

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct GpuParams {
    // Must stay byte-for-byte aligned with shaders/common.wgsl. The first 16
    // floats intentionally form one 64-byte scalar block before vec4 fields.
    black_point: f32,
    exposure: f32,
    contrast: f32,
    saturation: f32,
    vibrance: f32,
    highlight_clip: f32,
    chroma_denoise: f32,
    ca_red: f32,
    ca_blue: f32,
    highlight_reconstruction: f32,
    _tone_reserved_0: f32,
    _tone_reserved_1: f32,
    _tone_reserved_2: f32,
    _tone_reserved_3: f32,
    _tone_reserved_4: f32,
    _tone_reserved_5: f32,
    basic_tone: [f32; 4],
    presence: [f32; 4],
    highlight_options: [f32; 4],
    hsl_hue_0: [f32; 4],
    hsl_hue_1: [f32; 4],
    hsl_saturation_0: [f32; 4],
    hsl_saturation_1: [f32; 4],
    hsl_luminance_0: [f32; 4],
    hsl_luminance_1: [f32; 4],
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
            black_point: exposure.black_point,
            exposure: exposure.exposure,
            contrast: exposure.contrast,
            saturation: exposure.saturation,
            vibrance: exposure.vibrance,
            highlight_clip: exposure.highlight_clip,
            chroma_denoise: exposure.chroma_denoise,
            ca_red: exposure.ca_red,
            ca_blue: exposure.ca_blue,
            highlight_reconstruction: exposure.highlight_reconstruction,
            _tone_reserved_0: 0.0,
            _tone_reserved_1: 0.0,
            _tone_reserved_2: 0.0,
            _tone_reserved_3: 0.0,
            _tone_reserved_4: 0.0,
            _tone_reserved_5: 0.0,
            basic_tone: [
                exposure.highlights,
                exposure.shadows,
                exposure.whites,
                exposure.blacks,
            ],
            presence: [exposure.texture, exposure.clarity, exposure.dehaze, 0.0],
            highlight_options: [
                exposure.highlight_method.shader_value(),
                exposure.highlight_iterations.clamp(1, 4) as f32,
                exposure.highlight_color_adaptation.clamp(0.0, 1.0),
                0.0,
            ],
            hsl_hue_0: exposure.hsl_hue[..4].try_into().unwrap(),
            hsl_hue_1: exposure.hsl_hue[4..].try_into().unwrap(),
            hsl_saturation_0: exposure.hsl_saturation[..4].try_into().unwrap(),
            hsl_saturation_1: exposure.hsl_saturation[4..].try_into().unwrap(),
            hsl_luminance_0: exposure.hsl_luminance[..4].try_into().unwrap(),
            hsl_luminance_1: exposure.hsl_luminance[4..].try_into().unwrap(),
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
    passes: Vec<Pass>,
    _raw_texture: wgpu::Texture,
    _color_texture: wgpu::Texture,
    _reconstructed_raw_texture: wgpu::Texture,
    _highlight_work_a: wgpu::Texture,
    _highlight_work_b: wgpu::Texture,
    _tex1: wgpu::Texture,
    _tex2: wgpu::Texture,
    _scene_texture: wgpu::Texture,
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
        let demosaic_format = demosaic_work_format();

        // This is the raw-CFA output of the Ansel LCh reconstruction pass. It
        // is deliberately separate from the demosaic scratch textures so all
        // downstream samples see the same recovered sensor data.
        let reconstructed_raw_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw reconstructed raw CFA"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::R32Float],
        });

        // Ping-pong storage for the guided highlight passes. Each dispatch reads
        // one texture through binding 13 and writes the other through binding 14.
        let highlight_work_a = create_float_work_texture(device, size, "auraw highlight work A");
        let highlight_work_b = create_float_work_texture(device, size, "auraw highlight work B");

        // Preserve a scene-linear camera-RGB result between demosaic and the
        // display pass. This is what lets local Lightroom controls read true
        // RGB neighbourhoods instead of raw Bayer samples.
        let scene_texture = create_demosaic_texture(
            device,
            size,
            demosaic_format,
            "auraw scene-linear camera RGB",
        );

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

        let tex1 = create_demosaic_texture(device, size, demosaic_format, "auraw tex1");
        let tex2 = create_demosaic_texture(device, size, demosaic_format, "auraw tex2");

        let out_view = out_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let reconstructed_raw_view =
            reconstructed_raw_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let highlight_work_a_view =
            highlight_work_a.create_view(&wgpu::TextureViewDescriptor::default());
        let highlight_work_b_view =
            highlight_work_b.create_view(&wgpu::TextureViewDescriptor::default());
        let scene_view = scene_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let tex1_view = tex1.create_view(&wgpu::TextureViewDescriptor::default());
        let tex2_view = tex2.create_view(&wgpu::TextureViewDescriptor::default());
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

        let bgl_highlights = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl highlights"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                storage_texture_entry(
                    3,
                    wgpu::TextureFormat::R32Float,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
                texture_entry(13, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(
                    14,
                    wgpu::TextureFormat::Rgba16Float,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
            ],
        });

        let bgl1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl1"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(4, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        });

        let bgl2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl2"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(5, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(6, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        });

        let bgl3 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl3"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(7, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(8, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        });

        let bgl4 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl4"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(7, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(9, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(10, demosaic_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        });

        let bgl5 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl adjustments"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                texture_entry(11, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(
                    12,
                    wgpu::TextureFormat::Rgba8Unorm,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
            ],
        });

        let make_highlight_bind_group =
            |label: &str, read_view: &wgpu::TextureView, write_view: &wgpu::TextureView| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &bgl_highlights,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: params_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(&raw_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::TextureView(&color_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 3,
                            resource: wgpu::BindingResource::TextureView(&reconstructed_raw_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 13,
                            resource: wgpu::BindingResource::TextureView(read_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 14,
                            resource: wgpu::BindingResource::TextureView(write_view),
                        },
                    ],
                })
            };

        // Bind groups for the guided stages are created below together with
        // their pipelines. Every stage alternates A/B, and a disabled quality
        // stage performs an identity copy in WGSL to preserve the parity.

        let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg1"),
            layout: &bgl1,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&reconstructed_raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
            ],
        });

        let bg2 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg2"),
            layout: &bgl2,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&reconstructed_raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
            ],
        });

        let bg3 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg3"),
            layout: &bgl3,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&reconstructed_raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
            ],
        });

        let bg4 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg4"),
            layout: &bgl4,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&reconstructed_raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
                wgpu::BindGroupEntry {
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
            ],
        });

        let bg5 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg adjustments"),
            layout: &bgl5,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::TextureView(&out_view),
                },
            ],
        });

        // Desktop builds use 32-bit float demosaic scratch/output. Android
        // retains 16-bit float storage to control memory and device-feature
        // requirements. The WGSL storage declaration must match the selected
        // texture format, so desktop variants are generated once at startup.
        let bayer_rcd_p1 = demosaic_shader_source(SHADER_BAYER_RCD_P1);
        let bayer_rcd_p2 = demosaic_shader_source(SHADER_BAYER_RCD_P2);
        let bayer_rcd_p3 = demosaic_shader_source(SHADER_BAYER_RCD_P3);
        let bayer_rcd_p4 = demosaic_shader_source(SHADER_BAYER_RCD_P4);
        let xtrans_p1 = demosaic_shader_source(SHADER_XTRANS_P1);
        let xtrans_p2 = demosaic_shader_source(SHADER_XTRANS_P2);
        let xtrans_p3 = demosaic_shader_source(SHADER_XTRANS_P3);
        let xtrans_p4 = demosaic_shader_source(SHADER_XTRANS_P4);

        let make_pipeline =
            |source: &str, entry: &str, bgl: &wgpu::BindGroupLayout| -> wgpu::ComputePipeline {
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

        let mut passes = Vec::with_capacity(1 + HIGHLIGHT_GUIDED_ENTRY_POINTS.len() + 1 + 5);

        // Prepare writes the initial RGB estimate and reliability into A.
        passes.push(Pass {
            pipeline: make_pipeline(SHADER_HIGHLIGHTS, "highlight_prepare", &bgl_highlights),
            bind_group: make_highlight_bind_group(
                "bg highlight prepare",
                &highlight_work_b_view,
                &highlight_work_a_view,
            ),
        });

        // The multiscale solver ping-pongs through every declared stage.
        // Quality levels are handled inside each entry point, so all stages
        // are dispatched and disabled ones copy read -> write unchanged.
        for (index, entry) in HIGHLIGHT_GUIDED_ENTRY_POINTS.iter().enumerate() {
            let (read_view, write_view) = if index % 2 == 0 {
                (&highlight_work_a_view, &highlight_work_b_view)
            } else {
                (&highlight_work_b_view, &highlight_work_a_view)
            };
            let label = format!("bg {entry}");
            passes.push(Pass {
                pipeline: make_pipeline(SHADER_HIGHLIGHTS, entry, &bgl_highlights),
                bind_group: make_highlight_bind_group(&label, read_view, write_view),
            });
        }

        // Prepare leaves the data in A. An odd number of guided stages leaves
        // the final result in B; compute this from the table so future stage
        // additions cannot silently select the wrong texture.
        let (final_read_view, final_write_view) = if HIGHLIGHT_GUIDED_ENTRY_POINTS.len() % 2 == 0 {
            (&highlight_work_a_view, &highlight_work_b_view)
        } else {
            (&highlight_work_b_view, &highlight_work_a_view)
        };
        passes.push(Pass {
            pipeline: make_pipeline(SHADER_HIGHLIGHTS, "highlight_finalize", &bgl_highlights),
            bind_group: make_highlight_bind_group(
                "bg highlight finalize",
                final_read_view,
                final_write_view,
            ),
        });

        // Select the demosaic family from LibRaw's CFA classification.
        // Bayer uses the four-stage ratio-corrected path. Fuji X-Trans uses a
        // dedicated 6x6-pattern-aware seed/green/chroma/output sequence and
        // never enters code that assumes a 2x2 Bayer lattice.
        match raw.cfa_kind {
            CfaKind::Bayer => passes.extend([
                Pass {
                    pipeline: make_pipeline(bayer_rcd_p1.as_ref(), "bayer_rcd_directional", &bgl1),
                    bind_group: bg1,
                },
                Pass {
                    pipeline: make_pipeline(bayer_rcd_p2.as_ref(), "bayer_rcd_green", &bgl2),
                    bind_group: bg2,
                },
                Pass {
                    pipeline: make_pipeline(bayer_rcd_p3.as_ref(), "bayer_rcd_chroma", &bgl3),
                    bind_group: bg3,
                },
                Pass {
                    pipeline: make_pipeline(bayer_rcd_p4.as_ref(), "bayer_rcd_output", &bgl4),
                    bind_group: bg4,
                },
            ]),
            CfaKind::XTrans => passes.extend([
                Pass {
                    pipeline: make_pipeline(xtrans_p1.as_ref(), "xtrans_seed", &bgl1),
                    bind_group: bg1,
                },
                Pass {
                    pipeline: make_pipeline(xtrans_p2.as_ref(), "xtrans_refine_green", &bgl2),
                    bind_group: bg2,
                },
                Pass {
                    pipeline: make_pipeline(xtrans_p3.as_ref(), "xtrans_refine_chroma", &bgl3),
                    bind_group: bg3,
                },
                Pass {
                    pipeline: make_pipeline(xtrans_p4.as_ref(), "xtrans_output", &bgl4),
                    bind_group: bg4,
                },
            ]),
        }

        passes.push(Pass {
            pipeline: make_pipeline(SHADER_ADJUSTMENTS, "apply_lightroom_adjustments", &bgl5),
            bind_group: bg5,
        });

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
            _reconstructed_raw_texture: reconstructed_raw_texture,
            _highlight_work_a: highlight_work_a,
            _highlight_work_b: highlight_work_b,
            _tex1: tex1,
            _tex2: tex2,
            _scene_texture: scene_texture,
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

        for i in 0..self.passes.len() {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(&format!("auraw pass {}", i + 1)),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.passes[i].pipeline);
            pass.set_bind_group(0, &self.passes[i].bind_group, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);
        }

        queue.submit(Some(encoder.finish()));
    }
}

fn demosaic_work_format() -> wgpu::TextureFormat {
    if cfg!(target_os = "android") {
        wgpu::TextureFormat::Rgba16Float
    } else {
        wgpu::TextureFormat::Rgba32Float
    }
}

fn demosaic_shader_source(source: &str) -> Cow<'_, str> {
    if cfg!(target_os = "android") {
        Cow::Borrowed(source)
    } else {
        Cow::Owned(source.replace("rgba16float", "rgba32float"))
    }
}

fn create_demosaic_texture(
    device: &wgpu::Device,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[format],
    })
}

fn create_float_work_texture(
    device: &wgpu::Device,
    size: wgpu::Extent3d,
    label: &'static str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[wgpu::TextureFormat::Rgba16Float],
    })
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

fn storage_texture_entry(
    binding: u32,
    format: wgpu::TextureFormat,
    access: wgpu::StorageTextureAccess,
) -> wgpu::BindGroupLayoutEntry {
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

#[cfg(test)]
mod tests {
    use super::{
        HIGHLIGHT_GUIDED_ENTRY_POINTS, SHADER_ADJUSTMENTS, SHADER_BAYER_RCD_P1,
        SHADER_BAYER_RCD_P2, SHADER_BAYER_RCD_P3, SHADER_BAYER_RCD_P4, SHADER_HIGHLIGHTS,
        SHADER_XTRANS_P1, SHADER_XTRANS_P2, SHADER_XTRANS_P3, SHADER_XTRANS_P4,
    };

    #[test]
    fn compute_shaders_parse_and_validate() {
        for (name, source) in [
            ("highlight reconstruction", SHADER_HIGHLIGHTS),
            ("Bayer RCD pass 1", SHADER_BAYER_RCD_P1),
            ("Bayer RCD pass 2", SHADER_BAYER_RCD_P2),
            ("Bayer RCD pass 3", SHADER_BAYER_RCD_P3),
            ("Bayer RCD pass 4", SHADER_BAYER_RCD_P4),
            ("X-Trans pass 1", SHADER_XTRANS_P1),
            ("X-Trans pass 2", SHADER_XTRANS_P2),
            ("X-Trans pass 3", SHADER_XTRANS_P3),
            ("X-Trans pass 4", SHADER_XTRANS_P4),
            ("Lightroom adjustments", SHADER_ADJUSTMENTS),
        ] {
            let module = naga::front::wgsl::parse_str(source)
                .unwrap_or_else(|error| panic!("{name} did not parse: {error}"));
            naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            )
            .validate(&module)
            .unwrap_or_else(|error| panic!("{name} did not validate: {error}"));
        }
    }

    #[test]
    fn highlight_shader_exposes_every_dispatched_entry_point() {
        let module = naga::front::wgsl::parse_str(SHADER_HIGHLIGHTS)
            .expect("highlight shader did not parse");

        let expected_entry_points = std::iter::once("highlight_prepare")
            .chain(HIGHLIGHT_GUIDED_ENTRY_POINTS.iter().copied())
            .chain(std::iter::once("highlight_finalize"));

        for expected in expected_entry_points {
            assert!(
                module
                    .entry_points
                    .iter()
                    .any(|entry| entry.name == expected),
                "highlight shader is missing entry point {expected}"
            );
        }
    }

    #[test]
    fn demosaic_shaders_expose_every_dispatched_entry_point() {
        for (source, expected) in [
            (SHADER_BAYER_RCD_P1, "bayer_rcd_directional"),
            (SHADER_BAYER_RCD_P2, "bayer_rcd_green"),
            (SHADER_BAYER_RCD_P3, "bayer_rcd_chroma"),
            (SHADER_BAYER_RCD_P4, "bayer_rcd_output"),
            (SHADER_XTRANS_P1, "xtrans_seed"),
            (SHADER_XTRANS_P2, "xtrans_refine_green"),
            (SHADER_XTRANS_P3, "xtrans_refine_chroma"),
            (SHADER_XTRANS_P4, "xtrans_output"),
        ] {
            let module =
                naga::front::wgsl::parse_str(source).expect("demosaic shader did not parse");
            assert!(
                module
                    .entry_points
                    .iter()
                    .any(|entry| entry.name == expected),
                "demosaic shader is missing entry point {expected}"
            );
        }
    }

    #[test]
    fn gpu_params_follow_the_wgsl_uniform_layout() {
        // Ten active scalar values plus six reserved floats keep the stable
        // 64-byte prefix, followed by nine adjustment vec4s, six camera/raw
        // vec4s, then dimensions/padding. This catches accidental
        // Rust/WGSL field drift before it turns sliders into random values.
        assert_eq!(std::mem::size_of::<super::GpuParams>(), 320);
        assert_eq!(std::mem::offset_of!(super::GpuParams, basic_tone), 64);
        assert_eq!(
            std::mem::offset_of!(super::GpuParams, highlight_options),
            96
        );
        assert_eq!(std::mem::offset_of!(super::GpuParams, wb), 208);
        assert_eq!(std::mem::offset_of!(super::GpuParams, width), 304);
    }

    fn tone_curve_cpu(
        scene_ev: f32,
        contrast: f32,
        highlights: f32,
        shadows: f32,
        whites: f32,
        blacks: f32,
    ) -> f32 {
        const MIDDLE: f32 = 0.1842;

        fn bias(value: f32, shape: f32) -> f32 {
            let x = value.clamp(0.0, 1.0);
            let a = shape.clamp(0.05, 64.0);
            x / (a + (1.0 - a) * x).max(1e-6)
        }

        let black_ev = -8.0 - 2.0 * blacks.clamp(-1.0, 1.0);
        let white_ev = 4.0 - 1.5 * whites.clamp(-1.0, 1.0);
        let range_ev = (white_ev - black_ev).max(1.0);
        let position = ((scene_ev - black_ev) / range_ev).clamp(0.0, 1.0);
        let middle_position = (-black_ev / range_ev).clamp(0.05, 0.95);
        let middle_slope = 2.0f32.powf(contrast.clamp(-1.0, 1.0));
        let shadow_shape = (middle_slope * middle_position / MIDDLE
            * 2.0f32.powf(-1.25 * shadows.clamp(-1.0, 1.0)))
        .clamp(0.05, 64.0);
        let highlight_shape = ((1.0 - MIDDLE) / (middle_slope * (1.0 - middle_position)).max(1e-4)
            * 2.0f32.powf(-1.25 * highlights.clamp(-1.0, 1.0)))
        .clamp(0.05, 64.0);

        if position <= middle_position {
            return MIDDLE * bias(position / middle_position.max(1e-5), shadow_shape);
        }

        MIDDLE
            + (1.0 - MIDDLE)
                * bias(
                    (position - middle_position) / (1.0 - middle_position).max(1e-5),
                    highlight_shape,
                )
    }

    #[test]
    fn unified_tone_curve_is_monotonic_at_slider_extremes() {
        let controls = [-1.0f32, 0.0, 1.0];

        for &contrast in &controls {
            for &highlights in &controls {
                for &shadows in &controls {
                    for &whites in &controls {
                        for &blacks in &controls {
                            let mut previous = -1.0f32;
                            for sample in 0..=480 {
                                let scene_ev = -12.0 + sample as f32 * 0.05;
                                let mapped = tone_curve_cpu(
                                    scene_ev, contrast, highlights, shadows, whites, blacks,
                                );
                                assert!(mapped.is_finite());
                                assert!((-1e-6..=1.0 + 1e-6).contains(&mapped));
                                assert!(
                                    mapped + 1e-6 >= previous,
                                    "tone curve decreased at {scene_ev} EV for controls \
                                     c={contrast}, h={highlights}, s={shadows}, \
                                     w={whites}, b={blacks}: {previous} -> {mapped}"
                                );
                                previous = mapped;
                            }

                            let middle =
                                tone_curve_cpu(0.0, contrast, highlights, shadows, whites, blacks);
                            assert!((middle - 0.1842).abs() < 1e-5);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn gpu_pipeline_creates_with_real_bind_group_layouts_when_an_adapter_exists() {
        use super::{CfaKind, ExposureParams, LoadedRaw, RawGpuPipeline};
        use eframe::{egui_wgpu, wgpu};

        let instance = wgpu::Instance::default();
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        else {
            // Headless CI runners are allowed to lack a usable GPU. The
            // parser/validator test above still covers all WGSL in that case.
            return;
        };
        let Ok((device, queue)) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("auraw shader-layout test device"),
                ..Default::default()
            }))
        else {
            return;
        };

        let width = 12;
        let height = 12;
        let xtrans_pattern: [[u8; 6]; 6] = [
            [1, 2, 1, 1, 0, 1],
            [0, 1, 0, 2, 1, 2],
            [1, 2, 1, 1, 0, 1],
            [1, 0, 1, 1, 2, 1],
            [2, 1, 2, 0, 1, 0],
            [1, 0, 1, 1, 2, 1],
        ];
        let mut renderer =
            egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm, Default::default());

        for cfa_kind in [CfaKind::Bayer, CfaKind::XTrans] {
            let color_indices = (0..height)
                .flat_map(|y| {
                    (0..width).map(move |x| match cfa_kind {
                        CfaKind::Bayer => match (x % 2, y % 2) {
                            (0, 0) => 0,
                            (1, 1) => 2,
                            _ => 1,
                        },
                        CfaKind::XTrans => xtrans_pattern[(y % 6) as usize][(x % 6) as usize],
                    })
                })
                .collect();

            let raw = LoadedRaw {
                width,
                height,
                camera_make: "test".to_owned(),
                camera_model: "test".to_owned(),
                cfa_kind,
                raw_pixels: vec![2048; (width * height) as usize],
                color_indices,
                wb_coeffs: [1.0; 4],
                cam_to_srgb: [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                ],
                black_levels: [0.0; 4],
                white_levels: [4095.0; 4],
            };
            let params = super::GpuParams::new(&ExposureParams::default(), &raw);

            let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let pipeline = RawGpuPipeline::new(&device, &queue, &mut renderer, &raw, &params);
            let validation_error = pollster::block_on(validation_scope.pop());

            if let Err(error) = pipeline {
                panic!("{cfa_kind:?} GPU pipeline creation failed: {error:#}");
            }
            assert!(
                validation_error.is_none(),
                "{cfa_kind:?} wgpu layout/shader validation failed: {validation_error:?}"
            );
        }
    }
}
