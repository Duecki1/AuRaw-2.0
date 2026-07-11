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

const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

const REC2020_TO_SRGB: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 1.5489,  0.0955, -0.0701),
    vec3<f32>(-0.4830,  0.9123,  0.0597),
    vec3<f32>(-0.0657, -0.0077,  1.0105),
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

