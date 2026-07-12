struct Params {
    // Keep the scalar block at exactly 16 floats so the vec4 fields below
    // retain the same 16-byte alignment in Rust and WGSL uniforms. Two former
    // reserved slots now configure reduced-resolution adaptive tone analysis
    // and demosaic finishing while the stable 64-byte prefix does not move.
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
    tone_analysis_scale: f32,
    tone_guide_radius: f32,
    demosaic_mode: f32,
    dual_threshold: f32,
    frequency_chroma: f32,
    _demosaic_reserved: f32,
    // Highlights, shadows, whites, blacks. Values use the Lightroom-style
    // -100..100 UI domain and parameterize the adaptive scene-to-display map.
    basic_tone: vec4<f32>,
    // Texture, clarity, dehaze, reserved.
    presence: vec4<f32>,
    // Reconstruction method, guided passes, colour adaptation, reserved.
    highlight_options: vec4<f32>,
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
    black_levels: vec4<f32>,
    white_levels: vec4<f32>,
    width: u32,
    height: u32,
    tile_origin_x: i32,
    tile_origin_y: i32,
    full_width: u32,
    full_height: u32,
    _pad0: u32,
    _pad1: u32,
    // DCP/ICC LUT metadata: dimensions and packed-buffer offset.
    profile_hue_sat: vec4<u32>,
    profile_look: vec4<u32>,
    profile_tone: vec4<u32>,
    output_lut: vec4<u32>,
    // HueSat encoding, LookTable encoding, default exposure EV bits, reserved.
    profile_flags: vec4<u32>,
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
