// Stage 1: capture detail.
//
// This stage is intentionally restricted to the finest/acutance band. Creative
// Texture/Clarity live in detail_scale_space.wgsl and start at a wider spatial
// scale, so the two stages do not reinforce the same residual and create halos.

fn capture_detail_scale() -> f32 {
    // Capture sharpening follows sensor/acutance detail, not the much wider
    // subject-space scaling used by creative presence controls. A square-root
    // scale keeps proxy/full-resolution rendering visually stable without letting
    // large files turn Radius into a broad local-contrast operator.
    return clamp(sqrt(presence_reference_scale()), 0.74, 1.75);
}

fn capture_sharpen_blur_ev(pos: vec2<i32>, radius_pixels: f32, step: i32) -> f32 {
    let center_ev = log_luminance(adjustment_base_at(pos));
    let sigma_samples = clamp(radius_pixels / max(f32(step), 1.0), 0.58, 1.65);
    var weighted_sum = 0.0;
    var weight_sum = 0.0;

    // Compact bilateral Gaussian: enough support for deconvolution-like
    // acutance recovery, but deliberately too small to become Texture/Clarity.
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let sample_ev = log_luminance(
                adjustment_base_at(pos + vec2<i32>(dx * step, dy * step)),
            );
            let distance_squared = f32(dx * dx + dy * dy);
            let spatial = exp(-0.5 * distance_squared / (sigma_samples * sigma_samples));
            let delta = sample_ev - center_ev;
            // Stronger range rejection than the creative filters prevents
            // bright/dark edge energy from being averaged across an edge.
            let range = exp(-3.4 * delta * delta);
            let weight = spatial * range;
            weighted_sum = weighted_sum + sample_ev * weight;
            weight_sum = weight_sum + weight;
        }
    }
    return weighted_sum / max(weight_sum, 1e-6);
}

fn capture_sharpen_edge_strength(pos: vec2<i32>, step: i32) -> f32 {
    let left = log_luminance(adjustment_base_at(pos + vec2<i32>(-step, 0)));
    let right = log_luminance(adjustment_base_at(pos + vec2<i32>(step, 0)));
    let up = log_luminance(adjustment_base_at(pos + vec2<i32>(0, -step)));
    let down = log_luminance(adjustment_base_at(pos + vec2<i32>(0, step)));
    return length(vec2<f32>(right - left, down - up));
}

fn capture_local_ev_bounds(pos: vec2<i32>) -> vec2<f32> {
    var low = 1e20;
    var high = -1e20;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let value = log_luminance(adjustment_base_at(pos + vec2<i32>(dx, dy)));
            low = min(low, value);
            high = max(high, value);
        }
    }
    return vec2<f32>(low, high);
}

fn capture_impulse_coherence(pos: vec2<i32>, center_ev: f32) -> f32 {
    // Isolated single-pixel residuals are usually noise/demosaic sparkle. Real
    // fine detail tends to have support on at least one neighbouring axis.
    let left = log_luminance(adjustment_base_at(pos + vec2<i32>(-1, 0)));
    let right = log_luminance(adjustment_base_at(pos + vec2<i32>(1, 0)));
    let up = log_luminance(adjustment_base_at(pos + vec2<i32>(0, -1)));
    let down = log_luminance(adjustment_base_at(pos + vec2<i32>(0, 1)));
    let horizontal = min(abs(center_ev - left), abs(center_ev - right));
    let vertical = min(abs(center_ev - up), abs(center_ev - down));
    let support = min(horizontal, vertical);
    return 1.0 - smoothstep(0.055, 0.22, support);
}

fn apply_capture_sharpening(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = clamp(params.creative_effects.w / 150.0, 0.0, 1.0);
    if amount < 1e-6 {
        return rgb;
    }

    let radius = clamp(params.vignette_options.y, 0.5, 3.0);
    let detail = clamp(params.vignette_options.z / 100.0, 0.0, 1.0);
    let masking = clamp(params.vignette_options.w / 100.0, 0.0, 1.0);
    let radius_pixels = radius * capture_detail_scale();
    let step = clamp(i32(round(max(radius_pixels * 0.48, 1.0))), 1, 3);

    let center_ev = log_luminance(rgb);
    let base_ev = capture_sharpen_blur_ev(pos, radius_pixels, step);
    let acutance_ev = center_ev - base_ev;

    // Detail changes selection within the capture band; it does not widen the
    // radius into the creative Texture band. A restrained 4-neighbour residual
    // restores the very finest structure only near the top of the slider.
    let micro_left = log_luminance(adjustment_base_at(pos + vec2<i32>(-1, 0)));
    let micro_right = log_luminance(adjustment_base_at(pos + vec2<i32>(1, 0)));
    let micro_up = log_luminance(adjustment_base_at(pos + vec2<i32>(0, -1)));
    let micro_down = log_luminance(adjustment_base_at(pos + vec2<i32>(0, 1)));
    let micro_base_ev = 0.25 * (micro_left + micro_right + micro_up + micro_down);
    let micro_ev = center_ev - micro_base_ev;
    let selected_band = mix(acutance_ev, mix(acutance_ev, micro_ev, 0.42), detail * detail);

    // Shadow thresholding plus impulse coherence prevents capture sharpening
    // from turning sensor noise into crisp grain before creative detail runs.
    let shadow_noise = 1.0 - smoothstep(-8.2, -3.3, center_ev);
    let detail_threshold = mix(0.015, 0.0045, detail) * mix(1.0, 2.3, shadow_noise);
    let thresholded = soft_detail_threshold(selected_band, detail_threshold);
    let coherence = mix(1.0, capture_impulse_coherence(pos, center_ev), 0.72 + 0.20 * detail);

    var edge_mask = 1.0;
    if masking > 1e-6 {
        let edge_strength = capture_sharpen_edge_strength(pos, 1);
        let edge_threshold = mix(0.035, 0.62, pow(masking, 1.35));
        edge_mask = smoothstep(edge_threshold * 0.72, edge_threshold + 0.16, edge_strength);
    }

    let shadow_gate = smoothstep(-9.2, -4.3, center_ev);
    let highlight_gate = 1.0 - 0.62 * smoothstep(2.6, 6.0, center_ev);
    let strength = amount * mix(1.18, 1.72, detail);
    var sharpen_ev = clamp(
        thresholded * strength * coherence * edge_mask * shadow_gate * highlight_gate,
        -0.28,
        0.32,
    );

    // Constrain overshoot to the local 3x3 envelope plus a tiny allowance.
    // This is the main halo guard: sharpening can increase edge acutance but
    // cannot invent a bright/dark rim outside the neighbourhood extrema.
    let bounds = capture_local_ev_bounds(pos);
    let target_ev = clamp(center_ev + sharpen_ev, bounds.x - 0.018, bounds.y + 0.018);
    sharpen_ev = target_ev - center_ev;
    return max(rgb * exp2(sharpen_ev), vec3<f32>(0.0));
}
