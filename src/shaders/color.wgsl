fn apply_wb(rgb: vec3<f32>) -> vec3<f32> {
    return rgb * params.wb.rgb;
}

fn cam_to_srgb(rgb: vec3<f32>) -> vec3<f32> {
    let r = dot(params.cam_to_srgb_0.xyz, rgb);
    let g = dot(params.cam_to_srgb_1.xyz, rgb);
    let b = dot(params.cam_to_srgb_2.xyz, rgb);
    return vec3<f32>(r, g, b);
}

fn map_negative_gamut(rgb: vec3<f32>) -> vec3<f32> {
    let min_channel = min(min(rgb.r, rgb.g), rgb.b);
    if min_channel >= 0.0 {
        return rgb;
    }

    let lum = safe_luma(max(rgb, vec3<f32>(0.0)));
    let alpha = clamp(lum / max(lum - min_channel, 1e-6), 0.0, 1.0);
    return mix(vec3<f32>(lum), rgb, alpha);
}

fn srgb_oetf(c: vec3<f32>) -> vec3<f32> {
    let x = max(c, vec3<f32>(0.0));
    let lo = x * 12.92;
    let hi = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055;
    let cutoff = step(vec3<f32>(0.0031308), x);
    return mix(lo, hi, cutoff);
}

