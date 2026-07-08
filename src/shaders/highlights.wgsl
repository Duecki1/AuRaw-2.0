// highlights.wgsl

// highlights.wgsl

fn reconstruct_sensor_highlights(rgb: vec3<f32>) -> vec3<f32> {
    let peak = max(max(rgb.r, rgb.g), rgb.b);
    let floor_val = min(min(rgb.r, rgb.g), rgb.b);
    let spread = clamp((peak - floor_val) / max(peak, 1e-6), 0.0, 1.0);
    let blend = smoothstep(0.88, 1.12, peak) * smoothstep(0.10, 0.55, spread);

    if blend <= 0.0 {
        return rgb;
    }

    // Stable default: neutral collapse to preserve luminance
    var result = vec3<f32>(peak);

    let r = rgb.r;
    let g = rgb.g;
    let b = rgb.b;

    // Find the second highest channel to detect strong single-channel clipping
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let mid = max(min(r, g), b); // This is the second highest channel

    // Only attempt ratio reconstruction if one channel is heavily clipped
    // and the other two are relatively unclipped.
    if mx > 0.98 && mx > mid * 1.4 {
        var est = rgb;
        
        if r == mx {
            // R is clipped. Estimate R from G and B.
            est.r = g + (g - b);
        } else if g == mx {
            // G is clipped (rare in Bayer, but possible).
            est.g = r + (r - b);
        } else if b == mx {
            // B is clipped. Estimate B from R and G.
            est.b = r + (r - g);
        }

        // CRITICAL: Never let the reconstructed value drop below the original 
        // clipped value. We know the true intensity is AT LEAST `mx`.
        // This preserves the highlight's brightness while applying the hue correction.
        est.r = max(est.r, mx);
        est.g = max(est.g, mx);
        est.b = max(est.b, mx);

        result = est;
    }

    result = max(result, vec3<f32>(0.0));
    return mix(rgb, result, blend);
}