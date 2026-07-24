struct Params {
    // Keep the scalar block at exactly 16 floats so the vec4 fields below
    // retain the same 16-byte alignment in Rust and WGSL uniforms. Two former
    // reserved slots now configure reduced-resolution adaptive tone analysis
    // and demosaic finishing while the stable 64-byte prefix does not move.
    black_point: f32,
    exposure: f32,
    // Global WB temperature mirrored for the stable uniform ABI. The CPU uses
    // it to rebuild the camera matrix; shaders do not apply a second gain.
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
    // Global WB tint mirrored for the stable uniform ABI; see temperature.
    tint: f32,
    // Highlights, shadows, whites, blacks. These are scene-linear local
    // exposure-shaping controls evaluated before the display transform.
    basic_tone: vec4<f32>,
    // darktable sigmoid: white target, black target, paper exposure, film fog.
    sigmoid_curve: vec4<f32>,
    // darktable sigmoid: film power, paper power, hue preservation, method.
    sigmoid_power: vec4<f32>,
    // Texture, clarity, dehaze, Lightroom-style contrast.
    presence: vec4<f32>,
    // Glow amount, radius, highlight threshold, reserved.
    creative_effects: vec4<f32>,
    // Vignette amount, midpoint, roundness, feather.
    vignette: vec4<f32>,
    // Vignette highlight protection, followed by reserved values.
    vignette_options: vec4<f32>,
    // Reconstruction method, guided passes, colour adaptation, reserved.
    highlight_options: vec4<f32>,
    // Eight editable point-curve coordinates, packed as x0,y0,x1,y1.
    tone_curve_0: vec4<f32>,
    tone_curve_1: vec4<f32>,
    tone_curve_2: vec4<f32>,
    tone_curve_3: vec4<f32>,
    // Active point count, followed by reserved values.
    tone_curve_meta: vec4<f32>,
    // Independent scene-referred red, green and blue point curves.
    tone_curve_red_0: vec4<f32>,
    tone_curve_red_1: vec4<f32>,
    tone_curve_red_2: vec4<f32>,
    tone_curve_red_3: vec4<f32>,
    tone_curve_red_meta: vec4<f32>,
    tone_curve_green_0: vec4<f32>,
    tone_curve_green_1: vec4<f32>,
    tone_curve_green_2: vec4<f32>,
    tone_curve_green_3: vec4<f32>,
    tone_curve_green_meta: vec4<f32>,
    tone_curve_blue_0: vec4<f32>,
    tone_curve_blue_1: vec4<f32>,
    tone_curve_blue_2: vec4<f32>,
    tone_curve_blue_3: vec4<f32>,
    tone_curve_blue_meta: vec4<f32>,
    // Red, orange, yellow, green / aqua, blue, purple, magenta.
    hsl_hue_0: vec4<f32>,
    hsl_hue_1: vec4<f32>,
    hsl_saturation_0: vec4<f32>,
    hsl_saturation_1: vec4<f32>,
    hsl_luminance_0: vec4<f32>,
    hsl_luminance_1: vec4<f32>,
    wb: vec4<f32>,
    cam_to_srgb_0: vec4<f32>,
    cam_to_srgb_1: vec4<f32>,
    cam_to_srgb_2: vec4<f32>,
    // Neutral scene-working -> current camera-WB scene-working transform used
    // only for persisted/generated inpaint replacement pixels.
    inpaint_wb_0: vec4<f32>,
    inpaint_wb_1: vec4<f32>,
    inpaint_wb_2: vec4<f32>,
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
    // x = processing-formula version. y bit 0 = the active DCP provides the
    // baseline ProfileToneCurve and AuRaw's default sigmoid should not be
    // stacked on top of it. Remaining lanes are reserved.
    process_info: vec4<u32>,
    // Local adjustments. Each mask index maps directly to one layer in the
    // normalized R8 array texture sampled by adjustments.wgsl. mask_meta.w is
    // a feature bitset: bit 0 color mixer, bit 1 color grading.
    mask_counts: vec4<u32>,
    mask_meta: array<vec4<u32>, 32>,
    // Exposure, contrast, highlights, shadows.
    mask_adjust_0: array<vec4<f32>, 32>,
    // Whites, blacks, temperature, tint.
    mask_adjust_1: array<vec4<f32>, 32>,
    // Saturation, texture, clarity, dehaze.
    mask_adjust_2: array<vec4<f32>, 32>,
    mask_curve_0: array<vec4<f32>, 32>,
    mask_curve_1: array<vec4<f32>, 32>,
    mask_curve_2: array<vec4<f32>, 32>,
    mask_curve_3: array<vec4<f32>, 32>,
    mask_curve_4: array<vec4<f32>, 32>,
    mask_curve_5: array<vec4<f32>, 32>,
    mask_curve_6: array<vec4<f32>, 32>,
    mask_curve_7: array<vec4<f32>, 32>,
    mask_curve_red_0: array<vec4<f32>, 32>,
    mask_curve_red_1: array<vec4<f32>, 32>,
    mask_curve_red_2: array<vec4<f32>, 32>,
    mask_curve_red_3: array<vec4<f32>, 32>,
    mask_curve_red_4: array<vec4<f32>, 32>,
    mask_curve_red_5: array<vec4<f32>, 32>,
    mask_curve_red_6: array<vec4<f32>, 32>,
    mask_curve_red_7: array<vec4<f32>, 32>,
    mask_curve_green_0: array<vec4<f32>, 32>,
    mask_curve_green_1: array<vec4<f32>, 32>,
    mask_curve_green_2: array<vec4<f32>, 32>,
    mask_curve_green_3: array<vec4<f32>, 32>,
    mask_curve_green_4: array<vec4<f32>, 32>,
    mask_curve_green_5: array<vec4<f32>, 32>,
    mask_curve_green_6: array<vec4<f32>, 32>,
    mask_curve_green_7: array<vec4<f32>, 32>,
    mask_curve_blue_0: array<vec4<f32>, 32>,
    mask_curve_blue_1: array<vec4<f32>, 32>,
    mask_curve_blue_2: array<vec4<f32>, 32>,
    mask_curve_blue_3: array<vec4<f32>, 32>,
    mask_curve_blue_4: array<vec4<f32>, 32>,
    mask_curve_blue_5: array<vec4<f32>, 32>,
    mask_curve_blue_6: array<vec4<f32>, 32>,
    mask_curve_blue_7: array<vec4<f32>, 32>,
    mask_hsl_hue_0: array<vec4<f32>, 32>,
    mask_hsl_hue_1: array<vec4<f32>, 32>,
    mask_hsl_saturation_0: array<vec4<f32>, 32>,
    mask_hsl_saturation_1: array<vec4<f32>, 32>,
    mask_hsl_luminance_0: array<vec4<f32>, 32>,
    mask_hsl_luminance_1: array<vec4<f32>, 32>,
    // Four-way scene-referred grading. Wheels contain normalized hue,
    // saturation, luminance and a reserved slot. Options contain blending and
    // balance in normalized UI domains.
    grade_shadows: vec4<f32>,
    grade_midtones: vec4<f32>,
    grade_highlights: vec4<f32>,
    grade_global: vec4<f32>,
    grade_options: vec4<f32>,
    mask_grade_shadows: array<vec4<f32>, 32>,
    mask_grade_midtones: array<vec4<f32>, 32>,
    mask_grade_highlights: array<vec4<f32>, 32>,
    mask_grade_global: array<vec4<f32>, 32>,
    mask_grade_options: array<vec4<f32>, 32>,
}

@group(0) @binding(0) var<uniform> params: Params;
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
    return vec2<i32>(i32(params.width) - 1, i32(params.height) - 1);
}

fn tile_origin() -> vec2<i32> {
    return vec2<i32>(params.tile_origin_x, params.tile_origin_y);
}

fn full_image_max() -> vec2<i32> {
    return vec2<i32>(i32(params.full_width) - 1, i32(params.full_height) - 1);
}

fn clamp_pos(pos: vec2<i32>) -> vec2<i32> {
    return clamp(pos, vec2<i32>(0, 0), image_max());
}

fn safe_luma(rgb: vec3<f32>) -> f32 {
    return max(dot(rgb, LUMA), 1e-6);
}
