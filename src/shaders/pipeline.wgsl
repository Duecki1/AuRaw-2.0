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
@group(0) @binding(3) var out_tex: texture_storage_2d<rgba8unorm, write>;

const LUMA: vec3<f32> = vec3<f32>(0.2126, 0.7152, 0.0722);

fn image_max() -> vec2<i32> {
    return vec2<i32>(i32(params.width) - 1, i32(params.height) - 1);
}

fn clamp_pos(pos: vec2<i32>) -> vec2<i32> {
    return clamp(pos, vec2<i32>(0, 0), image_max());
}

fn color_at(pos: vec2<i32>) -> u32 {
    return textureLoad(color_tex, clamp_pos(pos), 0).r;
}

fn normalized_raw_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let color = color_at(p);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let black = params.black_levels[color];
    let white = max(params.white_levels[color], black + 1.0);
    return clamp((raw - black) / (white - black), 0.0, 4.0);
}

fn demosaic_channel(pos: vec2<i32>, channel: u32) -> f32 {
    if color_at(pos) == channel {
        return normalized_raw_at(pos);
    }

    var sum = 0.0;
    var count = 0.0;
    for(var dy = -2; dy <= 2; dy = dy + 1) {
        for(var dx = -2; dx <= 2; dx = dx + 1) {
            let p = pos + vec2<i32>(dx, dy);
            if color_at(p) == channel {
                sum = sum + normalized_raw_at(p);
                count = count + 1.0;
            }
        }
    }

    if count > 0.0 {
        return sum / count;
    }
    return normalized_raw_at(pos);
}

fn demosaic(pos: vec2<i32>) -> vec3<f32> {
    return vec3<f32>(
        demosaic_channel(pos, 0u),
        demosaic_channel(pos, 1u),
        demosaic_channel(pos, 2u)
    );
}

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

fn reconstruct_highlights(rgb: vec3<f32>) -> vec3<f32> {
    let threshold = 0.96 + params.clip * 0.04;
    let peak = max(max(rgb.r, rgb.g), rgb.b);
    if peak <= threshold {
        return rgb;
    }

    let safe = min(rgb, vec3<f32>(threshold));
    let lum = max(dot(safe, LUMA), 1e-6);
    let hue = safe / max(max(safe.r, safe.g), max(safe.b, 1e-6));
    let recovered = hue * lum / max(dot(hue, LUMA), 1e-6);
    let blend = smoothstep(threshold, 1.25, peak);
    return mix(rgb, recovered, blend);
}

fn compress_highlights(rgb: vec3<f32>) -> vec3<f32> {
    if params.hlcompr <= 0.0 {
        return rgb;
    }

    let lum = max(dot(rgb, LUMA), 1e-6);
    let shoulder = params.hlcomprthresh / 800.0 + 0.1;
    let range = max(1.0 - shoulder, 1e-3);
    let amount = params.hlcompr / 100.0;
    let compressed = select(
        lum,
        shoulder + range * (1.0 - exp(-(lum - shoulder) * amount / range)),
        lum > shoulder
    );
    return rgb * (compressed / lum);
}

fn apply_brightness(rgb: vec3<f32>) -> vec3<f32> {
    if abs(params.brightness) < 1e-6 {
        return rgb;
    }
    let b = params.brightness * 2.0;
    let gamma = select(1.0 - b, 1.0 / max(1.0 + b, 1e-3), b >= 0.0);
    return pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(gamma));
}

fn apply_contrast(rgb: vec3<f32>) -> vec3<f32> {
    if abs(params.contrast) < 1e-6 {
        return rgb;
    }
    let contrast = params.contrast + 1.0;
    let middle = max(params.middle_grey / 100.0, 1e-4);
    let lum = max(dot(rgb, LUMA), 1e-6);
    let contrast_lum = pow(max(lum / middle, 0.0), contrast) * middle;
    return rgb * (contrast_lum / lum);
}

fn apply_saturation_vibrance(rgb: vec3<f32>) -> vec3<f32> {
    if abs(params.saturation) < 1e-6 && abs(params.vibrance) < 1e-6 {
        return rgb;
    }

    let average = (rgb.r + rgb.g + rgb.b) / 3.0;
    let delta = length(vec3<f32>(average) - rgb);
    let vibrance = params.vibrance / 1.4;
    let power = pow(max(delta, 0.0), max(abs(vibrance), 1e-6));
    let protection = vibrance * (1.0 - power);
    return vec3<f32>(average) + (1.0 + params.saturation + protection) * (rgb - vec3<f32>(average));
}

fn filmic_tonemap(rgb: vec3<f32>) -> vec3<f32> {
    let x = max(rgb, vec3<f32>(0.0));
    let lum = max(dot(x, LUMA), 1e-6);
    let middle = 0.1842;
    let white = max(params.filmic_white, 0.1);
    let black = min(params.filmic_black, -0.1);
    let log_lum = log2(lum / middle);
    let t = clamp((log_lum - black) / (white - black), 0.0, 1.0);
    let toe_shoulder = t * t * (3.0 - 2.0 * t);
    let mapped_lum = mix(0.0, 1.0, toe_shoulder);
    return x * (mapped_lum / lum);
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

    var rgb = demosaic(vec2<i32>(x, y));

    rgb = apply_wb(rgb);
    rgb = reconstruct_highlights(rgb);
    rgb = cam_to_srgb(rgb);
    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));
    rgb = compress_highlights(rgb);
    rgb = apply_brightness(rgb);
    rgb = apply_contrast(rgb);
    rgb = apply_saturation_vibrance(rgb);
    rgb = filmic_tonemap(rgb);
    rgb = srgb_oetf(rgb);

    textureStore(out_tex, vec2<i32>(x, y), vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0));
}
