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

    let toe = smoothstep(0.0, mid_t, t) * display_middle;
    let shoulder = display_middle + smoothstep(mid_t, 1.0, t) * (1.0 - display_middle);
    let mapped_lum = select(toe, shoulder, t >= mid_t);

    return x * (mapped_lum / lum);
}

fn display_render(rgb: vec3<f32>) -> vec3<f32> {
    let mapped = filmic_tonemap(rgb);
    let encoded = srgb_oetf(mapped);
    return clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0));
}

