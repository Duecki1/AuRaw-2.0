// highlights.wgsl

fn reconstruct_sensor_highlights(rgb: vec3<f32>, clip_mask: f32) -> vec3<f32> {
    // Use WGSL modulo operator % instead of mod()
    let r_clipped = (clip_mask % 10.0) >= 1.0;
    let g_clipped = ((clip_mask / 10.0) % 10.0) >= 1.0;
    let b_clipped = (clip_mask / 100.0) >= 1.0;

    if (!r_clipped && !g_clipped && !b_clipped) {
        return rgb;
    }

    let r = rgb.r;
    let g = rgb.g;
    let b = rgb.b;

    var result = rgb;

    if (r_clipped && !g_clipped && !b_clipped) {
        result.r = g + (g - b);
    } else if (g_clipped && !r_clipped && !b_clipped) {
        result.g = r + (r - b);
    } else if (b_clipped && !r_clipped && !g_clipped) {
        result.b = r + (r - g);
    } else {
        // Two or more channels are clipped -> neutral collapse to preserve luminance
        let peak = max(max(r, g), b);
        result = vec3<f32>(peak);
    }

    return max(result, vec3<f32>(0.0));
}