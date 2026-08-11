use super::basicadj::sigmoid_contrast_from_percent;
use super::gpu_cache::PersistentGpuPipelineCache;
use super::sigmoid::coefficients as sigmoid_coefficients;
use crate::pipeline::{
    export_mask_atlas_edge_limit, mask_atlas_edge, AiDenoisedImage, CfaKind, ExposureParams,
    GeometryTransform, HighlightReconstructionMethod, IccOutputTransform, LoadedRaw, MaskEffect,
    MaskStack, PointCurve, ProcessingStage, RawThumbnail, RenderingIntent,
    GLOBAL_TEMPERATURE_LIMIT, GLOBAL_TINT_OFFSET_LIMIT, HUE_ROTATION_LIMIT_DEGREES,
    MAX_LOCAL_MASKS,
};
use anyhow::{anyhow, Context, Result};
use bytemuck::{Pod, Zeroable};
use std::borrow::Cow;
use std::sync::{Arc, Condvar, Mutex};
use wgpu::util::DeviceExt;

use crate::gpu_errors::GpuErrorScopes;

mod readback;
mod resources;
mod shader_manager;

use readback::*;
use resources::*;
use shader_manager::ShaderManager;

#[cfg(test)]
mod tests;

const GPU_PARAMS_ABI_VERSION: u32 = 5;
const MASK_EFFECT_ID_SHIFT: u32 = 8;
// The public ABI marker retains the historical monolithic payload size while
// the runtime uses independently allocated stage uniforms.
const GPU_PARAMS_ABI_SIZE_BYTES: u32 = 1_072;
const CAMERA_UNIFORMS_SIZE_BYTES: u32 = 416;
const SCENE_TONE_UNIFORMS_SIZE_BYTES: u32 = 768;
const EFFECTS_UNIFORMS_SIZE_BYTES: u32 = 208;
const GPU_STAGE_UNIFORM_SIZE_BYTES: u32 =
    CAMERA_UNIFORMS_SIZE_BYTES + SCENE_TONE_UNIFORMS_SIZE_BYTES + EFFECTS_UNIFORMS_SIZE_BYTES;
// Resource accounting rounds each independently allocated buffer to 256 bytes.
const GPU_STAGE_UNIFORM_ALLOCATION_BYTES: u64 = 512 + 768 + 256;
const MASK_DATA_SIZE_BYTES: u64 = (std::mem::size_of::<MaskData>() * MAX_LOCAL_MASKS) as u64;
const WORK_FORMAT_MARKER: &str = "rgba16float /* AURAW_WORK_FORMAT */";
const DEFAULT_WORKGROUP_ATTRIBUTE: &str = "@workgroup_size(8, 8, 1)";
const TONE_STATS_SIZE_BYTES: u64 = 2 * std::mem::size_of::<[f32; 4]>() as u64;
const DESKTOP_GPU_WORKING_SET_LIMIT_BYTES: u64 = 1_500 * 1024 * 1024;
const ANDROID_GPU_WORKING_SET_LIMIT_BYTES: u64 = 384 * 1024 * 1024;

/// Two-dimensional compute workgroup shape used by image and tone-guide
/// shaders. The Z dimension remains one because every dispatch targets a 2D
/// texture. The default preserves AuRaw's historical 8x8 configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComputeWorkgroupSize {
    pub x: u32,
    pub y: u32,
}

impl ComputeWorkgroupSize {
    pub const DEFAULT: Self = Self { x: 8, y: 8 };

    pub fn new(x: u32, y: u32) -> Result<Self> {
        if x == 0 || y == 0 {
            return Err(anyhow!("compute workgroup dimensions must be non-zero"));
        }
        x.checked_mul(y)
            .ok_or_else(|| anyhow!("compute workgroup dimensions overflow"))?;
        Ok(Self { x, y })
    }

    pub fn validate_for_limits(self, limits: &wgpu::Limits) -> Result<()> {
        let invocations = self
            .x
            .checked_mul(self.y)
            .ok_or_else(|| anyhow!("compute workgroup dimensions overflow"))?;
        if self.x > limits.max_compute_workgroup_size_x
            || self.y > limits.max_compute_workgroup_size_y
            || invocations > limits.max_compute_invocations_per_workgroup
        {
            return Err(anyhow!(
                "compute workgroup {}x{} exceeds device limits ({}x{}, {} invocations)",
                self.x,
                self.y,
                limits.max_compute_workgroup_size_x,
                limits.max_compute_workgroup_size_y,
                limits.max_compute_invocations_per_workgroup,
            ));
        }
        Ok(())
    }

    fn dispatch_for_extent(self, width: u32, height: u32) -> [u32; 3] {
        [width.div_ceil(self.x), height.div_ceil(self.y), 1]
    }
}

impl Default for ComputeWorkgroupSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

pub(super) fn specialize_compute_workgroup_size<'a>(
    source: &'a str,
    workgroup_size: ComputeWorkgroupSize,
) -> Cow<'a, str> {
    if workgroup_size == ComputeWorkgroupSize::DEFAULT
        || !source.contains(DEFAULT_WORKGROUP_ATTRIBUTE)
    {
        return Cow::Borrowed(source);
    }
    Cow::Owned(source.replace(
        DEFAULT_WORKGROUP_ATTRIBUTE,
        &format!(
            "@workgroup_size({}, {}, 1)",
            workgroup_size.x, workgroup_size.y
        ),
    ))
}

/// Logical domains carried by the post-demosaic graph. These contracts are
/// independent of pass fusion: multiple adjacent nodes may execute in one GPU
/// pass, but a scene-domain edit may never consume display-referred pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenderDomain {
    CameraLinear,
    SceneLinear,
    LookAdjustedScene,
    DisplayLinear,
    OutputEncoded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RenderStageContract {
    name: &'static str,
    input: RenderDomain,
    output: RenderDomain,
}

const EXPLICIT_RENDER_GRAPH: [RenderStageContract; 6] = [
    RenderStageContract {
        name: "camera_characterization",
        input: RenderDomain::CameraLinear,
        output: RenderDomain::SceneLinear,
    },
    RenderStageContract {
        name: "scene_edits",
        input: RenderDomain::SceneLinear,
        output: RenderDomain::SceneLinear,
    },
    RenderStageContract {
        name: "optional_look",
        input: RenderDomain::SceneLinear,
        output: RenderDomain::LookAdjustedScene,
    },
    RenderStageContract {
        name: "view_transform",
        input: RenderDomain::LookAdjustedScene,
        output: RenderDomain::DisplayLinear,
    },
    RenderStageContract {
        name: "display_black_toe",
        input: RenderDomain::DisplayLinear,
        output: RenderDomain::DisplayLinear,
    },
    RenderStageContract {
        name: "output_encoding",
        input: RenderDomain::DisplayLinear,
        output: RenderDomain::OutputEncoded,
    },
];

fn explicit_render_graph_contracts_are_contiguous() -> bool {
    EXPLICIT_RENDER_GRAPH
        .windows(2)
        .all(|pair| pair[0].output == pair[1].input)
        && EXPLICIT_RENDER_GRAPH[0].name == "camera_characterization"
        && EXPLICIT_RENDER_GRAPH[3].name == "view_transform"
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

#[cfg(test)]
const SHADER_COMMON_FOR_TESTS: &str = SHADER_COMMON;

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
    /// Half-float image intermediates for lower memory use and faster previews.
    Preview,
    /// Full-float demosaic, scene, and highlight-reconstruction intermediates.
    #[default]
    High,
}

fn expected_pass_count(cfa_kind: CfaKind) -> usize {
    let demosaic_passes = match cfa_kind {
        CfaKind::Bayer => 6,
        CfaKind::XTrans => 10,
    };
    // One highlight reconstruction pass, demosaic, six colour-denoise scales,
    // four tone-analysis passes, and thirteen adjustment/output passes.
    1 + demosaic_passes + COLOR_DENOISE_ENTRY_POINTS.len() + 4 + 13
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
const SHADER_REGRESSION_SCENE: &str = include_str!("../shaders/regression_scene.wgsl");

const SHADER_SCENE_ADJUSTMENTS: &str = include_str!("../shaders/scene_adjustments.wgsl");
const SHADER_MASK_EFFECTS_SHARED: &str = include_str!("../shaders/mask_effects/shared.wgsl");
const SHADER_MASK_GLOW: &str = include_str!("../shaders/mask_effects/glow.wgsl");
const SHADER_MASK_NEON: &str = include_str!("../shaders/mask_effects/neon.wgsl");
const SHADER_CREATIVE_EFFECTS: &str = include_str!("../shaders/creative_effects.wgsl");
const SHADER_VIEW_TRANSFORM: &str = include_str!("../shaders/view_transform.wgsl");

const SHADER_INPAINT_DOWNSAMPLE: &str = r#"
struct ResizeParams {
    source_origin_x: u32,
    source_origin_y: u32,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    _pad0: u32,
    _pad1: u32,
    cam_to_working_0: vec4<f32>,
    cam_to_working_1: vec4<f32>,
    cam_to_working_2: vec4<f32>,
};

@group(0) @binding(0) var<uniform> params: ResizeParams;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var output_tex: texture_storage_2d<rgba32float, write>;

fn sample_camera_bilinear(position: vec2<f32>) -> vec3<f32> {
    let dimensions = vec2<i32>(textureDimensions(source_tex));
    let coordinate = position - vec2<f32>(0.5);
    let base = vec2<i32>(floor(coordinate));
    let fraction = fract(coordinate);
    let maximum = dimensions - vec2<i32>(1);
    let p00 = clamp(base, vec2<i32>(0), maximum);
    let p10 = clamp(base + vec2<i32>(1, 0), vec2<i32>(0), maximum);
    let p01 = clamp(base + vec2<i32>(0, 1), vec2<i32>(0), maximum);
    let p11 = clamp(base + vec2<i32>(1), vec2<i32>(0), maximum);
    let top = mix(
        textureLoad(source_tex, p00, 0).xyz,
        textureLoad(source_tex, p10, 0).xyz,
        fraction.x,
    );
    let bottom = mix(
        textureLoad(source_tex, p01, 0).xyz,
        textureLoad(source_tex, p11, 0).xyz,
        fraction.x,
    );
    return mix(top, bottom, fraction.y);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.output_width || gid.y >= params.output_height {
        return;
    }
    let scale = vec2<f32>(
        f32(params.source_width) / f32(params.output_width),
        f32(params.source_height) / f32(params.output_height),
    );
    let samples_x = clamp(u32(ceil(scale.x)), 1u, 8u);
    let samples_y = clamp(u32(ceil(scale.y)), 1u, 8u);
    let footprint_origin = vec2<f32>(
        f32(params.source_origin_x) + f32(gid.x) * scale.x,
        f32(params.source_origin_y) + f32(gid.y) * scale.y,
    );
    var camera_rgb = vec3<f32>(0.0);
    for (var sample_y = 0u; sample_y < 8u; sample_y = sample_y + 1u) {
        if sample_y >= samples_y { break; }
        for (var sample_x = 0u; sample_x < 8u; sample_x = sample_x + 1u) {
            if sample_x >= samples_x { break; }
            let offset = vec2<f32>(
                (f32(sample_x) + 0.5) / f32(samples_x),
                (f32(sample_y) + 0.5) / f32(samples_y),
            ) * scale;
            camera_rgb = camera_rgb + sample_camera_bilinear(footprint_origin + offset);
        }
    }
    camera_rgb = camera_rgb / f32(samples_x * samples_y);
    let working_rgb = vec3<f32>(
        dot(params.cam_to_working_0.xyz, camera_rgb),
        dot(params.cam_to_working_1.xyz, camera_rgb),
        dot(params.cam_to_working_2.xyz, camera_rgb),
    );
    textureStore(
        output_tex,
        vec2<i32>(i32(gid.x), i32(gid.y)),
        vec4<f32>(working_rgb, 1.0),
    );
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
struct InpaintResizeParams {
    source_origin_x: u32,
    source_origin_y: u32,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    _pad0: u32,
    _pad1: u32,
    cam_to_working_0: [f32; 4],
    cam_to_working_1: [f32; 4],
    cam_to_working_2: [f32; 4],
}

const _: () = assert!(std::mem::size_of::<InpaintResizeParams>() == 80);

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
    inpaint_wb_0: [f32; 4],
    inpaint_wb_1: [f32; 4],
    inpaint_wb_2: [f32; 4],
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
    // WGSL mat3x3 uniform columns have a 16-byte stride. The fourth value in
    // every Rust column is explicit padding and is ignored by the shader.
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
const _: () = assert!(GPU_STAGE_UNIFORM_SIZE_BYTES == 1_392);

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

/// Runtime-adjustable shader calibration values. The defaults are byte-for-byte
/// equivalents of the former WGSL file-level constants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuShaderTuning {
    pub rec2020_to_xyz: [[f32; 4]; 3],
    pub xyz_to_rec2020: [[f32; 4]; 3],
    pub xyz_to_bradford: [[f32; 4]; 3],
    pub bradford_to_xyz: [[f32; 4]; 3],
    pub vignette_dark_half_fit: [f32; 4],
    pub vignette_dark_full_fit: [f32; 4],
    pub vignette_light_half_fit: [f32; 4],
    pub vignette_light_full_fit: [f32; 4],
    pub capture_scale_sigma: [f32; 4],
    pub capture_thresholds: [f32; 4],
    pub capture_mask_coherence: [f32; 4],
}

impl Default for GpuShaderTuning {
    fn default() -> Self {
        Self {
            rec2020_to_xyz: [
                [0.6369580, 0.2627002, 0.0000000, 0.0],
                [0.1446169, 0.6779981, 0.0280727, 0.0],
                [0.1688809, 0.0593017, 1.0609851, 0.0],
            ],
            xyz_to_rec2020: [
                [1.7166512, -0.6666844, 0.0176399, 0.0],
                [-0.3556708, 1.6164812, -0.0427706, 0.0],
                [-0.2533663, 0.0157685, 0.9421031, 0.0],
            ],
            xyz_to_bradford: [
                [0.8951000, -0.7502000, 0.0389000, 0.0],
                [0.2664000, 1.7135000, -0.0685000, 0.0],
                [-0.1614000, 0.0367000, 1.0296000, 0.0],
            ],
            bradford_to_xyz: [
                [0.9869929, 0.4323053, -0.0085287, 0.0],
                [-0.1470543, 0.5183603, 0.0400428, 0.0],
                [0.1599627, 0.0492912, 0.9684867, 0.0],
            ],
            vignette_dark_half_fit: [0.10, 1.235, 2.88, 0.86],
            vignette_dark_full_fit: [0.02, 1.135, 3.46, 1.0],
            vignette_light_half_fit: [0.305, 1.24, 4.36, 0.90],
            vignette_light_full_fit: [0.13, 1.075, 5.66, 1.0],
            capture_scale_sigma: [0.74, 1.75, 0.58, 1.65],
            capture_thresholds: [0.015, 0.0045, 0.055, 0.28],
            capture_mask_coherence: [0.035, 0.62, 0.055, 0.22],
        }
    }
}

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

fn pack_local_point_curve(curve: &PointCurve) -> [[f32; 4]; 8] {
    let mut packed = [[0.0; 4]; 8];
    for (pair, values) in packed.iter_mut().take(4).enumerate() {
        *values = [
            curve.points[pair * 2][0],
            curve.points[pair * 2][1],
            curve.points[pair * 2 + 1][0],
            curve.points[pair * 2 + 1][1],
        ];
    }
    packed[4] = [
        curve.len.clamp(2, 8) as f32,
        if curve.is_identity() { 1.0 } else { 0.0 },
        0.0,
        0.0,
    ];
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
        hue.clamp(-HUE_ROTATION_LIMIT_DEGREES, HUE_ROTATION_LIMIT_DEGREES),
        0.0,
    ]
}

fn matrix3_from_rows4(rows: [[f32; 4]; 3]) -> [[f32; 3]; 3] {
    rows.map(|row| [row[0], row[1], row[2]])
}

fn invert_matrix3(m: [[f32; 3]; 3]) -> Option<[[f32; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * inv,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * inv,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * inv,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * inv,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * inv,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * inv,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * inv,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * inv,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * inv,
        ],
    ])
}

fn multiply_matrix3(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for row in 0..3 {
        for col in 0..3 {
            out[row][col] = a[row][0] * b[0][col] + a[row][1] * b[1][col] + a[row][2] * b[2][col];
        }
    }
    out
}

fn rows4_from_matrix3(matrix: [[f32; 3]; 3]) -> [[f32; 4]; 3] {
    matrix.map(|row| [row[0], row[1], row[2], 0.0])
}

fn camera_transform_with_white_balance(
    mut transform: [[f32; 4]; 3],
    white_balance: [f32; 4],
) -> [[f32; 4]; 3] {
    let logical = [
        white_balance[0],
        0.5 * (white_balance[1] + white_balance[3]),
        white_balance[2],
    ];
    for row in &mut transform {
        for column in 0..3 {
            row[column] *= logical[column];
        }
    }
    transform
}

fn inpaint_neutral_to_current_transform(
    neutral: [[f32; 4]; 3],
    current: [[f32; 4]; 3],
) -> [[f32; 4]; 3] {
    let neutral3 = matrix3_from_rows4(neutral);
    let current3 = matrix3_from_rows4(current);
    let Some(neutral_inverse) = invert_matrix3(neutral3) else {
        return [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ];
    };
    rows4_from_matrix3(multiply_matrix3(current3, neutral_inverse))
}

fn composite_inpaint_rgba16f(destination: &mut [u16], rgb: [f32; 3], alpha: f32) {
    debug_assert_eq!(destination.len(), 4);
    use half::f16;

    let source_alpha = alpha.clamp(0.0, 1.0);
    let destination_alpha = f16::from_bits(destination[3]).to_f32().clamp(0.0, 1.0);
    let retained_destination = destination_alpha * (1.0 - source_alpha);
    let output_alpha = source_alpha + retained_destination;
    if output_alpha <= 1e-6 {
        destination.fill(0);
        return;
    }
    for channel in 0..3 {
        let previous = f16::from_bits(destination[channel]).to_f32();
        let output = (rgb[channel] * source_alpha + previous * retained_destination) / output_alpha;
        destination[channel] = f16::from_f32(output).to_bits();
    }
    destination[3] = f16::from_f32(output_alpha).to_bits();
}

fn canonicalize_green_noise(mut coefficients: [f32; 4], green2_present: bool) -> [f32; 4] {
    if green2_present {
        let green = 0.5 * (coefficients[1] + coefficients[3]);
        // Keep both green slots canonical. The dual-demosaic shader averages
        // G1/G2 once; retaining the original G2 in alpha would bias the result
        // to 25% G1 / 75% G2 after a second average.
        coefficients[1] = green;
        coefficients[3] = green;
    }
    coefficients
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
        let (white_balance, camera_transform, profile_weight) = raw
            .adjusted_white_balance_and_camera_transform(
                exposure
                    .temperature
                    .clamp(-GLOBAL_TEMPERATURE_LIMIT, GLOBAL_TEMPERATURE_LIMIT),
                exposure
                    .tint
                    .clamp(-GLOBAL_TINT_OFFSET_LIMIT, GLOBAL_TINT_OFFSET_LIMIT),
            );
        let inpaint_wb_transform = inpaint_neutral_to_current_transform(
            camera_transform_with_white_balance(raw.cam_to_srgb, raw.wb_coeffs),
            camera_transform_with_white_balance(camera_transform, white_balance),
        );
        let mut profile_layout = raw.camera_profile.gpu_layout();
        profile_layout.flags[3] = profile_weight.clamp(0.0, 1.0).to_bits();
        let profile_stages = profile_layout.stages();
        debug_assert_eq!(
            profile_stages.characterization.hue_sat_2,
            profile_layout.hue_sat_2
        );
        let mut sigmoid_params = exposure.sigmoid;
        sigmoid_params.contrast = sigmoid_contrast_from_percent(exposure.contrast);
        let sigmoid = sigmoid_coefficients(sigmoid_params);
        let shader_tuning = GpuShaderTuning::default();
        // Sigmoid is the single view transform, so Contrast changes its
        // middle-grey slope without switching view operators.
        let mut mask_data = [MaskData::zeroed(); MAX_LOCAL_MASKS];
        for (index, mask) in masks.masks.iter().take(MAX_LOCAL_MASKS).enumerate() {
            if mask.effect == MaskEffect::Glow {
                let glow = mask.effect_settings.glow;
                let active = mask.enabled && glow.is_active();
                mask_data[index] = MaskData {
                    metadata: [
                        u32::from(active),
                        u32::from(active),
                        0,
                        mask.effect.shader_id() << MASK_EFFECT_ID_SHIFT,
                    ],
                    adjust_0: [
                        glow.amount.clamp(0.0, 100.0),
                        glow.radius.clamp(0.0, 100.0),
                        glow.core.clamp(0.0, 100.0),
                        0.0,
                    ],
                    adjust_1: [
                        glow.color[0].clamp(0.0, 1.0),
                        glow.color[1].clamp(0.0, 1.0),
                        glow.color[2].clamp(0.0, 1.0),
                        0.0,
                    ],
                    ..MaskData::zeroed()
                };
                continue;
            }
            if mask.effect == MaskEffect::Neon {
                let neon = mask.effect_settings.neon;
                let active = mask.enabled && neon.is_active();
                mask_data[index] = MaskData {
                    metadata: [
                        u32::from(active),
                        u32::from(active),
                        0,
                        mask.effect.shader_id() << MASK_EFFECT_ID_SHIFT,
                    ],
                    adjust_0: [
                        neon.amount.clamp(0.0, 100.0),
                        neon.edge_width.clamp(0.5, 8.0),
                        neon.detail.clamp(0.0, 100.0),
                        neon.glow.clamp(0.0, 100.0),
                    ],
                    adjust_1: [
                        neon.color[0].clamp(0.0, 1.0),
                        neon.color[1].clamp(0.0, 1.0),
                        neon.color[2].clamp(0.0, 1.0),
                        neon.background.clamp(0.0, 100.0),
                    ],
                    ..MaskData::zeroed()
                };
                continue;
            }
            let adjustment = mask.adjustments;
            // Placeholder effect types retain any adjustment values so a user
            // can switch back without losing work, but they must not apply
            // those hidden values to the image.
            let adjustment_enabled = mask.enabled && mask.effect.uses_adjustments();
            let has_hsl = adjustment.has_color_mixer();
            let curve_flags = adjustment.curve_feature_flags();
            let has_grading = adjustment.has_color_grading();
            let has_hue = adjustment.hue.abs() > 1e-6;
            let (hsl_hue_0, hsl_hue_1) = split_eight(adjustment.hsl_hue);
            let (hsl_saturation_0, hsl_saturation_1) = split_eight(adjustment.hsl_saturation);
            let (hsl_luminance_0, hsl_luminance_1) = split_eight(adjustment.hsl_luminance);
            mask_data[index] = MaskData {
                metadata: [
                    u32::from(adjustment_enabled),
                    u32::from(!adjustment.is_neutral()),
                    curve_flags,
                    u32::from(has_hsl) | (u32::from(has_grading) << 1) | (u32::from(has_hue) << 2),
                ],
                adjust_0: [
                    adjustment.exposure.clamp(-5.0, 5.0),
                    adjustment.contrast.clamp(-100.0, 100.0),
                    adjustment.highlights.clamp(-100.0, 100.0),
                    adjustment.shadows.clamp(-100.0, 100.0),
                ],
                adjust_1: [
                    adjustment.whites.clamp(-100.0, 100.0),
                    adjustment.blacks.clamp(-100.0, 100.0),
                    adjustment.temperature.clamp(-100.0, 100.0),
                    adjustment.tint.clamp(-100.0, 100.0),
                ],
                adjust_2: [
                    adjustment.saturation.clamp(-100.0, 100.0),
                    adjustment.texture.clamp(-100.0, 100.0),
                    adjustment.clarity.clamp(-100.0, 100.0),
                    adjustment.dehaze.clamp(-100.0, 100.0),
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
            };
        }
        let (hsl_hue_0, hsl_hue_1) = split_eight(exposure.hsl_hue);
        let (hsl_saturation_0, hsl_saturation_1) = split_eight(exposure.hsl_saturation);
        let (hsl_luminance_0, hsl_luminance_1) = split_eight(exposure.hsl_luminance);
        let highlight_method = shader_highlight_method(raw.cfa_kind, exposure.highlight_method);
        let opposed_chroma = if highlight_method >= 1.5 {
            raw.inpaint_opposed_chroma(
                exposure.black_point,
                exposure.highlight_clip,
                exposure.ai_denoise_enabled,
            )
        } else {
            [0.0; 3]
        };

        let camera = CameraUniforms {
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
            _pad_0: 0.0,
            _pad_1: 0.0,
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
            inpaint_wb_0: inpaint_wb_transform[0],
            inpaint_wb_1: inpaint_wb_transform[1],
            inpaint_wb_2: inpaint_wb_transform[2],
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
        };
        let scene_tone = SceneToneUniforms {
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
            mask_counts: [masks.masks.len().min(MAX_LOCAL_MASKS) as u32, 0, 0, 0],
            grade_shadows: pack_color_grade_wheel(exposure.color_grading.shadows),
            grade_midtones: pack_color_grade_wheel(exposure.color_grading.midtones),
            grade_highlights: pack_color_grade_wheel(exposure.color_grading.highlights),
            grade_global: pack_color_grade_wheel(exposure.color_grading.global),
            grade_options: pack_view_color_options(exposure.color_grading, exposure.hue),
            rec2020_to_xyz: shader_tuning.rec2020_to_xyz,
            xyz_to_rec2020: shader_tuning.xyz_to_rec2020,
            xyz_to_bradford: shader_tuning.xyz_to_bradford,
            bradford_to_xyz: shader_tuning.bradford_to_xyz,
        };
        // Global and masked Glow share one linear diffusion chain. Using the
        // widest active request preserves every halo's support while avoiding
        // another full-resolution texture stack for a non-destructive local
        // effect.
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
        let effects = EffectsUniforms {
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
            vignette_dark_half_fit: shader_tuning.vignette_dark_half_fit,
            vignette_dark_full_fit: shader_tuning.vignette_dark_full_fit,
            vignette_light_half_fit: shader_tuning.vignette_light_half_fit,
            vignette_light_full_fit: shader_tuning.vignette_light_full_fit,
            capture_scale_sigma: shader_tuning.capture_scale_sigma,
            capture_thresholds: shader_tuning.capture_thresholds,
            capture_mask_coherence: shader_tuning.capture_mask_coherence,
        };
        Self {
            camera,
            scene_tone,
            effects,
            mask_data: Box::new(mask_data),
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

        // Match GeometryInverseMap / preview geometry exactly: forward mapping
        // is quarter-turn * rotation * shear * flip. Convert source-normalized
        // deltas directly into final-frame normalized deltas so the shader can
        // evaluate the vignette before geometry resampling without baking it
        // into the source image's orientation.
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

    /// Replaces shader calibration lanes. A subsequent `recompute` uploads only
    /// the changed stage uniform buffers; compute pipelines are reused.
    pub fn with_shader_tuning(mut self, tuning: GpuShaderTuning) -> Self {
        self.set_shader_tuning(tuning);
        self
    }

    pub fn set_shader_tuning(&mut self, tuning: GpuShaderTuning) {
        self.scene_tone.rec2020_to_xyz = tuning.rec2020_to_xyz;
        self.scene_tone.xyz_to_rec2020 = tuning.xyz_to_rec2020;
        self.scene_tone.xyz_to_bradford = tuning.xyz_to_bradford;
        self.scene_tone.bradford_to_xyz = tuning.bradford_to_xyz;
        self.effects.vignette_dark_half_fit = tuning.vignette_dark_half_fit;
        self.effects.vignette_dark_full_fit = tuning.vignette_dark_full_fit;
        self.effects.vignette_light_half_fit = tuning.vignette_light_half_fit;
        self.effects.vignette_light_full_fit = tuning.vignette_light_full_fit;
        self.effects.capture_scale_sigma = tuning.capture_scale_sigma;
        self.effects.capture_thresholds = tuning.capture_thresholds;
        self.effects.capture_mask_coherence = tuning.capture_mask_coherence;
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

    /// Maps the normalized local-mask atlas to a source-image sub-rectangle.
    /// Coordinates are packed as UNORM16 pairs into fields that were already
    /// reserved in the GPU ABI, giving sub-pixel precision on ordinary RAWs
    /// without growing the scene-tone uniform block.
    pub fn with_mask_uv_rect(mut self, rect: [f32; 4]) -> Self {
        self.set_mask_uv_rect(rect);
        // All atlas texels are valid. This sentinel avoids coupling ordinary
        // callers to a pipeline's texture dimensions.
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
        // Saturation and Vibrance live in apply_scene_effects_node alongside the
        // presence controls. They must therefore keep the intermediate passes
        // enabled even when Texture, Clarity, Dehaze, and Glow are all neutral.
        // Vignette runs in the always-dispatched display-linear view pass.
        // Capture sharpening now lives in the always-run pre-tone sharpen/tone
        // pass and must not force these later optional passes.
        // Omitting Saturation/Vibrance here made both global color
        // sliders a no-op.
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
            {
                return true;
            }

            // The local tone shader is physically part of the optional
            // intermediate chain. Keep that chain scheduled when any control
            // handled by apply_local_scene_tone_node is active. Exposure is
            // already applied by prepare_scene_node, Blacks is deferred to the
            // display-linear view node, and local mixer/grading run in the
            // always-dispatched view pass, so none of those need this gate.
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
    workgroup_size: ComputeWorkgroupSize,
}

#[derive(Clone)]
pub struct RawGpuProgramTemplate {
    cfa_kind: CfaKind,
    processing_quality: ProcessingQuality,
    workgroup_size: ComputeWorkgroupSize,
    pipelines: Vec<wgpu::ComputePipeline>,
    pipeline_cache: Option<Arc<PersistentGpuPipelineCache>>,
}

/// Shared completion state for Android's startup export-program prewarm.
/// Export workers can wait on this without blocking the UI thread, then reuse
/// the same immutable compute-pipeline handles for every subsequent export.
pub struct GpuProgramPrewarm {
    result: Mutex<Option<std::result::Result<Arc<RawGpuProgramTemplate>, String>>>,
    ready: Condvar,
}

impl GpuProgramPrewarm {
    #[cfg(target_os = "android")]
    pub fn new() -> Self {
        Self {
            result: Mutex::new(None),
            ready: Condvar::new(),
        }
    }

    #[cfg(target_os = "android")]
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

pub struct RawGpuPipeline {
    pub egui_texture_id: Option<egui::TextureId>,
    pub width: u32,
    pub height: u32,
    cfa_kind: CfaKind,
    processing_quality: ProcessingQuality,
    workgroup_size: ComputeWorkgroupSize,
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
    has_ai_scene: bool,
    has_ai_cfa: bool,
    display_linear_texture: wgpu::Texture,
    _tone_guide_a: wgpu::Texture,
    _tone_guide_b: wgpu::Texture,
    mask_texture: wgpu::Texture,
    mask_layer_capacity: usize,
    inpaint_texture: wgpu::Texture,
    legacy_inpaint_camera_to_working: [[f32; 4]; 3],
    mask_atlas_edge: u32,
    profile_buffer: wgpu::Buffer,
    profile_buffer_size_bytes: u64,
    output_lut_offset_bytes: u64,
    out_texture: wgpu::Texture,
    _out_view: wgpu::TextureView,
    pipeline_cache: Option<Arc<PersistentGpuPipelineCache>>,
    // Declared last so GPU textures/buffers are dropped before process-wide
    // admission capacity is returned.
    _gpu_budget_reservation: GpuBudgetReservation,
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
                // A poisoned cache must not prevent rendering. Fall back to a
                // complete upload while preserving the stage-specific buffers.
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
            workgroup_size: self.workgroup_size,
            pipelines: self
                .passes
                .iter()
                .map(|pass| pass.pipeline.clone())
                .collect(),
            pipeline_cache: self.pipeline_cache.clone(),
        }
    }

    #[cfg(target_os = "android")]
    fn into_program_template(self) -> RawGpuProgramTemplate {
        RawGpuProgramTemplate {
            cfa_kind: self.cfa_kind,
            processing_quality: self.processing_quality,
            workgroup_size: self.workgroup_size,
            pipelines: self.passes.into_iter().map(|pass| pass.pipeline).collect(),
            pipeline_cache: self.pipeline_cache,
        }
    }

    /// Compiles the complete interactive preview compute program set against a
    /// tiny synthetic RAW. The returned pipeline is only a program template:
    /// later real RAWs allocate their own textures/bind groups while cloning
    /// these already-compiled compute pipeline handles. This keeps startup
    /// prewarming independent of image resolution and has no effect on output.
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

    /// Compiles the complete full-quality export compute program set against a
    /// tiny synthetic RAW. The returned pipeline is retained only as a program
    /// template, allowing tiled export to clone already-compiled pipeline
    /// handles while allocating its own image-sized resources and mask atlas.
    #[cfg(target_os = "android")]
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
        Self::new_internal(
            device,
            queue,
            None,
            None,
            pipeline_cache,
            &raw,
            &params,
            quality,
            RawGpuPipelineConfig {
                // When requested for the export template, keep the local-mask
                // allocation small. Its textures are never rendered; only the
                // compiled program handles are retained and reused later.
                mask_atlas_edge_override,
                ..Default::default()
            },
        )
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
        Self::new_internal(
            device,
            queue,
            Some(renderer),
            None,
            None,
            raw,
            params,
            quality,
            RawGpuPipelineConfig::default(),
        )
    }

    pub fn new_headless_with_quality(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
    ) -> Result<Self> {
        Self::new_internal(
            device,
            queue,
            None,
            None,
            None,
            raw,
            params,
            quality,
            RawGpuPipelineConfig::default(),
        )
    }

    /// Creates a headless pipeline whose 2D compute entrypoints and dispatch
    /// counts use the same explicit workgroup shape. This is intended for
    /// device-specific benchmarking; normal callers retain the 8x8 default.
    pub fn new_headless_with_quality_and_workgroup_size(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        workgroup_size: ComputeWorkgroupSize,
    ) -> Result<Self> {
        Self::new_internal(
            device,
            queue,
            None,
            None,
            None,
            raw,
            params,
            quality,
            RawGpuPipelineConfig {
                workgroup_size,
                ..Default::default()
            },
        )
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
            None,
            raw,
            params,
            quality,
            RawGpuPipelineConfig {
                mask_atlas_edge_override: Some(mask_edge),
                ..Default::default()
            },
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
        let program_template = template.program_template();
        Self::new_internal(
            device,
            queue,
            None,
            Some(&program_template),
            program_template.pipeline_cache.clone(),
            raw,
            params,
            quality,
            RawGpuPipelineConfig {
                workgroup_size: program_template.workgroup_size,
                ..Default::default()
            },
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
        let program_template = template.program_template();
        Self::new_internal(
            device,
            queue,
            None,
            Some(&program_template),
            program_template.pipeline_cache.clone(),
            raw,
            params,
            quality,
            RawGpuPipelineConfig {
                mask_atlas_edge_override: Some(mask_edge),
                workgroup_size: program_template.workgroup_size,
            },
        )
    }

    /// Allocates export-sized resources while cloning compute pipelines from a
    /// lightweight startup-prewarmed program template.
    pub fn new_headless_reusing_program_template_with_mask_edge(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        template: &RawGpuProgramTemplate,
        mask_edge: u32,
    ) -> Result<Self> {
        Self::new_internal(
            device,
            queue,
            None,
            Some(template),
            template.pipeline_cache.clone(),
            raw,
            params,
            quality,
            RawGpuPipelineConfig {
                mask_atlas_edge_override: Some(mask_edge),
                workgroup_size: template.workgroup_size,
            },
        )
    }

    /// Reuses an owned program template for a normal interactive preview.
    /// Keeping this template separate from image-sized textures lets preview
    /// surfaces be replaced without recompiling the complete shader graph.
    pub fn new_headless_reusing_program_template(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        template: &RawGpuProgramTemplate,
    ) -> Result<Self> {
        Self::new_internal(
            device,
            queue,
            None,
            Some(template),
            template.pipeline_cache.clone(),
            raw,
            params,
            quality,
            RawGpuPipelineConfig {
                workgroup_size: template.workgroup_size,
                ..Default::default()
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_internal(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mut renderer: Option<&mut egui_wgpu::Renderer>,
        program_template: Option<&RawGpuProgramTemplate>,
        pipeline_cache: Option<Arc<PersistentGpuPipelineCache>>,
        raw: &LoadedRaw,
        params: &GpuParams,
        quality: ProcessingQuality,
        config: RawGpuPipelineConfig,
    ) -> Result<Self> {
        validate_raw(raw)?;
        config
            .workgroup_size
            .validate_for_limits(&device.limits())?;
        if let Some(template) = program_template {
            if template.cfa_kind != raw.cfa_kind
                || template.processing_quality != quality
                || template.workgroup_size != config.workgroup_size
                || template.pipelines.len() != expected_pass_count(raw.cfa_kind)
            {
                return Err(anyhow!(
                    "cannot reuse GPU programs from an incompatible pipeline"
                ));
            }
        }

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
        let image_workgroups = config
            .workgroup_size
            .dispatch_for_extent(raw.width, raw.height);
        let tone_workgroups = config
            .workgroup_size
            .dispatch_for_extent(tone_size.width, tone_size.height);
        let single_workgroup = [1, 1, 1];

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
            tone_scale,
            mask_atlas_edge,
            mask_layers: u32::try_from(mask_layer_capacity)
                .map_err(|_| anyhow!("mask layer capacity does not fit in u32"))?,
            profile_buffer_bytes: profile_buffer_size_bytes,
            stage_uniform_buffer_bytes: GPU_STAGE_UNIFORM_ALLOCATION_BYTES,
            mask_data_buffer_bytes: MASK_DATA_SIZE_BYTES,
        })?;
        let gpu_budget_reservation =
            GpuBudgetReservation::acquire(&resource_plan, gpu_working_set_limit_bytes())?;

        // wgpu's create_* methods do not return allocation errors. Capture the
        // complete construction sequence so a driver OOM becomes this
        // constructor's Result instead of reaching wgpu's fatal default handler.
        let gpu_error_scopes = GpuErrorScopes::push(device);

        // Admission succeeds before the first device allocation, including all
        // other live main/detail/navigation/headless pipelines in this process. Every persistent
        // allocation below has a corresponding named entry in `resource_plan`;
        // on-demand conversion/readback peaks and the 20% safety margin are also
        // reserved before construction begins.
        let ai_image = raw.ai_denoised_image();
        let ai_cfa = params
            .uses_ai_denoise()
            .then(|| ai_image.as_ref().and_then(AiDenoisedImage::bayer_cfa))
            .flatten();
        let has_ai_cfa = ai_cfa.is_some();
        let raw_texture = create_raw_texture(
            device,
            queue,
            raw,
            ai_cfa.unwrap_or(raw.raw_pixels.as_slice()),
        );
        let color_texture = create_color_texture(device, queue, raw);
        let black_texture = create_black_texture(device, queue, raw);

        // All demosaic stages sample this canonical reconstructed CFA.
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
        // display pass. This is what lets local Lightroom controls read true
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
        // The ordinary full-frame interactive pipeline reserves all 32 layers
        // so masks can be added without rebuilding it. Explicit-edge detail and
        // export pipelines allocate only the layers they actually sample.
        let mask_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw normalized local-mask atlas"),
            size: wgpu::Extent3d {
                width: mask_atlas_edge,
                height: mask_atlas_edge,
                depth_or_array_layers: mask_layer_capacity as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::R16Float],
        });
        // Do not upload an all-zero atlas here. With 32 supported layers that would
        // create a very large temporary CPU allocation. Every active layer is uploaded
        // before the first recompute, and shaders never sample layers beyond mask_counts.x.

        let inpaint_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw pre-adjustment inpaint layer"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[wgpu::TextureFormat::Rgba16Float],
        });
        let empty_inpaint_len = usize::try_from(
            u64::from(raw.width)
                .checked_mul(u64::from(raw.height))
                .and_then(|value| value.checked_mul(4))
                .ok_or_else(|| anyhow!("zero inpaint upload length overflows"))?,
        )
        .map_err(|_| anyhow!("zero inpaint upload length does not fit in usize"))?;
        let empty_inpaint = vec![0u16; empty_inpaint_len];
        queue.write_texture(
            copy_texture(&inpaint_texture),
            bytemuck::cast_slice(&empty_inpaint),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(
                    raw.width
                        .checked_mul(8)
                        .ok_or_else(|| anyhow!("inpaint upload row byte count overflows"))?,
                ),
                rows_per_image: Some(raw.height),
            },
            size,
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
        let inpaint_view = inpaint_texture.create_view(&wgpu::TextureViewDescriptor::default());
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

        let output_lut_offset_bytes =
            u64::from(profile_gpu_data.layout.output[3]) * std::mem::size_of::<[f32; 4]>() as u64;
        let profile_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("auraw DCP and ICC profile LUTs"),
            contents: bytemuck::cast_slice(&profile_gpu_data.words),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let camera_uniforms_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("auraw camera uniforms"),
            contents: params.camera_bytes(),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let scene_tone_uniforms_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("auraw scene-tone uniforms"),
                contents: params.scene_tone_bytes(),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let effects_uniforms_buffer =
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("auraw effects uniforms"),
                contents: params.effects_bytes(),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let mask_data_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("auraw local-mask data"),
            contents: params.mask_data_bytes(),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
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

        // Groups 1 and 2 are intentionally identical for every compute
        // pipeline. Reusing them across passes isolates scene-tone and effects
        // updates from the camera/raw resource bind groups in group 0.
        let bgl_scene_tone = program_template
            .map(|template| template.pipelines[0].get_bind_group_layout(1))
            .unwrap_or_else(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("bgl scene-tone uniforms"),
                    entries: &[buffer_entry(0)],
                })
            });
        let bgl_effects = program_template
            .map(|template| template.pipelines[0].get_bind_group_layout(2))
            .unwrap_or_else(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("bgl effects uniforms"),
                    entries: &[buffer_entry(0)],
                })
            });

        let scene_tone_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg scene-tone uniforms"),
            layout: &bgl_scene_tone,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: scene_tone_uniforms_buffer.as_entire_binding(),
            }],
        });
        let effects_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg effects uniforms"),
            layout: &bgl_effects,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: effects_uniforms_buffer.as_entire_binding(),
            }],
        });

        let demosaic_start_for_programs = 1;
        let demosaic_high_pass_count = match raw.cfa_kind {
            CfaKind::Bayer => 3,
            CfaKind::XTrans => 7,
        };
        let dual_green_for_programs = demosaic_start_for_programs + demosaic_high_pass_count;
        let dual_rgb_for_programs = dual_green_for_programs + 1;
        let demosaic_finish_for_programs = dual_rgb_for_programs + 1;
        let color_denoise_for_programs = demosaic_finish_for_programs + 1;
        let tone_prepare_for_programs =
            color_denoise_for_programs + COLOR_DENOISE_ENTRY_POINTS.len();
        let adjustment_prepare_for_programs = tone_prepare_for_programs + 4;
        let reused_layout = |pass_index: usize| {
            program_template.map(|template| template.pipelines[pass_index].get_bind_group_layout(0))
        };

        let bgl_highlights = reused_layout(0).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl highlights"),
                entries: &[
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
            })
        });

        let bgl1 = reused_layout(demosaic_start_for_programs).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl1"),
                entries: &[
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
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
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
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
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
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

        let bgl_dual_green = reused_layout(dual_green_for_programs).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl dual demosaic green"),
                entries: &[
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        20,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            })
        });

        let bgl_dual_rgb = reused_layout(dual_rgb_for_programs).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl dual demosaic rgb"),
                entries: &[
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(21, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        22,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            })
        });

        let bgl4 = (matches!(raw.cfa_kind, CfaKind::Bayer)
            .then(|| reused_layout(demosaic_finish_for_programs))
            .flatten())
        .unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl4"),
                entries: &[
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(7, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(9, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(23, wgpu::TextureSampleType::Float { filterable: false }),
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
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(9, wgpu::TextureSampleType::Float { filterable: false }),
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
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(27, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(28, wgpu::TextureSampleType::Float { filterable: false }),
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
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(9, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(29, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(30, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        26,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            })
        });

        let bgl_xtrans_finish = (matches!(raw.cfa_kind, CfaKind::XTrans)
            .then(|| reused_layout(demosaic_finish_for_programs))
            .flatten())
        .unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl X-Trans finish"),
                entries: &[
                    common_entries[0],
                    common_entries[1],
                    common_entries[2],
                    common_entries[3],
                    texture_entry(3, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(26, wgpu::TextureSampleType::Float { filterable: false }),
                    texture_entry(23, wgpu::TextureSampleType::Float { filterable: false }),
                    storage_texture_entry(
                        10,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                ],
            })
        });

        let bgl_color_denoise = reused_layout(color_denoise_for_programs).unwrap_or_else(|| {
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("bgl multiscale color denoise"),
                entries: &[
                    buffer_entry(0),
                    storage_texture_entry(
                        10,
                        demosaic_format,
                        wgpu::StorageTextureAccess::WriteOnly,
                    ),
                    texture_entry(11, wgpu::TextureSampleType::Float { filterable: false }),
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
                    texture_entry(32, wgpu::TextureSampleType::Float { filterable: false }),
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
                    label: Some("bgl scene preparation"),
                    entries: &[
                        common_entries[0],
                        common_entries[1],
                        common_entries[2],
                        common_entries[3],
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
                        texture_entry(32, wgpu::TextureSampleType::Float { filterable: false }),
                        storage_buffer_entry(33, true),
                    ],
                })
            });

        let bgl_adjust_tone =
            reused_layout(adjustment_prepare_for_programs + 1).unwrap_or_else(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("bgl scene tone edits"),
                    entries: &[
                        buffer_entry(0),
                        texture_entry(22, wgpu::TextureSampleType::Float { filterable: false }),
                        storage_texture_entry(
                            23,
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
                        storage_buffer_entry(33, true),
                    ],
                })
            });

        let bgl_adjust_effects =
            reused_layout(adjustment_prepare_for_programs + 3).unwrap_or_else(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("bgl scene presence and color"),
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
                        storage_buffer_entry(33, true),
                    ],
                })
            });

        let bgl_glow_prepare =
            reused_layout(adjustment_prepare_for_programs + 5).unwrap_or_else(|| {
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
                        texture_array_entry(
                            27,
                            wgpu::TextureSampleType::Float { filterable: true },
                        ),
                        sampler_entry(28),
                        storage_buffer_entry(33, true),
                    ],
                })
            });

        let bgl_glow_blur =
            reused_layout(adjustment_prepare_for_programs + 6).unwrap_or_else(|| {
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

        let bgl_adjust_creative = reused_layout(adjustment_prepare_for_programs + 11)
            .unwrap_or_else(|| {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("bgl creative glow"),
                    entries: &[
                        buffer_entry(0),
                        texture_entry(24, wgpu::TextureSampleType::Float { filterable: false }),
                        storage_texture_entry(
                            25,
                            work_format,
                            wgpu::StorageTextureAccess::WriteOnly,
                        ),
                        texture_entry(30, wgpu::TextureSampleType::Float { filterable: false }),
                        texture_array_entry(
                            27,
                            wgpu::TextureSampleType::Float { filterable: true },
                        ),
                        sampler_entry(28),
                        storage_buffer_entry(33, true),
                    ],
                })
            });

        let bgl_adjust_render = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("bgl scene look view and output"),
            entries: &[
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
        });
        let bgl_adjust_render =
            reused_layout(adjustment_prepare_for_programs + 12).unwrap_or(bgl_adjust_render);

        let bg_highlights = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg highlight reconstruction"),
            layout: &bgl_highlights,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
            ],
        });

        let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg1"),
            layout: &bgl1,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                    resource: camera_uniforms_buffer.as_entire_binding(),
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

        let (dual_green_view, dual_low_view) = match raw.cfa_kind {
            CfaKind::Bayer => (&highlight_work_a_view, &highlight_work_b_view),
            CfaKind::XTrans => (&tex1_view, &tex2_view),
        };

        let bg_dual_green = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg dual demosaic green"),
            layout: &bgl_dual_green,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                    resource: wgpu::BindingResource::TextureView(dual_green_view),
                },
            ],
        });

        let bg_dual_rgb = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg dual demosaic rgb"),
            layout: &bgl_dual_rgb,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                    binding: 21,
                    resource: wgpu::BindingResource::TextureView(dual_green_view),
                },
                wgpu::BindGroupEntry {
                    binding: 22,
                    resource: wgpu::BindingResource::TextureView(dual_low_view),
                },
            ],
        });

        let bg4 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg4"),
            layout: &bgl4,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                    binding: 23,
                    resource: wgpu::BindingResource::TextureView(dual_low_view),
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
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                    binding: 9,
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
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                    binding: 27,
                    resource: wgpu::BindingResource::TextureView(&highlight_work_a_view),
                },
                wgpu::BindGroupEntry {
                    binding: 28,
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
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                    binding: 9,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
                wgpu::BindGroupEntry {
                    binding: 29,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 30,
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
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                    binding: 23,
                    resource: wgpu::BindingResource::TextureView(dual_low_view),
                },
                wgpu::BindGroupEntry {
                    binding: 10,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
            ],
        });

        let make_color_denoise_bind_group =
            |label: &str, read_view: &wgpu::TextureView, write_view: &wgpu::TextureView| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some(label),
                    layout: &bgl_color_denoise,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: camera_uniforms_buffer.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 10,
                            resource: wgpu::BindingResource::TextureView(write_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 11,
                            resource: wgpu::BindingResource::TextureView(read_view),
                        },
                    ],
                })
            };
        // Six passes end back in scene_texture. Disabled Fast/Balanced scales
        // are explicit copies so every quality setting has identical parity.
        let bg_color_denoise_0 =
            make_color_denoise_bind_group("bg color denoise scale 1", &scene_view, &tex1_view);
        let bg_color_denoise_1 =
            make_color_denoise_bind_group("bg color denoise scale 2", &tex1_view, &tex2_view);
        let bg_color_denoise_2 =
            make_color_denoise_bind_group("bg color denoise scale 4", &tex2_view, &tex1_view);
        let bg_color_denoise_3 =
            make_color_denoise_bind_group("bg color denoise scale 8", &tex1_view, &tex2_view);
        let bg_color_denoise_4 =
            make_color_denoise_bind_group("bg color denoise scale 16", &tex2_view, &tex1_view);
        let bg_color_denoise_5 =
            make_color_denoise_bind_group("bg color denoise scale 32", &tex1_view, &scene_view);

        let bg_tone_prepare = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg tone prepare"),
            layout: &bgl_tone_prepare,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                wgpu::BindGroupEntry {
                    binding: 32,
                    resource: wgpu::BindingResource::TextureView(&inpaint_view),
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
                            resource: camera_uniforms_buffer.as_entire_binding(),
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
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                wgpu::BindGroupEntry {
                    binding: 32,
                    resource: wgpu::BindingResource::TextureView(&inpaint_view),
                },
                wgpu::BindGroupEntry {
                    binding: 33,
                    resource: mask_data_buffer.as_entire_binding(),
                },
            ],
        });

        let bg_adjust_tone = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg scene tone edits"),
            layout: &bgl_adjust_tone,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                wgpu::BindGroupEntry {
                    binding: 33,
                    resource: mask_data_buffer.as_entire_binding(),
                },
            ],
        });

        let bg_adjust_local_tone = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg local scene tone edits"),
            layout: &bgl_adjust_tone,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 22,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
                wgpu::BindGroupEntry {
                    binding: 23,
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
                wgpu::BindGroupEntry {
                    binding: 33,
                    resource: mask_data_buffer.as_entire_binding(),
                },
            ],
        });

        let bg_adjust_effects = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg scene presence and color"),
            layout: &bgl_adjust_effects,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
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
                wgpu::BindGroupEntry {
                    binding: 33,
                    resource: mask_data_buffer.as_entire_binding(),
                },
            ],
        });

        let bg_adjust_effects_copy = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg scene effects copy"),
            layout: &bgl_adjust_effects,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 22,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
                wgpu::BindGroupEntry {
                    binding: 23,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
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
                wgpu::BindGroupEntry {
                    binding: 33,
                    resource: mask_data_buffer.as_entire_binding(),
                },
            ],
        });

        // Glow is extracted from the completed local-effects image in tex1.
        // Five adjacent B3-spline diffusion stages then ping-pong through tex2
        // and the display-linear surface. The latter is safe scratch here: the
        // final render overwrites it only after the creative composite.
        let bg_glow_prepare = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg Glow source extraction"),
            layout: &bgl_glow_prepare,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 24,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 31,
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
                wgpu::BindGroupEntry {
                    binding: 33,
                    resource: mask_data_buffer.as_entire_binding(),
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
                            resource: camera_uniforms_buffer.as_entire_binding(),
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
            make_glow_blur_bind_group("bg Glow diffusion 0", &tex2_view, &display_linear_view);
        let bg_glow_blur_1 =
            make_glow_blur_bind_group("bg Glow diffusion 1", &display_linear_view, &tex2_view);
        let bg_glow_blur_2 =
            make_glow_blur_bind_group("bg Glow diffusion 2", &tex2_view, &display_linear_view);
        let bg_glow_blur_3 =
            make_glow_blur_bind_group("bg Glow diffusion 3", &display_linear_view, &tex2_view);
        let bg_glow_blur_4 =
            make_glow_blur_bind_group("bg Glow diffusion 4", &tex2_view, &display_linear_view);

        // The creative pass keeps the untouched local-effects result in tex1,
        // composites the final Glow diffusion from display_linear and writes
        // the result into tex2. The post-crop vignette is applied later in the
        // always-dispatched display-linear view pass.
        let bg_adjust_creative = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg creative glow"),
            layout: &bgl_adjust_creative,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 24,
                    resource: wgpu::BindingResource::TextureView(&tex1_view),
                },
                wgpu::BindGroupEntry {
                    binding: 25,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
                wgpu::BindGroupEntry {
                    binding: 30,
                    resource: wgpu::BindingResource::TextureView(&display_linear_view),
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
                    binding: 33,
                    resource: mask_data_buffer.as_entire_binding(),
                },
            ],
        });

        let bg_adjust_render = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bg scene look view and output"),
            layout: &bgl_adjust_render,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: camera_uniforms_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 12,
                    resource: wgpu::BindingResource::TextureView(&out_view),
                },
                wgpu::BindGroupEntry {
                    binding: 26,
                    resource: wgpu::BindingResource::TextureView(&tex2_view),
                },
                wgpu::BindGroupEntry {
                    binding: 16,
                    resource: tone_stats_buffer.as_entire_binding(),
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
                wgpu::BindGroupEntry {
                    binding: 33,
                    resource: mask_data_buffer.as_entire_binding(),
                },
            ],
        });

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

        let mut shader_manager = program_template
            .is_none()
            .then(|| ShaderManager::new_with_workgroup_size(work_format, config.workgroup_size))
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
        let highlight_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw highlight module",
                    SHADER_HIGHLIGHTS,
                    "highlights.wgsl",
                )
            })
            .transpose()?;
        let bayer_rcd_p1_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw Bayer RCD pass 1",
                    bayer_rcd_p1.as_ref(),
                    "pass1.wgsl",
                )
            })
            .transpose()?;
        let bayer_rcd_p2_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw Bayer RCD pass 2",
                    bayer_rcd_p2.as_ref(),
                    "pass2.wgsl",
                )
            })
            .transpose()?;
        let bayer_rcd_p3_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw Bayer RCD pass 3",
                    bayer_rcd_p3.as_ref(),
                    "pass3.wgsl",
                )
            })
            .transpose()?;
        let bayer_rcd_p4_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw Bayer RCD pass 4",
                    bayer_rcd_p4.as_ref(),
                    "pass4.wgsl",
                )
            })
            .transpose()?;
        let dual_demosaic_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw robust dual demosaic",
                    dual_demosaic.as_ref(),
                    "dual_demosaic.wgsl",
                )
            })
            .transpose()?;
        let xtrans_demosaic_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw grouped X-Trans demosaic",
                    xtrans_demosaic.as_ref(),
                    "xtrans_demosaic.wgsl",
                )
            })
            .transpose()?;
        let xtrans_finish_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw X-Trans finish",
                    xtrans_finish.as_ref(),
                    "xtrans_finish.wgsl",
                )
            })
            .transpose()?;
        let color_denoise_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw multiscale color denoise",
                    color_denoise_shader.as_ref(),
                    "color_denoise.wgsl",
                )
            })
            .transpose()?;
        let tone_analysis_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw tone analysis",
                    SHADER_TONE_ANALYSIS,
                    "tone_analysis.wgsl",
                )
            })
            .transpose()?;
        let scene_adjustments_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw scene adjustments",
                    scene_adjustments_shader.as_ref(),
                    "scene_adjustments.wgsl",
                )
            })
            .transpose()?;
        let creative_effects_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw creative effects",
                    SHADER_CREATIVE_EFFECTS,
                    "creative_effects.wgsl",
                )
            })
            .transpose()?;
        let view_transform_module = program_template
            .is_none()
            .then(|| {
                create_shader(
                    "auraw view transform",
                    SHADER_VIEW_TRANSFORM,
                    "view_transform.wgsl",
                )
            })
            .transpose()?;
        debug_assert!(explicit_render_graph_contracts_are_contiguous());

        let mut next_program_index = 0usize;
        let mut make_pipeline = |shader: Option<&wgpu::ShaderModule>,
                                 entry: &str,
                                 bgl: &wgpu::BindGroupLayout|
         -> wgpu::ComputePipeline {
            let program_index = next_program_index;
            next_program_index += 1;
            if let Some(template) = program_template {
                return template.pipelines[program_index].clone();
            }
            let shader = shader.expect("shader module exists without a program template");
            let pll = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&format!("pll_{}", entry)),
                bind_group_layouts: &[Some(bgl), Some(&bgl_scene_tone), Some(&bgl_effects)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pll),
                module: shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: pipeline_cache.as_ref().map(|cache| cache.raw()),
            })
        };

        let mut passes = Vec::with_capacity(expected_pass_count(raw.cfa_kind));

        // Reconstruct clipped photosites before every demosaic path.
        passes.push(Pass {
            pipeline: make_pipeline(
                highlight_module.as_ref(),
                "highlight_reconstruct",
                &bgl_highlights,
            ),
            bind_group: bg_highlights,
            workgroups: image_workgroups,
        });

        let demosaic_start_index = passes.len();
        // Build the high-detail reference first. The robust low-frequency
        // branch is represented by two real full-frame buffers, but its two
        // dispatches are skipped at encode time unless Dual mode is selected.
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
            ]),
            CfaKind::XTrans => passes.extend([
                Pass {
                    pipeline: make_pipeline(xtrans_demosaic_module.as_ref(), "xtrans_seed", &bgl1),
                    bind_group: bg1,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_demosaic_module.as_ref(),
                        "xtrans_markesteijn_pass1",
                        &bgl2,
                    ),
                    bind_group: bg2.clone(),
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_demosaic_module.as_ref(),
                        "xtrans_markesteijn_pass2",
                        &bgl3,
                    ),
                    bind_group: bg3,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_demosaic_module.as_ref(),
                        "xtrans_markesteijn_pass3",
                        &bgl2,
                    ),
                    bind_group: bg2,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_demosaic_module.as_ref(),
                        "xtrans_markesteijn_derivatives",
                        &bgl_xtrans_derivatives,
                    ),
                    bind_group: bg_xtrans_derivatives,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_demosaic_module.as_ref(),
                        "xtrans_markesteijn_homogeneity",
                        &bgl_xtrans_homogeneity,
                    ),
                    bind_group: bg_xtrans_homogeneity,
                    workgroups: image_workgroups,
                },
                Pass {
                    pipeline: make_pipeline(
                        xtrans_demosaic_module.as_ref(),
                        "xtrans_markesteijn_accumulate",
                        &bgl_xtrans_accumulate,
                    ),
                    bind_group: bg_xtrans_accumulate,
                    workgroups: image_workgroups,
                },
            ]),
        }

        let demosaic_dual_start_index = passes.len();
        passes.extend([
            Pass {
                pipeline: make_pipeline(
                    dual_demosaic_module.as_ref(),
                    "dual_green_reconstruct",
                    &bgl_dual_green,
                ),
                bind_group: bg_dual_green,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    dual_demosaic_module.as_ref(),
                    "dual_rgb_reconstruct",
                    &bgl_dual_rgb,
                ),
                bind_group: bg_dual_rgb,
                workgroups: image_workgroups,
            },
        ]);
        let demosaic_dual_end_index = passes.len();

        let demosaic_finish_index = passes.len();
        match raw.cfa_kind {
            CfaKind::Bayer => passes.push(Pass {
                pipeline: make_pipeline(bayer_rcd_p4_module.as_ref(), "bayer_rcd_output", &bgl4),
                bind_group: bg4,
                workgroups: image_workgroups,
            }),
            CfaKind::XTrans => passes.push(Pass {
                pipeline: make_pipeline(
                    xtrans_finish_module.as_ref(),
                    "xtrans_demosaic_finish",
                    &bgl_xtrans_finish,
                ),
                bind_group: bg_xtrans_finish,
                workgroups: image_workgroups,
            }),
        }

        let color_denoise_start_index = passes.len();
        let color_denoise_bind_groups = [
            bg_color_denoise_0,
            bg_color_denoise_1,
            bg_color_denoise_2,
            bg_color_denoise_3,
            bg_color_denoise_4,
            bg_color_denoise_5,
        ];
        for (entry, bind_group) in COLOR_DENOISE_ENTRY_POINTS
            .iter()
            .zip(color_denoise_bind_groups)
        {
            passes.push(Pass {
                pipeline: make_pipeline(color_denoise_module.as_ref(), entry, &bgl_color_denoise),
                bind_group,
                workgroups: image_workgroups,
            });
        }
        let color_denoise_end_index = passes.len();

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
        let adjustment_tone_pass_index = adjustment_prepare_pass_index + 1;
        let adjustment_effects_pass_index = adjustment_prepare_pass_index + 3;
        let glow_prepare_pass_index = adjustment_prepare_pass_index + 5;
        let glow_blur_start_index = adjustment_prepare_pass_index + 6;
        let glow_blur_end_index = glow_blur_start_index + 5;
        let adjustment_creative_pass_index = glow_blur_end_index;
        let adjustment_render_pass_index = adjustment_creative_pass_index + 1;

        passes.extend([
            Pass {
                pipeline: make_pipeline(
                    scene_adjustments_module.as_ref(),
                    "prepare_scene_node",
                    &bgl_adjust_prepare,
                ),
                bind_group: bg_adjust_prepare,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    scene_adjustments_module.as_ref(),
                    "apply_scene_tone_node",
                    &bgl_adjust_tone,
                ),
                bind_group: bg_adjust_tone,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    scene_adjustments_module.as_ref(),
                    "apply_local_scene_tone_node",
                    &bgl_adjust_tone,
                ),
                bind_group: bg_adjust_local_tone,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    creative_effects_module.as_ref(),
                    "apply_scene_effects_node",
                    &bgl_adjust_effects,
                ),
                bind_group: bg_adjust_effects,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    creative_effects_module.as_ref(),
                    "copy_scene_effects_node",
                    &bgl_adjust_effects,
                ),
                bind_group: bg_adjust_effects_copy,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    creative_effects_module.as_ref(),
                    "prepare_glow_source",
                    &bgl_glow_prepare,
                ),
                bind_group: bg_glow_prepare,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    creative_effects_module.as_ref(),
                    "diffuse_glow_0",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_0,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    creative_effects_module.as_ref(),
                    "diffuse_glow_1",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_1,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    creative_effects_module.as_ref(),
                    "diffuse_glow_2",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_2,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    creative_effects_module.as_ref(),
                    "diffuse_glow_3",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_3,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    creative_effects_module.as_ref(),
                    "diffuse_glow_4",
                    &bgl_glow_blur,
                ),
                bind_group: bg_glow_blur_4,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    creative_effects_module.as_ref(),
                    "apply_creative_effects",
                    &bgl_adjust_creative,
                ),
                bind_group: bg_adjust_creative,
                workgroups: image_workgroups,
            },
            Pass {
                pipeline: make_pipeline(
                    view_transform_module.as_ref(),
                    "apply_view_node",
                    &bgl_adjust_render,
                ),
                bind_group: bg_adjust_render,
                workgroups: image_workgroups,
            },
        ]);

        let expected_programs = expected_pass_count(raw.cfa_kind);
        if next_program_index != expected_programs || passes.len() != expected_programs {
            return Err(anyhow!(
                "GPU render-plan mismatch for {:?}: built {} passes and consumed {} programs; expected {}",
                raw.cfa_kind,
                passes.len(),
                next_program_index,
                expected_programs,
            ));
        }

        let egui_texture_id = renderer.as_deref_mut().map(|renderer| {
            renderer.register_native_texture(device, &out_view, wgpu::FilterMode::Linear)
        });

        let pipeline = Self {
            egui_texture_id,
            width: raw.width,
            height: raw.height,
            cfa_kind: raw.cfa_kind,
            processing_quality: quality,
            workgroup_size: config.workgroup_size,
            camera_uniforms_buffer,
            scene_tone_uniforms_buffer,
            effects_uniforms_buffer,
            scene_tone_bind_group,
            effects_bind_group,
            uploaded_stage_uniforms: Mutex::new(UploadedStageUniforms {
                camera: params.camera,
                scene_tone: params.scene_tone,
                effects: params.effects,
            }),
            mask_data_buffer,
            tone_histogram_buffer,
            tone_stats_buffer,
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
            has_ai_scene,
            has_ai_cfa,
            display_linear_texture,
            _tone_guide_a: tone_guide_a,
            _tone_guide_b: tone_guide_b,
            mask_texture,
            mask_layer_capacity,
            inpaint_texture,
            legacy_inpaint_camera_to_working: raw.cam_to_srgb,
            mask_atlas_edge,
            profile_buffer,
            profile_buffer_size_bytes,
            output_lut_offset_bytes,
            out_texture,
            _out_view: out_view,
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

    /// Uploads one normalized, anti-aliased local-mask layer as IEEE-754 half
    /// floats. Subject / Not Subject refinement is already composited by the
    /// core rasterizer before this upload, so the atlas remains the single mask
    /// source consumed by preview and export shaders. Export can allocate a
    /// larger atlas for higher spatial fidelity.
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

    /// Uploads the persisted baseline inpainting result into this pipeline's
    /// local geometry. `tile_origin_*` and `full_*` map crop/export pipelines
    /// back to full-image normalized coordinates. RGB is scene-linear Rec.2020
    /// RGBA16F; alpha is the replacement mask consumed before Develop edits.
    pub fn update_inpaint_layer(
        &self,
        queue: &wgpu::Queue,
        layer: Option<&crate::pipeline::InpaintLayer>,
        tile_origin_x: i32,
        tile_origin_y: i32,
        full_width: u32,
        full_height: u32,
    ) -> Result<()> {
        let rgba_elements = u64::from(self.width)
            .checked_mul(u64::from(self.height))
            .and_then(|pixels| pixels.checked_mul(4))
            .and_then(|elements| usize::try_from(elements).ok())
            .ok_or_else(|| anyhow!("GPU inpaint upload element count overflows"))?;
        let mut rgba16f = vec![0u16; rgba_elements];
        if let Some(layer) = layer {
            if full_width == 0 || full_height == 0 {
                return Err(anyhow!("invalid inpaint coordinate space for GPU upload"));
            }
            // Project only each sparse patch's covered rectangle into this
            // pipeline. This keeps preview refresh cost proportional to healed
            // area instead of image_pixels × stroke_count.
            for patch in layer.patches.iter() {
                if !patch.is_valid() {
                    return Err(anyhow!("invalid inpaint patch for GPU upload"));
                }
                let global_x0 = ((u64::from(patch.x) * u64::from(full_width))
                    / u64::from(patch.source_width)) as i64;
                let global_y0 = ((u64::from(patch.y) * u64::from(full_height))
                    / u64::from(patch.source_height)) as i64;
                let patch_right = patch
                    .x
                    .checked_add(patch.width)
                    .ok_or_else(|| anyhow!("inpaint patch horizontal extent overflows"))?;
                let patch_bottom = patch
                    .y
                    .checked_add(patch.height)
                    .ok_or_else(|| anyhow!("inpaint patch vertical extent overflows"))?;
                let global_x1 = (u64::from(patch_right)
                    .checked_mul(u64::from(full_width))
                    .ok_or_else(|| anyhow!("inpaint patch horizontal projection overflows"))?
                    .div_ceil(u64::from(patch.source_width)))
                    as i64;
                let global_y1 = (u64::from(patch_bottom)
                    .checked_mul(u64::from(full_height))
                    .ok_or_else(|| anyhow!("inpaint patch vertical projection overflows"))?
                    .div_ceil(u64::from(patch.source_height)))
                    as i64;

                let local_x0 =
                    (global_x0 - i64::from(tile_origin_x)).clamp(0, i64::from(self.width)) as u32;
                let local_y0 =
                    (global_y0 - i64::from(tile_origin_y)).clamp(0, i64::from(self.height)) as u32;
                let local_x1 =
                    (global_x1 - i64::from(tile_origin_x)).clamp(0, i64::from(self.width)) as u32;
                let local_y1 =
                    (global_y1 - i64::from(tile_origin_y)).clamp(0, i64::from(self.height)) as u32;

                for y in local_y0..local_y1 {
                    let global_y = tile_origin_y + y as i32;
                    if global_y < 0 || global_y >= full_height as i32 {
                        continue;
                    }
                    let source_y = (global_y as f32 + 0.5) * patch.source_height as f32
                        / full_height as f32
                        - 0.5;
                    for x in local_x0..local_x1 {
                        let global_x = tile_origin_x + x as i32;
                        if global_x < 0 || global_x >= full_width as i32 {
                            continue;
                        }
                        let source_x = (global_x as f32 + 0.5) * patch.source_width as f32
                            / full_width as f32
                            - 0.5;
                        let Some((mut rgb, alpha)) =
                            patch.sample_linear_rec2020_bilinear(source_x, source_y)
                        else {
                            continue;
                        };
                        if alpha <= 1e-6 {
                            continue;
                        }
                        rgb = patch.resolve_neutral_working_rgb(
                            rgb,
                            self.legacy_inpaint_camera_to_working,
                        );
                        let destination = u64::from(y)
                            .checked_mul(u64::from(self.width))
                            .and_then(|row| row.checked_add(u64::from(x)))
                            .and_then(|pixel| pixel.checked_mul(4))
                            .and_then(|offset| usize::try_from(offset).ok())
                            .ok_or_else(|| anyhow!("GPU inpaint destination offset overflows"))?;
                        composite_inpaint_rgba16f(
                            &mut rgba16f[destination..destination + 4],
                            rgb,
                            alpha,
                        );
                    }
                }
            }
        }
        queue.write_texture(
            copy_texture(&self.inpaint_texture),
            bytemuck::cast_slice(&rgba16f),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(
                    self.width
                        .checked_mul(8)
                        .ok_or_else(|| anyhow!("GPU inpaint upload row byte count overflows"))?,
                ),
                rows_per_image: Some(self.height),
            },
            texture_size(self.width, self.height),
        );
        Ok(())
    }

    pub const fn mask_atlas_edge(&self) -> u32 {
        self.mask_atlas_edge
    }

    pub const fn mask_layer_capacity(&self) -> usize {
        self.mask_layer_capacity
    }

    /// Whether this image-sized graph already contains every immutable AI
    /// source texture needed for the requested state. Bayer AI output replaces
    /// the mosaic upload and therefore must match exactly. X-Trans uses a
    /// separate scene texture which may safely remain resident while disabled.
    pub const fn immutable_ai_source_matches(&self, cfa_kind: CfaKind, enabled: bool) -> bool {
        match cfa_kind {
            CfaKind::Bayer => self.has_ai_cfa == enabled,
            CfaKind::XTrans => !enabled || self.has_ai_scene,
        }
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

    /// Updates the preview/display transform from an RGB ICC profile. Desktop
    /// builds accept matrix-shaper and LUT/CLUT profiles through LCMS2, then
    /// upload the sampled 3D LUT without rebuilding pipelines or bind groups.
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

    /// Dispatches exactly one dependency stage. GPU submission is asynchronous;
    /// callers can spread Raw -> Tone -> Output across event-loop iterations.
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
        read_rgba8_texture_region_blocking(
            device,
            queue,
            &self.out_texture,
            x,
            y,
            width,
            height,
            self.width,
            self.height,
            "auraw tiled export readback",
        )
    }

    /// Queues a display-linear RGBA32F copy and immediately returns a mapped
    /// readback handle. Export can submit the next tile before waiting on this
    /// handle, overlapping GPU work with CPU readback/encoding.
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
            x,
            y,
            width,
            height,
            self.width,
            self.height,
            "auraw pipelined display-linear export readback",
        )
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

    /// Runs only RAW reconstruction and returns its white-balanced camera-RGB
    /// boundary. RawNIND's linear Rec.2020 variant uses this for non-Bayer
    /// sensors before converting into the model's declared colour space.
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
                "regression scene conversion requires ProcessingQuality::High (RGBA32Float)"
            ));
        }
        self.render_scene_conversion_blocking(device, queue, params, "write_regression_scene")
    }

    /// Renders the neutral scene-working image used as LaMa input. Unlike the
    /// regression rendition this stops before DCP HueSatMap/default exposure so
    /// an inpainted replacement can be reinserted at exactly the same stage.
    pub fn render_inpaint_working_scene_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: &GpuParams,
    ) -> Result<Vec<f32>> {
        self.render_scene_conversion_blocking(device, queue, params, "write_inpaint_working_scene")
    }

    fn render_scene_conversion_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: &GpuParams,
        entry_point: &str,
    ) -> Result<Vec<f32>> {
        // The conversion target/readback is always RGBA32Float. The local source
        // pipeline may use RGBA16Float intermediates so its already-compiled preview
        // programs can be reused without a brush-release compile stall. The readback
        // remains scene-linear Rec.2020 f32 all the way to the explicit LaMa model
        // boundary; no 8-bit intermediate is created here.

        self.upload_params(queue, params);
        let size = texture_size(self.width, self.height);
        let working_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw scene conversion RGBA32F"),
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
            label: Some("auraw scene conversion layout"),
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
            label: Some("auraw scene conversion bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.camera_uniforms_buffer.as_entire_binding(),
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
        let mut shader_manager = ShaderManager::new_with_workgroup_size(
            processing_work_format(self.processing_quality),
            self.workgroup_size,
        )
        .context("initialize regression-scene WGSL composer")?;
        let shader = shader_manager.create_shader_module(
            device,
            "auraw scene conversion shader",
            SHADER_REGRESSION_SCENE,
            "regression_scene.wgsl",
        )?;
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("auraw scene conversion pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("auraw scene conversion pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some(entry_point),
            compilation_options: Default::default(),
            cache: self.pipeline_cache.as_ref().map(|cache| cache.raw()),
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw scene conversion encoder"),
        });
        self.encode_raw_stage(&mut encoder, params);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("auraw scene conversion pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let workgroups = self
                .workgroup_size
                .dispatch_for_extent(self.width, self.height);
            pass.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
        }
        // Submit the render first, then use the shared chunked RGBA32F readback.
        // Queue submission ordering guarantees every copy sees the completed
        // working texture, while each MAP_READ buffer stays well below wgpu's
        // max_buffer_size instead of allocating one crop-sized buffer.
        queue.submit(Some(encoder.finish()));
        read_rgba32_texture_rgb_blocking(
            device,
            queue,
            &working_texture,
            self.width,
            self.height,
            "auraw scene conversion readback",
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_inpaint_working_scene_region_resized_blocking(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        params: &GpuParams,
        source_x: u32,
        source_y: u32,
        source_width: u32,
        source_height: u32,
        output_width: u32,
        output_height: u32,
    ) -> Result<Vec<f32>> {
        if source_width == 0
            || source_height == 0
            || output_width == 0
            || output_height == 0
            || source_x
                .checked_add(source_width)
                .is_none_or(|right| right > self.width)
            || source_y
                .checked_add(source_height)
                .is_none_or(|bottom| bottom > self.height)
        {
            return Err(anyhow!("invalid inpainting resize rectangle"));
        }

        self.upload_params(queue, params);
        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("auraw inpaint working resize RGBA32F"),
            size: texture_size(output_width, output_height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba32Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[wgpu::TextureFormat::Rgba32Float],
        });
        let output_view = output_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let scene_view = self
            .scene_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let resize_params = InpaintResizeParams {
            source_origin_x: source_x,
            source_origin_y: source_y,
            source_width,
            source_height,
            output_width,
            output_height,
            _pad0: 0,
            _pad1: 0,
            cam_to_working_0: params.camera.cam_to_srgb_0,
            cam_to_working_1: params.camera.cam_to_srgb_1,
            cam_to_working_2: params.camera.cam_to_srgb_2,
        };
        let resize_params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("auraw inpaint resize params"),
            contents: bytemuck::bytes_of(&resize_params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("auraw inpaint resize layout"),
            entries: &[
                buffer_entry(0),
                texture_entry(1, wgpu::TextureSampleType::Float { filterable: false }),
                storage_texture_entry(
                    2,
                    wgpu::TextureFormat::Rgba32Float,
                    wgpu::StorageTextureAccess::WriteOnly,
                ),
            ],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("auraw inpaint resize bind group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: resize_params_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&scene_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&output_view),
                },
            ],
        });
        let resize_shader =
            specialize_compute_workgroup_size(SHADER_INPAINT_DOWNSAMPLE, self.workgroup_size);
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("auraw inpaint resize shader"),
            source: wgpu::ShaderSource::Wgsl(resize_shader),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("auraw inpaint resize pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("auraw inpaint resize pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: self.pipeline_cache.as_ref().map(|cache| cache.raw()),
        });

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("auraw inpaint resize encoder"),
        });
        self.encode_raw_stage(&mut encoder, params);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("auraw inpaint resize pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            let workgroups = self
                .workgroup_size
                .dispatch_for_extent(output_width, output_height);
            pass.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
        }
        queue.submit(Some(encoder.finish()));
        read_rgba32_texture_rgb_blocking(
            device,
            queue,
            &output_texture,
            output_width,
            output_height,
            "auraw scene conversion readback",
        )
    }

    fn encode_raw_stage(&self, encoder: &mut wgpu::CommandEncoder, params: &GpuParams) {
        if self.has_ai_scene && params.uses_ai_denoise() {
            // The RawNIND result was uploaded directly into the camera-RGB
            // scene boundary. Tone analysis and the complete output stage,
            // including capture sharpening, still execute normally.
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
        // Capture sharpening and tone finalization are always required: when
        // optional presence/creative effects are neutral this pass already
        // writes the final adjustment image into tex2 for the render pass.
        self.encode_pass(encoder, self.adjustment_tone_pass_index);
        if params.needs_intermediate_adjustment_passes() {
            self.encode_pass(encoder, self.adjustment_effects_pass_index - 1);
            self.encode_pass(encoder, self.adjustment_effects_pass_index);
            self.encode_pass(encoder, self.adjustment_effects_pass_index + 1);
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
        pass.set_bind_group(1, &self.scene_tone_bind_group, &[]);
        pass.set_bind_group(2, &self.effects_bind_group, &[]);
        let workgroups = self.passes[index].workgroups;
        pass.dispatch_workgroups(workgroups[0], workgroups[1], workgroups[2]);
    }

    fn encode_pass_range(&self, encoder: &mut wgpu::CommandEncoder, start: usize, end: usize) {
        for index in start..end {
            self.encode_pass(encoder, index);
        }
    }
}
