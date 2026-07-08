struct Params {
    black: f32,
    exposure: f32,
    _pad0: f32,
    _pad1: f32,
    wb: vec4<f32>,
    cam_to_srgb_0: vec4<f32>,
    cam_to_srgb_1: vec4<f32>,
    cam_to_srgb_2: vec4<f32>,
    width: u32,
    height: u32,
    cfa_pattern: u32,
    black_level: f32,
    white_level: f32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var demosaiced_tex: texture_2d<f32>;
@group(0) @binding(2) var out_tex: texture_storage_2d<rgba8unorm, write>;

fn apply_wb(rgb: vec3<f32>) -> vec3<f32> {
    return rgb * params.wb.rgb;
}

fn cam_to_srgb(rgb: vec3<f32>) -> vec3<f32> {
    let r = dot(params.cam_to_srgb_0.xyz, rgb);
    let g = dot(params.cam_to_srgb_1.xyz, rgb);
    let b = dot(params.cam_to_srgb_2.xyz, rgb);
    return vec3<f32>(r, g, b);
}

fn apply_exposure(rgb: vec3<f32>) -> vec3<f32> {
    let white = exp2(-params.exposure);
    let scale = 1.0 / (white - params.black);
    return (rgb - vec3<f32>(params.black)) * scale;
}

fn tonemap(rgb: vec3<f32>) -> vec3<f32> {
    let x = max(rgb, vec3<f32>(0.0));
    let luma = max(dot(x, vec3<f32>(0.2126, 0.7152, 0.0722)), 1e-6);
    let luma_mapped = luma / (luma + 1.0) * 1.06;
    return x * (luma_mapped / luma);
}

fn srgb_oetf(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    let cutoff = step(vec3<f32>(0.0031308), c);
    return mix(lo, hi, cutoff);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height {
        return;
    }
    let x = i32(gid.x);
    let y = i32(gid.y);

    var rgb = textureLoad(demosaiced_tex, vec2<i32>(x, y), 0).rgb;

    rgb = apply_wb(rgb);
    rgb = cam_to_srgb(rgb);
    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));
    rgb = tonemap(rgb);
    rgb = srgb_oetf(rgb);

    textureStore(out_tex, vec2<i32>(x, y), vec4<f32>(rgb, 1.0));
}