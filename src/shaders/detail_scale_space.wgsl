// Stage 2: creative detail scale-space.
//
// Texture and Clarity operate on adjacent, non-overlapping Laplacian bands:
//   texture = center - fine base
//   clarity = fine base - broad base
// Capture sharpening is handled earlier in detail_capture.wgsl. Keeping the
// bands disjoint prevents Amount/Texture/Clarity from reinforcing one residual.

fn creative_fine_base_ev(pos: vec2<i32>) -> f32 {
    // Start outside the capture-acutance footprint. The 5x5 bilateral base
    // follows subject-space scaling and rejects hard-edge cross-talk.
    let step = presence_step(1.65, 5);
    return bilateral_log_luminance(pos, 2, step, 10.5);
}

fn creative_broad_base_ev(pos: vec2<i32>) -> f32 {
    let clarity_reference = select(4.5, 5.5, camera_uniforms.tone_guide_radius > 3.5);
    let step = presence_step(clarity_reference, 14);
    return atrous_log_luminance(pos, step, 1.05);
}

fn creative_edge_guard(pos: vec2<i32>) -> f32 {
    let step = presence_step(1.0, 3);
    let left = log_luminance(adjustment_base_at(pos + vec2<i32>(-step, 0)));
    let right = log_luminance(adjustment_base_at(pos + vec2<i32>(step, 0)));
    let up = log_luminance(adjustment_base_at(pos + vec2<i32>(0, -step)));
    let down = log_luminance(adjustment_base_at(pos + vec2<i32>(0, step)));
    let gradient = length(vec2<f32>(right - left, down - up));
    // Preserve local contrast near ordinary texture while backing away from
    // high-contrast silhouettes where wide-band boosts would create halos.
    return 1.0 - 0.78 * smoothstep(0.48, 1.25, gradient);
}

fn apply_texture_and_clarity_values(
    pos: vec2<i32>,
    rgb: vec3<f32>,
    texture_value: f32,
    clarity_value: f32,
) -> vec3<f32> {
    let texture = perceptual_control(texture_value);
    let clarity = perceptual_control(clarity_value);
    if abs(texture) < 1e-6 && abs(clarity) < 1e-6 {
        return rgb;
    }

    let center_ev = log_luminance(rgb);
    let fine_base_ev = creative_fine_base_ev(pos);
    var broad_base_ev = fine_base_ev;
    if abs(clarity) >= 1e-6 {
        broad_base_ev = creative_broad_base_ev(pos);
    }

    // True adjacent Laplacian bands: no shared residual between Texture and
    // Clarity, unlike center-broad formulations that double-count fine detail.
    let texture_band_ev = center_ev - fine_base_ev;
    let clarity_band_ev = fine_base_ev - broad_base_ev;

    let signal_gate = smoothstep(-7.4, -2.35, center_ev);
    let shadow_noise = 1.0 - signal_gate;
    let texture_threshold = mix(0.028, 0.006, signal_gate) * mix(1.0, 1.65, shadow_noise);
    let positive_texture = soft_detail_threshold(texture_band_ev, texture_threshold);
    var negative_texture_base_ev = fine_base_ev;
    if texture < 0.0 {
        // Lightroom's negative endpoint smooths a wider surface band than its
        // positive microcontrast control. The lower range weight follows
        // surface variation while continuing to reject hard silhouettes.
        negative_texture_base_ev = bilateral_log_luminance(
            pos,
            3,
            presence_step(1.65, 5),
            3.0,
        );
    }
    let negative_texture = clamp(center_ev - negative_texture_base_ev, -0.32, 0.32);

    let midtone_gate = smoothstep(-7.0, -2.25, center_ev)
        * (1.0 - 0.74 * smoothstep(0.9, 3.6, center_ev));
    let selected_clarity = soft_detail_threshold(clarity_band_ev, 0.0065);
    let halo_guard = creative_edge_guard(pos);
    let percentiles = tone_percentiles();
    let center_relative_ev = center_ev - log2(SCENE_MIDDLE_GREY);
    let clarity_scene_gate = tone_smoothstep(
        1.0,
        2.5,
        percentiles.p995 - percentiles.p005,
    );
    let clarity_tone_position = tone_smoothstep(
        percentiles.p05 + 0.50,
        percentiles.p50 - 0.10,
        center_relative_ev,
    );
    let positive_clarity_tone = select(
        0.0,
        clarity * mix(-1.25, 0.36, clarity_tone_position) * clarity_scene_gate,
        clarity > 0.0,
    );

    // Positive Texture amplifies the thresholded surface band. Negative
    // Texture is deliberately a convex move toward its bilateral base rather
    // than a signed high-pass gain: a multiplier above one crosses the base
    // and turns smoothing into inverted texture at the -100 endpoint.
    let positive_texture_strength = 7.50 * mix(0.88, 1.12, texture);
    var texture_ev = texture
        * positive_texture
        * positive_texture_strength
        * mix(0.72, 1.0, signal_gate);
    if texture < 0.0 {
        let smoothing = min(
            (-texture) * 0.70 * mix(0.72, 1.0, signal_gate),
            0.90,
        );
        texture_ev = -negative_texture * smoothing;
    }

    let clarity_strength = select(1.55, 5.40, clarity >= 0.0)
        * mix(0.90, 1.10, abs(clarity));
    let clarity_ev = clarity * selected_clarity * clarity_strength * midtone_gate * halo_guard;
    let delta_ev = clamp(texture_ev + clarity_ev + positive_clarity_tone, -1.90, 1.20);
    // Scalar detail gain preserves RGB ratios. Keep already-nonnegative
    // camera-characterized Rec.2020 values exact; invoke the perceptual
    // projector only when the operation creates a negative component.
    return gamut_project_nonnegative_rec2020(rgb * exp2(delta_ev));
}
