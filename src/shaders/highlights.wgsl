// highlights.wgsl

fn reconstruct_sensor_highlights(rgb: vec3<f32>) -> vec3<f32> {
    let peak = max(max(rgb.r, rgb.g), rgb.b);
    let floor_val = min(min(rgb.r, rgb.g), rgb.b);
    let spread = clamp((peak - floor_val) / max(peak, 1e-6), 0.0, 1.0);
    let blend = smoothstep(0.88, 1.12, peak) * smoothstep(0.10, 0.55, spread);

    if blend <= 0.0 {
        return rgb;
    }

    var result = rgb;
    let r = rgb.r;
    let g = rgb.g;
    let b = rgb.b;

    // Identify which channel is the clipped peak and estimate it
    // from the other two using color-difference extrapolation.
    if r >= peak - 1e-6 && g < r {
        // R is clipped: R_est = G + (G - B) -> preserves warm hue
        result.r = g + (g - b);
    } else if b >= peak - 1e-6 && g < b {
        // B is clipped: B_est = G + (G - R) -> preserves cool hue
        result.b = g + (g - r);
    } else if g >= peak - 1e-6 && r < g {
        // G is clipped (rare in Bayer): G_est = (R + B) / 2
        result.g = (r + b) * 0.5;
    }

    result = max(result, vec3<f32>(0.0));
    return mix(rgb, result, blend);
}