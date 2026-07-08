fn reconstruct_sensor_highlights(rgb: vec3<f32>) -> vec3<f32> {
    let peak = max(max(rgb.r, rgb.g), rgb.b);
    let floor_value = min(min(rgb.r, rgb.g), rgb.b);
    let spread = clamp((peak - floor_value) / max(peak, 1e-6), 0.0, 1.0);
    let blend = smoothstep(0.88, 1.12, peak) * smoothstep(0.10, 0.55, spread);

    // Blend toward the channel peak rather than an unweighted r+g+b average.
    // This is still camera-native RGB (pre color-matrix), where channel
    // magnitudes reflect sensor gain, not perceptual luminance. Averaging
    // unweighted here pushes highlights toward the wrong hue once the
    // camera->sRGB matrix runs afterward. Using max() reconstructs toward
    // a clipped-but-neutral white point instead, avoiding the color cast.
    let neutral = vec3<f32>(peak);
    return mix(rgb, neutral, blend);
}

fn reconstruct_display_highlights(rgb: vec3<f32>) -> vec3<f32> {
    let peak = max(max(rgb.r, rgb.g), rgb.b);
    let lum = safe_luma(max(rgb, vec3<f32>(0.0)));
    let threshold = 0.92 + params.clip * 0.04;
    let blend = smoothstep(threshold, 1.30, peak);
    return mix(rgb, vec3<f32>(lum), blend * 0.75);
}

fn compress_highlights(rgb: vec3<f32>) -> vec3<f32> {
    if params.hlcompr <= 0.0 {
        return rgb;
    }

    let lum = safe_luma(rgb);
    let shoulder = params.hlcomprthresh / 800.0 + 0.15;
    let range = max(1.0 - shoulder, 1e-3);
    let amount = params.hlcompr / 60.0;
    let compressed = select(
        lum,
        shoulder + range * log(1.0 + (lum - shoulder) * amount / range) / log(1.0 + amount),
        lum > shoulder
    );
    return rgb * (compressed / lum);
}
