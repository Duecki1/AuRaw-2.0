fn filmic_tonemap(rgb: vec3<f32>) -> vec3<f32> {
    let x = max(rgb, vec3<f32>(0.0));
    let lum = safe_luma(x);
    let scene_middle = 0.1842;
    let display_middle = max(params.middle_grey / 100.0, 0.01);
    let white = max(params.filmic_white, 0.1);
    let black = min(params.filmic_black, -0.1);

    let log_lum = log2(lum / scene_middle);
    let t = clamp((log_lum - black) / max(white - black, 1e-3), 0.0, 1.0);
    let mid_t = clamp((0.0 - black) / max(white - black, 1e-3), 0.05, 0.95);

    let mid_slope = 1.0;

    var mapped_lum: f32;
    if t < mid_t {
        let u = t / max(mid_t, 1e-3);
        let h01 = (-2.0 * u + 3.0) * u * u;
        let h11 = (u - 1.0) * u * u;
        mapped_lum = h01 * display_middle + h11 * mid_t * mid_slope;
    } else {
        let u = (t - mid_t) / max(1.0 - mid_t, 1e-3);
        let h00 = (2.0 * u - 3.0) * u * u + 1.0;
        let h10 = (u - 2.0) * u * u + u;
        let h01 = (-2.0 * u + 3.0) * u * u;
        mapped_lum = h00 * display_middle + h10 * (1.0 - mid_t) * mid_slope + h01;
    }

    return x * (mapped_lum / lum);
}

fn compress_display_gamut(rgb: vec3<f32>) -> vec3<f32> {
    let x = max(rgb, vec3<f32>(0.0));
    let peak = max(max(x.r, x.g), x.b);
    if peak <= 1.0 {
        return x;
    }

    let lum = clamp(safe_luma(x), 0.0, 1.0);
    let boundary = vec3<f32>(lum);
    let scale = clamp((1.0 - lum) / max(peak - lum, 1e-6), 0.0, 1.0);
    return mix(boundary, x, scale);
}

fn display_render(rgb: vec3<f32>) -> vec3<f32> {
    let mapped = filmic_tonemap(rgb);

    let srgb_linear = REC2020_TO_SRGB * mapped;

    let display_linear = compress_display_gamut(srgb_linear);

    let encoded = srgb_oetf(display_linear);

    return clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0));
}