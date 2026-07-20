use super::sigmoid::coefficients as sigmoid_coefficients;
use crate::pipeline::{
    export_mask_atlas_edge_limit, mask_atlas_edge, CfaKind, ExposureParams, IccOutputTransform,
    LoadedRaw, MaskStack, PointCurve, ProcessingStage, RawThumbnail, RenderingIntent, SigmoidParams,
    GLOBAL_TEMPERATURE_LIMIT,
    MAX_LOCAL_MASKS,
};
use anyhow::{anyhow, Result};
use bytemuck::{Pod, Zeroable};
use eframe::{egui, egui_wgpu, wgpu};
use std::borrow::Cow;
use wgpu::util::DeviceExt;

mod readback;
mod resources;

use readback::*;
use resources::*;

#[cfg(test)]
mod tests;

const GPU_PARAMS_ABI_VERSION: u32 = 1;
const GPU_PARAMS_ABI_SIZE_BYTES: u32 = 6_960;
const WORK_FORMAT_MARKER: &str = "rgba16float /* AURAW_WORK_FORMAT */";
const TONE_STATS_SIZE_BYTES: u64 = 2 * std::mem::size_of::<[f32; 4]>() as u64;
const DESKTOP_GPU_WORKING_SET_LIMIT_BYTES: u64 = 1_500 * 1024 * 1024;
const ANDROID_GPU_WORKING_SET_LIMIT_BYTES: u64 = 384 * 1024 * 1024;

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

fn expected_pass_count(cfa_kind: CfaKind) -> usize {
    let demosaic_passes = match cfa_kind {
        CfaKind::Bayer => 4,
        CfaKind::XTrans => 8,
    };
    // Highlight prepare + guided stages + two finalize variants, followed by
    // demosaic, four tone-analysis passes, and ten adjustment/output passes
    // (base, local effects, Glow extraction + five diffusion stages, creative
    // composite, and final render).
    1 + HIGHLIGHT_GUIDED_ENTRY_POINTS.len() + 2 + demosaic_passes + 4 + 10
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
    abi_version: u32,
    abi_size_bytes: u32,
    // Local half-open rectangle whose pixels contribute to a tiled export's
    // global histogram. Ordinary preview analysis uses the complete image.
    tone_histogram_bounds: [u32; 4],
    profile_hue_sat: [u32; 4],
    profile_look: [u32; 4],
    profile_tone: [u32; 4],
    output_lut: [u32; 4],
    profile_flags: [u32; 4],
    // Explicit processing-formula version. Future formula changes migrate this
    // value deliberately rather than silently changing existing edits.
    process_info: [u32; 4],
    // Local mask count followed by reserved values. Each fixed mask index maps
    // directly to one layer in the normalized R16F mask atlas.
    mask_counts: [u32; 4],
    mask_meta: [[u32; 4]; MAX_LOCAL_MASKS],
    // Exposure, contrast, highlights, shadows.
    mask_adjust_0: [[f32; 4]; MAX_LOCAL_MASKS],
    // Whites, blacks, temperature, tint.
    mask_adjust_1: [[f32; 4]; MAX_LOCAL_MASKS],
    // Saturation, texture, clarity, dehaze.
    mask_adjust_2: [[f32; 4]; MAX_LOCAL_MASKS],
    // 32-sample scene-luminance curve for each local mask.
    mask_curve_0: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_1: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_2: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_3: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_4: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_5: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_6: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_7: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_red_0: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_red_1: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_red_2: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_red_3: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_red_4: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_red_5: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_red_6: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_red_7: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_green_0: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_green_1: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_green_2: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_green_3: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_green_4: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_green_5: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_green_6: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_green_7: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_blue_0: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_blue_1: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_blue_2: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_blue_3: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_blue_4: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_blue_5: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_blue_6: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_curve_blue_7: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_hsl_hue_0: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_hsl_hue_1: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_hsl_saturation_0: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_hsl_saturation_1: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_hsl_luminance_0: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_hsl_luminance_1: [[f32; 4]; MAX_LOCAL_MASKS],
    // Perceptual scene-referred color grading. Wheels are packed as normalized
    // hue, saturation, luminance, reserved. Options are blending, balance.
    grade_shadows: [f32; 4],
    grade_midtones: [f32; 4],
    grade_highlights: [f32; 4],
    grade_global: [f32; 4],
    grade_options: [f32; 4],
    mask_grade_shadows: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_grade_midtones: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_grade_highlights: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_grade_global: [[f32; 4]; MAX_LOCAL_MASKS],
    mask_grade_options: [[f32; 4]; MAX_LOCAL_MASKS],
}

const _: () = assert!(std::mem::size_of::<GpuParams>() == GPU_PARAMS_ABI_SIZE_BYTES as usize);

fn split_eight(values: [f32; 8]) -> ([f32; 4], [f32; 4]) {
    (
        [values[0], values[1], values[2], values[3]],
        [values[4], values[5], values[6], values[7]],
    )
}

fn pack_color_grade_wheel(wheel: crate::pipeline::ColorGradeWheel) -> [f32; 4] {
    [
        wheel.hue.rem_euclid(360.0) / 360.0,
        (wheel.saturation / 100.0).clamp(0.0, 1.0),
        (wheel.luminance / 100.0).clamp(-1.0, 1.0),
        0.0,
    ]
}

fn pack_color_grade_options(grading: crate::pipeline::ColorGrading) -> [f32; 4] {
    [
        (grading.blending / 100.0).clamp(0.0, 1.0),
        (grading.balance / 100.0).clamp(-1.0, 1.0),
        0.0,
        0.0,
    ]
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
        let (camera_transform, profile_weight) = raw.adjusted_camera_transform(
            exposure
                .temperature
                .clamp(-GLOBAL_TEMPERATURE_LIMIT, GLOBAL_TEMPERATURE_LIMIT),
            exposure.tint.clamp(-100.0, 100.0),
        );
        let mut profile_layout = raw.camera_profile.gpu_layout();
        profile_layout.flags[3] = profile_weight.clamp(0.0, 1.0).to_bits();
        let sigmoid = sigmoid_coefficients(exposure.sigmoid);
        // Keep the DCP profile tone as the baseline rendition at untouched
        // Rendering defaults, but use a highlight-only display shoulder rather
        // than the previous hard [0, 1] clamp. Custom Sigmoid settings still
        // opt into the full AuRaw scene-to-display transform.
        let use_profile_base_tone = raw.camera_profile.tone_curve.is_some()
            && exposure.sigmoid == SigmoidParams::default();
        let mut mask_meta = [[0u32; 4]; MAX_LOCAL_MASKS];
        let mut mask_adjust_0 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_adjust_1 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_adjust_2 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_curves = [[[0.0f32; 4]; 8]; MAX_LOCAL_MASKS];
        let mut mask_curves_red = [[[0.0f32; 4]; 8]; MAX_LOCAL_MASKS];
        let mut mask_curves_green = [[[0.0f32; 4]; 8]; MAX_LOCAL_MASKS];
        let mut mask_curves_blue = [[[0.0f32; 4]; 8]; MAX_LOCAL_MASKS];
        let mut mask_hsl_hue_0 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_hsl_hue_1 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_hsl_saturation_0 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_hsl_saturation_1 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_hsl_luminance_0 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_hsl_luminance_1 = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_grade_shadows = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_grade_midtones = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_grade_highlights = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_grade_global = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        let mut mask_grade_options = [[0.0f32; 4]; MAX_LOCAL_MASKS];
        for (index, mask) in masks.masks.iter().take(MAX_LOCAL_MASKS).enumerate() {
            let adjustment = mask.adjustments;
            let has_hsl = adjustment
                .hsl_hue
                .iter()
                .chain(&adjustment.hsl_saturation)
                .chain(&adjustment.hsl_luminance)
                .any(|value| value.abs() > 1e-6);
            let curve_flags = u32::from(!adjustment.tone_curve.is_identity())
                | (u32::from(!adjustment.tone_curve_red.is_identity()) << 1)
                | (u32::from(!adjustment.tone_curve_green.is_identity()) << 2)
                | (u32::from(!adjustment.tone_curve_blue.is_identity()) << 3);
            let has_grading = !adjustment.color_grading.is_neutral();
            mask_meta[index] = [
                u32::from(mask.enabled),
                u32::from(!adjustment.is_neutral()),
                curve_flags,
                u32::from(has_hsl) | (u32::from(has_grading) << 1),
            ];
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
            for sample in 0..32 {
                let x = sample as f32 / 31.0;
                mask_curves[index][sample / 4][sample % 4] =
                    evaluate_point_curve(&adjustment.tone_curve, x);
                mask_curves_red[index][sample / 4][sample % 4] =
                    evaluate_point_curve(&adjustment.tone_curve_red, x);
                mask_curves_green[index][sample / 4][sample % 4] =
                    evaluate_point_curve(&adjustment.tone_curve_green, x);
                mask_curves_blue[index][sample / 4][sample % 4] =
                    evaluate_point_curve(&adjustment.tone_curve_blue, x);
            }
            let (hue_0, hue_1) = split_eight(adjustment.hsl_hue);
            let (saturation_0, saturation_1) = split_eight(adjustment.hsl_saturation);
            let (luminance_0, luminance_1) = split_eight(adjustment.hsl_luminance);
            mask_hsl_hue_0[index] = hue_0;
            mask_hsl_hue_1[index] = hue_1;
            mask_hsl_saturation_0[index] = saturation_0;
            mask_hsl_saturation_1[index] = saturation_1;
            mask_hsl_luminance_0[index] = luminance_0;
            mask_hsl_luminance_1[index] = luminance_1;
            mask_grade_shadows[index] = pack_color_grade_wheel(adjustment.color_grading.shadows);
            mask_grade_midtones[index] = pack_color_grade_wheel(adjustment.color_grading.midtones);
            mask_grade_highlights[index] =
                pack_color_grade_wheel(adjustment.color_grading.highlights);
            mask_grade_global[index] = pack_color_grade_wheel(adjustment.color_grading.global);
            mask_grade_options[index] = pack_color_grade_options(adjustment.color_grading);
        }
        let (hsl_hue_0, hsl_hue_1) = split_eight(exposure.hsl_hue);
        let (hsl_saturation_0, hsl_saturation_1) = split_eight(exposure.hsl_saturation);
        let (hsl_luminance_0, hsl_luminance_1) = split_eight(exposure.hsl_luminance);

        Self {
            black_point: exposure.black_point,
            exposure: exposure.exposure,
            temperature: exposure
                .temperature
                .clamp(-GLOBAL_TEMPERATURE_LIMIT, GLOBAL_TEMPERATURE_LIMIT),
            saturation: exposure.saturation,
            vibrance: exposure.vibrance,
            highlight_clip: exposure.highlight_clip,
            chroma_denoise: exposure.chroma_denoise,
            ca_red: exposure.ca_red,
            ca_blue: exposure.ca_blue,
            highlight_reconstruction: exposure.highlight_reconstruction,
            tone_analysis_scale: tone_analysis_scale() as f32,
            tone_guide_radius: if cfg!(target_os = "android") {
                3.0
            } else {
                5.0
            },
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
                if exposure.tone_curve.is_identity() {
                    1.0
                } else {
                    0.0
                },
                0.0,
                0.0,
            ],
            tone_curve_red_0: [
                exposure.tone_curve_red.points[0][0],
                exposure.tone_curve_red.points[0][1],
                exposure.tone_curve_red.points[1][0],
                exposure.tone_curve_red.points[1][1],
            ],
            tone_curve_red_1: [
                exposure.tone_curve_red.points[2][0],
                exposure.tone_curve_red.points[2][1],
                exposure.tone_curve_red.points[3][0],
                exposure.tone_curve_red.points[3][1],
            ],
            tone_curve_red_2: [
                exposure.tone_curve_red.points[4][0],
                exposure.tone_curve_red.points[4][1],
                exposure.tone_curve_red.points[5][0],
                exposure.tone_curve_red.points[5][1],
            ],
            tone_curve_red_3: [
                exposure.tone_curve_red.points[6][0],
                exposure.tone_curve_red.points[6][1],
                exposure.tone_curve_red.points[7][0],
                exposure.tone_curve_red.points[7][1],
            ],
            tone_curve_red_meta: [
                exposure.tone_curve_red.len.clamp(2, 8) as f32,
                if exposure.tone_curve_red.is_identity() {
                    1.0
                } else {
                    0.0
                },
                0.0,
                0.0,
            ],
            tone_curve_green_0: [
                exposure.tone_curve_green.points[0][0],
                exposure.tone_curve_green.points[0][1],
                exposure.tone_curve_green.points[1][0],
                exposure.tone_curve_green.points[1][1],
            ],
            tone_curve_green_1: [
                exposure.tone_curve_green.points[2][0],
                exposure.tone_curve_green.points[2][1],
                exposure.tone_curve_green.points[3][0],
                exposure.tone_curve_green.points[3][1],
            ],
            tone_curve_green_2: [
                exposure.tone_curve_green.points[4][0],
                exposure.tone_curve_green.points[4][1],
                exposure.tone_curve_green.points[5][0],
                exposure.tone_curve_green.points[5][1],
            ],
            tone_curve_green_3: [
                exposure.tone_curve_green.points[6][0],
                exposure.tone_curve_green.points[6][1],
                exposure.tone_curve_green.points[7][0],
                exposure.tone_curve_green.points[7][1],
            ],
            tone_curve_green_meta: [
                exposure.tone_curve_green.len.clamp(2, 8) as f32,
                if exposure.tone_curve_green.is_identity() {
                    1.0
                } else {
                    0.0
                },
                0.0,
                0.0,
            ],
            tone_curve_blue_0: [
                exposure.tone_curve_blue.points[0][0],
                exposure.tone_curve_blue.points[0][1],
                exposure.tone_curve_blue.points[1][0],
                exposure.tone_curve_blue.points[1][1],
            ],
            tone_curve_blue_1: [
                exposure.tone_curve_blue.points[2][0],
                exposure.tone_curve_blue.points[2][1],
                exposure.tone_curve_blue.points[3][0],
                exposure.tone_curve_blue.points[3][1],
            ],
            tone_curve_blue_2: [
                exposure.tone_curve_blue.points[4][0],
                exposure.tone_curve_blue.points[4][1],
                exposure.tone_curve_blue.points[5][0],
                exposure.tone_curve_blue.points[5][1],
            ],
            tone_curve_blue_3: [
                exposure.tone_curve_blue.points[6][0],
                exposure.tone_curve_blue.points[6][1],
                exposure.tone_curve_blue.points[7][0],
                exposure.tone_curve_blue.points[7][1],
            ],
            tone_curve_blue_meta: [
                exposure.tone_curve_blue.len.clamp(2, 8) as f32,
                if exposure.tone_curve_blue.is_identity() {
                    1.0
                } else {
                    0.0
                },
                0.0,
                0.0,
            ],
            hsl_hue_0,
            hsl_hue_1,
            hsl_saturation_0,
            hsl_saturation_1,
            hsl_luminance_0,
            hsl_luminance_1,
            wb: raw.wb_coeffs,
            cam_to_srgb_0: camera_transform[0],
            cam_to_srgb_1: camera_transform[1],
            cam_to_srgb_2: camera_transform[2],
            black_levels: raw.black_levels,
            white_levels: raw.white_levels,
            width: raw.width,
            height: raw.height,
            tile_origin_x,
            tile_origin_y,
            full_width,
            full_height,
            abi_version: GPU_PARAMS_ABI_VERSION,
            abi_size_bytes: GPU_PARAMS_ABI_SIZE_BYTES,
            tone_histogram_bounds: [0, 0, raw.width, raw.height],
            profile_hue_sat: profile_layout.hue_sat,
            profile_look: profile_layout.look,
            profile_tone: profile_layout.tone,
            output_lut: profile_layout.output,
            profile_flags: profile_layout.flags,
            process_info: [
                exposure.process_version,
                u32::from(use_profile_base_tone),
                0,
                0,
            ],
            mask_counts: [masks.masks.len().min(MAX_LOCAL_MASKS) as u32, 0, 0, 0],
            mask_meta,
            mask_adjust_0,
            mask_adjust_1,
            mask_adjust_2,
            mask_curve_0: mask_curves.map(|curve| curve[0]),
            mask_curve_1: mask_curves.map(|curve| curve[1]),
            mask_curve_2: mask_curves.map(|curve| curve[2]),
            mask_curve_3: mask_curves.map(|curve| curve[3]),
            mask_curve_4: mask_curves.map(|curve| curve[4]),
            mask_curve_5: mask_curves.map(|curve| curve[5]),
            mask_curve_6: mask_curves.map(|curve| curve[6]),
            mask_curve_7: mask_curves.map(|curve| curve[7]),
            mask_curve_red_0: mask_curves_red.map(|curve| curve[0]),
            mask_curve_red_1: mask_curves_red.map(|curve| curve[1]),
            mask_curve_red_2: mask_curves_red.map(|curve| curve[2]),
            mask_curve_red_3: mask_curves_red.map(|curve| curve[3]),
            mask_curve_red_4: mask_curves_red.map(|curve| curve[4]),
            mask_curve_red_5: mask_curves_red.map(|curve| curve[5]),
            mask_curve_red_6: mask_curves_red.map(|curve| curve[6]),
            mask_curve_red_7: mask_curves_red.map(|curve| curve[7]),
            mask_curve_green_0: mask_curves_green.map(|curve| curve[0]),
            mask_curve_green_1: mask_curves_green.map(|curve| curve[1]),
            mask_curve_green_2: mask_curves_green.map(|curve| curve[2]),
            mask_curve_green_3: mask_curves_green.map(|curve| curve[3]),
            mask_curve_green_4: mask_curves_green.map(|curve| curve[4]),
            mask_curve_green_5: mask_curves_green.map(|curve| curve[5]),
            mask_curve_green_6: mask_curves_green.map(|curve| curve[6]),
            mask_curve_green_7: mask_curves_green.map(|curve| curve[7]),
            mask_curve_blue_0: mask_curves_blue.map(|curve| curve[0]),
            mask_curve_blue_1: mask_curves_blue.map(|curve| curve[1]),
            mask_curve_blue_2: mask_curves_blue.map(|curve| curve[2]),
            mask_curve_blue_3: mask_curves_blue.map(|curve| curve[3]),
            mask_curve_blue_4: mask_curves_blue.map(|curve| curve[4]),
            mask_curve_blue_5: mask_curves_blue.map(|curve| curve[5]),
            mask_curve_blue_6: mask_curves_blue.map(|curve| curve[6]),
            mask_curve_blue_7: mask_curves_blue.map(|curve| curve[7]),
            mask_hsl_hue_0,
            mask_hsl_hue_1,
            mask_hsl_saturation_0,
            mask_hsl_saturation_1,
            mask_hsl_luminance_0,
            mask_hsl_luminance_1,
            grade_shadows: pack_color_grade_wheel(exposure.color_grading.shadows),
            grade_midtones: pack_color_grade_wheel(exposure.color_grading.midtones),
            grade_highlights: pack_color_grade_wheel(exposure.color_grading.highlights),
            grade_global: pack_color_grade_wheel(exposure.color_grading.global),
            grade_options: pack_color_grade_options(exposure.color_grading),
            mask_grade_shadows,
            mask_grade_midtones,
            mask_grade_highlights,
            mask_grade_global,
            mask_grade_options,
        }
    }

    pub fn with_tone_histogram_bounds(mut self, x: u32, y: u32, width: u32, height: u32) -> Self {
        self.tone_histogram_bounds = [
            x,
            y,
            x.saturating_add(width).min(self.width),
            y.saturating_add(height).min(self.height),
        ];
        self
    }

    fn needs_guided_highlight_passes(&self) -> bool {
        self.highlight_options[0] >= 1.5 && self.highlight_reconstruction > 1e-6
    }

    fn needs_intermediate_adjustment_passes(&self) -> bool {
        // Saturation and Vibrance live in apply_lightroom_effects alongside the
        // presence controls. They must therefore keep the intermediate passes
        // enabled even when Texture, Clarity, Dehaze, Glow, and Vignette are all
        // neutral. Omitting them made both global color sliders a no-op.
        let global_effects = self.saturation.abs() > 1e-6
            || self.vibrance.abs() > 1e-6
            || self.presence[..3].iter().any(|value| value.abs() > 1e-6);
        let creative = self.creative_effects[0].abs() > 1e-6 || self.vignette[0].abs() > 1e-6;
        let local_count = (self.mask_counts[0] as usize).min(MAX_LOCAL_MASKS);
        let local_effects = self.mask_adjust_2[..local_count]
            .iter()
            .any(|values| values.iter().any(|value| value.abs() > 1e-6));
        global_effects || creative || local_effects
    }

    fn needs_glow_passes(&self) -> bool {
        self.creative_effects[0].abs() > 1e-6
    }
}

fn evaluate_point_curve(curve: &PointCurve, input: f32) -> f32 {
    let count = curve.len.clamp(2, 8) as usize;
    let x = input.clamp(0.0, 1.0);
    let segment = (0..count - 1)
        .find(|index| x <= curve.points[index + 1][0])
        .unwrap_or(count - 2);
    let p0 = curve.points[segment];
    let p1 = curve.points[segment + 1];
    let width = (p1[0] - p0[0]).max(1e-5);
    let secant = |a: [f32; 2], b: [f32; 2]| (b[1] - a[1]) / (b[0] - a[0]).max(1e-5);
    let tangent = |index: usize| {
        if index == 0 {
            secant(curve.points[0], curve.points[1])
        } else if index + 1 >= count {
            secant(curve.points[count - 2], curve.points[count - 1])
        } else {
            let previous = secant(curve.points[index - 1], curve.points[index]);
            let next = secant(curve.points[index], curve.points[index + 1]);
            if previous * next <= 0.0 {
                0.0
            } else {
                2.0 * previous * next / (previous + next).abs().max(1e-6)
                    * (previous + next).signum()
            }
        }
    };
    let t = ((x - p0[0]) / width).clamp(0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    let value = (2.0 * t3 - 3.0 * t2 + 1.0) * p0[1]
        + (t3 - 2.0 * t2 + t) * tangent(segment) * width
        + (-2.0 * t3 + 3.0 * t2) * p1[1]
        + (t3 - t2) * tangent(segment + 1) * width;
    value.clamp(p0[1].min(p1[1]), p0[1].max(p1[1]))
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
    cfa_kind: CfaKind,
    processing_quality: ProcessingQuality,
    params_buffer: wgpu::Buffer,
    tone_histogram_buffer: wgpu::Buffer,
    tone_stats_buffer: wgpu::Buffer,
    raw_stage_end: usize,
    tone_prepare_pass_index: usize,
    tone_reduce_pass_index: usize,
    tone_stage_end: usize,
    highlight_guided_start: usize,
    highlight_guided_end: usize,
    highlight_finalize_guided_index: usize,
    highlight_finalize_direct_index: usize,
    demosaic_start_index: usize,
    adjustment_prepare_pass_index: usize,
    adjustment_effects_pass_index: usize,
    glow_prepare_pass_index: usize,
    glow_blur_start_index: usize,
    glow_blur_end_index: usize,
    adjustment_creative_pass_index: usize,
    adjustment_render_pass_index: usize,
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
    display_linear_texture: wgpu::Texture,
    _tone_guide_a: wgpu::Texture,
    _tone_guide_b: wgpu::Texture,
    mask_texture: wgpu::Texture,
    mask_atlas_edge: u32,
    profile_buffer: wgpu::Buffer,
    profile_buffer_size_bytes: u64,
    output_lut_offset_bytes: u64,
    out_texture: wgpu::Texture,
    _out_view: wgpu::TextureView,
}

/// A cheap, thread-safe handle to one completed display output. Reading it on
/// a worker keeps thumbnail cache refreshes off the render thread.
#[derive(Clone)]
pub struct GpuOutputSnapshot {
    texture: wgpu::Texture,
    width: u32,
    height: u32,
}

impl GpuOutputSnapshot {
    pub fn read_thumbnail_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        maximum_edge: u32,
    ) -> Result<RawThumbnail> {
        if maximum_edge == 0 {
            return Err(anyhow!("thumbnail edge must be non-zero"));
        }
        let rgba = read_rgba8_texture_region_blocking(
            device,
            queue,
            &self.texture,
            0,
            0,
            self.width,
            self.height,
            self.width,
            self.height,
            "auraw developed thumbnail readback",
        )?;
        let image = image::RgbaImage::from_raw(self.width, self.height, rgba)
            .ok_or_else(|| anyhow!("developed thumbnail readback has an invalid byte count"))?;
        let image = image::DynamicImage::ImageRgba8(image)
            .thumbnail(maximum_edge, maximum_edge)
            .to_rgba8();
        let (width, height) = image.dimensions();
        Ok(RawThumbnail {
            width,
            height,
            rgba: image.into_raw(),
        })
    }
}

impl RawGpuPipeline {
    pub fn output_snapshot(&self) -> GpuOutputSnapshot {
        GpuOutputSnapshot {
            texture: self.out_texture.clone(),
            width: self.width,
            height: self.height,
        }
    }

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
        Self::new_internal(
            device,
            queue,
            Some(renderer),
            None,
            raw,
            params,
            quality,
            None,
        )
    }

    pub fn new_headless_with_quality(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
    ) -> Result<Self> {
        Self::new_internal(device, queue, None, None, raw, params, quality, None)
    }

    /// Creates a headless pipeline with an explicit normalized-mask atlas edge.
    /// Full-quality export uses a larger atlas than the interactive preview so
    /// fine mask edges are not limited to preview resolution.
    pub fn new_headless_with_quality_and_mask_edge(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        mask_edge: u32,
    ) -> Result<Self> {
        Self::new_internal(
            device,
            queue,
            None,
            None,
            raw,
            params,
            quality,
            Some(mask_edge),
        )
    }

    /// Allocates a new set of textures and bind groups while reusing the
    /// already-compiled compute programs from another pipeline with the same
    /// CFA family and processing quality. This avoids recompiling the complete
    /// RAW pipeline whenever the zoomed preview moves to a new crop.
    pub fn new_headless_reusing_programs(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        template: &Self,
    ) -> Result<Self> {
        Self::new_internal(
            device,
            queue,
            None,
            Some(template),
            raw,
            params,
            quality,
            None,
        )
    }

    /// Reuses compiled programs while allocating a smaller local-mask atlas.
    /// This is intended for the very-low-resolution full-frame navigation proxy,
    /// where a 2048px atlas would cost more CPU/GPU work than the image itself.
    pub fn new_headless_reusing_programs_with_mask_edge(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        template: &Self,
        mask_edge: u32,
    ) -> Result<Self> {
        Self::new_internal(
            device,
            queue,
            None,
            Some(template),
            raw,
            params,
            quality,
            Some(mask_edge),
        )
    }

    fn new_internal(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: Option<&mut egui_wgpu::Renderer>,
        program_template: Option<&Self>,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        mask_atlas_edge_override: Option<u32>,
    ) -> Result<Self> {
        validate_raw(raw)?;
        validate_gpu_working_set(raw.width, raw.height, quality)?;
        if let Some(template) = program_template {
            if template.cfa_kind != raw.cfa_kind
                || template.processing_quality != quality
                || template.passes.len() != expected_pass_count(raw.cfa_kind)
            {
                return Err(anyhow!(
                    "cannot reuse GPU programs from an incompatible pipeline"
                ));
            }
        }

        let raw_texture = create_raw_texture(device, queue, raw);
        let color_texture = create_color_texture(device, queue, raw);
        let black_texture = create_black_texture(device, queue, raw);
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
        let scene_texture = create_demosaic_texture(
            device,
            size,
            demosaic_format,
            "auraw scene-linear camera RGB",
        );

        // The final creative result is tone-mapped into display-linear Rec.2020
        // before any output transfer function is applied. Export reads this
        // surface so resizing happens after demosaic/tone processing and before
        // sRGB encoding.
        let display_linear_texture =
            create_demosaic_texture(device, size, work_format, "auraw display-linear Rec.2020");

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
        let default_mask_atlas_edge = mask_atlas_edge();
        let mask_atlas_edge = mask_atlas_edge_override
            .unwrap_or(default_mask_atlas_edge)
            .clamp(64, export_mask_atlas_edge_limit());
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
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::R16Float],
        });
        let empty_masks =
            vec![0u16; mask_atlas_edge as usize * mask_atlas_edge as usize * MAX_LOCAL_MASKS];
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &mask_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&empty_masks),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(mask_atlas_edge * 2),
                rows_per_image: Some(mask_atlas_edge),
            },
            wgpu::Extent3d {
                width: mask_atlas_edge,
                height: mask_atlas_edge,
                depth_or_array_layers: MAX_LOCAL_MASKS as u32,
            },
        );

        let out_view = out_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let display_linear_view =
            display_linear_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let reconstructed_raw_view =
            reconstructed_raw_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let highlight_work_a_view =
            highlight_work_a.create_view(&wgpu::TextureViewDescriptor::default());
        let highlight_work_b_view =
            highlight_work_b.create_view(&wgpu::TextureViewDescriptor::default());
        let scene_view = scene_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let tex1_view = tex1.create_view(&wgpu::TextureViewDescriptor::default());
        let tex2_view = tex2.create_view(&wgpu::TextureViewDescriptor::default());
        let tone_guide_a_view = tone_guide_a.create_view(&wgpu::TextureViewDescriptor::default());
        let tone_guide_b_view = tone_guide_b.create_view(&wgpu::TextureViewDescriptor::default());
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
        profile_gpu_data.validate()?;
        let profile_buffer_size_bytes = u64::try_from(
            profile_gpu_data
                .words
                .len()
                .checked_mul(std::mem::size_of::<[f32; 4]>())
                .ok_or_else(|| anyhow!("GPU profile buffer size overflows"))?,
        )
        .map_err(|_| anyhow!("GPU profile buffer size does not fit in u64"))?;
        let output_lut_offset_bytes =
            u64::from(profile_gpu_data.layout.output[3]) * std::mem::size_of::<[f32; 4]>() as u64;
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
            size: TONE_STATS_SIZE_BYTES,
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

        let demosaic_start_for_programs = 1 + HIGHLIGHT_GUIDED_ENTRY_POINTS.len() + 2;
        let demosaic_pass_count = match raw.cfa_kind {
            CfaKind::Bayer => 4,
            CfaKind::XTrans => 8,
        };
        let tone_prepare_for_programs = demosaic_start_for_programs + demosaic_pass_count;
        let adjustment_prepare_for_programs = tone_prepare_for_programs + 4;
        let reused_layout = |pass_index: usize| {
            program_template.map(|template| {
                template.passes[pass_index]
                    .pipeline
                    .get_bind_group_layout(0)
            })
        };

        let bgl_highlights = reused_layout(0).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            })
        });

        let bgl1 = reused_layout(demosaic_start_for_programs).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            })
        });

        let bgl2 = reused_layout(demosaic_start_for_programs + 1).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            })
        });

        let bgl3 = reused_layout(demosaic_start_for_programs + 2).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            })
        });

        let bgl4 = (matches!(raw.cfa_kind, CfaKind::Bayer)
            .then(|| reused_layout(demosaic_start_for_programs + 3))
            .flatten())
        .unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
            })
        });

        // X-Trans Markesteijn-3 uses the two highlight work textures as
        // derivative scratch after highlight reconstruction has finalized.
        // This retains the reference eight-direction homogeneity stages without
        // allocating eight full-resolution RGB candidate images.
        let bgl_xtrans_derivatives = (matches!(raw.cfa_kind, CfaKind::XTrans)
            .then(|| reused_layout(demosaic_start_for_programs + 4))
            .flatten())
        .unwrap_or_else(|| {
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
            })
        });

        let bgl_xtrans_homogeneity = (matches!(raw.cfa_kind, CfaKind::XTrans)
            .then(|| reused_layout(demosaic_start_for_programs + 5))
            .flatten())
        .unwrap_or_else(|| {
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
            })
        });

        let bgl_xtrans_accumulate = (matches!(raw.cfa_kind, CfaKind::XTrans)
            .then(|| reused_layout(demosaic_start_for_programs + 6))
            .flatten())
        .unwrap_or_else(|| {
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
            })
        });

        let bgl_xtrans_finish = (matches!(raw.cfa_kind, CfaKind::XTrans)
            .then(|| reused_layout(demosaic_start_for_programs + 7))
            .flatten())
        .unwrap_or_else(|| {
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
            })
        });

        let bgl_tone_prepare = reused_layout(tone_prepare_for_programs).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl tone prepare"),
                entries: &[
                    buffer_entry(0),
                    texture_entry(11, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_buffer_entry(15, false),
                    storage_buffer_entry(20, true),
                    storage_texture_entry(18, tone_format, wgpu::StorageTextureAccess::WriteOnly),
                ],
            })
        });

        let bgl_tone_blur = reused_layout(tone_prepare_for_programs + 1).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl tone guide blur"),
                entries: &[
                    buffer_entry(0),
                    texture_entry(17, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(18, tone_format, wgpu::StorageTextureAccess::WriteOnly),
                ],
            })
        });

        let bgl_tone_reduce = reused_layout(tone_prepare_for_programs + 3).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl tone histogram reduction"),
                entries: &[
                    storage_buffer_entry(15, false),
                    storage_buffer_entry(16, false),
                ],
            })
        });

        let bgl_adjust_prepare =
            reused_layout(adjustment_prepare_for_programs).unwrap_or_else(|| {
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
                        texture_array_entry(
                            27,
                            wgpu::TextureSampleType::Float { filterable: true },
                        ),
                        sampler_entry(28),
                    ],
                })
            });

        let bgl_adjust_effects =
            reused_layout(adjustment_prepare_for_programs + 1).unwrap_or_else(|| {
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
                        storage_buffer_entry(16, true),
                        texture_array_entry(
                            27,
                            wgpu::TextureSampleType::Float { filterable: true },
                        ),
                        sampler_entry(28),
                    ],
                })
            });

        let bgl_glow_prepare =
            reused_layout(adjustment_prepare_for_programs + 2).unwrap_or_else(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("bgl Glow source extraction"),
                    entries: &[
                        buffer_entry(0),
                        texture_entry(24, wgpu::TextureSampleType::Float { filterable: false }),
                        storage_texture_entry(
                            31,
                            work_format,
                            wgpu::StorageTextureAccess::WriteOnly,
                        ),
                    ],
                })
            });

        let bgl_glow_blur =
            reused_layout(adjustment_prepare_for_programs + 3).unwrap_or_else(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("bgl Glow diffusion"),
                    entries: &[
                        buffer_entry(0),
                        texture_entry(30, wgpu::TextureSampleType::Float { filterable: false }),
                        storage_texture_entry(
                            31,
                            work_format,
                            wgpu::StorageTextureAccess::WriteOnly,
                        ),
                    ],
                })
            });

        let bgl_adjust_creative = reused_layout(adjustment_prepare_for_programs + 8)
            .unwrap_or_else(|| {
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
                        texture_entry(30, wgpu::TextureSampleType::Float { filterable: false }),
                    ],
                })
            });

        let bgl_adjust_render = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
                texture_array_entry(27, wgpu::TextureSampleType::Float { filterable: true }),
                sampler_entry(28),
                storage_texture_entry(29, work_format, wgpu::StorageTextureAccess::WriteOnly),
            ],
        });
        let bgl_adjust_render =
            reused_layout(adjustment_prepare_for_programs + 9).unwrap_or(bgl_adjust_render);

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
            |label: &str, read_view: &wgpu::TextureView, write_view: &wgpu::TextureView| {
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
                    binding: 16,
                    resource: tone_stats_buffer.as_entire_binding(),
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

        // Glow is extracted from the completed local-effects image in tex2.
        // Five adjacent B3-spline diffusion stages then ping-pong through tex1
        // and the display-linear surface. The latter is safe scratch here: the
        // final render overwrites it only after the creative composite.
        let bg_glow_prepare = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg Glow source extraction"),
            layout: &bgl_glow_prepare,
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
                    binding: 31,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
            ],
        });

        let make_glow_blur_bind_group =
            |label: &str, read_view: &wgpu::TextureView, write_view: &wgpu::TextureView| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &bgl_glow_blur,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: params_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 30,
                            resource: wgpu::BindingResource::TextureView(read_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 31,
                            resource: wgpu::BindingResource::TextureView(write_view),
                        },
                    ],
                })
            };
        let bg_glow_blur_0 =
            make_glow_blur_bind_group("bg Glow diffusion 0", &tex1_view, &display_linear_view);
        let bg_glow_blur_1 =
            make_glow_blur_bind_group("bg Glow diffusion 1", &display_linear_view, &tex1_view);
        let bg_glow_blur_2 =
            make_glow_blur_bind_group("bg Glow diffusion 2", &tex1_view, &display_linear_view);
        let bg_glow_blur_3 =
            make_glow_blur_bind_group("bg Glow diffusion 3", &display_linear_view, &tex1_view);
        let bg_glow_blur_4 =
            make_glow_blur_bind_group("bg Glow diffusion 4", &tex1_view, &display_linear_view);

        // The creative pass keeps the untouched local-effects result in tex2,
        // composites the final Glow diffusion from display_linear, applies the
        // post-crop vignette, and writes the complete result back into tex1.
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
                wgpu::BindGroupEntry {
                    binding: 30,
                    resource: wgpu::BindingResource::TextureView(&display_linear_view),
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
                wgpu::BindGroupEntry {
                    binding: 27,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 28,
                    resource: wgpu::BindingResource::Sampler(&mask_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 29,
                    resource: wgpu::BindingResource::TextureView(&display_linear_view),
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

        let create_shader = |label: &'static str, source: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            })
        };
        // One module per WGSL source. Entry-point pipelines below share these
        // modules instead of recompiling the same source for every pass.
        let highlight_module = program_template
            .is_none()
            .then(|| create_shader("auraw highlight module", highlight_shader.as_ref()));
        let bayer_rcd_p1_module = program_template
            .is_none()
            .then(|| create_shader("auraw Bayer RCD pass 1", bayer_rcd_p1.as_ref()));
        let bayer_rcd_p2_module = program_template
            .is_none()
            .then(|| create_shader("auraw Bayer RCD pass 2", bayer_rcd_p2.as_ref()));
        let bayer_rcd_p3_module = program_template
            .is_none()
            .then(|| create_shader("auraw Bayer RCD pass 3", bayer_rcd_p3.as_ref()));
        let bayer_rcd_p4_module = program_template
            .is_none()
            .then(|| create_shader("auraw Bayer RCD pass 4", bayer_rcd_p4.as_ref()));
        let xtrans_p1_module = program_template
            .is_none()
            .then(|| create_shader("auraw X-Trans pass 1", xtrans_p1.as_ref()));
        let xtrans_p2_module = program_template
            .is_none()
            .then(|| create_shader("auraw X-Trans pass 2", xtrans_p2.as_ref()));
        let xtrans_p3_module = program_template
            .is_none()
            .then(|| create_shader("auraw X-Trans pass 3", xtrans_p3.as_ref()));
        let xtrans_p4_module = program_template
            .is_none()
            .then(|| create_shader("auraw X-Trans pass 4", xtrans_p4.as_ref()));
        let xtrans_p5_module = program_template
            .is_none()
            .then(|| create_shader("auraw X-Trans pass 5", xtrans_p5.as_ref()));
        let xtrans_p6_module = program_template
            .is_none()
            .then(|| create_shader("auraw X-Trans pass 6", xtrans_p6.as_ref()));
        let xtrans_p7_module = program_template
            .is_none()
            .then(|| create_shader("auraw X-Trans pass 7", xtrans_p7.as_ref()));
        let tone_analysis_module = program_template
            .is_none()
            .then(|| create_shader("auraw tone analysis", SHADER_TONE_ANALYSIS));
        let adjustments_module = program_template
            .is_none()
            .then(|| create_shader("auraw adjustments", adjustments_shader.as_ref()));

        let mut next_program_index = 0usize;
        let mut make_pipeline = |shader: Option<&wgpu::ShaderModule>,
                                 entry: &str,
                                 bgl: &wgpu::BindGroupLayout|
         -> wgpu::ComputePipeline {
            let program_index = next_program_index;
            next_program_index += 1;
            if let Some(template) = program_template {
                return template.passes[program_index].pipeline.clone();
            }
            let shader = shader.expect("shader module exists without a program template");
            let pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&format!("pll_{}", entry)),
                bind_group_layouts: &[Some(bgl)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pll),
                module: shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };

        let mut passes = Vec::with_capacity(expected_pass_count(raw.cfa_kind));

        // Prepare writes the initial RGB estimate and reliability into A.
        passes.push(Pass {
            pipeline: make_pipeline(
                highlight_module.as_ref(),
                "highlight_prepare",
                &bgl_highlights,
            ),
            bind_group: make_highlight_bind_group(
                "bg highlight prepare",
                &highlight_work_b_view,
                &highlight_work_a_view,
            ),
            workgroups: image_workgroups,
        });

        // The multiscale solver ping-pongs through every declared stage.
        // Guided passes are omitted entirely for Off/LCh/zero-strength edits.
        let highlight_guided_start = passes.len();
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
                pipeline: make_pipeline(highlight_module.as_ref(), entry, &bgl_highlights),
                bind_group: make_highlight_bind_group(&label, read_view, write_view),
                workgroups: image_workgroups,
            });
        }

        let highlight_guided_end = passes.len();

        // Prepare leaves the data in A. The guided final source is derived from
        // the same parity helper used by the stage planner and covered by tests.
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
        let highlight_finalize_guided_index = passes.len();
        passes.push(Pass {
            pipeline: make_pipeline(
                highlight_module.as_ref(),
                "highlight_finalize",
                &bgl_highlights,
            ),
            bind_group: make_highlight_bind_group(
                "bg highlight finalize guided",
                final_read_view,
                final_write_view,
            ),
            workgroups: image_workgroups,
        });

        // Off, LCh, and zero-strength guided modes finalize directly from the
        // prepare texture. This avoids eleven full-frame copy dispatches.
        let highlight_finalize_direct_index = passes.len();
        passes.push(Pass {
            pipeline: make_pipeline(
                highlight_module.as_ref(),
                "highlight_finalize",
                &bgl_highlights,
            ),
            bind_group: make_highlight_bind_group(
                "bg highlight finalize direct",
                &highlight_work_a_view,
                &highlight_work_b_view,
            ),
            workgroups: image_workgroups,
        });

        let demosaic_start_index = passes.len();
        // Select the demosaic family from LibRaw's CFA classification.
        // Bayer uses the four-stage ratio-corrected reference path. Fuji
        // X-Trans seeds an RGB image, performs three green/chroma refinement
        // passes, then selects among eight homogeneity-guided candidates.
        match raw.cfa_kind {
            CfaKind::Bayer => passes.extend([
                Pass {
                    pipeline: make_pipeline(
                        bayer_rcd_p1_module.as_ref(),
                        "bayer_rcd_directional",
                        &bgl1,
                    ),
                    bind_group: bg1,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(bayer_rcd_p2_module.as_ref(), "bayer_rcd_green", &bgl2),
                    bind_group: bg2,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        bayer_rcd_p3_module.as_ref(),
                        "bayer_rcd_chroma",
                        &bgl3,
                    ),
                    bind_group: bg3,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        bayer_rcd_p4_module.as_ref(),
                        "bayer_rcd_output",
                        &bgl4,
                    ),
                    bind_group: bg4,
                    workgroups: image_workgroups,
                },
            ]),
            CfaKind::XTrans => passes.extend([
                Pass {
                    pipeline: make_pipeline(xtrans_p1_module.as_ref(), "xtrans_seed", &bgl1),
                    bind_group: bg1,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p2_module.as_ref(),
                        "xtrans_markesteijn_pass1",
                        &bgl2,
                    ),
                    bind_group: bg2.clone(),
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p3_module.as_ref(),
                        "xtrans_markesteijn_pass2",
                        &bgl3,
                    ),
                    bind_group: bg3,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p2_module.as_ref(),
                        "xtrans_markesteijn_pass3",
                        &bgl2,
                    ),
                    bind_group: bg2,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p4_module.as_ref(),
                        "xtrans_markesteijn_derivatives",
                        &bgl_xtrans_derivatives,
                    ),
                    bind_group: bg_xtrans_derivatives,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p5_module.as_ref(),
                        "xtrans_markesteijn_homogeneity",
                        &bgl_xtrans_homogeneity,
                    ),
                    bind_group: bg_xtrans_homogeneity,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p6_module.as_ref(),
                        "xtrans_markesteijn_accumulate",
                        &bgl_xtrans_accumulate,
                    ),
                    bind_group: bg_xtrans_accumulate,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_p7_module.as_ref(),
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
                    tone_analysis_module.as_ref(),
                    "tone_guide_prepare",
                    &bgl_tone_prepare,
                ),
                bind_group: bg_tone_prepare,
                workgroups: tone_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    tone_analysis_module.as_ref(),
                    "tone_guide_horizontal",
                    &bgl_tone_blur,
                ),
                bind_group: bg_tone_horizontal,
                workgroups: tone_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    tone_analysis_module.as_ref(),
                    "tone_guide_vertical",
                    &bgl_tone_blur,
                ),
                bind_group: bg_tone_vertical,
                workgroups: tone_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    tone_analysis_module.as_ref(),
                    "tone_reduce_histogram",
                    &bgl_tone_reduce,
                ),
                bind_group: bg_tone_reduce,
                workgroups: single_workgroup,
            },
        ]);

        let tone_reduce_pass_index = tone_prepare_pass_index + 3;
        let tone_stage_end = passes.len();
        let adjustment_prepare_pass_index = passes.len();
        let adjustment_effects_pass_index = adjustment_prepare_pass_index + 1;
        let glow_prepare_pass_index = adjustment_prepare_pass_index + 2;
        let glow_blur_start_index = adjustment_prepare_pass_index + 3;
        let glow_blur_end_index = glow_blur_start_index + 5;
        let adjustment_creative_pass_index = glow_blur_end_index;
        let adjustment_render_pass_index = adjustment_creative_pass_index + 1;

        passes.extend([
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "prepare_adjustment_base",
                    &bgl_adjust_prepare,
                ),
                bind_group: bg_adjust_prepare,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "apply_lightroom_effects",
                    &bgl_adjust_effects,
                ),
                bind_group: bg_adjust_effects,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "prepare_glow_source",
                    &bgl_glow_prepare,
                ),
                bind_group: bg_glow_prepare,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "diffuse_glow_0",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_0,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "diffuse_glow_1",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_1,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "diffuse_glow_2",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_2,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "diffuse_glow_3",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_3,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "diffuse_glow_4",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_4,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "apply_creative_effects",
                    &bgl_adjust_creative,
                ),
                bind_group: bg_adjust_creative,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    adjustments_module.as_ref(),
                    "apply_lightroom_adjustments",
                    &bgl_adjust_render,
                ),
                bind_group: bg_adjust_render,
                workgroups: image_workgroups,
            },
        ]);

        debug_assert_eq!(next_program_index, expected_pass_count(raw.cfa_kind));

        let egui_texture_id = renderer.map(|renderer| {
            renderer.register_native_texture(device, &out_view, wgpu::FilterMode::Linear)
        });

        let pipeline = Self {
            egui_texture_id,
            width: raw.width,
            height: raw.height,
            cfa_kind: raw.cfa_kind,
            processing_quality: quality,
            params_buffer,
            tone_histogram_buffer,
            tone_stats_buffer,
            raw_stage_end,
            tone_prepare_pass_index,
            tone_reduce_pass_index,
            tone_stage_end,
            highlight_guided_start,
            highlight_guided_end,
            highlight_finalize_guided_index,
            highlight_finalize_direct_index,
            demosaic_start_index,
            adjustment_prepare_pass_index,
            adjustment_effects_pass_index,
            glow_prepare_pass_index,
            glow_blur_start_index,
            glow_blur_end_index,
            adjustment_creative_pass_index,
            adjustment_render_pass_index,
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
            display_linear_texture,
            _tone_guide_a: tone_guide_a,
            _tone_guide_b: tone_guide_b,
            mask_texture,
            mask_atlas_edge,
            profile_buffer,
            profile_buffer_size_bytes,
            output_lut_offset_bytes,
            out_texture,
            _out_view: out_view,
        };
        Ok(pipeline)
    }

    /// Uploads one normalized, anti-aliased local-mask layer as IEEE-754 half
    /// floats. Preview and export share the same shader path, while export can
    /// allocate a larger atlas for higher spatial fidelity.
    pub fn update_mask_layer(&self, queue: &wgpu::Queue, layer: usize, values: &[u16]) -> Result<()> {
        if layer >= MAX_LOCAL_MASKS {
            return Err(anyhow!("local-mask layer {layer} is out of range"));
        }
        let expected = self.mask_atlas_edge as usize * self.mask_atlas_edge as usize;
        if values.len() != expected {
            return Err(anyhow!(
                "local-mask layer has {} samples, expected {expected}",
                values.len()
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
            bytemuck::cast_slice(values),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.mask_atlas_edge * 2),
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

        let texture_id =
            renderer.register_native_texture(device, &self._out_view, wgpu::FilterMode::Linear);
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
            return Err(anyhow!(
                "output ICC LUT edge does not match the GPU profile layout"
            ));
        }
        let bytes = bytemuck::cast_slice(transform.entries());
        let end = self
            .output_lut_offset_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| anyhow!("output ICC LUT buffer range overflows"))?;
        if end > self.profile_buffer_size_bytes {
            return Err(anyhow!(
                "output ICC LUT would write past the validated GPU profile buffer"
            ));
        }
        queue.write_buffer(&self.profile_buffer, self.output_lut_offset_bytes, bytes);
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
        self.encode_raw_stage(&mut encoder, params);
        self.encode_pass_range(
            &mut encoder,
            self.tone_prepare_pass_index,
            self.tone_stage_end,
        );
        self.encode_output_stage(&mut encoder, params);
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
            ProcessingStage::Raw => self.encode_raw_stage(&mut encoder, params),
            ProcessingStage::Tone => {
                encoder.clear_buffer(&self.tone_histogram_buffer, 0, None);
                self.encode_pass_range(
                    &mut encoder,
                    self.tone_prepare_pass_index,
                    self.tone_stage_end,
                );
            }
            ProcessingStage::Output => self.encode_output_stage(&mut encoder, params),
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

    /// Clears the reusable histogram before a full-resolution tiled analysis.
    pub fn begin_export_tone_analysis(&self, queue: &wgpu::Queue, device: &wgpu::Device) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw export tone histogram clear"),
        });
        encoder.clear_buffer(&self.tone_histogram_buffer, 0, None);
        queue.submit(Some(encoder.finish()));
    }

    /// Copies the full-frame adaptive tone anchors into a crop/detail pipeline.
    /// Spatial guides remain crop-local, but global percentiles (including the
    /// Dehaze ambient-light anchor) must not change while the user pans or
    /// zooms. Call this after the detail Tone stage and before its Output stage.
    pub fn inherit_tone_statistics(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        full_frame: &Self,
    ) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw inherit full-frame tone statistics"),
        });
        encoder.copy_buffer_to_buffer(
            &full_frame.tone_stats_buffer,
            0,
            &self.tone_stats_buffer,
            0,
            TONE_STATS_SIZE_BYTES,
        );
        queue.submit(Some(encoder.finish()));
    }

    /// Demosaics one native-resolution tile and adds only its non-halo core to
    /// the shared export histogram. The bounds in `params` prevent duplicated
    /// halo pixels from biasing the full-image percentiles.
    pub fn accumulate_export_tone_tile(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
    ) {
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(params));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw native-resolution export tone tile"),
        });
        self.encode_raw_stage(&mut encoder, params);
        self.encode_pass_range(
            &mut encoder,
            self.tone_prepare_pass_index,
            self.tone_prepare_pass_index + 1,
        );
        queue.submit(Some(encoder.finish()));
    }

    /// Reduces the histogram accumulated from every native-resolution core.
    pub fn finish_export_tone_analysis(&self, queue: &wgpu::Queue, device: &wgpu::Device) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw export tone histogram reduction"),
        });
        self.encode_pass_range(
            &mut encoder,
            self.tone_reduce_pass_index,
            self.tone_reduce_pass_index + 1,
        );
        queue.submit(Some(encoder.finish()));
    }

    /// Executes one export tile using the full-resolution histogram cached by
    /// `finish_export_tone_analysis`. The tile still builds its own halo-aware
    /// tone guide; skipping reduction keeps the global statistics unchanged.
    pub fn dispatch_export_tile(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
    ) {
        queue.write_buffer(&self.params_buffer, 0, bytemuck::bytes_of(params));
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw tiled export encoder"),
        });

        self.encode_raw_stage(&mut encoder, params);
        self.encode_pass_range(
            &mut encoder,
            self.tone_prepare_pass_index,
            self.tone_reduce_pass_index,
        );
        self.encode_output_stage(&mut encoder, params);
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

    /// Copies a post-tone-map, display-linear Rec.2020 sub-rectangle as
    /// tightly packed RGB32F. High-quality export uses this surface so any
    /// resize occurs before the output transfer function and after demosaic.
    pub fn read_display_linear_region_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<f32>> {
        if self.scene_format != wgpu::TextureFormat::Rgba32Float {
            return Err(anyhow!(
                "display-linear export readback requires ProcessingQuality::High (RGBA32Float)"
            ));
        }
        read_rgba32_texture_region_rgb_blocking(
            device,
            queue,
            &self.display_linear_texture,
            x,
            y,
            width,
            height,
            self.width,
            self.height,
            "auraw display-linear export readback",
        )
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
        let raw_view = self
            .raw_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
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
        self.encode_raw_stage(&mut encoder, params);
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

    fn encode_raw_stage(&self, encoder: &mut wgpu::CommandEncoder, params: &GpuParams) {
        self.encode_pass(encoder, 0);
        if params.needs_guided_highlight_passes() {
            self.encode_pass_range(
                encoder,
                self.highlight_guided_start,
                self.highlight_guided_end,
            );
            self.encode_pass(encoder, self.highlight_finalize_guided_index);
        } else {
            self.encode_pass(encoder, self.highlight_finalize_direct_index);
        }
        self.encode_pass_range(encoder, self.demosaic_start_index, self.raw_stage_end);
    }

    fn encode_output_stage(&self, encoder: &mut wgpu::CommandEncoder, params: &GpuParams) {
        self.encode_pass(encoder, self.adjustment_prepare_pass_index);
        if params.needs_intermediate_adjustment_passes() {
            self.encode_pass(encoder, self.adjustment_effects_pass_index);
            if params.needs_glow_passes() {
                self.encode_pass(encoder, self.glow_prepare_pass_index);
                self.encode_pass_range(
                    encoder,
                    self.glow_blur_start_index,
                    self.glow_blur_end_index,
                );
            }
            self.encode_pass(encoder, self.adjustment_creative_pass_index);
        }
        self.encode_pass(encoder, self.adjustment_render_pass_index);
    }

    fn encode_pass(&self, encoder: &mut wgpu::CommandEncoder, index: usize) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(&format!("auraw pass {}", index + 1)),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.passes[index].pipeline);
        pass.set_bind_group(0, &self.passes[index].bind_group, &[]);
        let workgroups = self.passes[index].workgroups;
        pass.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
    }

    fn encode_pass_range(&self, encoder: &mut wgpu::CommandEncoder, start: usize, end: usize) {
        for index in start..end {
            self.encode_pass(encoder, index);
        }
    }
}
