struct Params {
    // Keep the scalar block at exactly 16 floats so the vec4 fields below
    // retain the same 16-byte alignment in Rust and WGSL uniforms.
    black_point: f32,
    exposure: f32,
    hlcompr: f32,
    hlcomprthresh: f32,
    contrast: f32,
    middle_grey: f32,
    brightness: f32,
    saturation: f32,
    vibrance: f32,
    highlight_clip: f32,
    filmic_white: f32,
    filmic_black: f32,
    chroma_denoise: f32,
    ca_red: f32,
    ca_blue: f32,
    highlight_reconstruction: f32,
    // Highlights, shadows, whites, blacks. Values use the Lightroom-style
    // -100..100 UI domain and are converted to scene-linear stops in WGSL.
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
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var raw_tex: texture_2d<u32>;
@group(0) @binding(2) var color_tex: texture_2d<u32>;

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

fn clamp_pos(pos: vec2<i32>) -> vec2<i32> {
    return clamp(pos, vec2<i32>(0, 0), image_max());
}

fn safe_luma(rgb: vec3<f32>) -> f32 {
    return max(dot(rgb, LUMA), 1e-6);
}
