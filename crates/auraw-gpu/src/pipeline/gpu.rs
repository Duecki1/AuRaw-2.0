use super::basicadj::sigmoid_contrast_from_percent;
use super::gpu_cache::PersistentGpuPipelineCache;
use super::sigmoid::coefficients as sigmoid_coefficients;
use crate::pipeline::{
    canonical_remove_scene_to_pipeline_scene, effect_params, export_mask_atlas_edge_limit,
    mask_atlas_edge, model_srgb_to_display_linear_rec2020, pipeline_scene_to_working_rec2020,
    working_rec2020_to_canonical_remove_scene, AiDenoisedImage, CfaKind, ExposureParams,
    GeometryTransform, HighlightReconstructionMethod, IccOutputTransform, LoadedRaw, LocalMask,
    MaskEffect, MaskStack, PointCurve, ProcessingStage, RawThumbnail, RemoveEditState, RemovePatch,
    RenderingIntent, SigmoidParams, GLOBAL_TEMPERATURE_LIMIT, GLOBAL_TINT_OFFSET_LIMIT,
    MAX_LOCAL_MASKS,
};
use anyhow::{anyhow, Context, Result};
use bytemuck::{Pod, Zeroable};
use std::sync::{Arc, Condvar, Mutex};

use crate::gpu_errors::GpuErrorScopes;

mod builder;
mod readback;
mod resources;
mod shader_manager;

use builder::*;
use readback::*;
use resources::*;
use shader_manager::ShaderManager;

#[cfg(test)]
mod tests;

const GPU_PARAMS_ABI_VERSION: u32 = 5;
const MASK_EFFECT_ID_SHIFT: u32 = 8;
pub(super) const LIGHT_RAYS_MASK_ATLAS_EDGE: u32 = if cfg!(target_os = "android") {
    256
} else {
    512
};
const GPU_PARAMS_ABI_SIZE_BYTES: u32 = 1_072;
const CAMERA_UNIFORMS_SIZE_BYTES: u32 = 368;
const SCENE_TONE_UNIFORMS_SIZE_BYTES: u32 = 768;
const EFFECTS_UNIFORMS_SIZE_BYTES: u32 = 208;
const GPU_STAGE_UNIFORM_SIZE_BYTES: u32 =
    CAMERA_UNIFORMS_SIZE_BYTES + SCENE_TONE_UNIFORMS_SIZE_BYTES + EFFECTS_UNIFORMS_SIZE_BYTES;
const GPU_STAGE_UNIFORM_ALLOCATION_BYTES: u64 = 512 + 768 + 256;
const MASK_DATA_SIZE_BYTES: u64 = (std::mem::size_of::<MaskData>() * MAX_LOCAL_MASKS) as u64;
const WORK_FORMAT_MARKER: &str = "rgba16float /* AURAW_WORK_FORMAT */";
const WORKGROUP_EDGE: u32 = 8;
const TONE_STATS_SIZE_BYTES: u64 = 2 * std::mem::size_of::<[f32; 4]>() as u64;
const DESKTOP_GPU_WORKING_SET_LIMIT_BYTES: u64 = 1_500 * 1024 * 1024;
const ANDROID_GPU_WORKING_SET_LIMIT_BYTES: u64 = 384 * 1024 * 1024;

fn remove_patch_coverage(patch: &RemovePatch, x: f32, y: f32) -> u8 {
    if patch.alpha.is_empty() || patch.bounds.width == 0 || patch.bounds.height == 0 {
        return 0;
    }
    let px = x
        .round()
        .clamp(0.0, patch.bounds.width.saturating_sub(1) as f32) as usize;
    let py = y
        .round()
        .clamp(0.0, patch.bounds.height.saturating_sub(1) as f32) as usize;
    patch.alpha[py * patch.bounds.width as usize + px]
}

fn sample_remove_patch_scene(
    patch: &RemovePatch,
    x: f32,
    y: f32,
    raw: &LoadedRaw,
    exposure: &ExposureParams,
) -> [f32; 3] {
    let width = patch.bounds.width as usize;
    let height = patch.bounds.height as usize;
    if width == 0 || height == 0 {
        return [0.0; 3];
    }
    let x = x.clamp(0.0, width.saturating_sub(1) as f32);
    let y = y.clamp(0.0, height.saturating_sub(1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let decode = |index: usize| {
        if patch.rgb_scene16f.len() == width * height * 3 {
            [
                half::f16::from_bits(patch.rgb_scene16f[index * 3]).to_f32(),
                half::f16::from_bits(patch.rgb_scene16f[index * 3 + 1]).to_f32(),
                half::f16::from_bits(patch.rgb_scene16f[index * 3 + 2]).to_f32(),
            ]
        } else if patch.rgb_srgb16.len() == width * height * 3 {
            let working = model_srgb_to_display_linear_rec2020([
                patch.rgb_srgb16[index * 3] as f32 / 65_535.0,
                patch.rgb_srgb16[index * 3 + 1] as f32 / 65_535.0,
                patch.rgb_srgb16[index * 3 + 2] as f32 / 65_535.0,
            ]);
            working_rec2020_to_canonical_remove_scene(raw, exposure, working)
        } else {
            [0.0; 3]
        }
    };
    let a = decode(y0 * width + x0);
    let b = decode(y0 * width + x1);
    let c = decode(y1 * width + x0);
    let d = decode(y1 * width + x1);
    std::array::from_fn(|channel| {
        let top = a[channel] + (b[channel] - a[channel]) * tx;
        let bottom = c[channel] + (d[channel] - c[channel]) * tx;
        top + (bottom - top) * ty
    })
}

fn dispatch_for_extent(width: u32, height: u32) -> [u32; 3] {
    [
        width.div_ceil(WORKGROUP_EDGE),
        height.div_ceil(WORKGROUP_EDGE),
        1,
    ]
}

const SHADER_COMMON: &str = include_str!("../shaders/common.wgsl");
const SHADER_COLOR: &str = include_str!("../shaders/color.wgsl");
const SHADER_NOISE: &str = include_str!("../shaders/noise.wgsl");
const SHADER_RAW_SAMPLING: &str = include_str!("../shaders/raw_sampling.wgsl");
const SHADER_PROFILE: &str = include_str!("../shaders/profile.wgsl");
const SHADER_BASIC_ADJUSTMENTS: &str = include_str!("../shaders/basic_adjustments.wgsl");
const SHADER_TONE_COMMON: &str = include_str!("../shaders/tone_common.wgsl");
const SHADER_TONEMAP: &str = include_str!("../shaders/tonemap.wgsl");
const SHADER_NOISE_CA_FINISH: &str = include_str!("../shaders/noise_ca_finish.wgsl");
const SHADER_DETAIL_CAPTURE: &str = include_str!("../shaders/detail_capture.wgsl");
const SHADER_DETAIL_SCALE_SPACE: &str = include_str!("../shaders/detail_scale_space.wgsl");

const SHADER_HIGHLIGHTS: &str = include_str!("../shaders/highlights.wgsl");

const COLOR_DENOISE_ENTRY_POINTS: [&str; 6] = [
    "color_denoise_scale_1",
    "color_denoise_scale_2",
    "color_denoise_scale_4",
    "color_denoise_scale_8",
    "color_denoise_scale_16",
    "color_denoise_scale_32",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProcessingQuality {
    Preview,
    #[default]
    High,
}

fn expected_pass_count(cfa_kind: CfaKind) -> usize {
    let demosaic_passes = match cfa_kind {
        CfaKind::Bayer => 6,
        CfaKind::XTrans => 10,
    };
    1 + demosaic_passes + COLOR_DENOISE_ENTRY_POINTS.len() + 4 + 18
}

const SHADER_BAYER_RCD_P1: &str = include_str!("../shaders/pass1.wgsl");
const SHADER_BAYER_RCD_P2: &str = include_str!("../shaders/pass2.wgsl");
const SHADER_BAYER_RCD_P3: &str = include_str!("../shaders/pass3.wgsl");
const SHADER_BAYER_RCD_P4: &str = include_str!("../shaders/pass4.wgsl");
const SHADER_DUAL_DEMOSAIC: &str = include_str!("../shaders/dual_demosaic.wgsl");
const SHADER_XTRANS_DEMOSAIC: &str = include_str!("../shaders/xtrans_demosaic.wgsl");
const SHADER_XTRANS_FINISH: &str = include_str!("../shaders/xtrans_finish.wgsl");
const SHADER_COLOR_DENOISE: &str = include_str!("../shaders/color_denoise.wgsl");
const SHADER_TONE_ANALYSIS: &str = include_str!("../shaders/tone_analysis.wgsl");

const SHADER_SCENE_ADJUSTMENTS: &str = include_str!("../shaders/scene_adjustments.wgsl");
const SHADER_MASK_EFFECTS_SHARED: &str = include_str!("../shaders/mask_effects/shared.wgsl");
const SHADER_MASK_ATMOSPHERE: &str = include_str!("../shaders/mask_effects/atmosphere.wgsl");
const SHADER_MASK_BLUR: &str = include_str!("../shaders/mask_effects/blur.wgsl");
const SHADER_MASK_EDGE_GLOW: &str = include_str!("../shaders/mask_effects/edge_glow.wgsl");
const SHADER_MASK_GLOW: &str = include_str!("../shaders/mask_effects/glow.wgsl");
const SHADER_MASK_LENS_BLUR: &str = include_str!("../shaders/mask_effects/lens_blur.wgsl");
const SHADER_MASK_LIGHT_RAYS: &str = include_str!("../shaders/mask_effects/light_rays.wgsl");
const SHADER_MASK_MOTION_BLUR: &str = include_str!("../shaders/mask_effects/motion_blur.wgsl");
const SHADER_MASK_NEON: &str = include_str!("../shaders/mask_effects/neon.wgsl");
const SHADER_MASK_PIXELATE: &str = include_str!("../shaders/mask_effects/pixelate.wgsl");
const SHADER_MASK_RADIAL_BLUR: &str = include_str!("../shaders/mask_effects/radial_blur.wgsl");
const SHADER_MASK_TILT_SHIFT: &str = include_str!("../shaders/mask_effects/tilt_shift.wgsl");
const SHADER_CREATIVE_EFFECTS: &str = include_str!("../shaders/creative_effects.wgsl");
const SHADER_VIEW_TRANSFORM: &str = include_str!("../shaders/view_transform.wgsl");

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct CameraUniforms {
    black_point: f32,
    temperature: f32,
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
    _pad_0: f32,
    _pad_1: f32,
    _pad_2: f32,
    highlight_options: [f32; 4],
    noise_shot: [f32; 4],
    noise_read: [f32; 4],
    noise_options: [f32; 4],
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
    tone_histogram_bounds: [u32; 4],
    profile_hue_sat: [u32; 4],
    profile_look: [u32; 4],
    profile_tone: [u32; 4],
    output_lut: [u32; 4],
    profile_flags: [u32; 4],
    ai_denoise_enabled: u32,
    user_exposure_bits: u32,
    _pad_camera_0: u32,
    _pad_camera_1: u32,
}

const _: () = assert!(std::mem::size_of::<CameraUniforms>() == CAMERA_UNIFORMS_SIZE_BYTES as usize);

fn raster_uses_scene_view_transform(exposure: &ExposureParams) -> bool {
    const EPSILON: f32 = 1e-6;
    let default = SigmoidParams::default();
    exposure.contrast.abs() > EPSILON
        || (exposure.sigmoid.contrast - default.contrast).abs() > EPSILON
        || (exposure.sigmoid.skew - default.skew).abs() > EPSILON
        || (exposure.sigmoid.display_white_target - default.display_white_target).abs() > EPSILON
        || (exposure.sigmoid.display_black_target - default.display_black_target).abs() > EPSILON
        || (exposure.sigmoid.hue_preservation - default.hue_preservation).abs() > EPSILON
        || exposure.sigmoid.color_processing != default.color_processing
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct SceneToneUniforms {
    exposure: f32,
    saturation: f32,
    vibrance: f32,
    _pad_0: f32,
    basic_tone: [f32; 4],
    sigmoid_curve: [f32; 4],
    sigmoid_power: [f32; 4],
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
    mask_counts: [u32; 4],
    grade_shadows: [f32; 4],
    grade_midtones: [f32; 4],
    grade_highlights: [f32; 4],
    grade_global: [f32; 4],
    grade_options: [f32; 4],
    rec2020_to_xyz: [[f32; 4]; 3],
    xyz_to_rec2020: [[f32; 4]; 3],
    xyz_to_bradford: [[f32; 4]; 3],
    bradford_to_xyz: [[f32; 4]; 3],
}

const _: () =
    assert!(std::mem::size_of::<SceneToneUniforms>() == SCENE_TONE_UNIFORMS_SIZE_BYTES as usize);

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct EffectsUniforms {
    presence: [f32; 4],
    creative_effects: [f32; 4],
    vignette: [f32; 4],
    vignette_options: [f32; 4],
    vignette_frame: [f32; 4],
    vignette_transform: [f32; 4],
    vignette_dark_half_fit: [f32; 4],
    vignette_dark_full_fit: [f32; 4],
    vignette_light_half_fit: [f32; 4],
    vignette_light_full_fit: [f32; 4],
    capture_scale_sigma: [f32; 4],
    capture_thresholds: [f32; 4],
    capture_mask_coherence: [f32; 4],
}

const _: () =
    assert!(std::mem::size_of::<EffectsUniforms>() == EFFECTS_UNIFORMS_SIZE_BYTES as usize);
const _: () = assert!(GPU_STAGE_UNIFORM_SIZE_BYTES == 1_344);

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct MaskData {
    metadata: [u32; 4],
    adjust_0: [f32; 4],
    adjust_1: [f32; 4],
    adjust_2: [f32; 4],
    curves: [[f32; 4]; 8],
    grade_shadows: [f32; 4],
    grade_midtones: [f32; 4],
    grade_highlights: [f32; 4],
    grade_global: [f32; 4],
    grade_options: [f32; 4],
    curves_red: [[f32; 4]; 8],
    curves_green: [[f32; 4]; 8],
    curves_blue: [[f32; 4]; 8],
    hsl_hue_0: [f32; 4],
    hsl_hue_1: [f32; 4],
    hsl_saturation_0: [f32; 4],
    hsl_saturation_1: [f32; 4],
    hsl_luminance_0: [f32; 4],
    hsl_luminance_1: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<MaskData>() == 752);

#[derive(Clone, Debug)]
pub struct GpuParams {
    camera: CameraUniforms,
    scene_tone: SceneToneUniforms,
    effects: EffectsUniforms,
    mask_data: Box<[MaskData]>,
}

impl GpuParams {
    fn camera_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(&self.camera)
    }

    fn scene_tone_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(&self.scene_tone)
    }

    fn effects_bytes(&self) -> &[u8] {
        bytemuck::bytes_of(&self.effects)
    }

    fn mask_data_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.mask_data[..])
    }
}

fn split_eight(values: [f32; 8]) -> ([f32; 4], [f32; 4]) {
    (
        [values[0], values[1], values[2], values[3]],
        [values[4], values[5], values[6], values[7]],
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PackedPointCurve {
    pairs: [[f32; 4]; 4],
    meta: [f32; 4],
}

fn pack_point_curve(curve: &PointCurve) -> PackedPointCurve {
    let pairs = std::array::from_fn(|pair| {
        [
            curve.points[pair * 2][0],
            curve.points[pair * 2][1],
            curve.points[pair * 2 + 1][0],
            curve.points[pair * 2 + 1][1],
        ]
    });
    PackedPointCurve {
        pairs,
        meta: [
            curve.len.clamp(2, 8) as f32,
            if curve.is_identity() { 1.0 } else { 0.0 },
            0.0,
            0.0,
        ],
    }
}

fn pack_local_point_curve(curve: &PointCurve) -> [[f32; 4]; 8] {
    let curve = pack_point_curve(curve);
    let mut packed = [[0.0; 4]; 8];
    packed[..4].copy_from_slice(&curve.pairs);
    packed[4] = curve.meta;
    packed
}

fn pack_color_grade_wheel(wheel: crate::pipeline::ColorGradeWheel) -> [f32; 4] {
    [
        color_grade_hue_turns(wheel.hue),
        (wheel.saturation / 100.0).clamp(0.0, 1.0),
        (wheel.luminance / 100.0).clamp(-1.0, 1.0),
        0.0,
    ]
}

fn color_grade_hue_turns(hue_degrees: f32) -> f32 {
    let hue = hue_degrees.rem_euclid(360.0) / 60.0;
    let sector = hue.floor() as u32;
    let fraction = hue - sector as f32;
    let value = 0.9;
    let (r, g, b) = match sector % 6 {
        0 => (value, value * fraction, 0.0),
        1 => (value * (1.0 - fraction), value, 0.0),
        2 => (0.0, value, value * fraction),
        3 => (0.0, value * (1.0 - fraction), value),
        4 => (value * fraction, 0.0, value),
        _ => (value, 0.0, value * (1.0 - fraction)),
    };
    let decode = |encoded: f32| {
        if encoded <= 0.04045 {
            encoded / 12.92
        } else {
            ((encoded + 0.055) / 1.055).powf(2.4)
        }
    };
    let rgb = [decode(r), decode(g), decode(b)];
    let l = 0.412_221_46 * rgb[0] + 0.536_332_55 * rgb[1] + 0.051_445_995 * rgb[2];
    let m = 0.211_903_5 * rgb[0] + 0.680_699_5 * rgb[1] + 0.107_396_96 * rgb[2];
    let s = 0.088_302_46 * rgb[0] + 0.281_718_85 * rgb[1] + 0.629_978_7 * rgb[2];
    let l = l.cbrt();
    let m = m.cbrt();
    let s = s.cbrt();
    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let b = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    b.atan2(a).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU
}

fn shader_highlight_method(cfa_kind: CfaKind, method: HighlightReconstructionMethod) -> f32 {
    match (cfa_kind, method) {
        (CfaKind::XTrans, HighlightReconstructionMethod::Lch) => {
            HighlightReconstructionMethod::InpaintOpposed.shader_value()
        }
        (_, method) => method.shader_value(),
    }
}

fn pack_view_color_options(grading: crate::pipeline::ColorGrading, hue: f32) -> [f32; 4] {
    [
        (grading.blending / 100.0).clamp(0.0, 1.0),
        (grading.balance / 100.0).clamp(-1.0, 1.0),
        effect_params::adjustment::HUE.clamp(hue),
        0.0,
    ]
}

fn canonicalize_green_noise(mut coefficients: [f32; 4], green2_present: bool) -> [f32; 4] {
    if green2_present {
        let green = 0.5 * (coefficients[1] + coefficients[3]);
        coefficients[1] = green;
        coefficients[3] = green;
    }
    coefficients
}

#[derive(Clone, Copy)]
struct GpuTileInfo {
    origin_x: i32,
    origin_y: i32,
    full_width: u32,
    full_height: u32,
}

struct GpuParamContext<'a> {
    exposure: &'a ExposureParams,
    masks: &'a MaskStack,
    raw: &'a LoadedRaw,
    tile: GpuTileInfo,
}

fn effect_mask_data(
    effect: MaskEffect,
    active: bool,
    adjust_0: [f32; 4],
    adjust_1: [f32; 4],
    adjust_2: [f32; 4],
) -> MaskData {
    MaskData {
        metadata: [
            u32::from(active),
            u32::from(active),
            0,
            effect.shader_id() << MASK_EFFECT_ID_SHIFT,
        ],
        adjust_0,
        adjust_1,
        adjust_2,
        ..MaskData::zeroed()
    }
}

fn pack_effect_mask(mask: &LocalMask) -> Option<MaskData> {
    let zero = [0.0; 4];
    let data = match mask.effect {
        MaskEffect::Blur => {
            let effect = mask.effect_settings.blur;
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::blur::AMOUNT.clamp(effect.amount),
                    effect_params::blur::RADIUS.clamp(effect.radius),
                    0.0,
                    0.0,
                ],
                zero,
                zero,
            )
        }
        MaskEffect::LensBlur => {
            let effect = mask.effect_settings.lens_blur;
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::lens_blur::AMOUNT.clamp(effect.amount),
                    effect_params::lens_blur::RADIUS.clamp(effect.radius),
                    effect_params::lens_blur::BLADES
                        .clamp(effect.blades)
                        .round(),
                    effect_params::lens_blur::ROTATION.clamp(effect.rotation),
                ],
                [
                    effect_params::lens_blur::HIGHLIGHTS.clamp(effect.highlight_boost),
                    0.0,
                    0.0,
                    0.0,
                ],
                zero,
            )
        }
        MaskEffect::MotionBlur => {
            let effect = mask.effect_settings.motion_blur;
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::motion_blur::AMOUNT.clamp(effect.amount),
                    effect_params::motion_blur::DISTANCE.clamp(effect.distance),
                    effect_params::motion_blur::ANGLE.clamp(effect.angle),
                    0.0,
                ],
                zero,
                zero,
            )
        }
        MaskEffect::RadialBlur => {
            let effect = mask.effect_settings.radial_blur;
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::radial_blur::AMOUNT.clamp(effect.amount),
                    effect_params::radial_blur::STRENGTH.clamp(effect.strength),
                    effect_params::radial_blur::CENTER_X.clamp(effect.center[0]),
                    effect_params::radial_blur::CENTER_Y.clamp(effect.center[1]),
                ],
                [effect.mode.shader_value(), 0.0, 0.0, 0.0],
                zero,
            )
        }
        MaskEffect::TiltShift => {
            let effect = mask.effect_settings.tilt_shift;
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::tilt_shift::AMOUNT.clamp(effect.amount),
                    effect_params::tilt_shift::RADIUS.clamp(effect.radius),
                    effect_params::tilt_shift::CENTER_X.clamp(effect.center[0]),
                    effect_params::tilt_shift::CENTER_Y.clamp(effect.center[1]),
                ],
                [
                    effect_params::tilt_shift::ANGLE.clamp(effect.angle),
                    effect_params::tilt_shift::FOCUS_WIDTH.clamp(effect.focus_width),
                    effect_params::tilt_shift::FEATHER.clamp(effect.feather),
                    0.0,
                ],
                zero,
            )
        }
        MaskEffect::EdgeGlow => {
            let effect = mask.effect_settings.edge_glow;
            let color = effect_params::edge_glow::COLOR.clamp(effect.color);
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::edge_glow::AMOUNT.clamp(effect.amount),
                    effect_params::edge_glow::EDGE_WIDTH.clamp(effect.edge_width),
                    effect_params::edge_glow::DETAIL.clamp(effect.detail),
                    effect_params::edge_glow::GLOW.clamp(effect.glow),
                ],
                [color[0], color[1], color[2], 0.0],
                zero,
            )
        }
        MaskEffect::Glow => {
            let effect = mask.effect_settings.glow;
            let color = effect_params::glow::COLOR.clamp(effect.color);
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::glow::AMOUNT.clamp(effect.amount),
                    effect_params::glow::RADIUS.clamp(effect.radius),
                    effect_params::glow::CORE.clamp(effect.core),
                    0.0,
                ],
                [color[0], color[1], color[2], 0.0],
                zero,
            )
        }
        MaskEffect::Neon => {
            let effect = mask.effect_settings.neon;
            let color = effect_params::neon::COLOR.clamp(effect.color);
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::neon::AMOUNT.clamp(effect.amount),
                    effect_params::neon::EDGE_WIDTH.clamp(effect.edge_width),
                    effect_params::neon::DETAIL.clamp(effect.detail),
                    effect_params::neon::GLOW.clamp(effect.glow),
                ],
                [
                    color[0],
                    color[1],
                    color[2],
                    effect_params::neon::BACKGROUND.clamp(effect.background),
                ],
                zero,
            )
        }
        MaskEffect::LightRays => {
            let effect = mask.effect_settings.light_rays;
            let color = effect_params::light_rays::COLOR.clamp(effect.color);
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::light_rays::AMOUNT.clamp(effect.amount),
                    effect_params::light_rays::LENGTH.clamp(effect.length),
                    effect_params::light_rays::SOURCE_X.clamp(effect.source[0]),
                    effect_params::light_rays::SOURCE_Y.clamp(effect.source[1]),
                ],
                [
                    color[0],
                    color[1],
                    color[2],
                    effect_params::light_rays::FADE.clamp(effect.fade),
                ],
                [
                    effect_params::light_rays::SPREAD.clamp(effect.spread),
                    effect_params::light_rays::RAY_COUNT.clamp(effect.ray_count),
                    effect_params::light_rays::VARIATION.clamp(effect.variation),
                    effect_params::light_rays::SOFTNESS.clamp(effect.softness),
                ],
            )
        }
        MaskEffect::Pixelate => {
            let effect = mask.effect_settings.pixelate;
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::pixelate::AMOUNT.clamp(effect.amount),
                    effect_params::pixelate::BLOCK_SIZE.clamp(effect.block_size),
                    0.0,
                    0.0,
                ],
                zero,
                zero,
            )
        }
        MaskEffect::Fog => {
            let effect = mask.effect_settings.fog;
            let color = effect_params::fog::COLOR.clamp(effect.color);
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::fog::AMOUNT.clamp(effect.amount),
                    effect_params::fog::DENSITY.clamp(effect.density),
                    effect_params::fog::SCALE.clamp(effect.scale),
                    effect_params::fog::SOFTNESS.clamp(effect.softness),
                ],
                [
                    color[0],
                    color[1],
                    color[2],
                    effect_params::fog::VARIATION.clamp(effect.variation),
                ],
                [effect_params::fog::SEED.clamp(effect.seed), 0.0, 0.0, 0.0],
            )
        }
        MaskEffect::Smoke => {
            let effect = mask.effect_settings.smoke;
            let color = effect_params::smoke::COLOR.clamp(effect.color);
            effect_mask_data(
                mask.effect,
                mask.enabled && effect.is_active(),
                [
                    effect_params::smoke::AMOUNT.clamp(effect.amount),
                    effect_params::smoke::DENSITY.clamp(effect.density),
                    effect_params::smoke::SCALE.clamp(effect.scale),
                    effect_params::smoke::TURBULENCE.clamp(effect.turbulence),
                ],
                [
                    color[0],
                    color[1],
                    color[2],
                    effect_params::smoke::ANGLE.clamp(effect.angle),
                ],
                [
                    effect_params::smoke::SOFTNESS.clamp(effect.softness),
                    effect_params::smoke::SEED.clamp(effect.seed),
                    0.0,
                    0.0,
                ],
            )
        }
        MaskEffect::Adjustment => return None,
    };
    Some(data)
}

fn pack_adjustment_mask(mask: &LocalMask) -> MaskData {
    let adjustment = mask.adjustments;
    let adjustment_enabled = mask.enabled && mask.effect.uses_adjustments();
    let has_hsl = adjustment.has_color_mixer();
    let curve_flags = adjustment.curve_feature_flags();
    let has_grading = adjustment.has_color_grading();
    let has_hue = adjustment.hue.abs() > 1e-6;
    let (hsl_hue_0, hsl_hue_1) = split_eight(adjustment.hsl_hue);
    let (hsl_saturation_0, hsl_saturation_1) = split_eight(adjustment.hsl_saturation);
    let (hsl_luminance_0, hsl_luminance_1) = split_eight(adjustment.hsl_luminance);
    MaskData {
        metadata: [
            u32::from(adjustment_enabled),
            u32::from(!adjustment.is_neutral()),
            curve_flags,
            u32::from(has_hsl) | (u32::from(has_grading) << 1) | (u32::from(has_hue) << 2),
        ],
        adjust_0: [
            effect_params::adjustment::EXPOSURE.clamp(adjustment.exposure),
            effect_params::adjustment::CONTRAST.clamp(adjustment.contrast),
            effect_params::adjustment::HIGHLIGHTS.clamp(adjustment.highlights),
            effect_params::adjustment::SHADOWS.clamp(adjustment.shadows),
        ],
        adjust_1: [
            effect_params::adjustment::WHITES.clamp(adjustment.whites),
            effect_params::adjustment::BLACKS.clamp(adjustment.blacks),
            effect_params::adjustment::TEMPERATURE.clamp(adjustment.temperature),
            effect_params::adjustment::TINT.clamp(adjustment.tint),
        ],
        adjust_2: [
            effect_params::adjustment::SATURATION.clamp(adjustment.saturation),
            effect_params::adjustment::TEXTURE.clamp(adjustment.texture),
            effect_params::adjustment::CLARITY.clamp(adjustment.clarity),
            effect_params::adjustment::DEHAZE.clamp(adjustment.dehaze),
        ],
        curves: pack_local_point_curve(&adjustment.tone_curve),
        grade_shadows: pack_color_grade_wheel(adjustment.color_grading.shadows),
        grade_midtones: pack_color_grade_wheel(adjustment.color_grading.midtones),
        grade_highlights: pack_color_grade_wheel(adjustment.color_grading.highlights),
        grade_global: pack_color_grade_wheel(adjustment.color_grading.global),
        grade_options: pack_view_color_options(adjustment.color_grading, adjustment.hue),
        curves_red: pack_local_point_curve(&adjustment.tone_curve_red),
        curves_green: pack_local_point_curve(&adjustment.tone_curve_green),
        curves_blue: pack_local_point_curve(&adjustment.tone_curve_blue),
        hsl_hue_0,
        hsl_hue_1,
        hsl_saturation_0,
        hsl_saturation_1,
        hsl_luminance_0,
        hsl_luminance_1,
    }
}

fn pack_mask_params(masks: &MaskStack) -> Box<[MaskData]> {
    let mut packed = [MaskData::zeroed(); MAX_LOCAL_MASKS];
    for (destination, mask) in packed.iter_mut().zip(masks.masks.iter()) {
        *destination = pack_effect_mask(mask).unwrap_or_else(|| pack_adjustment_mask(mask));
    }
    Box::new(packed)
}

fn pack_camera_params(ctx: &GpuParamContext<'_>) -> CameraUniforms {
    let exposure = ctx.exposure;
    let raw = ctx.raw;
    let GpuTileInfo {
        origin_x: tile_origin_x,
        origin_y: tile_origin_y,
        full_width,
        full_height,
    } = ctx.tile;
    let (white_balance, camera_transform, profile_weight) = raw
        .adjusted_white_balance_and_camera_transform(
            exposure
                .temperature
                .clamp(-GLOBAL_TEMPERATURE_LIMIT, GLOBAL_TEMPERATURE_LIMIT),
            exposure
                .tint
                .clamp(-GLOBAL_TINT_OFFSET_LIMIT, GLOBAL_TINT_OFFSET_LIMIT),
        );
    let mut profile_layout = raw.camera_profile.gpu_layout();
    profile_layout.flags[3] = profile_weight.clamp(0.0, 1.0).to_bits();
    let profile_stages = profile_layout.stages();
    debug_assert_eq!(
        profile_stages.characterization.hue_sat_2,
        profile_layout.hue_sat_2
    );
    let highlight_method = if raw.is_pre_demosaiced_raster() {
        0.0
    } else {
        shader_highlight_method(raw.cfa_kind, exposure.highlight_method)
    };
    let opposed_chroma = if highlight_method >= 1.5 {
        raw.inpaint_opposed_chroma(
            exposure.black_point,
            exposure.highlight_clip,
            exposure.ai_denoise_enabled,
        )
    } else {
        [0.0; 3]
    };

    CameraUniforms {
        black_point: exposure.black_point,
        temperature: exposure
            .temperature
            .clamp(-GLOBAL_TEMPERATURE_LIMIT, GLOBAL_TEMPERATURE_LIMIT),
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
        tint: exposure
            .tint
            .clamp(-GLOBAL_TINT_OFFSET_LIMIT, GLOBAL_TINT_OFFSET_LIMIT),
        _pad_0: if raw.is_pre_demosaiced_raster() {
            1.0
        } else {
            0.0
        },
        _pad_1: if !raw.is_pre_demosaiced_raster() || raster_uses_scene_view_transform(exposure) {
            1.0
        } else {
            0.0
        },
        _pad_2: 0.0,
        highlight_options: [
            highlight_method,
            opposed_chroma[0],
            opposed_chroma[1],
            opposed_chroma[2],
        ],
        noise_shot: canonicalize_green_noise(
            std::array::from_fn(|channel| {
                raw.noise_profile.shot[channel] * white_balance[channel].max(0.0)
            }),
            raw.noise_profile.green2_present,
        ),
        noise_read: canonicalize_green_noise(
            std::array::from_fn(|channel| {
                let wb = white_balance[channel].max(0.0);
                raw.noise_profile.read[channel] * wb * wb
            }),
            raw.noise_profile.green2_present,
        ),
        noise_options: [
            (exposure.luminance_denoise / 100.0).clamp(0.0, 1.0),
            (exposure.denoise_detail / 100.0).clamp(0.0, 1.0),
            exposure.denoise_quality.shader_value(),
            raw.noise_profile.confidence.clamp(0.0, 1.0),
        ],
        wb: white_balance,
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
        profile_hue_sat: profile_stages.characterization.hue_sat,
        profile_look: profile_stages.optional_look.look_table,
        profile_tone: profile_stages.view.profile_tone,
        output_lut: profile_stages.output.output_lut,
        profile_flags: profile_layout.flags,
        ai_denoise_enabled: u32::from(exposure.ai_denoise_enabled),
        user_exposure_bits: exposure.exposure.to_bits(),
        _pad_camera_0: 0,
        _pad_camera_1: 0,
    }
}

fn pack_scene_tone_params(ctx: &GpuParamContext<'_>) -> SceneToneUniforms {
    let exposure = ctx.exposure;
    let masks = ctx.masks;
    let mut sigmoid_params = exposure.sigmoid;
    sigmoid_params.contrast = sigmoid_contrast_from_percent(exposure.contrast);
    let sigmoid = sigmoid_coefficients(sigmoid_params);
    let (hsl_hue_0, hsl_hue_1) = split_eight(exposure.hsl_hue);
    let (hsl_saturation_0, hsl_saturation_1) = split_eight(exposure.hsl_saturation);
    let (hsl_luminance_0, hsl_luminance_1) = split_eight(exposure.hsl_luminance);
    let tone_curve = pack_point_curve(&exposure.tone_curve);
    let tone_curve_red = pack_point_curve(&exposure.tone_curve_red);
    let tone_curve_green = pack_point_curve(&exposure.tone_curve_green);
    let tone_curve_blue = pack_point_curve(&exposure.tone_curve_blue);

    SceneToneUniforms {
        exposure: exposure.exposure,
        saturation: exposure.saturation,
        vibrance: exposure.vibrance,
        _pad_0: 0.0,
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
        tone_curve_0: tone_curve.pairs[0],
        tone_curve_1: tone_curve.pairs[1],
        tone_curve_2: tone_curve.pairs[2],
        tone_curve_3: tone_curve.pairs[3],
        tone_curve_meta: tone_curve.meta,
        tone_curve_red_0: tone_curve_red.pairs[0],
        tone_curve_red_1: tone_curve_red.pairs[1],
        tone_curve_red_2: tone_curve_red.pairs[2],
        tone_curve_red_3: tone_curve_red.pairs[3],
        tone_curve_red_meta: tone_curve_red.meta,
        tone_curve_green_0: tone_curve_green.pairs[0],
        tone_curve_green_1: tone_curve_green.pairs[1],
        tone_curve_green_2: tone_curve_green.pairs[2],
        tone_curve_green_3: tone_curve_green.pairs[3],
        tone_curve_green_meta: tone_curve_green.meta,
        tone_curve_blue_0: tone_curve_blue.pairs[0],
        tone_curve_blue_1: tone_curve_blue.pairs[1],
        tone_curve_blue_2: tone_curve_blue.pairs[2],
        tone_curve_blue_3: tone_curve_blue.pairs[3],
        tone_curve_blue_meta: tone_curve_blue.meta,
        hsl_hue_0,
        hsl_hue_1,
        hsl_saturation_0,
        hsl_saturation_1,
        hsl_luminance_0,
        hsl_luminance_1,
        mask_counts: [masks.masks.len().min(MAX_LOCAL_MASKS) as u32, 0, 0, 0],
        grade_shadows: pack_color_grade_wheel(exposure.color_grading.shadows),
        grade_midtones: pack_color_grade_wheel(exposure.color_grading.midtones),
        grade_highlights: pack_color_grade_wheel(exposure.color_grading.highlights),
        grade_global: pack_color_grade_wheel(exposure.color_grading.global),
        grade_options: pack_view_color_options(exposure.color_grading, exposure.hue),
        rec2020_to_xyz: [
            [0.636_958, 0.262_700_2, 0.0, 0.0],
            [0.144_616_9, 0.677_998_1, 0.028_072_7, 0.0],
            [0.168_880_9, 0.059_301_7, 1.060_985_1, 0.0],
        ],
        xyz_to_rec2020: [
            [1.716_651_2, -0.666_684_4, 0.017_639_9, 0.0],
            [-0.355_670_8, 1.616_481_2, -0.042_770_6, 0.0],
            [-0.253_366_3, 0.015_768_5, 0.942_103_1, 0.0],
        ],
        xyz_to_bradford: [
            [0.8951, -0.7502, 0.0389, 0.0],
            [0.2664, 1.7135, -0.0685, 0.0],
            [-0.1614, 0.0367, 1.0296, 0.0],
        ],
        bradford_to_xyz: [
            [0.986_992_9, 0.432_305_3, -0.008_528_7, 0.0],
            [-0.147_054_3, 0.518_360_3, 0.040_042_8, 0.0],
            [0.159_962_7, 0.049_291_2, 0.968_486_7, 0.0],
        ],
    }
}

fn pack_effect_params(ctx: &GpuParamContext<'_>, mask_data: &[MaskData]) -> EffectsUniforms {
    let exposure = ctx.exposure;
    let GpuTileInfo {
        full_width,
        full_height,
        ..
    } = ctx.tile;
    let local_glow_radius = mask_data
        .iter()
        .filter(|mask| {
            mask.metadata[0] != 0
                && mask.metadata[3] >> MASK_EFFECT_ID_SHIFT == MaskEffect::Glow.shader_id()
        })
        .map(|mask| mask.adjust_0[1])
        .fold(0.0_f32, f32::max);
    let global_glow_radius = if exposure.glow_amount.abs() > 1e-6 {
        exposure.glow_radius.clamp(0.0, 100.0)
    } else {
        0.0
    };
    EffectsUniforms {
        presence: [exposure.texture, exposure.clarity, exposure.dehaze, 0.0],
        creative_effects: [
            exposure.glow_amount.clamp(0.0, 100.0),
            global_glow_radius.max(local_glow_radius),
            exposure.glow_threshold.clamp(0.0, 100.0),
            exposure.sharpen_amount.clamp(0.0, 150.0),
        ],
        vignette: [
            exposure.vignette_amount.clamp(-100.0, 100.0),
            exposure.vignette_midpoint.clamp(0.0, 100.0),
            exposure.vignette_roundness.clamp(-100.0, 100.0),
            exposure.vignette_feather.clamp(0.0, 100.0),
        ],
        vignette_options: [
            exposure.vignette_highlights.clamp(0.0, 100.0),
            exposure.sharpen_radius.clamp(0.5, 3.0),
            exposure.sharpen_detail.clamp(0.0, 100.0),
            exposure.sharpen_masking.clamp(0.0, 100.0),
        ],
        vignette_frame: [
            0.5,
            0.5,
            full_width.max(1) as f32,
            full_height.max(1) as f32,
        ],
        vignette_transform: [1.0, 0.0, 0.0, 1.0],
        vignette_dark_half_fit: [0.10, 1.235, 2.88, 0.86],
        vignette_dark_full_fit: [0.02, 1.135, 3.46, 1.0],
        vignette_light_half_fit: [0.305, 1.24, 4.36, 0.90],
        vignette_light_full_fit: [0.13, 1.075, 5.66, 1.0],
        capture_scale_sigma: [0.74, 1.75, 0.58, 1.65],
        capture_thresholds: [0.015, 0.0045, 0.055, 0.28],
        capture_mask_coherence: [0.035, 0.62, 0.055, 0.22],
    }
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
        let context = GpuParamContext {
            exposure,
            masks,
            raw,
            tile: GpuTileInfo {
                origin_x: tile_origin_x,
                origin_y: tile_origin_y,
                full_width,
                full_height,
            },
        };
        let mask_data = pack_mask_params(masks);
        Self {
            camera: pack_camera_params(&context),
            scene_tone: pack_scene_tone_params(&context),
            effects: pack_effect_params(&context, &mask_data),
            mask_data,
        }
    }

    pub fn with_vignette_geometry(mut self, geometry: GeometryTransform) -> Self {
        let geometry = geometry.sanitized();
        let crop = geometry.crop;
        let source_width = self.camera.full_width.max(1) as f32;
        let source_height = self.camera.full_height.max(1) as f32;
        let crop_width = ((crop[2] - crop[0]) * source_width).max(1e-6);
        let crop_height = ((crop[3] - crop[1]) * source_height).max(1e-6);
        let center_u = (crop[0] + crop[2]) * 0.5;
        let center_v = (crop[1] + crop[3]) * 0.5;

        let fx = if geometry.flip_horizontal { -1.0 } else { 1.0 };
        let fy = if geometry.flip_vertical { -1.0 } else { 1.0 };
        let shx = geometry.horizontal_transform.to_radians().tan();
        let shy = geometry.vertical_transform.to_radians().tan();
        let angle = geometry.rotation_degrees.to_radians();
        let cos = angle.cos();
        let sin = angle.sin();
        let affine = [
            cos * fx - sin * shy * fx,
            cos * shx * fy - sin * fy,
            sin * fx + cos * shy * fx,
            sin * shx * fy + cos * fy,
        ];
        let quarter = match geometry.quarter_turns % 4 {
            0 => [1.0, 0.0, 0.0, 1.0],
            1 => [0.0, -1.0, 1.0, 0.0],
            2 => [-1.0, 0.0, 0.0, -1.0],
            _ => [0.0, 1.0, -1.0, 0.0],
        };
        let forward = [
            quarter[0] * affine[0] + quarter[1] * affine[2],
            quarter[0] * affine[1] + quarter[1] * affine[3],
            quarter[2] * affine[0] + quarter[3] * affine[2],
            quarter[2] * affine[1] + quarter[3] * affine[3],
        ];
        let (output_width, output_height) = if geometry.quarter_turns.is_multiple_of(2) {
            (crop_width, crop_height)
        } else {
            (crop_height, crop_width)
        };

        self.effects.vignette_frame = [center_u, center_v, output_width, output_height];
        self.effects.vignette_transform = [
            forward[0] * source_width / output_width,
            forward[1] * source_height / output_width,
            forward[2] * source_width / output_height,
            forward[3] * source_height / output_height,
        ];
        self
    }

    pub fn with_tone_histogram_bounds(mut self, x: u32, y: u32, width: u32, height: u32) -> Self {
        self.camera.tone_histogram_bounds = [
            x,
            y,
            x.saturating_add(width).min(self.camera.width),
            y.saturating_add(height).min(self.camera.height),
        ];
        self
    }

    pub fn with_mask_uv_rect(mut self, rect: [f32; 4]) -> Self {
        self.set_mask_uv_rect(rect);
        self.scene_tone.mask_counts[3] = u32::MAX;
        self
    }

    pub fn with_mask_uv_rect_and_extent(
        mut self,
        rect: [f32; 4],
        texture_extent: [u32; 2],
    ) -> Self {
        self.set_mask_uv_rect(rect);
        let width = texture_extent[0].clamp(1, u16::MAX as u32);
        let height = texture_extent[1].clamp(1, u16::MAX as u32);
        self.scene_tone.mask_counts[3] = width | (height << 16);
        self
    }

    fn set_mask_uv_rect(&mut self, rect: [f32; 4]) {
        let pack = |u: f32, v: f32| {
            let u = (u.clamp(0.0, 1.0) * 65_535.0).round() as u32;
            let v = (v.clamp(0.0, 1.0) * 65_535.0).round() as u32;
            u | (v << 16)
        };
        let min_u = rect[0].min(rect[2]);
        let min_v = rect[1].min(rect[3]);
        let max_u = rect[0].max(rect[2]);
        let max_v = rect[1].max(rect[3]);
        self.scene_tone.mask_counts[1] = pack(min_u, min_v);
        self.scene_tone.mask_counts[2] = pack(max_u, max_v);
    }

    fn uses_ai_denoise(&self) -> bool {
        self.camera.ai_denoise_enabled != 0
    }

    fn needs_dual_demosaic_passes(&self) -> bool {
        self.camera.demosaic_mode >= 1.5
    }

    fn needs_intermediate_adjustment_passes(&self) -> bool {
        let global_effects = self.scene_tone.saturation.abs() > 1e-6
            || self.scene_tone.vibrance.abs() > 1e-6
            || self.effects.presence[..3]
                .iter()
                .any(|value| value.abs() > 1e-6);
        let creative = self.effects.creative_effects[0].abs() > 1e-6;
        let local_count = (self.scene_tone.mask_counts[0] as usize).min(MAX_LOCAL_MASKS);
        let local_effects = (0..local_count).any(|index| {
            let local = self.mask_data[index];
            let state = local.metadata;
            if state[0] == 0 || state[1] == 0 {
                return false;
            }
            let effect_id = state[3] >> MASK_EFFECT_ID_SHIFT;
            if effect_id == MaskEffect::Neon.shader_id()
                || effect_id == MaskEffect::Glow.shader_id()
                || effect_id == MaskEffect::LightRays.shader_id()
                || effect_id == MaskEffect::Blur.shader_id()
                || effect_id == MaskEffect::LensBlur.shader_id()
                || effect_id == MaskEffect::MotionBlur.shader_id()
                || effect_id == MaskEffect::RadialBlur.shader_id()
                || effect_id == MaskEffect::TiltShift.shader_id()
                || effect_id == MaskEffect::EdgeGlow.shader_id()
                || effect_id == MaskEffect::Pixelate.shader_id()
                || effect_id == MaskEffect::Fog.shader_id()
                || effect_id == MaskEffect::Smoke.shader_id()
            {
                return true;
            }

            let tone = local.adjust_0[1..].iter().any(|value| value.abs() > 1e-6);
            let white_balance = local.adjust_1[0].abs() > 1e-6
                || local.adjust_1[2].abs() > 1e-6
                || local.adjust_1[3].abs() > 1e-6;
            let curves = state[2] != 0;
            let presence_or_saturation = local.adjust_2.iter().any(|value| value.abs() > 1e-6);
            tone || white_balance || curves || presence_or_saturation
        });
        global_effects || creative || local_effects
    }

    fn needs_glow_passes(&self) -> bool {
        if self.effects.creative_effects[0].abs() > 1e-6 {
            return true;
        }
        let local_count = (self.scene_tone.mask_counts[0] as usize).min(MAX_LOCAL_MASKS);
        self.mask_data[..local_count].iter().any(|mask| {
            mask.metadata[0] != 0
                && mask.metadata[3] >> MASK_EFFECT_ID_SHIFT == MaskEffect::Glow.shader_id()
        })
    }

    fn needs_blur_passes(&self) -> bool {
        let local_count = (self.scene_tone.mask_counts[0] as usize).min(MAX_LOCAL_MASKS);
        self.mask_data[..local_count].iter().any(|mask| {
            if mask.metadata[0] == 0 {
                return false;
            }
            matches!(
                mask.metadata[3] >> MASK_EFFECT_ID_SHIFT,
                id if id == MaskEffect::Blur.shader_id()
                    || id == MaskEffect::LensBlur.shader_id()
                    || id == MaskEffect::MotionBlur.shader_id()
                    || id == MaskEffect::RadialBlur.shader_id()
                    || id == MaskEffect::TiltShift.shader_id()
            )
        })
    }

    fn needs_progressive_blur_passes(&self) -> bool {
        let local_count = (self.scene_tone.mask_counts[0] as usize).min(MAX_LOCAL_MASKS);
        self.mask_data[..local_count].iter().any(|mask| {
            mask.metadata[0] != 0
                && mask.metadata[3] >> MASK_EFFECT_ID_SHIFT == MaskEffect::Blur.shader_id()
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct UploadedStageUniforms {
    camera: CameraUniforms,
    scene_tone: SceneToneUniforms,
    effects: EffectsUniforms,
}

struct Pass {
    pipeline: wgpu::ComputePipeline,
    bind_group: wgpu::BindGroup,
    workgroups: [u32; 3],
}

#[derive(Clone, Copy, Debug, Default)]
struct RawGpuPipelineConfig {
    mask_atlas_edge_override: Option<u32>,
}

#[derive(Clone)]
pub struct RawGpuProgramTemplate {
    cfa_kind: CfaKind,
    processing_quality: ProcessingQuality,
    pipelines: Vec<wgpu::ComputePipeline>,
    pipeline_cache: Option<Arc<PersistentGpuPipelineCache>>,
}

#[derive(Default)]
pub struct GpuProgramPrewarm {
    result: Mutex<Option<std::result::Result<Arc<RawGpuProgramTemplate>, String>>>,
    ready: Condvar,
}

impl GpuProgramPrewarm {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish(&self, result: std::result::Result<RawGpuProgramTemplate, String>) {
        let Ok(mut slot) = self.result.lock() else {
            return;
        };
        if slot.is_none() {
            *slot = Some(result.map(Arc::new));
            self.ready.notify_all();
        }
    }

    pub fn wait(&self) -> std::result::Result<Arc<RawGpuProgramTemplate>, String> {
        let mut slot = self
            .result
            .lock()
            .map_err(|_| "GPU export prewarm state was poisoned".to_owned())?;
        while slot.is_none() {
            slot = self
                .ready
                .wait(slot)
                .map_err(|_| "GPU export prewarm state was poisoned".to_owned())?;
        }
        slot.as_ref()
            .expect("GPU export prewarm result is present")
            .clone()
    }
}

pub struct ToneStatisticsSnapshot {
    buffer: wgpu::Buffer,
}

pub struct RawGpuPipeline {
    pub egui_texture_id: Option<egui::TextureId>,
    pub width: u32,
    pub height: u32,
    cfa_kind: CfaKind,
    processing_quality: ProcessingQuality,
    camera_uniforms_buffer: wgpu::Buffer,
    scene_tone_uniforms_buffer: wgpu::Buffer,
    effects_uniforms_buffer: wgpu::Buffer,
    scene_tone_bind_group: wgpu::BindGroup,
    effects_bind_group: wgpu::BindGroup,
    uploaded_stage_uniforms: Mutex<UploadedStageUniforms>,
    mask_data_buffer: wgpu::Buffer,
    tone_histogram_buffer: wgpu::Buffer,
    tone_stats_buffer: wgpu::Buffer,
    tone_prepare_pass_index: usize,
    tone_reduce_pass_index: usize,
    tone_stage_end: usize,
    demosaic_start_index: usize,
    demosaic_dual_start_index: usize,
    demosaic_dual_end_index: usize,
    demosaic_finish_index: usize,
    color_denoise_start_index: usize,
    color_denoise_end_index: usize,
    adjustment_prepare_pass_index: usize,
    adjustment_tone_pass_index: usize,
    adjustment_effects_pass_index: usize,
    mask_blur_start_index: usize,
    mask_blur_end_index: usize,
    glow_prepare_pass_index: usize,
    glow_blur_start_index: usize,
    glow_blur_end_index: usize,
    adjustment_creative_pass_index: usize,
    adjustment_render_pass_index: usize,
    post_blur_glow_passes: Vec<Pass>,
    post_blur_creative_pass: Pass,
    post_blur_render_pass: Pass,
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
    has_ai_scene: bool,
    has_raster_scene: bool,
    has_ai_cfa: bool,
    display_linear_texture: wgpu::Texture,
    _tone_guide_a: wgpu::Texture,
    _tone_guide_b: wgpu::Texture,
    mask_texture: wgpu::Texture,
    light_rays_mask_texture: wgpu::Texture,
    mask_layer_capacity: usize,
    mask_atlas_edge: u32,
    profile_buffer: wgpu::Buffer,
    profile_buffer_size_bytes: u64,
    output_lut_offset_bytes: u64,
    out_texture: wgpu::Texture,
    _out_view: wgpu::TextureView,
    pipeline_cache: Option<Arc<PersistentGpuPipelineCache>>,
    _gpu_budget_reservation: GpuBudgetReservation,
}

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
            TextureReadbackRegion::full(
                self.width,
                self.height,
                "auraw developed thumbnail readback",
            ),
        )?;
        let image = image::RgbaImage::from_raw(self.width, self.height, rgba)
            .ok_or_else(|| anyhow!("developed thumbnail readback has an invalid byte count"))?;
        let image = crate::thumbnail_cache::downscale_to_fit(
            image::DynamicImage::ImageRgba8(image),
            maximum_edge,
        )
        .to_rgba8();
        let (width, height) = image.dimensions();
        Ok(RawThumbnail {
            width,
            height,
            rgba: image.into_raw(),
        })
    }
}

struct RawGpuPipelineBuild<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    renderer: Option<&'a mut egui_wgpu::Renderer>,
    program_template: Option<&'a RawGpuProgramTemplate>,
    pipeline_cache: Option<Arc<PersistentGpuPipelineCache>>,
    raw: &'a LoadedRaw,
    params: &'a GpuParams,
    quality: ProcessingQuality,
    config: RawGpuPipelineConfig,
}

impl RawGpuPipeline {
    fn upload_params(&self, queue: &wgpu::Queue, params: &GpuParams) {
        match self.uploaded_stage_uniforms.lock() {
            Ok(mut uploaded) => {
                if bytemuck::bytes_of(&uploaded.camera) != params.camera_bytes() {
                    queue.write_buffer(&self.camera_uniforms_buffer, 0, params.camera_bytes());
                    uploaded.camera = params.camera;
                }
                if bytemuck::bytes_of(&uploaded.scene_tone) != params.scene_tone_bytes() {
                    queue.write_buffer(
                        &self.scene_tone_uniforms_buffer,
                        0,
                        params.scene_tone_bytes(),
                    );
                    uploaded.scene_tone = params.scene_tone;
                }
                if bytemuck::bytes_of(&uploaded.effects) != params.effects_bytes() {
                    queue.write_buffer(&self.effects_uniforms_buffer, 0, params.effects_bytes());
                    uploaded.effects = params.effects;
                }
            }
            Err(_) => {
                queue.write_buffer(&self.camera_uniforms_buffer, 0, params.camera_bytes());
                queue.write_buffer(
                    &self.scene_tone_uniforms_buffer,
                    0,
                    params.scene_tone_bytes(),
                );
                queue.write_buffer(&self.effects_uniforms_buffer, 0, params.effects_bytes());
            }
        }
        queue.write_buffer(&self.mask_data_buffer, 0, params.mask_data_bytes());
    }

    pub fn program_template(&self) -> RawGpuProgramTemplate {
        RawGpuProgramTemplate {
            cfa_kind: self.cfa_kind,
            processing_quality: self.processing_quality,
            pipelines: self
                .passes
                .iter()
                .map(|pass| pass.pipeline.clone())
                .collect(),
            pipeline_cache: self.pipeline_cache.clone(),
        }
    }

    fn into_program_template(self) -> RawGpuProgramTemplate {
        RawGpuProgramTemplate {
            cfa_kind: self.cfa_kind,
            processing_quality: self.processing_quality,
            pipelines: self.passes.into_iter().map(|pass| pass.pipeline).collect(),
            pipeline_cache: self.pipeline_cache,
        }
    }

    pub fn prewarm_preview_template(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cfa_kind: CfaKind,
    ) -> Result<Self> {
        Self::prewarm_preview_template_with_cache(device, queue, cfa_kind, None)
    }

    pub fn prewarm_preview_template_with_cache(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cfa_kind: CfaKind,
        pipeline_cache: Option<Arc<PersistentGpuPipelineCache>>,
    ) -> Result<Self> {
        Self::prewarm_template_with_quality_and_cache(
            device,
            queue,
            cfa_kind,
            ProcessingQuality::Preview,
            pipeline_cache,
            None,
        )
    }

    pub fn prewarm_export_program_template_with_cache(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cfa_kind: CfaKind,
        pipeline_cache: Option<Arc<PersistentGpuPipelineCache>>,
    ) -> Result<RawGpuProgramTemplate> {
        Self::prewarm_template_with_quality_and_cache(
            device,
            queue,
            cfa_kind,
            ProcessingQuality::High,
            pipeline_cache,
            Some(64),
        )
        .map(Self::into_program_template)
    }

    fn prewarm_template_with_quality_and_cache(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cfa_kind: CfaKind,
        quality: ProcessingQuality,
        pipeline_cache: Option<Arc<PersistentGpuPipelineCache>>,
        mask_atlas_edge_override: Option<u32>,
    ) -> Result<Self> {
        const EDGE: u32 = 16;
        let pixels = (EDGE * EDGE) as usize;
        let cfa_pattern = match cfa_kind {
            CfaKind::Bayer => vec![0u8, 1, 1, 2],
            CfaKind::XTrans => vec![
                0u8, 1, 0, 0, 1, 0, 1, 2, 1, 2, 1, 2, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 1, 2, 1,
                2, 1, 2, 0, 1, 0, 0, 1, 0,
            ],
        };
        let cfa_period = match cfa_kind {
            CfaKind::Bayer => (2, 2),
            CfaKind::XTrans => (6, 6),
        };
        let raw = LoadedRaw {
            width: EDGE,
            height: EDGE,
            camera_make: "AuRaw".to_owned(),
            camera_model: "GPU prewarm".to_owned(),
            lens_make: String::new(),
            lens_model: String::new(),
            focal_length: 0.0,
            aperture: 0.0,
            focus_distance: 0.0,
            capture_metadata: Default::default(),
            cfa_kind,
            raw_pixels: vec![0u16; pixels],
            scene_linear_raster: None,
            color_indices: crate::pipeline::CompactPixelMap::repeating(
                EDGE,
                EDGE,
                cfa_period.0,
                cfa_period.1,
                cfa_pattern,
            ),
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            black_levels: [0.0; 4],
            black_levels_per_pixel: crate::pipeline::CompactPixelMap::repeating(
                EDGE,
                EDGE,
                1,
                1,
                vec![0.0f32],
            ),
            white_levels: [65535.0; 4],
            noise_profile: crate::pipeline::NoiseProfile::default(),
            camera_profile: crate::pipeline::CameraProfile::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
            lens_geometry: None,
            ai_denoised: Arc::new(std::sync::RwLock::new(None)),
            opposed_chroma_cache: Default::default(),
        };
        let exposure = ExposureParams::scene_referred_default();
        let masks = MaskStack::default();
        let params = GpuParams::new(&exposure, &masks, &raw);
        Self::new_internal(RawGpuPipelineBuild {
            device,
            queue,
            renderer: None,
            program_template: None,
            pipeline_cache,
            raw: &raw,
            params: &params,
            quality,
            config: RawGpuPipelineConfig {
                mask_atlas_edge_override,
            },
        })
    }

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
        Self::new_internal(RawGpuPipelineBuild {
            device,
            queue,
            renderer: Some(renderer),
            program_template: None,
            pipeline_cache: None,
            raw,
            params,
            quality,
            config: RawGpuPipelineConfig::default(),
        })
    }

    pub fn new_headless_with_quality(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
    ) -> Result<Self> {
        Self::new_internal(RawGpuPipelineBuild {
            device,
            queue,
            renderer: None,
            program_template: None,
            pipeline_cache: None,
            raw,
            params,
            quality,
            config: RawGpuPipelineConfig::default(),
        })
    }

    pub fn new_headless_with_quality_and_mask_edge(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        mask_edge: u32,
    ) -> Result<Self> {
        Self::new_internal(RawGpuPipelineBuild {
            device,
            queue,
            renderer: None,
            program_template: None,
            pipeline_cache: None,
            raw,
            params,
            quality,
            config: RawGpuPipelineConfig {
                mask_atlas_edge_override: Some(mask_edge),
            },
        })
    }

    pub fn new_headless_reusing_programs(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        template: &Self,
    ) -> Result<Self> {
        let program_template = template.program_template();
        Self::new_internal(RawGpuPipelineBuild {
            device,
            queue,
            renderer: None,
            program_template: Some(&program_template),
            pipeline_cache: program_template.pipeline_cache.clone(),
            raw,
            params,
            quality,
            config: RawGpuPipelineConfig::default(),
        })
    }

    pub fn new_headless_reusing_programs_with_mask_edge(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        template: &Self,
        mask_edge: u32,
    ) -> Result<Self> {
        let program_template = template.program_template();
        Self::new_internal(RawGpuPipelineBuild {
            device,
            queue,
            renderer: None,
            program_template: Some(&program_template),
            pipeline_cache: program_template.pipeline_cache.clone(),
            raw,
            params,
            quality,
            config: RawGpuPipelineConfig {
                mask_atlas_edge_override: Some(mask_edge),
            },
        })
    }

    pub fn new_headless_reusing_program_template_with_mask_edge(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        template: &RawGpuProgramTemplate,
        mask_edge: u32,
    ) -> Result<Self> {
        Self::new_internal(RawGpuPipelineBuild {
            device,
            queue,
            renderer: None,
            program_template: Some(template),
            pipeline_cache: template.pipeline_cache.clone(),
            raw,
            params,
            quality,
            config: RawGpuPipelineConfig {
                mask_atlas_edge_override: Some(mask_edge),
            },
        })
    }

    pub fn new_headless_reusing_program_template(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        template: &RawGpuProgramTemplate,
    ) -> Result<Self> {
        Self::new_internal(RawGpuPipelineBuild {
            device,
            queue,
            renderer: None,
            program_template: Some(template),
            pipeline_cache: template.pipeline_cache.clone(),
            raw,
            params,
            quality,
            config: RawGpuPipelineConfig::default(),
        })
    }

    fn new_internal(build: RawGpuPipelineBuild<'_>) -> Result<Self> {
        let RawGpuPipelineBuild {
            device,
            queue,
            renderer,
            program_template,
            pipeline_cache,
            raw,
            params,
            quality,
            config,
        } = build;
        let mut renderer = renderer;
        validate_raw(raw)?;
        if let Some(template) = program_template {
            if template.cfa_kind != raw.cfa_kind
                || template.processing_quality != quality
                || template.pipelines.len() != expected_pass_count(raw.cfa_kind)
            {
                return Err(anyhow!(
                    "cannot reuse GPU programs from an incompatible pipeline"
                ));
            }
        }

        let geometry = compute_derived_geometry(raw, params, quality, config);

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

        let resource_plan = build_gpu_resource_plan(GpuResourcePlanInput {
            width: raw.width,
            height: raw.height,
            quality,
            tone_scale: tone_analysis_scale(),
            mask_atlas_edge: geometry.mask_atlas_edge,
            mask_layers: u32::try_from(geometry.mask_layer_capacity)
                .map_err(|_| anyhow!("mask layer capacity does not fit in u32"))?,
            profile_buffer_bytes: profile_buffer_size_bytes,
            stage_uniform_buffer_bytes: GPU_STAGE_UNIFORM_ALLOCATION_BYTES,
            mask_data_buffer_bytes: MASK_DATA_SIZE_BYTES,
        })?;
        let gpu_budget_reservation =
            GpuBudgetReservation::acquire(&resource_plan, gpu_working_set_limit_bytes())?;

        let gpu_error_scopes = GpuErrorScopes::push(device);

        let ai_image = raw.ai_denoised_image();
        let ai_cfa = params
            .uses_ai_denoise()
            .then(|| ai_image.as_ref().and_then(AiDenoisedImage::bayer_cfa))
            .flatten();
        let has_ai_cfa = ai_cfa.is_some();
        let (surfaces, has_ai_scene) =
            create_pipeline_surfaces(device, queue, raw, ai_cfa, &geometry)?;
        let has_raster_scene = raw.is_pre_demosaiced_raster();

        let buffers = create_pipeline_buffers(
            device,
            params,
            &profile_gpu_data.words,
            profile_gpu_data.layout.output[3],
        );

        let layouts = create_bind_group_layouts(
            device,
            program_template,
            raw.cfa_kind,
            geometry.demosaic_format,
            geometry.work_format,
            geometry.tone_format,
        );
        let groups = create_bind_groups(device, &layouts, &buffers, &surfaces, raw.cfa_kind);

        let shaders = load_shader_set(
            device,
            program_template.is_some(),
            geometry.demosaic_format,
            geometry.work_format,
        )?;

        let assembled = assemble_passes(
            device,
            program_template,
            pipeline_cache.as_ref(),
            &layouts,
            &groups,
            &shaders,
            raw.cfa_kind,
            geometry.image_workgroups,
            geometry.tone_workgroups,
        )?;
        let AssembledPasses {
            passes,
            post_blur_glow_passes,
            post_blur_creative_pass,
            post_blur_render_pass,
            indices,
        } = assembled;

        let egui_texture_id = renderer.as_deref_mut().map(|renderer| {
            renderer.register_native_texture(device, &surfaces.out_view, wgpu::FilterMode::Linear)
        });

        let pipeline = Self {
            egui_texture_id,
            width: raw.width,
            height: raw.height,
            cfa_kind: raw.cfa_kind,
            processing_quality: quality,
            camera_uniforms_buffer: buffers.camera_uniforms_buffer,
            scene_tone_uniforms_buffer: buffers.scene_tone_uniforms_buffer,
            effects_uniforms_buffer: buffers.effects_uniforms_buffer,
            scene_tone_bind_group: groups.scene_tone_bind_group,
            effects_bind_group: groups.effects_bind_group,
            uploaded_stage_uniforms: Mutex::new(UploadedStageUniforms {
                camera: params.camera,
                scene_tone: params.scene_tone,
                effects: params.effects,
            }),
            mask_data_buffer: buffers.mask_data_buffer,
            tone_histogram_buffer: buffers.tone_histogram_buffer,
            tone_stats_buffer: buffers.tone_stats_buffer,
            tone_prepare_pass_index: indices.tone_prepare_pass_index,
            tone_reduce_pass_index: indices.tone_reduce_pass_index,
            tone_stage_end: indices.tone_stage_end,
            demosaic_start_index: indices.demosaic_start_index,
            demosaic_dual_start_index: indices.demosaic_dual_start_index,
            demosaic_dual_end_index: indices.demosaic_dual_end_index,
            demosaic_finish_index: indices.demosaic_finish_index,
            color_denoise_start_index: indices.color_denoise_start_index,
            color_denoise_end_index: indices.color_denoise_end_index,
            adjustment_prepare_pass_index: indices.adjustment_prepare_pass_index,
            adjustment_tone_pass_index: indices.adjustment_tone_pass_index,
            adjustment_effects_pass_index: indices.adjustment_effects_pass_index,
            mask_blur_start_index: indices.mask_blur_start_index,
            mask_blur_end_index: indices.mask_blur_end_index,
            glow_prepare_pass_index: indices.glow_prepare_pass_index,
            glow_blur_start_index: indices.glow_blur_start_index,
            glow_blur_end_index: indices.glow_blur_end_index,
            adjustment_creative_pass_index: indices.adjustment_creative_pass_index,
            adjustment_render_pass_index: indices.adjustment_render_pass_index,
            post_blur_glow_passes,
            post_blur_creative_pass,
            post_blur_render_pass,
            passes,
            raw_texture: surfaces.raw_texture,
            color_texture: surfaces.color_texture,
            black_texture: surfaces.black_texture,
            _reconstructed_raw_texture: surfaces.reconstructed_raw_texture,
            _highlight_work_a: surfaces.highlight_work_a,
            _highlight_work_b: surfaces.highlight_work_b,
            _tex1: surfaces.tex1,
            _tex2: surfaces.tex2,
            scene_texture: surfaces.scene_texture,
            scene_format: geometry.demosaic_format,
            has_ai_scene,
            has_raster_scene,
            has_ai_cfa,
            display_linear_texture: surfaces.display_linear_texture,
            _tone_guide_a: surfaces.tone_guide_a,
            _tone_guide_b: surfaces.tone_guide_b,
            mask_texture: surfaces.mask_texture,
            light_rays_mask_texture: surfaces.light_rays_mask_texture,
            mask_layer_capacity: geometry.mask_layer_capacity,
            mask_atlas_edge: geometry.mask_atlas_edge,
            profile_buffer: buffers.profile_buffer,
            profile_buffer_size_bytes,
            output_lut_offset_bytes: buffers.output_lut_offset_bytes,
            out_texture: surfaces.out_texture,
            _out_view: surfaces.out_view,
            pipeline_cache,
            _gpu_budget_reservation: gpu_budget_reservation,
        };
        if let Err(error) = gpu_error_scopes.finish("create RAW GPU pipeline") {
            if let (Some(renderer), Some(texture_id)) = (renderer, pipeline.egui_texture_id) {
                renderer.free_texture(&texture_id);
            }
            return Err(error);
        }
        Ok(pipeline)
    }

    pub fn update_mask_layer(
        &self,
        queue: &wgpu::Queue,
        layer: usize,
        values: &[u16],
    ) -> Result<()> {
        if layer >= self.mask_layer_capacity {
            return Err(anyhow!(
                "local-mask layer {layer} exceeds atlas capacity {}",
                self.mask_layer_capacity
            ));
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

    pub fn update_mask_layer_region(
        &self,
        queue: &wgpu::Queue,
        layer: usize,
        width: u32,
        height: u32,
        values: &[u16],
    ) -> Result<()> {
        if layer >= self.mask_layer_capacity {
            return Err(anyhow!(
                "local-mask layer {layer} exceeds atlas capacity {}",
                self.mask_layer_capacity
            ));
        }
        if width == 0
            || height == 0
            || width > self.mask_atlas_edge
            || height > self.mask_atlas_edge
        {
            return Err(anyhow!(
                "local-mask region {width}x{height} exceeds {}x{} atlas",
                self.mask_atlas_edge,
                self.mask_atlas_edge
            ));
        }
        let expected = width as usize * height as usize;
        if values.len() != expected {
            return Err(anyhow!(
                "local-mask region has {} samples, expected {expected}",
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
                bytes_per_row: Some(width * 2),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    pub(crate) fn update_light_rays_mask_layer(
        &self,
        queue: &wgpu::Queue,
        layer: usize,
        values: &[u16],
    ) -> Result<()> {
        if layer >= self.mask_layer_capacity {
            return Err(anyhow!(
                "Light Rays mask layer {layer} exceeds atlas capacity {}",
                self.mask_layer_capacity
            ));
        }
        let edge = LIGHT_RAYS_MASK_ATLAS_EDGE;
        let expected = edge as usize * edge as usize;
        if values.len() != expected {
            return Err(anyhow!(
                "Light Rays mask layer has {} samples, expected {expected}",
                values.len()
            ));
        }
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.light_rays_mask_texture,
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
                bytes_per_row: Some(edge * 2),
                rows_per_image: Some(edge),
            },
            wgpu::Extent3d {
                width: edge,
                height: edge,
                depth_or_array_layers: 1,
            },
        );
        Ok(())
    }

    pub fn update_light_rays_mask_layers(
        &self,
        queue: &wgpu::Queue,
        masks: &MaskStack,
        image_width: u32,
        image_height: u32,
    ) -> Result<()> {
        let edge = LIGHT_RAYS_MASK_ATLAS_EDGE;
        for (layer, mask) in masks
            .masks
            .iter()
            .take(self.mask_layer_capacity)
            .enumerate()
        {
            if mask.effect != MaskEffect::LightRays {
                continue;
            }
            let values = masks.rasterize_layer_f16(layer, edge, edge, image_width, image_height);
            self.update_light_rays_mask_layer(queue, layer, &values)?;
        }
        Ok(())
    }

    pub const fn mask_atlas_edge(&self) -> u32 {
        self.mask_atlas_edge
    }

    pub const fn mask_layer_capacity(&self) -> usize {
        self.mask_layer_capacity
    }

    pub const fn immutable_ai_source_matches(&self, cfa_kind: CfaKind, enabled: bool) -> bool {
        match cfa_kind {
            CfaKind::Bayer => self.has_ai_cfa == enabled,
            CfaKind::XTrans => !enabled || self.has_ai_scene,
        }
    }

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

    pub fn set_display_icc_profile(
        &self,
        queue: &wgpu::Queue,
        profile_bytes: &[u8],
        intent: RenderingIntent,
    ) -> Result<()> {
        let transform = IccOutputTransform::from_icc(profile_bytes, intent)?;
        self.write_output_transform(queue, &transform)
    }

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

    pub fn recompute(&self, queue: &wgpu::Queue, device: &wgpu::Device, params: &GpuParams) {
        self.upload_params(queue, params);
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

    pub fn dispatch_stage(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
        stage: ProcessingStage,
    ) {
        self.upload_params(queue, params);
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

        if self.has_raster_scene {
            anyhow::ensure!(
                raw.is_pre_demosaiced_raster(),
                "reusable raster pipeline received a sensor RAW tile"
            );
            anyhow::ensure!(
                upload_ai_scene_texture(queue, &self.scene_texture, self.scene_format, raw)?,
                "reusable raster pipeline received a tile without scene-linear RGB"
            );
            return Ok(());
        }

        let ai_image = raw.ai_denoised_image();
        let raw_pixels = if self.has_ai_cfa {
            ai_image
                .as_ref()
                .and_then(AiDenoisedImage::bayer_cfa)
                .context("reusable AI-denoise pipeline received a tile without denoised CFA")?
        } else {
            raw.raw_pixels.as_slice()
        };
        queue.write_texture(
            copy_texture(&self.raw_texture),
            bytemuck::cast_slice(raw_pixels),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(raw.width * 2),
                rows_per_image: Some(raw.height),
            },
            texture_size(raw.width, raw.height),
        );
        upload_color_texture(queue, &self.color_texture, raw);
        upload_black_texture(queue, &self.black_texture, raw);
        if self.has_ai_scene {
            anyhow::ensure!(
                upload_ai_scene_texture(queue, &self.scene_texture, self.scene_format, raw)?,
                "reusable AI-denoise pipeline received a tile without derived model output"
            );
        }
        Ok(())
    }

    pub fn begin_export_tone_analysis(&self, queue: &wgpu::Queue, device: &wgpu::Device) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw export tone histogram clear"),
        });
        encoder.clear_buffer(&self.tone_histogram_buffer, 0, None);
        queue.submit(Some(encoder.finish()));
    }

    pub fn dispatch_stage_with_remove(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
        stage: ProcessingStage,
        edits: &RemoveEditState,
        source_raw: &LoadedRaw,
        exposure: &ExposureParams,
        source_origin: [f32; 2],
        source_size: [f32; 2],
    ) -> Result<()> {
        self.dispatch_stage(queue, device, params, stage);
        if stage == ProcessingStage::Raw {
            self.upload_remove_scene_patches(
                queue,
                edits,
                source_raw,
                exposure,
                source_origin,
                source_size,
            )?;
        }
        Ok(())
    }

    pub fn recompute_with_remove(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
        edits: &RemoveEditState,
        source_raw: &LoadedRaw,
        exposure: &ExposureParams,
        source_origin: [f32; 2],
        source_size: [f32; 2],
    ) -> Result<()> {
        for stage in [
            ProcessingStage::Raw,
            ProcessingStage::Tone,
            ProcessingStage::Output,
        ] {
            self.dispatch_stage_with_remove(
                queue,
                device,
                params,
                stage,
                edits,
                source_raw,
                exposure,
                source_origin,
                source_size,
            )?;
        }
        Ok(())
    }

    pub fn snapshot_tone_statistics(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> ToneStatisticsSnapshot {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("auraw tone statistics snapshot"),
            size: TONE_STATS_SIZE_BYTES,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw capture tone statistics"),
        });
        encoder.copy_buffer_to_buffer(
            &self.tone_stats_buffer,
            0,
            &buffer,
            0,
            TONE_STATS_SIZE_BYTES,
        );
        queue.submit(Some(encoder.finish()));
        ToneStatisticsSnapshot { buffer }
    }

    pub fn inherit_tone_statistics_snapshot(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        snapshot: &ToneStatisticsSnapshot,
    ) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw inherit tone statistics snapshot"),
        });
        encoder.copy_buffer_to_buffer(
            &snapshot.buffer,
            0,
            &self.tone_stats_buffer,
            0,
            TONE_STATS_SIZE_BYTES,
        );
        queue.submit(Some(encoder.finish()));
    }

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

    pub fn accumulate_export_tone_tile(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
    ) {
        self.upload_params(queue, params);
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

    pub fn accumulate_export_tone_tile_with_remove(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
        edits: &RemoveEditState,
        source_raw: &LoadedRaw,
        exposure: &ExposureParams,
        source_origin: [f32; 2],
        source_size: [f32; 2],
    ) -> Result<()> {
        self.dispatch_stage_with_remove(
            queue,
            device,
            params,
            ProcessingStage::Raw,
            edits,
            source_raw,
            exposure,
            source_origin,
            source_size,
        )?;
        self.upload_params(queue, params);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw Remove-aware export tone tile"),
        });
        self.encode_pass_range(
            &mut encoder,
            self.tone_prepare_pass_index,
            self.tone_prepare_pass_index + 1,
        );
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

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

    pub fn dispatch_export_tile(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
    ) {
        self.upload_params(queue, params);
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

    pub fn dispatch_export_tile_with_remove(
        &self,
        queue: &wgpu::Queue,
        device: &wgpu::Device,
        params: &GpuParams,
        edits: &RemoveEditState,
        source_raw: &LoadedRaw,
        exposure: &ExposureParams,
        source_origin: [f32; 2],
        source_size: [f32; 2],
    ) -> Result<()> {
        self.dispatch_stage_with_remove(
            queue,
            device,
            params,
            ProcessingStage::Raw,
            edits,
            source_raw,
            exposure,
            source_origin,
            source_size,
        )?;
        self.upload_params(queue, params);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw Remove-aware tiled export encoder"),
        });
        self.encode_pass_range(
            &mut encoder,
            self.tone_prepare_pass_index,
            self.tone_reduce_pass_index,
        );
        self.encode_output_stage(&mut encoder, params);
        queue.submit(Some(encoder.finish()));
        Ok(())
    }

    pub fn read_output_region_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>> {
        read_rgba8_texture_region_blocking(
            device,
            queue,
            &self.out_texture,
            TextureReadbackRegion {
                origin: [x, y],
                extent: [width, height],
                texture_extent: [self.width, self.height],
                label: "auraw tiled export readback",
            },
        )
    }

    pub fn begin_display_linear_region_readback(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Result<PendingRgba32Readback> {
        if self.scene_format != wgpu::TextureFormat::Rgba32Float {
            return Err(anyhow!(
                "display-linear export readback requires ProcessingQuality::High (RGBA32Float)"
            ));
        }
        begin_rgba32_texture_region_rgb_readback(
            device,
            queue,
            &self.display_linear_texture,
            TextureReadbackRegion {
                origin: [x, y],
                extent: [width, height],
                texture_extent: [self.width, self.height],
                label: "auraw pipelined display-linear export readback",
            },
        )
    }

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
            TextureReadbackRegion {
                origin: [x, y],
                extent: [width, height],
                texture_extent: [self.width, self.height],
                label: "auraw display-linear export readback",
            },
        )
    }

    pub fn upload_remove_scene_patches(
        &self,
        queue: &wgpu::Queue,
        edits: &RemoveEditState,
        source_raw: &LoadedRaw,
        exposure: &ExposureParams,
        source_origin: [f32; 2],
        source_size: [f32; 2],
    ) -> Result<()> {
        if edits.is_empty() || source_size[0] <= 0.0 || source_size[1] <= 0.0 {
            return Ok(());
        }
        let scale_x = self.width as f32 / source_size[0];
        let scale_y = self.height as f32 / source_size[1];
        if !scale_x.is_finite() || !scale_y.is_finite() || scale_x <= 0.0 || scale_y <= 0.0 {
            return Ok(());
        }

        for stroke in &edits.strokes {
            for patch in &stroke.patches {
                let left = (((patch.bounds.x as f32 - source_origin[0]) * scale_x).floor() as i64)
                    .clamp(0, self.width as i64) as u32;
                let top = (((patch.bounds.y as f32 - source_origin[1]) * scale_y).floor() as i64)
                    .clamp(0, self.height as i64) as u32;
                let right = (((patch.bounds.right() as f32 - source_origin[0]) * scale_x).ceil()
                    as i64)
                    .clamp(0, self.width as i64) as u32;
                let bottom = (((patch.bounds.bottom() as f32 - source_origin[1]) * scale_y).ceil()
                    as i64)
                    .clamp(0, self.height as i64) as u32;
                if right <= left || bottom <= top {
                    continue;
                }

                for y in top..bottom {
                    let native_y = source_origin[1] + (y as f32 + 0.5) / scale_y;
                    let local_y = native_y - patch.bounds.y as f32 - 0.5;
                    let mut x = left;
                    while x < right {
                        while x < right {
                            let native_x = source_origin[0] + (x as f32 + 0.5) / scale_x;
                            let local_x = native_x - patch.bounds.x as f32 - 0.5;
                            if remove_patch_coverage(patch, local_x, local_y) != 0 {
                                break;
                            }
                            x += 1;
                        }
                        if x >= right {
                            break;
                        }
                        let run_start = x;
                        let mut samples = Vec::new();
                        while x < right {
                            let native_x = source_origin[0] + (x as f32 + 0.5) / scale_x;
                            let local_x = native_x - patch.bounds.x as f32 - 0.5;
                            if remove_patch_coverage(patch, local_x, local_y) == 0 {
                                break;
                            }
                            let canonical = sample_remove_patch_scene(
                                patch, local_x, local_y, source_raw, exposure,
                            );
                            let source_scene = canonical_remove_scene_to_pipeline_scene(
                                source_raw, exposure, canonical,
                            );
                            let scene = if self.has_raster_scene {
                                pipeline_scene_to_working_rec2020(source_raw, source_scene)
                            } else {
                                source_scene
                            };
                            samples.push(scene);
                            x += 1;
                        }
                        if samples.is_empty() {
                            continue;
                        }
                        let run_width = samples.len() as u32;
                        match self.scene_format {
                            wgpu::TextureFormat::Rgba16Float => {
                                let mut rgba = Vec::<u16>::with_capacity(samples.len() * 4);
                                for rgb in samples {
                                    for value in rgb {
                                        let finite = if value.is_finite() { value } else { 0.0 };
                                        rgba.push(
                                            half::f16::from_f32(finite.clamp(-65_504.0, 65_504.0))
                                                .to_bits(),
                                        );
                                    }
                                    rgba.push(half::f16::ONE.to_bits());
                                }
                                queue.write_texture(
                                    wgpu::TexelCopyTextureInfo {
                                        texture: &self.scene_texture,
                                        mip_level: 0,
                                        origin: wgpu::Origin3d {
                                            x: run_start,
                                            y,
                                            z: 0,
                                        },
                                        aspect: wgpu::TextureAspect::All,
                                    },
                                    bytemuck::cast_slice(&rgba),
                                    wgpu::TexelCopyBufferLayout {
                                        offset: 0,
                                        bytes_per_row: Some(run_width * 8),
                                        rows_per_image: Some(1),
                                    },
                                    wgpu::Extent3d {
                                        width: run_width,
                                        height: 1,
                                        depth_or_array_layers: 1,
                                    },
                                );
                            }
                            wgpu::TextureFormat::Rgba32Float => {
                                let mut rgba = Vec::<f32>::with_capacity(samples.len() * 4);
                                for rgb in samples {
                                    rgba.extend(rgb.map(|value| {
                                        if value.is_finite() {
                                            value
                                        } else {
                                            0.0
                                        }
                                    }));
                                    rgba.push(1.0);
                                }
                                queue.write_texture(
                                    wgpu::TexelCopyTextureInfo {
                                        texture: &self.scene_texture,
                                        mip_level: 0,
                                        origin: wgpu::Origin3d {
                                            x: run_start,
                                            y,
                                            z: 0,
                                        },
                                        aspect: wgpu::TextureAspect::All,
                                    },
                                    bytemuck::cast_slice(&rgba),
                                    wgpu::TexelCopyBufferLayout {
                                        offset: 0,
                                        bytes_per_row: Some(run_width * 16),
                                        rows_per_image: Some(1),
                                    },
                                    wgpu::Extent3d {
                                        width: run_width,
                                        height: 1,
                                        depth_or_array_layers: 1,
                                    },
                                );
                            }
                            format => {
                                return Err(anyhow!("unsupported Remove scene format {format:?}"));
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

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

    pub fn render_camera_scene_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: &GpuParams,
    ) -> Result<Vec<f32>> {
        if self.scene_format != wgpu::TextureFormat::Rgba32Float {
            return Err(anyhow!(
                "camera scene readback requires ProcessingQuality::High (RGBA32Float)"
            ));
        }
        self.upload_params(queue, params);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw camera scene readback encoder"),
        });
        self.encode_raw_stage(&mut encoder, params);
        queue.submit(Some(encoder.finish()));
        self.read_scene_texture_blocking(device, queue)
    }

    fn encode_raw_stage(&self, encoder: &mut wgpu::CommandEncoder, params: &GpuParams) {
        if self.has_raster_scene {
            if params.camera.chroma_denoise > 1e-6 {
                self.encode_pass_range(
                    encoder,
                    self.color_denoise_start_index,
                    self.color_denoise_end_index,
                );
            }
            return;
        }
        if self.has_ai_scene && params.uses_ai_denoise() {
            return;
        }
        self.encode_pass(encoder, 0);
        self.encode_pass_range(
            encoder,
            self.demosaic_start_index,
            self.demosaic_dual_start_index,
        );
        if params.needs_dual_demosaic_passes() {
            self.encode_pass_range(
                encoder,
                self.demosaic_dual_start_index,
                self.demosaic_dual_end_index,
            );
        }
        self.encode_pass(encoder, self.demosaic_finish_index);
        if params.camera.chroma_denoise > 1e-6 {
            self.encode_pass_range(
                encoder,
                self.color_denoise_start_index,
                self.color_denoise_end_index,
            );
        }
    }

    fn encode_output_stage(&self, encoder: &mut wgpu::CommandEncoder, params: &GpuParams) {
        self.encode_pass(encoder, self.adjustment_prepare_pass_index);
        self.encode_pass(encoder, self.adjustment_tone_pass_index);
        let blur_active = params.needs_blur_passes();
        if params.needs_intermediate_adjustment_passes() {
            self.encode_pass(encoder, self.adjustment_effects_pass_index - 1);
            self.encode_pass(encoder, self.adjustment_effects_pass_index);
            self.encode_pass(encoder, self.adjustment_effects_pass_index + 1);
            if blur_active {
                if params.needs_progressive_blur_passes() {
                    self.encode_pass_range(
                        encoder,
                        self.mask_blur_start_index,
                        self.mask_blur_end_index,
                    );
                } else {
                    self.encode_pass(encoder, self.mask_blur_start_index);
                }
            }
            if params.needs_glow_passes() {
                if blur_active {
                    for (index, pass) in self.post_blur_glow_passes.iter().enumerate() {
                        self.encode_bound_pass(
                            encoder,
                            pass,
                            &format!("post-Blur Glow pass {}", index + 1),
                        );
                    }
                } else {
                    self.encode_pass(encoder, self.glow_prepare_pass_index);
                    self.encode_pass_range(
                        encoder,
                        self.glow_blur_start_index,
                        self.glow_blur_end_index,
                    );
                }
            }
            if blur_active {
                self.encode_bound_pass(
                    encoder,
                    &self.post_blur_creative_pass,
                    "post-Blur creative pass",
                );
            } else {
                self.encode_pass(encoder, self.adjustment_creative_pass_index);
            }
        }
        if blur_active {
            self.encode_bound_pass(
                encoder,
                &self.post_blur_render_pass,
                "post-Blur render pass",
            );
        } else {
            self.encode_pass(encoder, self.adjustment_render_pass_index);
        }
    }

    fn encode_pass(&self, encoder: &mut wgpu::CommandEncoder, index: usize) {
        self.encode_bound_pass(
            encoder,
            &self.passes[index],
            &format!("auraw pass {}", index + 1),
        );
    }

    fn encode_bound_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        pass_record: &Pass,
        label: &str,
    ) {
        dispatch_compute(
            encoder,
            label,
            &pass_record.pipeline,
            &[
                &pass_record.bind_group,
                &self.scene_tone_bind_group,
                &self.effects_bind_group,
            ],
            pass_record.workgroups,
        );
    }

    fn encode_pass_range(&self, encoder: &mut wgpu::CommandEncoder, start: usize, end: usize) {
        for index in start..end {
            self.encode_pass(encoder, index);
        }
    }
}
