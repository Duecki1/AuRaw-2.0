struct Params {
    black: f32,
    exposure: f32,
    hlcompr: f32,
    hlcomprthresh: f32,
    contrast: f32,
    middle_grey: f32,
    brightness: f32,
    saturation: f32,
    vibrance: f32,
    clip: f32,
    filmic_white: f32,
    filmic_black: f32,
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

// Pass 1 -> Pass 2
@group(0) @binding(3) var tex1_write: texture_storage_2d<rgba16float, write>;
@group(0) @binding(4) var tex1_read: texture_2d<f32>;

// Pass 2 -> Pass 3
@group(0) @binding(5) var tex2_write: texture_storage_2d<rgba16float, write>;
@group(0) @binding(6) var tex2_read: texture_2d<f32>;

// Pass 3 -> Pass 4
@group(0) @binding(7) var tex3_write: texture_storage_2d<rgba16float, write>;
@group(0) @binding(8) var tex3_read: texture_2d<f32>;

// Pass 4 -> Output
@group(0) @binding(9) var out_tex: texture_storage_2d<rgba8unorm, write>;

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

// Fast raw fetch without 5x5 outlier rejection for high-pass filters
fn raw_cfa_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let color = min(color_at(p), 3u);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let black = params.black_levels[color];
    let white = max(params.white_levels[color], black + 1.0);
    return clamp((raw - black) / (white - black), 0.0, 4.0);
}