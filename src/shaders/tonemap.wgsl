// One monotonic scene-to-display curve. Exposure remains a scene-linear scale;
// contrast, highlights, shadows, whites and blacks only parameterize this map.
// The two Schlick-bias branches are strictly increasing for every positive
// shape value and meet at the fixed middle-grey anchor.
const SCENE_MIDDLE_GREY: f32 = 0.1842;
const DISPLAY_MIDDLE_GREY: f32 = 0.1842;

fn schlick_bias(value: f32, shape: f32) -> f32 {
    let x = clamp(value, 0.0, 1.0);
    let a = clamp(shape, 0.05, 64.0);
    return x / max(a + (1.0 - a) * x, 1e-6);
}

fn scene_to_display_luminance(scene_luminance: f32) -> f32 {
    let highlights = clamp(params.basic_tone.x / 100.0, -1.0, 1.0);
    let shadows = clamp(params.basic_tone.y / 100.0, -1.0, 1.0);
    let whites = clamp(params.basic_tone.z / 100.0, -1.0, 1.0);
    let blacks = clamp(params.basic_tone.w / 100.0, -1.0, 1.0);
    let contrast = clamp(params.contrast / 100.0, -1.0, 1.0);

    // Whites and blacks move the scene bounds instead of adding another gain
    // curve. Positive values brighten their respective end of the range.
    let black_ev = -8.0 - 2.0 * blacks;
    let white_ev = 4.0 - 1.5 * whites;
    let range_ev = max(white_ev - black_ev, 1.0);

    let scene_ev = log2(max(scene_luminance, 1e-8) / SCENE_MIDDLE_GREY);
    let position = clamp((scene_ev - black_ev) / range_ev, 0.0, 1.0);
    let middle_position = clamp(-black_ev / range_ev, 0.05, 0.95);

    // A common slope gives ordinary contrast. Shadows/highlights then bias one
    // side without changing endpoints or making the curve non-monotonic.
    let middle_slope = exp2(contrast);
    let shadow_shape = clamp(
        middle_slope * middle_position / DISPLAY_MIDDLE_GREY
            * exp2(-1.25 * shadows),
        0.05,
        64.0,
    );
    let highlight_shape = clamp(
        (1.0 - DISPLAY_MIDDLE_GREY)
            / max(middle_slope * (1.0 - middle_position), 1e-4)
            * exp2(-1.25 * highlights),
        0.05,
        64.0,
    );

    if position <= middle_position {
        let local = position / max(middle_position, 1e-5);
        return DISPLAY_MIDDLE_GREY * schlick_bias(local, shadow_shape);
    }

    let local = (position - middle_position) / max(1.0 - middle_position, 1e-5);
    return DISPLAY_MIDDLE_GREY
        + (1.0 - DISPLAY_MIDDLE_GREY) * schlick_bias(local, highlight_shape);
}

fn scene_to_display(rgb: vec3<f32>) -> vec3<f32> {
    let positive = max(rgb, vec3<f32>(0.0));
    let scene_luminance = safe_luma(positive);
    let display_luminance = scene_to_display_luminance(scene_luminance);
    return positive * (display_luminance / scene_luminance);
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
    let mapped = scene_to_display(rgb);
    let srgb_linear = REC2020_TO_SRGB * mapped;
    let display_linear = compress_display_gamut(srgb_linear);
    let encoded = srgb_oetf(display_linear);
    return clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0));
}
