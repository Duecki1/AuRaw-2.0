fn reconstruct_sensor_highlights(rgb: vec3<f32>) -> vec3<f32> {
    let peak = max(max(rgb.r, rgb.g), rgb.b);
    let floor_value = min(min(rgb.r, rgb.g), rgb.b);
    let spread = clamp((peak - floor_value) / max(peak, 1e-6), 0.0, 1.0);
    let blend = smoothstep(0.88, 1.12, peak) * smoothstep(0.10, 0.55, spread);

    let neutral = vec3<f32>(peak);
    return mix(rgb, neutral, blend);
}

fn reconstruct_display_highlights(rgb: vec3<f32>) -> vec3<f32> {
    let peak = max(max(rgb.r, rgb.g), rgb.b);
    let lum = safe_luma(max(rgb, vec3<f32>(0.0)));

    // *** FIX: threshold must be in scene-referred space, not [0,1] display. ***
    // After exposure, normal values can be 0.18–4.0+. The old threshold of
    // 0.92 caught upper midtones and desaturated them.
    let threshold = 1.5 + params.clip * 0.5;
    let blend = smoothstep(threshold, threshold * 2.0, peak);
    return mix(rgb, vec3<f32>(lum), blend * 0.75);
}

fn compress_highlights(rgb: vec3<f32>) -> vec3<f32> {
    if params.hlcompr <= 0.0 {
        return rgb;
    }

    let lum = safe_luma(rgb);

    // *** FIX: old threshold was 0.15 (15% gray), compressing most of the
    // image. New threshold starts at 75% gray, adjustable upward. ***
    let shoulder = 0.75 + params.hlcomprthresh / 400.0;
    let amount = max(params.hlcompr / 100.0, 0.001);

    // Reinhard-style soft knee: smooth, monotonic, asymptotic.
    // The old log formula had a problematic `range = 1 - shoulder` term
    // that went negative when shoulder > 1.
    var compressed = lum;
    if lum > shoulder {
        let excess = lum - shoulder;
        compressed = shoulder + excess / (1.0 + excess * amount / shoulder);
    }
    return rgb * (compressed / lum);
}