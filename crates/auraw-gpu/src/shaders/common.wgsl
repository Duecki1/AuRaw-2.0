// naga_oil composable modules are round-tripped through Naga's WGSL writer.
// Keep exported struct member names from ending in a digit: Naga reserves
// trailing numeric suffixes for name disambiguation and would rewrite them.

struct MaskData {
    // Enabled, has any edit, curve flags, color feature flags (mixer/grading/hue)
    // in the low byte plus a MaskEffect shader id in the high bytes.
    metadata: vec4<u32>,
    adjust_0_field: vec4<f32>,
    adjust_1_field: vec4<f32>,
    adjust_2_field: vec4<f32>,
    curves: array<vec4<f32>, 8>,
    grade_shadows: vec4<f32>,
    grade_midtones: vec4<f32>,
    grade_highlights: vec4<f32>,
    grade_global: vec4<f32>,
    // Color-grading blending, balance, uniform hue rotation in degrees, reserved.
    grade_options: vec4<f32>,
    // Newer local-edit features share the same per-layer storage record so the
    // uniform block stays compact as the local adjustment model evolves.
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
    // Camera/raw-stage scalar controls. The three reserved values keep the
    // following vec4 block on a strict 16-byte uniform boundary.
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
    // >0.5 means the source is a pre-demosaiced raster instead of a sensor CFA.
    _pad_0_field: f32,
    // >0.5 means apply the scene->display sigmoid view transform. Rendered
    // TIFFs disable this at defaults so their baked tone rendering is not
    // contrast-mapped a second time.
    _pad_1_field: f32,
    _pad_2_field: f32,
    // Reconstruction method followed by inpaint-opposed RGB chrominance offsets.
    highlight_options: vec4<f32>,
    // Per-CFA-plane sensor noise model: variance = shot * signal + read.
    noise_shot: vec4<f32>,
    noise_read: vec4<f32>,
    // Luma strength, detail protection, quality tier, profile confidence.
    noise_options: vec4<f32>,
    wb: vec4<f32>,
    cam_to_srgb_0_field: vec4<f32>,
    cam_to_srgb_1_field: vec4<f32>,
    cam_to_srgb_2_field: vec4<f32>,
    // Neutral scene-working -> current camera-WB scene-working transform used
    // only for persisted/generated inpaint replacement pixels.
    inpaint_wb_0_field: vec4<f32>,
    inpaint_wb_1_field: vec4<f32>,
    inpaint_wb_2_field: vec4<f32>,
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
    // Local half-open rectangle included in a tiled full-resolution histogram.
    tone_histogram_bounds: vec4<u32>,
    // DCP/ICC LUT metadata: dimensions and packed-buffer offset.
    profile_hue_sat: vec4<u32>,
    profile_look: vec4<u32>,
    profile_tone: vec4<u32>,
    output_lut: vec4<u32>,
    // HueSat encoding, LookTable encoding, default exposure EV bits, and the
    // live DCP dual-illuminant interpolation weight as f32 bits.
    profile_flags: vec4<u32>,
    // Runtime camera-stage switches and cached user Exposure bits.
    ai_denoise_enabled: u32,
    user_exposure_bits: u32,
    _pad_camera_0_field: u32,
    _pad_camera_1_field: u32,
}

struct SceneToneUniforms {
    // Scene-referred global controls. Padding makes the point-curve block begin
    // on the same 16-byte boundary in Rust and WGSL.
    exposure: f32,
    saturation: f32,
    vibrance: f32,
    _pad_0_field: f32,
    // Highlights, shadows, whites, blacks.
    basic_tone: vec4<f32>,
    // darktable sigmoid: white target, black target, paper exposure, film fog.
    sigmoid_curve: vec4<f32>,
    // darktable sigmoid: film power, paper power, hue preservation, method.
    sigmoid_power: vec4<f32>,
    // Eight editable point-curve coordinates, packed as x0,y0,x1,y1.
    tone_curve_0_field: vec4<f32>,
    tone_curve_1_field: vec4<f32>,
    tone_curve_2_field: vec4<f32>,
    tone_curve_3_field: vec4<f32>,
    tone_curve_meta: vec4<f32>,
    // Independent scene-referred red, green and blue point curves.
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
    // Red, orange, yellow, green / aqua, blue, purple, magenta.
    hsl_hue_0_field: vec4<f32>,
    hsl_hue_1_field: vec4<f32>,
    hsl_saturation_0_field: vec4<f32>,
    hsl_saturation_1_field: vec4<f32>,
    hsl_luminance_0_field: vec4<f32>,
    hsl_luminance_1_field: vec4<f32>,
    // Local mask count/atlas metadata shared by scene tone and view-adjacent
    // local edits.
    mask_counts: vec4<u32>,
    grade_shadows: vec4<f32>,
    grade_midtones: vec4<f32>,
    grade_highlights: vec4<f32>,
    grade_global: vec4<f32>,
    // Color-grading blending, balance, uniform hue rotation in degrees, reserved.
    grade_options: vec4<f32>,
    // Fixed scene-working colour transforms are stored as uniform matrices so
    // alternate working-space/adaptation calibrations can be supplied without
    // rebuilding the shader module. WGSL uniform matrices use a 16-byte stride
    // per vec3 column; Rust mirrors each column with a padded [f32; 4].
    rec2020_to_xyz: mat3x3<f32>,
    xyz_to_rec2020_field: mat3x3<f32>,
    xyz_to_bradford: mat3x3<f32>,
    bradford_to_xyz: mat3x3<f32>,
}

struct EffectsUniforms {
    // Texture, clarity, dehaze, reserved.
    presence: vec4<f32>,
    // Glow amount, radius, highlight threshold, capture-sharpen amount.
    creative_effects: vec4<f32>,
    // Vignette amount, midpoint, roundness, feather.
    vignette: vec4<f32>,
    // Vignette highlight protection, sharpen radius, detail, masking.
    vignette_options: vec4<f32>,
    // Source-space crop center and final-frame dimensions.
    vignette_frame: vec4<f32>,
    // Normalized source-to-final 2x2 affine transform.
    vignette_transform: vec4<f32>,
    // Lightroom-like vignette calibration anchors. Each lane stores
    // (smoothstep start, smoothstep end, falloff exponent, corner opacity).
    vignette_dark_half_fit: vec4<f32>,
    vignette_dark_full_fit: vec4<f32>,
    vignette_light_half_fit: vec4<f32>,
    vignette_light_full_fit: vec4<f32>,
    // Capture sharpening tuning, packed into complete 16-byte lanes.
    // x/y = capture scale min/max; z/w = bilateral sigma min/max.
    capture_scale_sigma: vec4<f32>,
    // x/y = fixed thresholds at Detail 0/100 EV;
    // z/w = edge-noise relief start/end EV.
    capture_thresholds: vec4<f32>,
    // x/y = masking threshold min/max EV;
    // z/w = impulse coherence full/zero EV.
    capture_mask_coherence: vec4<f32>,
}

// Camera/common resources remain in group 0 with image/storage resources.
// Scene tone and effects use independent bind groups so updating one stage does
// not invalidate either of the other uniform bindings.
@group(0) @binding(0) var<uniform> camera_uniforms: CameraUniforms;
@group(1) @binding(0) var<uniform> scene_tone_uniforms: SceneToneUniforms;
@group(2) @binding(0) var<uniform> effects_uniforms: EffectsUniforms;

@group(0) @binding(33) var<storage, read> mask_data: array<MaskData>;


@group(0) @binding(1) var raw_tex: texture_2d<u32>;
@group(0) @binding(2) var color_tex: texture_2d<u32>;
// LibRaw can provide a repeating row/column black pattern in addition to the
// four CFA-plane offsets. Keeping the effective value per photosite avoids
// fixed-pattern residuals before white balance and demosaic.
@group(0) @binding(19) var black_tex: texture_2d<f32>;

// Scene processing uses linear Rec.2020, so its luminance coefficients must be
// used for every colour-preserving tonal operation.
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
