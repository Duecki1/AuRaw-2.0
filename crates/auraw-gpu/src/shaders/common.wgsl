
// Naga reserves trailing numeric suffixes when rewriting composable modules.
// Exported struct member names must therefore not end in a digit.
struct MaskData {
    metadata: vec4<u32>,
    adjust_0_field: vec4<f32>,
    adjust_1_field: vec4<f32>,
    adjust_2_field: vec4<f32>,
    curves: array<vec4<f32>, 8>,
    grade_shadows: vec4<f32>,
    grade_midtones: vec4<f32>,
    grade_highlights: vec4<f32>,
    grade_global: vec4<f32>,
    grade_options: vec4<f32>,
    curves_red: array<vec4<f32>, 8>,
    curves_green: array<vec4<f32>, 8>,
    curves_blue: array<vec4<f32>, 8>,
    hsl_hue_0_field: vec4<f32>,
    hsl_hue_1_field: vec4<f32>,
    hsl_saturation_0_field: vec4<f32>,
    hsl_saturation_1_field: vec4<f32>,
    hsl_luminance_0_field: vec4<f32>,
    hsl_luminance_1_field: vec4<f32>,
}

fn mask_effect_id(metadata: vec4<u32>) -> u32 {
    return metadata.w >> 8u;
}

struct CameraUniforms {
    // Padding keeps the following vec4 on the Rust/WGSL 16-byte boundary.
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
    _pad_0_field: f32,
    _pad_1_field: f32,
    _pad_2_field: f32,
    highlight_options: vec4<f32>,
    noise_shot: vec4<f32>,
    noise_read: vec4<f32>,
    noise_options: vec4<f32>,
    wb: vec4<f32>,
    cam_to_srgb_0_field: vec4<f32>,
    cam_to_srgb_1_field: vec4<f32>,
    cam_to_srgb_2_field: vec4<f32>,
    black_levels: vec4<f32>,
    white_levels: vec4<f32>,
    width: u32,
    height: u32,
    tile_origin_x: i32,
    tile_origin_y: i32,
    full_width: u32,
    full_height: u32,
    abi_version: u32,
    abi_size_bytes: u32,
    tone_histogram_bounds: vec4<u32>,
    profile_hue_sat: vec4<u32>,
    profile_look: vec4<u32>,
    profile_tone: vec4<u32>,
    output_lut: vec4<u32>,
    profile_flags: vec4<u32>,
    ai_denoise_enabled: u32,
    user_exposure_bits: u32,
    _pad_camera_0_field: u32,
    _pad_camera_1_field: u32,
}

struct SceneToneUniforms {
    exposure: f32,
    saturation: f32,
    vibrance: f32,
    _pad_0_field: f32,
    basic_tone: vec4<f32>,
    sigmoid_curve: vec4<f32>,
    sigmoid_power: vec4<f32>,
    tone_curve_0_field: vec4<f32>,
    tone_curve_1_field: vec4<f32>,
    tone_curve_2_field: vec4<f32>,
    tone_curve_3_field: vec4<f32>,
    tone_curve_meta: vec4<f32>,
    tone_curve_red_0_field: vec4<f32>,
    tone_curve_red_1_field: vec4<f32>,
    tone_curve_red_2_field: vec4<f32>,
    tone_curve_red_3_field: vec4<f32>,
    tone_curve_red_meta: vec4<f32>,
    tone_curve_green_0_field: vec4<f32>,
    tone_curve_green_1_field: vec4<f32>,
    tone_curve_green_2_field: vec4<f32>,
    tone_curve_green_3_field: vec4<f32>,
    tone_curve_green_meta: vec4<f32>,
    tone_curve_blue_0_field: vec4<f32>,
    tone_curve_blue_1_field: vec4<f32>,
    tone_curve_blue_2_field: vec4<f32>,
    tone_curve_blue_3_field: vec4<f32>,
    tone_curve_blue_meta: vec4<f32>,
    hsl_hue_0_field: vec4<f32>,
    hsl_hue_1_field: vec4<f32>,
    hsl_saturation_0_field: vec4<f32>,
    hsl_saturation_1_field: vec4<f32>,
    hsl_luminance_0_field: vec4<f32>,
    hsl_luminance_1_field: vec4<f32>,
    mask_counts: vec4<u32>,
    grade_shadows: vec4<f32>,
    grade_midtones: vec4<f32>,
    grade_highlights: vec4<f32>,
    grade_global: vec4<f32>,
    grade_options: vec4<f32>,
    // WGSL uses a 16-byte vec3 matrix-column stride; Rust mirrors padded columns.
    rec2020_to_xyz: mat3x3<f32>,
    xyz_to_rec2020_field: mat3x3<f32>,
    xyz_to_bradford: mat3x3<f32>,
    bradford_to_xyz: mat3x3<f32>,
}

struct EffectsUniforms {
    presence: vec4<f32>,
    creative_effects: vec4<f32>,
    vignette: vec4<f32>,
    vignette_options: vec4<f32>,
    vignette_frame: vec4<f32>,
    vignette_transform: vec4<f32>,
    vignette_dark_half_fit: vec4<f32>,
    vignette_dark_full_fit: vec4<f32>,
    vignette_light_half_fit: vec4<f32>,
    vignette_light_full_fit: vec4<f32>,
    capture_scale_sigma: vec4<f32>,
    capture_thresholds: vec4<f32>,
    capture_mask_coherence: vec4<f32>,
}

@group(0) @binding(0) var<uniform> camera_uniforms: CameraUniforms;
@group(1) @binding(0) var<uniform> scene_tone_uniforms: SceneToneUniforms;
@group(2) @binding(0) var<uniform> effects_uniforms: EffectsUniforms;

@group(0) @binding(33) var<storage, read> mask_data: array<MaskData>;


@group(0) @binding(1) var raw_tex: texture_2d<u32>;
@group(0) @binding(2) var color_tex: texture_2d<u32>;
@group(0) @binding(19) var black_tex: texture_2d<f32>;

const LUMA: vec3<f32> = vec3<f32>(0.2627002, 0.6779981, 0.0593017);
const SRGB_LUMA: vec3<f32> = vec3<f32>(0.2126729, 0.7151522, 0.0721750);

const REC2020_TO_SRGB: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 1.6604910, -0.1245505, -0.0181508),
    vec3<f32>(-0.5876411,  1.1328999, -0.1005789),
    vec3<f32>(-0.0728499, -0.0083494,  1.1187297),
);

const SRGB_TO_REC2020: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.6274039, 0.0690973, 0.0163914),
    vec3<f32>(0.3292830, 0.9195404, 0.0880133),
    vec3<f32>(0.0433131, 0.0113623, 0.8955953),
);

fn image_max() -> vec2<i32> {
    return vec2<i32>(i32(camera_uniforms.width) - 1, i32(camera_uniforms.height) - 1);
}

fn tile_origin() -> vec2<i32> {
    return vec2<i32>(camera_uniforms.tile_origin_x, camera_uniforms.tile_origin_y);
}

fn full_image_max() -> vec2<i32> {
    return vec2<i32>(i32(camera_uniforms.full_width) - 1, i32(camera_uniforms.full_height) - 1);
}

fn clamp_pos(pos: vec2<i32>) -> vec2<i32> {
    return clamp(pos, vec2<i32>(0, 0), image_max());
}

fn safe_luma(rgb: vec3<f32>) -> f32 {
    return max(dot(rgb, LUMA), 1e-6);
}
