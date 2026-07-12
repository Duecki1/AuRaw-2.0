use super::sigmoid::coefficients as sigmoid_coefficients;
use crate::pipeline::{
    mask_atlas_edge, CfaKind, ExposureParams, IccOutputTransform, LoadedRaw, MaskStack,
    ProcessingStage, RenderingIntent, MAX_LOCAL_MASKS,
};
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcessingQuality {
    /// Half-float image intermediates for lower memory use and faster previews.
    Preview,
    /// Full-float demosaic, scene, and highlight-reconstruction intermediates.
    #[default]
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HighlightWorkSlot {
    A,
    B,
}

fn highlight_stage_slots(index: usize) -> (HighlightWorkSlot, HighlightWorkSlot) {
    if index % 2 == 0 {
        (HighlightWorkSlot::A, HighlightWorkSlot::B)
    } else {
        (HighlightWorkSlot::B, HighlightWorkSlot::A)
    }
}

fn highlight_final_read_slot(stage_count: usize) -> HighlightWorkSlot {
    if stage_count % 2 == 0 {
        HighlightWorkSlot::A
    } else {
        HighlightWorkSlot::B
    }
}

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
    include_str!("../shaders/xtrans_candidate_common.wgsl"),
    "\n",
    include_str!("../shaders/xtrans_pass4.wgsl")
);

const SHADER_XTRANS_P5: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/xtrans_pass5.wgsl")
);

const SHADER_XTRANS_P6: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/xtrans_candidate_common.wgsl"),
    "\n",
    include_str!("../shaders/xtrans_pass6.wgsl")
);

const SHADER_XTRANS_P7: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/raw_sampling.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/xtrans_pass7.wgsl")
);

const SHADER_TONE_ANALYSIS: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/profile.wgsl"),
    "\n",
    include_str!("../shaders/basic_adjustments.wgsl"),
    "\n",
    include_str!("../shaders/tone_common.wgsl"),
    "\n",
    include_str!("../shaders/tone_analysis.wgsl")
);

const SHADER_REGRESSION_SCENE: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/profile.wgsl"),
    "\n",
    include_str!("../shaders/regression_scene.wgsl")
);

const SHADER_ADJUSTMENTS: &str = concat!(
    include_str!("../shaders/common.wgsl"),
    "\n",
    include_str!("../shaders/color.wgsl"),
    "\n",
    include_str!("../shaders/profile.wgsl"),
    "\n",
    include_str!("../shaders/basic_adjustments.wgsl"),
    "\n",
    include_str!("../shaders/tone_common.wgsl"),
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
    temperature: f32,
    saturation: f32,
    vibrance: f32,
    highlight_clip: f32,
    chroma_denoise: f32,
    ca_red: f32,
    ca_blue: f32,
    highlight_reconstruction: f32,
    tone_analysis_scale: f32,
    tone_guide_radius: f32,
    demosaic_mode: f32,
    dual_threshold: f32,
    frequency_chroma: f32,
    tint: f32,
    basic_tone: [f32; 4],
    sigmoid_curve: [f32; 4],
    sigmoid_power: [f32; 4],
    presence: [f32; 4],
    creative_effects: [f32; 4],
    vignette: [f32; 4],
    vignette_options: [f32; 4],
    highlight_options: [f32; 4],
    tone_curve_0: [f32; 4],
    tone_curve_1: [f32; 4],
    tone_curve_2: [f32; 4],
    tone_curve_3: [f32; 4],
    tone_curve_meta: [f32; 4],
    tone_curve_red_0: [f32; 4],
    tone_curve_red_1: [f32; 4],
    tone_curve_red_2: [f32; 4],
    tone_curve_red_3: [f32; 4],
    tone_curve_red_meta: [f32; 4],
    tone_curve_green_0: [f32; 4],
    tone_curve_green_1: [f32; 4],
    tone_curve_green_2: [f32; 4],
    tone_curve_green_3: [f32; 4],
    tone_curve_green_meta: [f32; 4],
    tone_curve_blue_0: [f32; 4],
    tone_curve_blue_1: [f32; 4],
    tone_curve_blue_2: [f32; 4],
    tone_curve_blue_3: [f32; 4],
    tone_curve_blue_meta: [f32; 4],
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
    tile_origin_x: i32,
    tile_origin_y: i32,
    full_width: u32,
    full_height: u32,
    _pad0: u32,
    _pad1: u32,
    profile_hue_sat: [u32; 4],
    profile_look: [u32; 4],
    profile_tone: [u32; 4],
    output_lut: [u32; 4],
    profile_flags: [u32; 4],
    // Local mask count followed by reserved values. Each fixed mask index maps
    // directly to one layer in the normalized R8 mask atlas.
    mask_counts: [u32; 4],
    mask_meta: [[u32; 4]; MAX_LOCAL_MASKS],
    // Exposure, contrast, highlights, shadows.
    mask_adjust_0: [[f32; 4]; MAX_LOCAL_MASKS],
    // Whites, blacks, temperature, tint.
    mask_adjust_1: [[f32; 4]; MAX_LOCAL_MASKS],
    // Saturation, texture, clarity, dehaze.
    mask_adjust_2: [[f32; 4]; MAX_LOCAL_MASKS],
}

impl GpuParams {
    pub fn new(exposure: &ExposureParams, masks: &MaskStack, raw: &LoadedRaw) -> Self {
        Self::new_for_tile(exposure, masks, raw, 0, 0, raw.width, raw.height)
    }

    pub fn new_for_tile(
        exposure: &ExposureParams,
        masks: &MaskStack,
        raw: &LoadedRaw,
        tile_origin_x: i32,
        tile_origin_y: i32,
        full_width: u32,
        full_height: u32,
    ) -> Self {
        let profile_layout = raw.camera_profile.gpu_layout();
        let sigmoid = sigmoid_coefficients(exposure.sigmoid);
        let mut mask_meta = [[0u32; 4]; MAX_LOCAL_MASKS];
        let mut mask_adjust_0 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_adjust_1 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_adjust_2 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        for (index, mask) in masks.masks.iter().take(MAX_LOCAL_MASKS).enumerate() {
            let adjustment = mask.adjustments;
            mask_meta[index] = [u32::from(mask.enabled), u32::from(!adjustment.is_neutral()), 0, 0];
            mask_adjust_0[index] = [
                adjustment.exposure.clamp(-5.0, 5.0),
                adjustment.contrast.clamp(-100.0, 100.0),
                adjustment.highlights.clamp(-100.0, 100.0),
                adjustment.shadows.clamp(-100.0, 100.0),
            ];
            mask_adjust_1[index] = [
                adjustment.whites.clamp(-100.0, 100.0),
                adjustment.blacks.clamp(-100.0, 100.0),
                adjustment.temperature.clamp(-100.0, 100.0),
                adjustment.tint.clamp(-100.0, 100.0),
            ];
            mask_adjust_2[index] = [
                adjustment.saturation.clamp(-100.0, 100.0),
                adjustment.texture.clamp(-100.0, 100.0),
                adjustment.clarity.clamp(-100.0, 100.0),
                adjustment.dehaze.clamp(-100.0, 100.0),
            ];
        }
        Self {
            black_point: exposure.black_point,
            exposure: exposure.exposure,
            temperature: exposure.temperature.clamp(-100.0, 100.0),
            saturation: exposure.saturation,
            vibrance: exposure.vibrance,
            highlight_clip: exposure.highlight_clip,
            chroma_denoise: exposure.chroma_denoise,
            ca_red: exposure.ca_red,
            ca_blue: exposure.ca_blue,
            highlight_reconstruction: exposure.highlight_reconstruction,
            tone_analysis_scale: tone_analysis_scale() as f32,
            tone_guide_radius: if cfg!(target_os = "android") { 3.0 } else { 5.0 },
            demosaic_mode: exposure.demosaic_mode.shader_value(),
            dual_threshold: exposure.dual_threshold.clamp(0.0, 100.0),
            frequency_chroma: exposure.frequency_chroma.clamp(0.0, 1.0),
            tint: exposure.tint.clamp(-100.0, 100.0),
            basic_tone: [
                exposure.highlights,
                exposure.shadows,
                exposure.whites,
                exposure.blacks,
            ],
            sigmoid_curve: [
                sigmoid.white_target,
                sigmoid.black_target,
                sigmoid.paper_exposure,
                sigmoid.film_fog,
            ],
            sigmoid_power: [
                sigmoid.film_power,
                sigmoid.paper_power,
                sigmoid.hue_preservation,
                sigmoid.color_processing,
            ],
            presence: [
                exposure.texture,
                exposure.clarity,
                exposure.dehaze,
                exposure.contrast.clamp(-100.0, 100.0),
            ],
            creative_effects: [
                exposure.glow_amount.clamp(0.0, 100.0),
                exposure.glow_radius.clamp(0.0, 100.0),
                exposure.glow_threshold.clamp(0.0, 100.0),
                0.0,
            ],
            vignette: [
                exposure.vignette_amount.clamp(-100.0, 100.0),
                exposure.vignette_midpoint.clamp(0.0, 100.0),
                exposure.vignette_roundness.clamp(-100.0, 100.0),
                exposure.vignette_feather.clamp(0.0, 100.0),
            ],
            vignette_options: [
                exposure.vignette_highlights.clamp(0.0, 100.0),
                0.0,
                0.0,
                0.0,
            ],
            highlight_options: [
                exposure.highlight_method.shader_value(),
                exposure.highlight_iterations.clamp(1, 4) as f32,
                exposure.highlight_color_adaptation.clamp(0.0, 1.0),
                0.0,
            ],
            tone_curve_0: [
                exposure.tone_curve.points[0][0],
                exposure.tone_curve.points[0][1],
                exposure.tone_curve.points[1][0],
                exposure.tone_curve.points[1][1],
            ],
            tone_curve_1: [
                exposure.tone_curve.points[2][0],
                exposure.tone_curve.points[2][1],
                exposure.tone_curve.points[3][0],
                exposure.tone_curve.points[3][1],
            ],
            tone_curve_2: [
                exposure.tone_curve.points[4][0],
                exposure.tone_curve.points[4][1],
                exposure.tone_curve.points[5][0],
                exposure.tone_curve.points[5][1],
            ],
            tone_curve_3: [
                exposure.tone_curve.points[6][0],
                exposure.tone_curve.points[6][1],
                exposure.tone_curve.points[7][0],
                exposure.tone_curve.points[7][1],
            ],
            tone_curve_meta: [
                exposure.tone_curve.len.clamp(2, 8) as f32,
                if exposure.tone_curve.is_identity() { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ],
            tone_curve_red_0: [
                exposure.tone_curve_red.points[0][0], exposure.tone_curve_red.points[0][1],
                exposure.tone_curve_red.points[1][0], exposure.tone_curve_red.points[1][1],
            ],
            tone_curve_red_1: [
                exposure.tone_curve_red.points[2][0], exposure.tone_curve_red.points[2][1],
                exposure.tone_curve_red.points[3][0], exposure.tone_curve_red.points[3][1],
            ],
            tone_curve_red_2: [
                exposure.tone_curve_red.points[4][0], exposure.tone_curve_red.points[4][1],
                exposure.tone_curve_red.points[5][0], exposure.tone_curve_red.points[5][1],
            ],
            tone_curve_red_3: [
                exposure.tone_curve_red.points[6][0], exposure.tone_curve_red.points[6][1],
                exposure.tone_curve_red.points[7][0], exposure.tone_curve_red.points[7][1],
            ],
            tone_curve_red_meta: [
                exposure.tone_curve_red.len.clamp(2, 8) as f32,
                if exposure.tone_curve_red.is_identity() { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ],
            tone_curve_green_0: [
                exposure.tone_curve_green.points[0][0], exposure.tone_curve_green.points[0][1],
                exposure.tone_curve_green.points[1][0], exposure.tone_curve_green.points[1][1],
            ],
            tone_curve_green_1: [
                exposure.tone_curve_green.points[2][0], exposure.tone_curve_green.points[2][1],
                exposure.tone_curve_green.points[3][0], exposure.tone_curve_green.points[3][1],
            ],
            tone_curve_green_2: [
                exposure.tone_curve_green.points[4][0], exposure.tone_curve_green.points[4][1],
                exposure.tone_curve_green.points[5][0], exposure.tone_curve_green.points[5][1],
            ],
            tone_curve_green_3: [
                exposure.tone_curve_green.points[6][0], exposure.tone_curve_green.points[6][1],
                exposure.tone_curve_green.points[7][0], exposure.tone_curve_green.points[7][1],
            ],
            tone_curve_green_meta: [
                exposure.tone_curve_green.len.clamp(2, 8) as f32,
                if exposure.tone_curve_green.is_identity() { 1.0 } else { 0.0 },
                0.0,
                0.0,
            ],
            tone_curve_blue_0: [
                exposure.tone_curve_blue.points[0][0], exposure.tone_curve_blue.points[0][1],
                exposure.tone_curve_blue.points[1][0], exposure.tone_curve_blue.points[1][1],
            ],
            tone_curve_blue_1: [
                exposure.tone_curve_blue.points[2][0], exposure.tone_curve_blue.points[2][1],
                exposure.tone_curve_blue.points[3][0], exposure.tone_curve_blue.points[3][1],
            ],
            tone_curve_blue_2: [
                exposure.tone_curve_blue.points[4][0], exposure.tone_curve_blue.points[4][1],
                exposure.tone_curve_blue.points[5][0], exposure.tone_curve_blue.points[5][1],
            ],
            tone_curve_blue_3: [
                exposure.tone_curve_blue.points[6][0], exposure.tone_curve_blue.points[6][1],
                exposure.tone_curve_blue.points[7][0], exposure.tone_curve_blue.points[7][1],
            ],
            tone_curve_blue_meta: [
                exposure.tone_curve_blue.len.clamp(2, 8) as f32,
                if exposure.tone_curve_blue.is_identity() { 1.0 } else { 0.0 },
                0.0,
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
            tile_origin_x,
            tile_origin_y,
            full_width,
            full_height,
            _pad0: 0,
            _pad1: 0,
            profile_hue_sat: profile_layout.hue_sat,
            profile_look: profile_layout.look,
            profile_tone: profile_layout.tone,
            output_lut: profile_layout.output,
            profile_flags: profile_layout.flags,
            mask_counts: [masks.masks.len().min(MAX_LOCAL_MASKS) as u32, 0, 0, 0],
            mask_meta,
            mask_adjust_0,
            mask_adjust_1,
            mask_adjust_2,
        }
    }
}

struct Pass {
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    workgroups: [u32; 3],
}

pub struct RawGpuPipeline {
    pub egui_texture_id: Option<egui::TextureId>,
    pub width: u32,
    pub height: u32,
    params_buffer: wgpu::Buffer,
    tone_histogram_buffer: wgpu::Buffer,
    raw_stage_end: usize,
    tone_prepare_pass_index: usize,
    tone_reduce_pass_index: usize,
    tone_stage_end: usize,
    passes: Vec<Pass>,
    raw_texture: wgpu::Texture,
    color_texture: wgpu::Texture,
    black_texture: wgpu::Texture,
    _reconstructed_raw_texture: wgpu::Texture,
    _highlight_work_a: wgpu::Texture,
    _highlight_work_b: wgpu::Texture,
    _tex1: wgpu::Texture,
    _tex2: wgpu::Texture,
    scene_texture: wgpu::Texture,
    scene_format: wgpu::TextureFormat,
    tone_stats_buffer: wgpu::Buffer,
    _tone_guide_a: wgpu::Texture,
    _tone_guide_b: wgpu::Texture,
    mask_texture: wgpu::Texture,
    mask_atlas_edge: u32,
    profile_buffer: wgpu::Buffer,
    output_lut_offset_bytes: u64,
    out_texture: wgpu::Texture,
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
        Self::new_with_quality(
            device,
            queue,
            renderer,
            raw,
            params,
            default_processing_quality(),
        )
    }

    pub fn new_with_quality(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
    ) -> Result<Self> {
        Self::new_internal(device, queue, Some(renderer), raw, params, quality)
    }

    pub fn new_headless_with_quality(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
    ) -> Result<Self> {
        Self::new_internal(device, queue, None, raw, params, quality)
    }

    fn new_internal(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: Option<&mut egui_wgpu::Renderer>,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
    ) -> Result<Self> {
        validate_raw(raw)?;

        let raw_texture = create_raw_texture(device, queue, raw);
        let color_texture = create_color_texture(device, queue, raw);
        let black_texture = create_black_texture(device, queue, raw);
        let size = texture_size(raw.width, raw.height);
        let work_format = processing_work_format(quality);
        let demosaic_format = work_format;
        let highlight_work_format = work_format;
        let tone_scale = tone_analysis_scale();
        let tone_size = texture_size(raw.width.div_ceil(tone_scale), raw.height.div_ceil(tone_scale));
        let tone_format = tone_guide_format();
        let image_workgroups = [raw.width.div_ceil(8), raw.height.div_ceil(8), 1];
        let tone_workgroups = [tone_size.width.div_ceil(8), tone_size.height.div_ceil(8), 1];
        let single_workgroup = [1, 1, 1];

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
        // display pass. This is what lets local Lightroom controls read true
        // RGB neighbourhoods instead of raw Bayer samples.
        let scene_texture =
            create_demosaic_texture(device, size, demosaic_format, "auraw scene-linear camera RGB");

        let out_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw output texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });

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
        let mask_atlas_edge = mask_atlas_edge();
        let mask_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw normalized local-mask atlas"),
            size: wgpu::Extent3d {
                width: mask_atlas_edge,
                height: mask_atlas_edge,
                depth_or_array_layers: MAX_LOCAL_MASKS as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::R8Unorm],
        });
        let empty_masks = vec![
            0u8;
            mask_atlas_edge as usize
                * mask_atlas_edge as usize
                * MAX_LOCAL_MASKS
        ];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &mask_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &empty_masks,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(mask_atlas_edge),
                rows_per_image: Some(mask_atlas_edge),
            },
            wgpu::Extent3d {
                width: mask_atlas_edge,
                height: mask_atlas_edge,
                depth_or_array_layers: MAX_LOCAL_MASKS as u32,
            },
        );

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
        let tone_guide_a_view =
            tone_guide_a.create_view(&wgpu::TextureViewDescriptor::default());
        let tone_guide_b_view =
            tone_guide_b.create_view(&wgpu::TextureViewDescriptor::default());
        let raw_view = raw_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let black_view = black_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mask_view = mask_texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("auraw local-mask array view"),
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

        let default_output_transform = IccOutputTransform::srgb();
        let profile_gpu_data = raw.camera_profile.gpu_data(&default_output_transform);
        let output_lut_offset_bytes = u64::from(profile_gpu_data.layout.output[3])
            * std::mem::size_of::<[f32; 4]>() as u64;
        let profile_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("auraw DCP and ICC profile LUTs"),
            contents: bytemuck::cast_slice(&profile_gpu_data.words),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("auraw params"),
            contents: bytemuck::bytes_of(params),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let tone_histogram_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("auraw tone histogram"),
            size: 256 * std::mem::size_of::<u32>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let tone_stats_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("auraw tone statistics"),
            size: 2 * std::mem::size_of::<[f32; 4]>() as u64,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let common_entries = [
            buffer_entry(0),
            texture_entry(1, wgpu::TextureSampleType::Uint),
            texture_entry(2, wgpu::TextureSampleType::Uint),
            texture_entry(19, wgpu::TextureSampleType::Float { filterable: false }),
        ];

        let bgl_highlights = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl highlights"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                common_entries[3].clone(),
                storage_texture_entry(
                    3,
                    wgpu::TextureFormat::R32Float,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
                texture_entry(13, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(
                    14,
                    highlight_work_format,
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
                common_entries[3].clone(),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(
                    4,
                    demosaic_format,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
            ],
        });

        let bgl2 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl2"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                common_entries[3].clone(),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(5, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(
                    6,
                    demosaic_format,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
            ],
        });

        let bgl3 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl3"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                common_entries[3].clone(),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(7, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(
                    8,
                    demosaic_format,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
            ],
        });

        let bgl4 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl4"),
            entries: &[
                common_entries[0].clone(),
                common_entries[1].clone(),
                common_entries[2].clone(),
                common_entries[3].clone(),
                texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(7, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(9, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(
                    10,
                    demosaic_format,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
            ],
        });

        // X-Trans Markesteijn-3 uses the two highlight work textures as
        // derivative scratch after highlight reconstruction has finalized.
        // This retains the reference eight-direction homogeneity stages without
        // allocating eight full-resolution RGB candidate images.
        let bgl_xtrans_derivatives =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl X-Trans derivatives"),
                entries: &[
                    common_entries[0].clone(),
                    common_entries[1].clone(),
                    common_entries[2].clone(),
                    common_entries[3].clone(),
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(7, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        20,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                    storage_texture_entry(
                        21,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            });

        let bgl_xtrans_homogeneity =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl X-Trans homogeneity"),
                entries: &[
                    common_entries[0].clone(),
                    common_entries[1].clone(),
                    common_entries[2].clone(),
                    common_entries[3].clone(),
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(20, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(21, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        24,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                    storage_texture_entry(
                        25,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            });

        let bgl_xtrans_accumulate =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl X-Trans accumulate"),
                entries: &[
                    common_entries[0].clone(),
                    common_entries[1].clone(),
                    common_entries[2].clone(),
                    common_entries[3].clone(),
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(7, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(24, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(25, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        26,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            });

        let bgl_xtrans_finish =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl X-Trans finish"),
                entries: &[
                    common_entries[0].clone(),
                    common_entries[1].clone(),
                    common_entries[2].clone(),
                    common_entries[3].clone(),
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(26, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        10,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            });

        let bgl_tone_prepare =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl tone prepare"),
                entries: &[
                    buffer_entry(0),
                    texture_entry(11, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_buffer_entry(15, false),
                    storage_buffer_entry(20, true),
                    storage_texture_entry(
                        18,
                        tone_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            });

        let bgl_tone_blur =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl tone guide blur"),
                entries: &[
                    buffer_entry(0),
                    texture_entry(17, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        18,
                        tone_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            });

        let bgl_tone_reduce =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl tone histogram reduction"),
                entries: &[
                    storage_buffer_entry(15, false),
                    storage_buffer_entry(16, false),
                ],
            });

        let bgl_adjust_prepare =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl adjustment preparation"),
                entries: &[
                    common_entries[0].clone(),
                    common_entries[1].clone(),
                    common_entries[2].clone(),
                    common_entries[3].clone(),
                    texture_entry(11, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        21,
                        work_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                    storage_buffer_entry(16, true),
                    texture_entry(17, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_buffer_entry(20, true),
                    texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
                    sampler_entry(28),
                ],
            });

        let bgl_adjust_effects =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl Lightroom local effects"),
                entries: &[
                    buffer_entry(0),
                    texture_entry(22, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        23,
                        work_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                    texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
                    sampler_entry(28),
                ],
            });

        let bgl_adjust_creative =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl creative glow and vignette"),
                entries: &[
                    buffer_entry(0),
                    texture_entry(24, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        25,
                        work_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            });

        let bgl_adjust_render =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl perceptual color mixer and render"),
                entries: &[
                    buffer_entry(0),
                    storage_texture_entry(
                        12,
                        wgpu::TextureFormat::Rgba8Unorm,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                    texture_entry(26, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_buffer_entry(20, true),
                ],
            });

        let make_highlight_bind_group =
            |label: &str,
             read_view: &wgpu::TextureView,
             write_view: &wgpu::TextureView| {
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
                            binding: 19,
                            resource: wgpu::BindingResource::TextureView(&black_view),
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
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

        let bg_xtrans_derivatives = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg X-Trans derivatives"),
            layout: &bgl_xtrans_derivatives,
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
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
                    binding: 20,
                    resource: wgpu::BindingResource::TextureView(&highlight_work_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 21,
                    resource: wgpu::BindingResource::TextureView(&highlight_work_b_view),
                },
            ],
        });

        let bg_xtrans_homogeneity = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg X-Trans homogeneity"),
            layout: &bgl_xtrans_homogeneity,
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&reconstructed_raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 20,
                    resource: wgpu::BindingResource::TextureView(&highlight_work_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 21,
                    resource: wgpu::BindingResource::TextureView(&highlight_work_b_view),
                },
                wgpu::BindGroupEntry {
                    binding: 24,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 25,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
            ],
        });

        let bg_xtrans_accumulate = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg X-Trans accumulate"),
            layout: &bgl_xtrans_accumulate,
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
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
                    binding: 24,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 25,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 26,
                    resource: wgpu::BindingResource::TextureView(&highlight_work_a_view),
                },
            ],
        });

        let bg_xtrans_finish = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg X-Trans finish"),
            layout: &bgl_xtrans_finish,
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&reconstructed_raw_view),
                },
                wgpu::BindGroupEntry {
                    binding: 26,
                    resource: wgpu::BindingResource::TextureView(&highlight_work_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
            ],
        });

        let bg_tone_prepare = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg tone prepare"),
            layout: &bgl_tone_prepare,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 15,
                    resource: tone_histogram_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 20,
                    resource: profile_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 18,
                    resource: wgpu::BindingResource::TextureView(&tone_guide_a_view),
                },
            ],
        });

        let make_tone_blur_bind_group =
            |label: &str,
             read_view: &wgpu::TextureView,
             write_view: &wgpu::TextureView| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &bgl_tone_blur,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: params_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 17,
                            resource: wgpu::BindingResource::TextureView(read_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 18,
                            resource: wgpu::BindingResource::TextureView(write_view),
                        },
                    ],
                })
            };
        let bg_tone_horizontal = make_tone_blur_bind_group(
            "bg tone guide horizontal",
            &tone_guide_a_view,
            &tone_guide_b_view,
        );
        let bg_tone_vertical = make_tone_blur_bind_group(
            "bg tone guide vertical",
            &tone_guide_b_view,
            &tone_guide_a_view,
        );

        let bg_tone_reduce = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg tone histogram reduction"),
            layout: &bgl_tone_reduce,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 15,
                    resource: tone_histogram_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 16,
                    resource: tone_stats_buffer.as_entire_binding(),
                },
            ],
        });

        let bg_adjust_prepare = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg adjustment preparation"),
            layout: &bgl_adjust_prepare,
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 21,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 16,
                    resource: tone_stats_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 17,
                    resource: wgpu::BindingResource::TextureView(&tone_guide_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 20,
                    resource: profile_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 27,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 28,
                    resource: wgpu::BindingResource::Sampler(&mask_sampler),
                },
            ],
        });

        let bg_adjust_effects = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg Lightroom local effects"),
            layout: &bgl_adjust_effects,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 22,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 23,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
                wgpu::BindGroupEntry {
                    binding: 27,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 28,
                    resource: wgpu::BindingResource::Sampler(&mask_sampler),
                },
            ],
        });

        // The local-effects result is read from tex2 and the creative pass
        // writes back into tex1. This reuses the existing ping-pong textures
        // while ensuring Glow samples the exact same developed stage it adds to.
        let bg_adjust_creative = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg creative glow and vignette"),
            layout: &bgl_adjust_creative,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 24,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
                wgpu::BindGroupEntry {
                    binding: 25,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
            ],
        });

        let bg_adjust_render = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg perceptual color mixer and render"),
            layout: &bgl_adjust_render,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::TextureView(&out_view),
                },
                wgpu::BindGroupEntry {
                    binding: 26,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 20,
                    resource: profile_buffer.as_entire_binding(),
                },
            ],
        });

        // Storage texture declarations are format-specific in WGSL. Generate
        // the full-float variants once when High quality is selected. This now
        // covers highlight reconstruction as well as demosaic/scene buffers.
        let highlight_shader = work_shader_source(SHADER_HIGHLIGHTS, highlight_work_format);
        let bayer_rcd_p1 = work_shader_source(SHADER_BAYER_RCD_P1, demosaic_format);
        let bayer_rcd_p2 = work_shader_source(SHADER_BAYER_RCD_P2, demosaic_format);
        let bayer_rcd_p3 = work_shader_source(SHADER_BAYER_RCD_P3, demosaic_format);
        let bayer_rcd_p4 = work_shader_source(SHADER_BAYER_RCD_P4, demosaic_format);
        let xtrans_p1 = work_shader_source(SHADER_XTRANS_P1, demosaic_format);
        let xtrans_p2 = work_shader_source(SHADER_XTRANS_P2, demosaic_format);
        let xtrans_p3 = work_shader_source(SHADER_XTRANS_P3, demosaic_format);
        let xtrans_p4 = work_shader_source(SHADER_XTRANS_P4, demosaic_format);
        let xtrans_p5 = work_shader_source(SHADER_XTRANS_P5, demosaic_format);
        let xtrans_p6 = work_shader_source(SHADER_XTRANS_P6, demosaic_format);
        let xtrans_p7 = work_shader_source(SHADER_XTRANS_P7, demosaic_format);
        let adjustments_shader = work_shader_source(SHADER_ADJUSTMENTS, work_format);

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

        let mut passes = Vec::with_capacity(
            1 + HIGHLIGHT_GUIDED_ENTRY_POINTS.len() + 1 + 8 + 7,
        );

        // Prepare writes the initial RGB estimate and reliability into A.
        passes.push(Pass {
            pipeline: make_pipeline(highlight_shader.as_ref(), "highlight_prepare", &bgl_highlights),
            bind_group: make_highlight_bind_group(
                "bg highlight prepare",
                &highlight_work_b_view,
                &highlight_work_a_view,
            ),
            workgroups: image_workgroups,
        });

        // The multiscale solver ping-pongs through every declared stage.
        // Quality levels are handled inside each entry point, so all stages
        // are dispatched and disabled ones copy read -> write unchanged.
        for (index, entry) in HIGHLIGHT_GUIDED_ENTRY_POINTS.iter().enumerate() {
            let (read_slot, write_slot) = highlight_stage_slots(index);
            debug_assert_ne!(read_slot, write_slot);
            let read_view = match read_slot {
                HighlightWorkSlot::A => &highlight_work_a_view,
                HighlightWorkSlot::B => &highlight_work_b_view,
            };
            let write_view = match write_slot {
                HighlightWorkSlot::A => &highlight_work_a_view,
                HighlightWorkSlot::B => &highlight_work_b_view,
            };
            let label = format!("bg {entry}");
            passes.push(Pass {
                pipeline: make_pipeline(highlight_shader.as_ref(), entry, &bgl_highlights),
                bind_group: make_highlight_bind_group(&label, read_view, write_view),
                workgroups: image_workgroups,
            });
        }

        // Prepare leaves the data in A. The final source is derived from the
        // same parity helper used by the stage planner and covered by tests.
        let final_read_slot = highlight_final_read_slot(HIGHLIGHT_GUIDED_ENTRY_POINTS.len());
        let final_write_slot = match final_read_slot {
            HighlightWorkSlot::A => HighlightWorkSlot::B,
            HighlightWorkSlot::B => HighlightWorkSlot::A,
        };
        let final_read_view = match final_read_slot {
            HighlightWorkSlot::A => &highlight_work_a_view,
            HighlightWorkSlot::B => &highlight_work_b_view,
        };
        let final_write_view = match final_write_slot {
            HighlightWorkSlot::A => &highlight_work_a_view,
            HighlightWorkSlot::B => &highlight_work_b_view,
        };
        passes.push(Pass {
            pipeline: make_pipeline(highlight_shader.as_ref(), "highlight_finalize", &bgl_highlights),
            bind_group: make_highlight_bind_group(
                "bg highlight finalize",
                final_read_view,
                final_write_view,
            ),
            workgroups: image_workgroups,
        });

        // Select the demosaic family from LibRaw's CFA classification.
        // Bayer uses the four-stage ratio-corrected reference path. Fuji
        // X-Trans seeds an RGB image, performs three green/chroma refinement
        // passes, then selects among eight homogeneity-guided candidates.
        match raw.cfa_kind {
            CfaKind::Bayer => passes.extend([
                Pass {
                    pipeline: make_pipeline(
                        bayer_rcd_p1.as_ref(),
                        "bayer_rcd_directional",
                        &bgl1,
                    ),
                    bind_group: bg1,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        bayer_rcd_p2.as_ref(),
                        "bayer_rcd_green",
                        &bgl2,
                    ),
                    bind_group: bg2,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        bayer_rcd_p3.as_ref(),
                        "bayer_rcd_chroma",
                        &bgl3,
                    ),
                    bind_group: bg3,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        bayer_rcd_p4.as_ref(),
                        "bayer_rcd_output",
                        &bgl4,
                    ),
                    bind_group: bg4,
                    workgroups: image_workgroups,
                },
            ]),
            CfaKind::XTrans => passes.extend([
                Pass {
                    pipeline: make_pipeline(xtrans_p1.as_ref(), "xtrans_seed", &bgl1),
                    bind_group: bg1,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p2.as_ref(),
                        "xtrans_markesteijn_pass1",
                        &bgl2,
                    ),
                    bind_group: bg2.clone(),
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p3.as_ref(),
                        "xtrans_markesteijn_pass2",
                        &bgl3,
                    ),
                    bind_group: bg3,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p2.as_ref(),
                        "xtrans_markesteijn_pass3",
                        &bgl2,
                    ),
                    bind_group: bg2,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p4.as_ref(),
                        "xtrans_markesteijn_derivatives",
                        &bgl_xtrans_derivatives,
                    ),
                    bind_group: bg_xtrans_derivatives,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p5.as_ref(),
                        "xtrans_markesteijn_homogeneity",
                        &bgl_xtrans_homogeneity,
                    ),
                    bind_group: bg_xtrans_homogeneity,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p6.as_ref(),
                        "xtrans_markesteijn_accumulate",
                        &bgl_xtrans_accumulate,
                    ),
                    bind_group: bg_xtrans_accumulate,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p7.as_ref(),
                        "xtrans_demosaic_finish",
                        &bgl_xtrans_finish,
                    ),
                    bind_group: bg_xtrans_finish,
                    workgroups: image_workgroups,
                },
            ]),
        }

        let raw_stage_end = passes.len();

        // Analyze the unexposed scene at reduced resolution. The guide is
        // bilateral and the histogram reduction emits robust tonal anchors.
        // recompute() clears the histogram immediately before this pass.
        let tone_prepare_pass_index = passes.len();
        passes.extend([
            Pass {
                pipeline: make_pipeline(
                    SHADER_TONE_ANALYSIS,
                    "tone_guide_prepare",
                    &bgl_tone_prepare,
                ),
                bind_group: bg_tone_prepare,
                workgroups: tone_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    SHADER_TONE_ANALYSIS,
                    "tone_guide_horizontal",
                    &bgl_tone_blur,
                ),
                bind_group: bg_tone_horizontal,
                workgroups: tone_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    SHADER_TONE_ANALYSIS,
                    "tone_guide_vertical",
                    &bgl_tone_blur,
                ),
                bind_group: bg_tone_vertical,
                workgroups: tone_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    SHADER_TONE_ANALYSIS,
                    "tone_reduce_histogram",
                    &bgl_tone_reduce,
                ),
                bind_group: bg_tone_reduce,
                workgroups: single_workgroup,
            },
        ]);

        let tone_reduce_pass_index = tone_prepare_pass_index + 3;
        let tone_stage_end = passes.len();

        passes.extend([
            Pass {
                pipeline: make_pipeline(
                    adjustments_shader.as_ref(),
                    "prepare_adjustment_base",
                    &bgl_adjust_prepare,
                ),
                bind_group: bg_adjust_prepare,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_shader.as_ref(),
                    "apply_lightroom_effects",
                    &bgl_adjust_effects,
                ),
                bind_group: bg_adjust_effects,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_shader.as_ref(),
                    "apply_creative_effects",
                    &bgl_adjust_creative,
                ),
                bind_group: bg_adjust_creative,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_shader.as_ref(),
                    "apply_lightroom_adjustments",
                    &bgl_adjust_render,
                ),
                bind_group: bg_adjust_render,
                workgroups: image_workgroups,
            },
        ]);

        let egui_texture_id = renderer.map(|renderer| {
            renderer.register_native_texture(device, &out_view, wgpu::FilterMode::Linear)
        });

        let pipeline = Self {
            egui_texture_id,
            width: raw.width,
            height: raw.height,
            params_buffer,
            tone_histogram_buffer,
            raw_stage_end,
            tone_prepare_pass_index,
            tone_reduce_pass_index,
            tone_stage_end,
            passes,
            raw_texture,
            color_texture,
            black_texture,
            _reconstructed_raw_texture: reconstructed_raw_texture,
            _highlight_work_a: highlight_work_a,
            _highlight_work_b: highlight_work_b,
            _tex1: tex1,
            _tex2: tex2,
            scene_texture,
            scene_format: demosaic_format,
            tone_stats_buffer,
            _tone_guide_a: tone_guide_a,
            _tone_guide_b: tone_guide_b,
            mask_texture,
            mask_atlas_edge,
            profile_buffer,
            output_lut_offset_bytes,
            out_texture,
            _out_view: out_view,
        };
        Ok(pipeline)
    }

    /// Uploads one normalized, anti-aliased local-mask layer. The same atlas
    /// is sampled by preview proxies and full-resolution export tiles.
    pub fn update_mask_layer(
        &self,
        queue: &wgpu::Queue,
        layer: usize,
        bytes: &[u8],
    ) -> Result<()> {
        if layer >= MAX_LOCAL_MASKS {
            return Err(anyhow!("local-mask layer {layer} is out of range"));
        }
        let expected = self.mask_atlas_edge as usize * self.mask_atlas_edge as usize;
        if bytes.len() != expected {
            return Err(anyhow!(
                "local-mask layer has {} bytes, expected {expected}",
                bytes.len()
            ));
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.mask_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.mask_atlas_edge),
                rows_per_image: Some(self.mask_atlas_edge),
            },
            wgpu::Extent3d {
                width: self.mask_atlas_edge,
                height: self.mask_atlas_edge,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    pub const fn mask_atlas_edge(&self) -> u32 {
        self.mask_atlas_edge
    }

    /// Registers a headless pipeline's output texture with egui after the
    /// expensive GPU setup has completed on a worker thread.
    pub fn register_egui_texture(
        &mut self,
        device: &wgpu::Device,
        renderer: &mut egui_wgpu::Renderer,
    ) -> egui::TextureId {
        if let Some(texture_id) = self.egui_texture_id {
            return texture_id;
        }

        let texture_id = renderer.register_native_texture(
            device,
            &self._out_view,
            wgpu::FilterMode::Linear,
        );
        self.egui_texture_id = Some(texture_id);
        texture_id
    }

    /// Updates the preview/display transform from an RGB ICC matrix-shaper
    /// profile without rebuilding compute pipelines or bind groups.
    pub fn set_display_icc_profile(
        &self,
        queue: &wgpu::Queue,
        profile_bytes: &[u8],
        intent: RenderingIntent,
    ) -> Result<()> {
        let transform = IccOutputTransform::from_icc(profile_bytes, intent)?;
        self.write_output_transform(queue, &transform)
    }

    /// Alias for export-oriented callers that use the same managed output
    /// transform as the live preview.
    pub fn set_output_icc_profile(
        &self,
        queue: &wgpu::Queue,
        profile_bytes: &[u8],
        intent: RenderingIntent,
    ) -> Result<()> {
        self.set_display_icc_profile(queue, profile_bytes, intent)
    }

    pub fn reset_display_to_srgb(&self, queue: &wgpu::Queue) -> Result<()> {
        self.write_output_transform(queue, &IccOutputTransform::srgb())
    }

    pub fn write_output_transform(
        &self,
        queue: &wgpu::Queue,
        transform: &IccOutputTransform,
    ) -> Result<()> {
        if transform.size() != crate::pipeline::color_profile::OUTPUT_LUT_EDGE {
            return Err(anyhow!("output ICC LUT edge does not match the GPU profile layout"));
        }
        queue.write_buffer(
            &self.profile_buffer,
            self.output_lut_offset_bytes,
            bytemuck::cast_slice(transform.entries()),
        );
        Ok(())
    }

    /// Compatibility entry point that executes the complete pipeline. New UI
    /// code should prefer `dispatch_stage` so cached upstream results survive
    /// ordinary Develop adjustments.
    pub fn recompute(&self, queue: &wgpu::Queue, device: &wgpu::Device, params: &GpuParams) {
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(params));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw complete recompute encoder"),
        });
        encoder.clear_buffer(&self.tone_histogram_buffer, 0, None);
        self.encode_pass_range(&mut encoder, 0, self.passes.len());
        queue.submit(Some(encoder.finish()));
    }

    /// Dispatches exactly one dependency stage. GPU submission is asynchronous;
    /// callers can spread Raw -> Tone -> Output across event-loop iterations.
    pub fn dispatch_stage(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
        stage: ProcessingStage,
    ) {
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(params));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some(stage.label()),
        });

        match stage {
            ProcessingStage::Raw => self.encode_pass_range(&mut encoder, 0, self.raw_stage_end),
            ProcessingStage::Tone => {
                encoder.clear_buffer(&self.tone_histogram_buffer, 0, None);
                self.encode_pass_range(
                    &mut encoder,
                    self.tone_prepare_pass_index,
                    self.tone_stage_end,
                );
            }
            ProcessingStage::Output => {
                self.encode_pass_range(&mut encoder, self.tone_stage_end, self.passes.len());
            }
        }

        queue.submit(Some(encoder.finish()));
    }

    /// Replaces the sensor textures of a fixed-size headless pipeline so one
    /// allocation and one set of compiled compute pipelines can process every
    /// export tile.
    pub fn upload_raw_tile(&self, queue: &wgpu::Queue, raw: &LoadedRaw) -> Result<()> {
        if raw.width != self.width || raw.height != self.height {
            return Err(anyhow!(
                "tile dimensions {}x{} do not match reusable pipeline {}x{}",
                raw.width,
                raw.height,
                self.width,
                self.height
            ));
        }
        validate_raw(raw)?;

        queue.write_texture(
            copy_texture(&self.raw_texture),
            bytemuck::cast_slice(&raw.raw_pixels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(raw.width * 2),
                rows_per_image: Some(raw.height),
            },
            texture_size(raw.width, raw.height),
        );
        queue.write_texture(
            copy_texture(&self.color_texture),
            &raw.color_indices,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(raw.width),
                rows_per_image: Some(raw.height),
            },
            texture_size(raw.width, raw.height),
        );
        queue.write_texture(
            copy_texture(&self.black_texture),
            bytemuck::cast_slice(&raw.black_levels_per_pixel),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(raw.width * 4),
                rows_per_image: Some(raw.height),
            },
            texture_size(raw.width, raw.height),
        );
        Ok(())
    }

    /// Executes one export tile using the preview pipeline's cached global
    /// tone statistics. The tile still builds its own halo-aware tone guide,
    /// but skipping histogram reduction prevents tile-to-tile tonal seams.
    pub fn dispatch_export_tile(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
        global_tone_source: &RawGpuPipeline,
    ) {
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(params));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw tiled export encoder"),
        });

        self.encode_pass_range(&mut encoder, 0, self.raw_stage_end);
        self.encode_pass_range(
            &mut encoder,
            self.tone_prepare_pass_index,
            self.tone_reduce_pass_index,
        );
        encoder.copy_buffer_to_buffer(
            &global_tone_source.tone_stats_buffer,
            0,
            &self.tone_stats_buffer,
            0,
            2 * std::mem::size_of::<[f32; 4]>() as u64,
        );
        self.encode_pass_range(&mut encoder, self.tone_stage_end, self.passes.len());
        queue.submit(Some(encoder.finish()));
    }

    /// Copies an RGBA8 output sub-rectangle to CPU memory. This method blocks
    /// only the export worker thread; the interactive UI remains responsive.
    pub fn read_output_region_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        if width == 0 || height == 0 || x + width > self.width || y + height > self.height {
            return Err(anyhow!("invalid GPU readback rectangle"));
        }

        let unpadded_bytes_per_row = width * 4;
        let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
        let buffer_size = u64::from(padded_bytes_per_row) * u64::from(height);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("auraw tiled export readback"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw tiled export copy encoder"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.out_texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let submission = queue.submit(Some(encoder.finish()));

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        readback.map_async(wgpu::MapMode::Read, .., move |result| {
            let _ = sender.send(result);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: None,
            })
            .map_err(|error| anyhow!("GPU poll failed during export: {error}"))?;
        receiver
            .recv()
            .map_err(|_| anyhow!("GPU readback callback was dropped"))?
            .map_err(|error| anyhow!("GPU readback mapping failed: {error}"))?;

        let mapped = readback.get_mapped_range(..);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for row in 0..height as usize {
            let src = row * padded_bytes_per_row as usize;
            let dst = row * unpadded_bytes_per_row as usize;
            rgba[dst..dst + unpadded_bytes_per_row as usize]
                .copy_from_slice(&mapped[src..src + unpadded_bytes_per_row as usize]);
        }
        drop(mapped);
        readback.unmap();
        Ok(rgba)
    }

    /// Reads the internal demosaiced scene texture as tightly packed RGB32F.
    /// The raw stage must have been submitted before this call. Regression
    /// renders use `ProcessingQuality::High`, because half-float preview
    /// intermediates are intentionally rejected rather than silently widened.
    pub fn read_scene_texture_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> Result<Vec<f32>> {
        if self.scene_format != wgpu::TextureFormat::Rgba32Float {
            return Err(anyhow!(
                "scene texture readback requires ProcessingQuality::High (RGBA32Float), got {:?}",
                self.scene_format
            ));
        }
        read_rgba32_texture_rgb_blocking(
            device,
            queue,
            &self.scene_texture,
            self.width,
            self.height,
            "auraw scene texture readback",
        )
    }

    /// Runs the raw stage and converts its camera-RGB scene texture into the
    /// canonical scene-linear Rec.2020 representation used by the regression
    /// harness. This deliberately stops before creative look/tone modules and
    /// before the display transform.
    pub fn render_regression_scene_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: &GpuParams,
    ) -> Result<Vec<f32>> {
        if self.scene_format != wgpu::TextureFormat::Rgba32Float {
            return Err(anyhow!(
                "regression scene rendering requires ProcessingQuality::High (RGBA32Float)"
            ));
        }

        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(params));
        let size = texture_size(self.width, self.height);
        let working_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw regression scene-linear Rec.2020"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba32Float],
        });
        let working_view = working_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let scene_view = self
            .scene_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let raw_view = self.raw_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let color_view = self
            .color_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let black_view = self
            .black_texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("auraw regression scene layout"),
            entries: &[
                buffer_entry(0),
                texture_entry(1, wgpu::TextureSampleType::Uint),
                texture_entry(2, wgpu::TextureSampleType::Uint),
                texture_entry(19, wgpu::TextureSampleType::Float { filterable: false }),
                texture_entry(11, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(
                    12,
                    wgpu::TextureFormat::Rgba32Float,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
                storage_buffer_entry(20, true),
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("auraw regression scene bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.params_buffer.as_entire_binding(),
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
                    binding: 19,
                    resource: wgpu::BindingResource::TextureView(&black_view),
                },
                wgpu::BindGroupEntry {
                    binding: 11,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::TextureView(&working_view),
                },
                wgpu::BindGroupEntry {
                    binding: 20,
                    resource: self.profile_buffer.as_entire_binding(),
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("auraw regression scene shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_REGRESSION_SCENE.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("auraw regression scene pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("auraw regression scene pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("write_regression_scene"),
            compilation_options: Default::default(),
            cache: None,
        });

        let (readback, padded_bytes_per_row) = create_rgba32_readback_buffer(
            device,
            self.width,
            self.height,
            "auraw regression readback",
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw regression scene encoder"),
        });
        self.encode_pass_range(&mut encoder, 0, self.raw_stage_end);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("auraw regression scene conversion"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(self.width.div_ceil(8), self.height.div_ceil(8), 1);
        }
        encode_rgba32_texture_copy(
            &mut encoder,
            &working_texture,
            &readback,
            self.width,
            self.height,
            padded_bytes_per_row,
        );
        let submission = queue.submit(Some(encoder.finish()));
        map_rgba32_readback_rgb(
            device,
            &readback,
            submission,
            self.width,
            self.height,
            padded_bytes_per_row,
        )
    }

    fn encode_pass_range(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        start: usize,
        end: usize,
    ) {
        for i in start..end {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some(&format!("auraw pass {}", i + 1)),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.passes[i].pipeline);
            pass.set_bind_group(0, &self.passes[i].bind_group, &[]);
            let workgroups = self.passes[i].workgroups;
            pass.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
        }
    }
}

fn read_rgba32_texture_rgb_blocking(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    label: &'static str,
) -> Result<Vec<f32>> {
    let (readback, padded_bytes_per_row) =
        create_rgba32_readback_buffer(device, width, height, label);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some(label),
    });
    encode_rgba32_texture_copy(
        &mut encoder,
        texture,
        &readback,
        width,
        height,
        padded_bytes_per_row,
    );
    let submission = queue.submit(Some(encoder.finish()));
    map_rgba32_readback_rgb(
        device,
        &readback,
        submission,
        width,
        height,
        padded_bytes_per_row,
    )
}

fn create_rgba32_readback_buffer(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    label: &'static str,
) -> (wgpu::Buffer, u32) {
    let unpadded_bytes_per_row = width * 16;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: u64::from(padded_bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    (buffer, padded_bytes_per_row)
}

fn encode_rgba32_texture_copy(
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    readback: &wgpu::Buffer,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
) {
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        texture_size(width, height),
    );
}

fn map_rgba32_readback_rgb(
    device: &wgpu::Device,
    readback: &wgpu::Buffer,
    submission: wgpu::SubmissionIndex,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
) -> Result<Vec<f32>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    readback.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .map_err(|error| anyhow!("GPU poll failed during scene readback: {error}"))?;
    receiver
        .recv()
        .map_err(|_| anyhow!("GPU scene readback callback was dropped"))?
        .map_err(|error| anyhow!("GPU scene readback mapping failed: {error}"))?;

    let mapped = readback.get_mapped_range(..);
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for row in 0..height as usize {
        let row_start = row * padded_bytes_per_row as usize;
        for column in 0..width as usize {
            let pixel_start = row_start + column * 16;
            for channel in 0..3 {
                let offset = pixel_start + channel * 4;
                let bytes: [u8; 4] = mapped[offset..offset + 4]
                    .try_into()
                    .expect("RGBA32Float texel is four aligned f32 values");
                rgb.push(f32::from_le_bytes(bytes));
            }
        }
    }
    drop(mapped);
    readback.unmap();
    if rgb.iter().any(|value| !value.is_finite()) {
        return Err(anyhow!("scene texture readback contains NaN or infinity"));
    }
    Ok(rgb)
}

fn tone_analysis_scale() -> u32 {
    if cfg!(target_os = "android") { 8 } else { 4 }
}

fn tone_guide_format() -> wgpu::TextureFormat {
    // The guide is reduced-resolution, so R32Float costs little even on
    // Android and avoids optional R16Float storage-texture support.
    wgpu::TextureFormat::R32Float
}

fn default_processing_quality() -> ProcessingQuality {
    if cfg!(target_os = "android") {
        ProcessingQuality::Preview
    } else {
        ProcessingQuality::High
    }
}

fn processing_work_format(quality: ProcessingQuality) -> wgpu::TextureFormat {
    match quality {
        ProcessingQuality::Preview => wgpu::TextureFormat::Rgba16Float,
        ProcessingQuality::High => wgpu::TextureFormat::Rgba32Float,
    }
}

fn work_shader_source(source: &str, format: wgpu::TextureFormat) -> Cow<'_, str> {
    match format {
        wgpu::TextureFormat::Rgba16Float => Cow::Borrowed(source),
        wgpu::TextureFormat::Rgba32Float => {
            Cow::Owned(source.replace("rgba16float", "rgba32float"))
        }
        _ => unreachable!("unsupported AuRaw work texture format: {format:?}"),
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
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[format],
    })
}

fn create_tone_guide_texture(
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

fn storage_buffer_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
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
    if raw.width == 0 || raw.height == 0 {
        return Err(anyhow!("raw dimensions must be non-zero"));
    }
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
    if raw.black_levels_per_pixel.len() != pixels {
        return Err(anyhow!(
            "black-level map count mismatch: got {}, expected {}",
            raw.black_levels_per_pixel.len(),
            pixels
        ));
    }
    if raw.color_indices.iter().any(|channel| *channel > 3) {
        return Err(anyhow!("CFA index map contains a channel above 3"));
    }
    if raw.wb_coeffs.iter().any(|value| !value.is_finite() || *value <= 0.0) {
        return Err(anyhow!("white-balance coefficients must be finite and positive"));
    }
    if raw.cam_to_srgb.iter().flatten().any(|value| !value.is_finite()) {
        return Err(anyhow!("camera-to-working matrix contains a non-finite value"));
    }
    if raw.cam_to_srgb.iter().flatten().all(|value| value.abs() <= 1e-12) {
        return Err(anyhow!("camera-to-working matrix is empty"));
    }
    if raw.black_levels.iter().any(|value| !value.is_finite())
        || raw.white_levels.iter().any(|value| !value.is_finite())
    {
        return Err(anyhow!("black/white calibration contains a non-finite value"));
    }

    for (index, (&black, &channel)) in raw
        .black_levels_per_pixel
        .iter()
        .zip(&raw.color_indices)
        .enumerate()
    {
        let white = raw.white_levels[channel as usize];
        if !black.is_finite() || white <= black {
            return Err(anyhow!(
                "invalid black/white range at pixel {index}: black={black}, white={white}"
            ));
        }
    }
    Ok(())
}

fn texture_array_entry(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
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

fn create_black_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: &LoadedRaw,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("auraw per-pixel black levels"),
        size: texture_size(raw.width, raw.height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::R32Float],
    });
    queue.write_texture(
        copy_texture(&texture),
        bytemuck::cast_slice(&raw.black_levels_per_pixel),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(raw.width * 4),
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
        highlight_final_read_slot, highlight_stage_slots, processing_work_format,
        work_shader_source, HighlightWorkSlot, ProcessingQuality,
        HIGHLIGHT_GUIDED_ENTRY_POINTS, SHADER_ADJUSTMENTS, SHADER_BAYER_RCD_P1,
        SHADER_BAYER_RCD_P2, SHADER_BAYER_RCD_P3, SHADER_BAYER_RCD_P4,
        SHADER_HIGHLIGHTS, SHADER_REGRESSION_SCENE, SHADER_TONE_ANALYSIS,
        SHADER_XTRANS_P1, SHADER_XTRANS_P2, SHADER_XTRANS_P3, SHADER_XTRANS_P4,
        SHADER_XTRANS_P5, SHADER_XTRANS_P6, SHADER_XTRANS_P7,
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
            ("X-Trans derivatives", SHADER_XTRANS_P4),
            ("X-Trans homogeneity", SHADER_XTRANS_P5),
            ("X-Trans accumulation", SHADER_XTRANS_P6),
            ("X-Trans finish", SHADER_XTRANS_P7),
            ("adaptive tone analysis", SHADER_TONE_ANALYSIS),
            ("regression scene export", SHADER_REGRESSION_SCENE),
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
    fn high_quality_shader_variants_parse_and_use_full_float_storage() {
        for (name, source) in [
            (
                "32-bit highlight reconstruction",
                work_shader_source(
                    SHADER_HIGHLIGHTS,
                    processing_work_format(ProcessingQuality::High),
                ),
            ),
            (
                "32-bit Bayer pass 1",
                work_shader_source(
                    SHADER_BAYER_RCD_P1,
                    processing_work_format(ProcessingQuality::High),
                ),
            ),
            (
                "32-bit perceptual color mixer",
                work_shader_source(
                    SHADER_ADJUSTMENTS,
                    processing_work_format(ProcessingQuality::High),
                ),
            ),
        ] {
            assert!(!source.contains("rgba16float"));
            assert!(source.contains("rgba32float"));
            let module = naga::front::wgsl::parse_str(source.as_ref())
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
    fn highlight_ping_pong_plan_is_contiguous_and_finishes_on_expected_slot() {
        let mut current = HighlightWorkSlot::A;
        for index in 0..HIGHLIGHT_GUIDED_ENTRY_POINTS.len() {
            let (read, write) = highlight_stage_slots(index);
            assert_eq!(read, current, "stage {index} reads the wrong work texture");
            assert_ne!(read, write, "stage {index} aliases its input and output");
            current = write;
        }
        assert_eq!(
            current,
            highlight_final_read_slot(HIGHLIGHT_GUIDED_ENTRY_POINTS.len())
        );
        assert_eq!(current, HighlightWorkSlot::B);
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
                module.entry_points.iter().any(|entry| entry.name == expected),
                "highlight shader is missing entry point {expected}"
            );
        }
    }

    #[test]
    fn demosaic_reference_invariants_are_present() {
        assert!(SHADER_BAYER_RCD_P4.contains("const RCD_MARGIN: i32 = 9"));
        assert!(SHADER_BAYER_RCD_P4.contains("ppg_rgb_at"));
        assert!(SHADER_BAYER_RCD_P2.contains("green = mix(vertical.x, horizontal.x, vh)"));
        assert!(SHADER_BAYER_RCD_P3.contains("return mix(p_est, q_est, pq)"));

        assert!(SHADER_XTRANS_P6.contains("index < 8u"));
        assert!(SHADER_XTRANS_P5.contains("minimum * 8.0"));
        assert!(SHADER_XTRANS_P6.contains("mark_homo_sum5"));
        assert!(SHADER_XTRANS_P6.contains("index + 4u"));
        assert!(SHADER_XTRANS_P6.contains("MARKESTEIJN3_MARGIN"));

        assert!(SHADER_BAYER_RCD_P4.contains("for (var dy = -6; dy <= 6"));
        assert!(SHADER_XTRANS_P7.contains("for (var dy = -6; dy <= 6"));
        assert!(SHADER_BAYER_RCD_P4.contains("detail /= 256.0"));
        assert!(SHADER_XTRANS_P7.contains("detail /= 256.0"));
    }

    #[test]
    fn demosaic_shaders_expose_every_dispatched_entry_point() {
        for (source, expected) in [
            (SHADER_BAYER_RCD_P1, "bayer_rcd_directional"),
            (SHADER_BAYER_RCD_P2, "bayer_rcd_green"),
            (SHADER_BAYER_RCD_P3, "bayer_rcd_chroma"),
            (SHADER_BAYER_RCD_P4, "bayer_rcd_output"),
            (SHADER_XTRANS_P1, "xtrans_seed"),
            (SHADER_XTRANS_P2, "xtrans_markesteijn_pass1"),
            (SHADER_XTRANS_P2, "xtrans_markesteijn_pass3"),
            (SHADER_XTRANS_P3, "xtrans_markesteijn_pass2"),
            (SHADER_XTRANS_P4, "xtrans_markesteijn_derivatives"),
            (SHADER_XTRANS_P5, "xtrans_markesteijn_homogeneity"),
            (SHADER_XTRANS_P6, "xtrans_markesteijn_accumulate"),
            (SHADER_XTRANS_P7, "xtrans_demosaic_finish"),
        ] {
            let module =
                naga::front::wgsl::parse_str(source).expect("demosaic shader did not parse");
            assert!(
                module.entry_points.iter().any(|entry| entry.name == expected),
                "demosaic shader is missing entry point {expected}"
            );
        }
    }

    #[test]
    fn tone_analysis_shader_exposes_every_dispatched_entry_point() {
        let module = naga::front::wgsl::parse_str(SHADER_TONE_ANALYSIS)
            .expect("adaptive tone-analysis shader did not parse");

        for expected in [
            "tone_guide_prepare",
            "tone_guide_horizontal",
            "tone_guide_vertical",
            "tone_reduce_histogram",
        ] {
            assert!(
                module.entry_points.iter().any(|entry| entry.name == expected),
                "tone-analysis shader is missing entry point {expected}"
            );
        }
    }

    #[test]
    fn gpu_params_follow_the_wgsl_uniform_layout() {
        // Sixteen scalar values keep the stable 64-byte prefix. The two
        // darktable sigmoid vec4s follow the local-tone controls, then the
        // remaining adjustment, camera/raw, dimension and profile blocks.
        assert_eq!(std::mem::size_of::<super::GpuParams>(), 1344);
        assert_eq!(std::mem::offset_of!(super::GpuParams, basic_tone), 64);
        assert_eq!(std::mem::offset_of!(super::GpuParams, sigmoid_curve), 80);
        assert_eq!(std::mem::offset_of!(super::GpuParams, sigmoid_power), 96);
        assert_eq!(std::mem::offset_of!(super::GpuParams, creative_effects), 128);
        assert_eq!(std::mem::offset_of!(super::GpuParams, vignette), 144);
        assert_eq!(std::mem::offset_of!(super::GpuParams, vignette_options), 160);
        assert_eq!(std::mem::offset_of!(super::GpuParams, highlight_options), 176);
        assert_eq!(std::mem::offset_of!(super::GpuParams, tone_curve_0), 192);
        assert_eq!(std::mem::offset_of!(super::GpuParams, tone_curve_meta), 256);
        assert_eq!(std::mem::offset_of!(super::GpuParams, tone_curve_red_0), 272);
        assert_eq!(std::mem::offset_of!(super::GpuParams, tone_curve_green_0), 352);
        assert_eq!(std::mem::offset_of!(super::GpuParams, tone_curve_blue_0), 432);
        assert_eq!(std::mem::offset_of!(super::GpuParams, wb), 608);
        assert_eq!(std::mem::offset_of!(super::GpuParams, width), 704);
        assert_eq!(std::mem::offset_of!(super::GpuParams, tile_origin_x), 712);
        assert_eq!(std::mem::offset_of!(super::GpuParams, full_width), 720);
        assert_eq!(std::mem::offset_of!(super::GpuParams, profile_hue_sat), 736);
        assert_eq!(std::mem::offset_of!(super::GpuParams, profile_flags), 800);
        assert_eq!(std::mem::offset_of!(super::GpuParams, mask_counts), 816);
        assert_eq!(std::mem::offset_of!(super::GpuParams, mask_meta), 832);
        assert_eq!(std::mem::offset_of!(super::GpuParams, mask_adjust_0), 960);
        assert_eq!(std::mem::offset_of!(super::GpuParams, mask_adjust_1), 1088);
        assert_eq!(std::mem::offset_of!(super::GpuParams, mask_adjust_2), 1216);
    }

    #[test]
    fn adjustments_shader_contains_darktable_sigmoid_paths() {
        assert!(SHADER_ADJUSTMENTS.contains("generalized_loglogistic_sigmoid"));
        assert!(SHADER_ADJUSTMENTS.contains("preserve_hue_and_energy"));
        assert!(SHADER_ADJUSTMENTS.contains("sigmoid_rgb_ratio"));
        assert!(SHADER_ADJUSTMENTS.contains("hyperbolic_chroma"));
    }

    #[test]
    fn adjustments_shader_contains_lightroom_style_controls() {
        assert!(SHADER_ADJUSTMENTS.contains("apply_temperature_tint"));
        assert!(SHADER_ADJUSTMENTS.contains("apply_basic_contrast"));
        assert!(SHADER_ADJUSTMENTS.contains("apply_point_tone_curve"));
        assert!(SHADER_ADJUSTMENTS.contains("linear_srgb_to_oklab"));
        assert!(SHADER_ADJUSTMENTS.contains("skin_protection"));
        assert!(SHADER_ADJUSTMENTS.contains("prepare_adjustment_base"));
        assert!(SHADER_ADJUSTMENTS.contains("apply_lightroom_effects"));
        assert!(SHADER_ADJUSTMENTS.contains("apply_creative_effects"));
        assert!(SHADER_ADJUSTMENTS.contains("apply_glow"));
        assert!(SHADER_ADJUSTMENTS.contains("apply_vignette"));
        assert!(SHADER_ADJUSTMENTS.contains("stabilized_mixer_sample"));
        assert!(SHADER_ADJUSTMENTS.contains("positive_rec2020_from_oklab"));
        assert!(SHADER_ADJUSTMENTS.contains("mixer_luminance_ev"));
        assert!(!SHADER_ADJUSTMENTS.contains("rgb_to_hsl"));
        assert!(!SHADER_ADJUSTMENTS.contains("hsl_to_rgb"));
    }

    #[test]
    fn gpu_pipeline_renders_and_reads_scene_textures_when_an_adapter_exists() {
        use super::{
            CfaKind, ExposureParams, LoadedRaw, ProcessingQuality, RawGpuPipeline,
        };
        use eframe::wgpu;

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

        for cfa_kind in [CfaKind::Bayer, CfaKind::XTrans] {
            let color_indices = (0..height)
                .flat_map(|y| {
                    (0..width).map(move |x| match cfa_kind {
                        CfaKind::Bayer => match (x % 2, y % 2) {
                            (0, 0) => 0,
                            (1, 1) => 2,
                            _ => 1,
                        },
                        CfaKind::XTrans => {
                            xtrans_pattern[(y % 6) as usize][(x % 6) as usize]
                        }
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
                black_levels_per_pixel: vec![0.0; (width * height) as usize],
                white_levels: [4095.0; 4],
                camera_profile: Default::default(),
            };
            let params = super::GpuParams::new(&ExposureParams::default(), &crate::pipeline::MaskStack::default(), &raw);

            let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            let pipeline = RawGpuPipeline::new_headless_with_quality(
                &device,
                &queue,
                &raw,
                &params,
                ProcessingQuality::High,
            );
            let validation_error = pollster::block_on(validation_scope.pop());
            let pipeline = pipeline.unwrap_or_else(|error| {
                panic!("{cfa_kind:?} GPU pipeline creation failed: {error:#}")
            });
            assert!(
                validation_error.is_none(),
                "{cfa_kind:?} wgpu layout/shader validation failed: {validation_error:?}"
            );

            let regression_validation_scope =
                device.push_error_scope(wgpu::ErrorFilter::Validation);
            let regression = pipeline.render_regression_scene_blocking(&device, &queue, &params);
            let regression_validation_error =
                pollster::block_on(regression_validation_scope.pop());
            let regression = regression.unwrap_or_else(|error| {
                panic!("{cfa_kind:?} regression scene render failed: {error:#}")
            });
            assert!(
                regression_validation_error.is_none(),
                "{cfa_kind:?} regression shader validation failed: {regression_validation_error:?}"
            );
            assert_eq!(regression.len(), (width * height * 3) as usize);
            assert!(regression.iter().all(|value| value.is_finite()));

            let camera_scene = pipeline
                .read_scene_texture_blocking(&device, &queue)
                .unwrap_or_else(|error| {
                    panic!("{cfa_kind:?} scene texture readback failed: {error:#}")
                });
            assert_eq!(camera_scene.len(), (width * height * 3) as usize);
            assert!(camera_scene.iter().all(|value| value.is_finite()));
        }
    }
}
