// Post-demosaic scene-linear controls. Global controls are prepared into a
// full-precision texture first. Local detail Effects sample that exact image,
// then creative Effects sample the completed local-effects result. Keeping the
// stages separate prevents blur/detail residuals from becoming global exposure
// changes and gives Glow a same-stage highlight source.

@group(0) @binding(11) var scene_tex: texture_2d<f32>;
@group(0) @binding(12) var out_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(21) var adjustment_base_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(22) var adjustment_base_tex: texture_2d<f32>;
@group(0) @binding(23) var local_effects_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(24) var local_effects_tex: texture_2d<f32>;
@group(0) @binding(25) var creative_effects_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(26) var final_adjustment_tex: texture_2d<f32>;
@group(0) @binding(27) var local_mask_tex: texture_2d_array<f32>;
@group(0) @binding(28) var local_mask_sampler: sampler;
@group(0) @binding(29) var display_linear_out: texture_storage_2d<rgba16float, write>;

struct LocalAdjustmentMix {
    tone0: vec4<f32>,
    tone1: vec4<f32>,
    effects: vec4<f32>,
}

fn local_adjustment_mix(pos: vec2<i32>) -> LocalAdjustmentMix {
    var tone0 = vec4<f32>(0.0);
    var tone1 = vec4<f32>(0.0);
    var effects = vec4<f32>(0.0);
    let full_size = vec2<f32>(
        f32(max(params.full_width, 1u)),
        f32(max(params.full_height, 1u)),
    );
    let global_pos = vec2<f32>(pos + tile_origin()) + vec2<f32>(0.5);
    let uv = clamp(global_pos / full_size, vec2<f32>(0.0), vec2<f32>(1.0));
    let count = min(params.mask_counts.x, 8u);
    for (var index = 0u; index < 8u; index = index + 1u) {
        if index >= count {
            break;
        }
        let mask_state = params.mask_meta[index];
        if mask_state.x == 0u || mask_state.y == 0u {
            continue;
        }
        let weight = textureSampleLevel(
            local_mask_tex,
            local_mask_sampler,
            uv,
            i32(index),
            0.0,
        ).x;
        if weight <= 1e-5 {
            continue;
        }
        tone0 = tone0 + params.mask_adjust_0[index] * weight;
        tone1 = tone1 + params.mask_adjust_1[index] * weight;
        effects = effects + params.mask_adjust_2[index] * weight;
    }
    return LocalAdjustmentMix(tone0, tone1, effects);
}

fn scene_working_at(pos: vec2<i32>) -> vec3<f32> {
    let camera_rgb = textureLoad(scene_tex, clamp_pos(pos), 0).xyz;
    let white_balanced_camera = apply_camera_temperature_tint(camera_rgb);
    let working = map_negative_gamut(cam_to_working(white_balanced_camera));
    let profile_corrected = map_negative_gamut(apply_profile_hue_sat(working));
    let profile_exposure_ev = bitcast<f32>(params.profile_flags.z);
    return profile_corrected * exp2(profile_exposure_ev);
}

fn adjustment_base_at(pos: vec2<i32>) -> vec3<f32> {
    return max(textureLoad(adjustment_base_tex, clamp_pos(pos), 0).xyz, vec3<f32>(0.0));
}

fn local_effects_at(pos: vec2<i32>) -> vec3<f32> {
    return max(textureLoad(local_effects_tex, clamp_pos(pos), 0).xyz, vec3<f32>(0.0));
}

fn log_luminance(rgb: vec3<f32>) -> f32 {
    return log2(safe_luma(max(rgb, vec3<f32>(0.0))));
}

fn bilateral_log_luminance(pos: vec2<i32>, radius: i32, range_strength: f32) -> f32 {
    let center = log_luminance(adjustment_base_at(pos));
    let sigma = max(f32(radius) * 0.72, 0.85);
    var sum = 0.0;
    var sum_w = 0.0;

    // The shader has a fixed maximum footprint so mobile and desktop compile
    // the same code. `radius` selects 3x3, 5x5, or 7x7 behavior at runtime.
    for (var dy = -3; dy <= 3; dy = dy + 1) {
        for (var dx = -3; dx <= 3; dx = dx + 1) {
            if abs(dx) > radius || abs(dy) > radius { continue; }
            let sample_ev = log_luminance(adjustment_base_at(pos + vec2<i32>(dx, dy)));
            let distance_squared = f32(dx * dx + dy * dy);
            let spatial = exp(-0.5 * distance_squared / (sigma * sigma));
            let delta = sample_ev - center;
            let range = exp(-range_strength * delta * delta);
            let weight = spatial * range;
            sum = sum + sample_ev * weight;
            sum_w = sum_w + weight;
        }
    }
    return sum / max(sum_w, 1e-6);
}

fn atrous_kernel_weight(offset: i32) -> f32 {
    switch abs(offset) {
        case 0: { return 6.0; }
        case 1: { return 4.0; }
        default: { return 1.0; }
    }
}

fn atrous_log_luminance(pos: vec2<i32>, step: i32, range_strength: f32) -> f32 {
    let center = log_luminance(adjustment_base_at(pos));
    var sum = 0.0;
    var sum_w = 0.0;
    // A 5x5 B3-spline à-trous kernel samples a much wider spatial scale than
    // a dense 7x7 blur at lower cost. This gives Clarity a genuinely mid-scale
    // response (roughly 25-35 preview pixels) while remaining edge-aware.
    for (var ky = -2; ky <= 2; ky = ky + 1) {
        for (var kx = -2; kx <= 2; kx = kx + 1) {
            let sample_pos = pos + vec2<i32>(kx * step, ky * step);
            let sample_ev = log_luminance(adjustment_base_at(sample_pos));
            let delta = sample_ev - center;
            let spatial = atrous_kernel_weight(kx) * atrous_kernel_weight(ky);
            let range = exp(-range_strength * delta * delta);
            let weight = spatial * range;
            sum = sum + sample_ev * weight;
            sum_w = sum_w + weight;
        }
    }
    return sum / max(sum_w, 1e-6);
}

fn soft_detail_threshold(detail: f32, threshold: f32) -> f32 {
    return sign(detail) * max(abs(detail) - threshold, 0.0);
}

fn apply_texture_and_clarity_values(
    pos: vec2<i32>,
    rgb: vec3<f32>,
    texture_value: f32,
    clarity_value: f32,
) -> vec3<f32> {
    let texture = clamp(texture_value / 100.0, -1.0, 1.0);
    let clarity = clamp(clarity_value / 100.0, -1.0, 1.0);
    if abs(texture) < 1e-6 && abs(clarity) < 1e-6 {
        return rgb;
    }

    let center_ev = log_luminance(rgb);
    let fine_base_ev = bilateral_log_luminance(pos, 1, 8.0);
    let clarity_step = select(3, 4, params.tone_guide_radius > 3.5);
    let broad_base_ev = atrous_log_luminance(pos, clarity_step, 1.35);

    // Two true band-pass residuals. Because every term comes from the same
    // developed texture, a flat field produces exactly zero effect instead of
    // a global exposure offset.
    let fine_detail_ev = center_ev - fine_base_ev;
    let mid_detail_ev = fine_base_ev - broad_base_ev;

    // Fine-detail enhancement is noise-aware. At low signal, positive Texture
    // ignores tiny residuals that are more likely sensor/demosaic noise than
    // surface structure; negative Texture can still smooth them naturally.
    let signal_gate = smoothstep(-7.5, -2.5, center_ev);
    let fine_threshold = mix(0.070, 0.016, signal_gate);
    let positive_fine = soft_detail_threshold(fine_detail_ev, fine_threshold);
    let selected_fine = select(fine_detail_ev, positive_fine, texture >= 0.0);

    // Clarity is restricted to midtones and a broader spatial band. The soft
    // shoulder avoids halos around deep silhouettes and specular highlights.
    let midtone_gate = smoothstep(-6.0, -2.0, center_ev)
        * (1.0 - smoothstep(0.7, 3.5, center_ev));
    let selected_mid = soft_detail_threshold(mid_detail_ev, 0.010);

    let texture_ev = texture * selected_fine * 0.85;
    let clarity_ev = clarity * selected_mid * 2.00 * midtone_gate;
    let delta_ev = clamp(texture_ev + clarity_ev, -1.25, 1.25);
    return max(rgb * exp2(delta_ev), vec3<f32>(0.0));
}

fn apply_texture_and_clarity(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    return apply_texture_and_clarity_values(pos, rgb, params.presence.x, params.presence.y);
}

fn local_dark_channel(pos: vec2<i32>, radius: i32) -> f32 {
    var dark = 1e20;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            if abs(dx) > radius || abs(dy) > radius { continue; }
            let sample = adjustment_base_at(pos + vec2<i32>(dx, dy));
            dark = min(dark, min(sample.r, min(sample.g, sample.b)));
        }
    }
    return max(dark, 0.0);
}

fn apply_dehaze_value(pos: vec2<i32>, rgb: vec3<f32>, value: f32) -> vec3<f32> {
    let amount = clamp(value / 100.0, -1.0, 1.0);
    if abs(amount) < 1e-6 {
        return rgb;
    }

    // Estimate atmospheric veil from the same developed image used by the
    // other Effects. The neutral airlight model changes local contrast and
    // saturation together, rather than behaving like another Exposure slider.
    let center_lum = safe_luma(rgb);
    let broad_ev = bilateral_log_luminance(pos, 2, 1.6);
    let broad_lum = exp2(broad_ev);
    let dark = local_dark_channel(pos, 2);
    let veil = clamp(dark / max(broad_lum, 1e-6), 0.0, 1.0);
    let airlight_lum = max(center_lum, broad_lum);
    let airlight = vec3<f32>(airlight_lum);

    if amount > 0.0 {
        let transmission = clamp(1.0 - amount * veil * 0.72, 0.38, 1.0);
        let restored = (rgb - airlight * (1.0 - transmission)) / transmission;
        return max(restored, vec3<f32>(0.0));
    }

    let haze = -amount;
    let haze_mix = haze * (0.22 + 0.38 * (1.0 - veil));
    return mix(rgb, airlight, clamp(haze_mix, 0.0, 0.60));
}

fn apply_dehaze(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    return apply_dehaze_value(pos, rgb, params.presence.z);
}


fn extended_perceptual_luminance(linear_luma: f32) -> f32 {
    if linear_luma <= 1.0 {
        return pow(max(linear_luma, 0.0), 1.0 / 2.2);
    }
    return 1.0 + pow(linear_luma - 1.0, 1.0 / 2.2);
}

fn glow_emission(rgb: vec3<f32>, cutoff: f32) -> vec3<f32> {
    let linear_luma = safe_luma(rgb);
    let perceptual_luma = extended_perceptual_luminance(linear_luma);
    let cutoff_fade = smoothstep(cutoff, cutoff + 0.16, perceptual_luma);
    let excess = max(perceptual_luma - cutoff, 0.0);
    let range = max(2.25 - cutoff, 0.25);
    let intensity = pow(smoothstep(0.0, range, excess), 0.48);
    let black_gate = pow(smoothstep(0.0, 0.42, linear_luma), 0.5);

    // Preserve the source hue while giving the bloom the subtle warm bias of
    // optical diffusion. The source is normalized by luminance so a coloured
    // light blooms in its own colour instead of becoming neutral grey. The
    // ratio is softly clamped so very narrow-band highlights cannot explode
    // into dotted colour speckles when the blur radius becomes large.
    let colour_ratio = clamp(rgb / max(linear_luma, 1e-6), vec3<f32>(0.0), vec3<f32>(3.5));
    let warm_tint = vec3<f32>(1.025, 1.0, 0.975);
    return colour_ratio * warm_tint
        * intensity * pow(linear_luma, 0.62) * cutoff_fade * black_gate;
}

fn glow_source_at(pos: vec2<i32>, cutoff: f32) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    var sum_weight = 0.0;
    let center_luma = safe_luma(local_effects_at(pos));

    // A small, edge-aware prefilter suppresses isolated sparkle pixels and
    // demosaic specks before the larger bloom blur is applied. This keeps Glow
    // smooth and photographic instead of producing pointillist dots.
    for (var ky = -1; ky <= 1; ky = ky + 1) {
        for (var kx = -1; kx <= 1; kx = kx + 1) {
            let sample_pos = pos + vec2<i32>(kx, ky);
            let sample_rgb = local_effects_at(sample_pos);
            let sample_luma = safe_luma(sample_rgb);
            let spatial = select(2.0, 4.0, kx == 0 && ky == 0)
                * select(1.0, 2.0, kx == 0 || ky == 0);
            let range = exp(-8.0 * abs(sample_luma - center_luma));
            let weight = spatial * range;
            sum = sum + glow_emission(sample_rgb, cutoff) * weight;
            sum_weight = sum_weight + weight;
        }
    }
    return sum / max(sum_weight, 1e-6);
}

fn glow_blur_at(pos: vec2<i32>, step: i32, cutoff: f32) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    var sum_weight = 0.0;
    // A separable B3-spline kernel written as one 5x5 gather. The glow source
    // has already been prefiltered, so the large-radius gathers stay smooth
    // instead of turning isolated bright pixels into repeated dot artefacts.
    for (var ky = -2; ky <= 2; ky = ky + 1) {
        for (var kx = -2; kx <= 2; kx = kx + 1) {
            let weight = atrous_kernel_weight(kx) * atrous_kernel_weight(ky);
            let sample_pos = pos + vec2<i32>(kx * step, ky * step);
            sum = sum + glow_source_at(sample_pos, cutoff) * weight;
            sum_weight = sum_weight + weight;
        }
    }
    return sum / max(sum_weight, 1e-6);
}

fn apply_glow(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = clamp(params.creative_effects.x / 100.0, 0.0, 1.0);
    if amount < 1e-6 {
        return rgb;
    }

    let radius = clamp(params.creative_effects.y / 100.0, 0.0, 1.0);
    let threshold = clamp(params.creative_effects.z / 100.0, 0.0, 1.0);
    let cutoff = mix(0.06, 0.92, pow(threshold, 1.12));
    let reference_scale = clamp(
        f32(min(params.full_width, params.full_height)) / 1080.0,
        0.45,
        3.0,
    );
    let step_f = mix(1.0, 9.0, pow(radius, 1.35)) * reference_scale;
    let step_core = i32(clamp(round(max(step_f * 0.5, 1.0)), 1.0, 16.0));
    let step_near = i32(clamp(round(step_f), 1.0, 28.0));
    let step_far = min(step_near * 2, 48);

    let core_bloom = glow_blur_at(pos, step_core, cutoff);
    let near_bloom = glow_blur_at(pos, step_near, cutoff);
    let far_bloom = glow_blur_at(pos, step_far, cutoff);
    let bloom = core_bloom * 0.26
        + near_bloom * (0.48 - radius * 0.08)
        + far_bloom * (0.26 + radius * 0.08);

    // Very bright cores already carry their own energy. Protecting them keeps
    // Glow from clipping the light source while the blurred halo expands into
    // the surrounding darker pixels.
    let current_luma = safe_luma(rgb);
    let core_protection = 1.0 - 0.72 * smoothstep(1.0, 3.2, current_luma);
    return max(rgb + bloom * amount * 3.0 * core_protection, vec3<f32>(0.0));
}

fn full_image_uv(pos: vec2<i32>) -> vec2<f32> {
    let dimensions = max(
        vec2<f32>(f32(params.full_width), f32(params.full_height)),
        vec2<f32>(1.0),
    );
    let global_pos = clamp(pos + tile_origin(), vec2<i32>(0), full_image_max());
    return (vec2<f32>(global_pos) + vec2<f32>(0.5)) / dimensions;
}

fn vignette_distance(pos: vec2<i32>, roundness: f32) -> f32 {
    let dimensions = max(
        vec2<f32>(f32(params.full_width), f32(params.full_height)),
        vec2<f32>(1.0),
    );
    let p = abs(full_image_uv(pos) * 2.0 - vec2<f32>(1.0));
    let frame_ellipse = length(p);
    let frame_rectangle = pow(pow(p.x, 8.0) + pow(p.y, 8.0), 1.0 / 8.0);
    let short_dimension = max(min(dimensions.x, dimensions.y), 1.0);
    let image_circle = length(vec2<f32>(
        p.x * dimensions.x / short_dimension,
        p.y * dimensions.y / short_dimension,
    ));

    if roundness < 0.0 {
        return mix(frame_ellipse, frame_rectangle, -roundness);
    }
    return mix(frame_ellipse, image_circle, roundness);
}

fn apply_vignette(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = clamp(params.vignette.x / 100.0, -1.0, 1.0);
    if abs(amount) < 1e-6 {
        return rgb;
    }

    let midpoint = clamp(params.vignette.y / 100.0, 0.0, 1.0);
    let roundness = clamp(params.vignette.z / 100.0, -1.0, 1.0);
    let feather = clamp(params.vignette.w / 100.0, 0.0, 1.0);
    let distance = vignette_distance(pos, roundness);

    // Midpoint controls where the vignette is centred. Feather should then
    // spread the transition across a long distance instead of compressing most
    // of the darkening into the final few pixels near the edge.
    let midpoint_shaped = pow(midpoint, 0.80);
    let transition_center = mix(0.18, 0.992, midpoint_shaped);
    let inward_softness = mix(0.010, 0.72, feather)
        * mix(1.0, 0.62, midpoint_shaped * midpoint_shaped);
    let outward_softness = mix(0.020, 0.46, feather)
        * mix(1.0, 0.80, midpoint_shaped * midpoint_shaped);
    let transition_start = max(transition_center - inward_softness, 0.0);
    let transition_end = min(
        max(transition_center + outward_softness, transition_start + 0.02),
        1.0,
    );
    let mask = smoothstep(transition_start, transition_end, distance);

    // Lightroom-style negative vignettes are stronger than positive edge
    // brightening. Highlights restores bright edge detail only for a dark
    // vignette and never changes hue because the operation is a scalar gain.
    let edge_ev = select(amount * 2.45, amount * 1.55, amount > 0.0);
    var highlight_protection = 1.0;
    if amount < 0.0 {
        let highlights = clamp(params.vignette_options.x / 100.0, 0.0, 1.0);
        highlight_protection = 1.0
            - highlights * smoothstep(0.50, 2.4, safe_luma(rgb));
    }
    let delta_ev = clamp(edge_ev * mask * highlight_protection, -3.0, 2.0);
    return max(rgb * exp2(delta_ev), vec3<f32>(0.0));
}

// Lightroom's named HSL channels are a UI model, not a reason to process in
// mathematical HSL. HSL lightness is based on per-pixel RGB extrema and turns
// demosaic/chroma noise into luminance noise. The mixer below instead selects
// hues in perceptual OKLab, stabilizes only the selector with an edge-aware
// neighbourhood, and applies luminance as a scene-linear exposure gain.

struct MixerSample {
    lab: vec3<f32>,
    chroma: f32,
    hue_vector: vec2<f32>,
    confidence: f32,
}

struct MixerBandWeights {
    first: vec4<f32>,
    second: vec4<f32>,
    total: f32,
}

fn max_abs_vec4(value: vec4<f32>) -> f32 {
    return max(max(abs(value.x), abs(value.y)), max(abs(value.z), abs(value.w)));
}

fn color_mixer_strength() -> vec3<f32> {
    return vec3<f32>(
        max(max_abs_vec4(params.hsl_hue_0), max_abs_vec4(params.hsl_hue_1)),
        max(max_abs_vec4(params.hsl_saturation_0), max_abs_vec4(params.hsl_saturation_1)),
        max(max_abs_vec4(params.hsl_luminance_0), max_abs_vec4(params.hsl_luminance_1)),
    );
}

fn circular_distance(a: f32, b: f32) -> f32 {
    let d = abs(a - b);
    return min(d, 1.0 - d);
}

fn smooth_hue_bell(hue: f32, anchor: f32, width: f32) -> f32 {
    let t = clamp(1.0 - circular_distance(hue, anchor) / width, 0.0, 1.0);
    let feather = t * t * (3.0 - 2.0 * t);
    // Squaring the smoothstep keeps channel centers precise while retaining
    // soft overlaps between Lightroom's eight named colour ranges.
    return feather * feather;
}

fn mixer_band_weights(hue: f32) -> MixerBandWeights {
    // Red, orange, yellow, green, aqua, blue, purple, magenta. The
    // anchors are the OKLab hue angles of fully saturated sRGB swatches at
    // HSL hue 0, 30, 60, 120, 180, 240, 270 and 300 degrees. Using ordinary
    // HSL angles directly in OKLab would mislabel red as orange and yellow as
    // green. Widths follow the unequal perceptual spacing between anchors.
    let first = vec4<f32>(
        smooth_hue_bell(hue, 0.0812052, 0.160),
        smooth_hue_bell(hue, 0.1465993, 0.150),
        smooth_hue_bell(hue, 0.3049145, 0.150),
        smooth_hue_bell(hue, 0.3958204, 0.140),
    );
    let second = vec4<f32>(
        smooth_hue_bell(hue, 0.5410248, 0.180),
        smooth_hue_bell(hue, 0.7334778, 0.180),
        smooth_hue_bell(hue, 0.8160390, 0.100),
        smooth_hue_bell(hue, 0.9121206, 0.160),
    );
    return MixerBandWeights(first, second, dot(first, vec4<f32>(1.0)) + dot(second, vec4<f32>(1.0)));
}

fn mixer_band_value(weights: MixerBandWeights, first: vec4<f32>, second: vec4<f32>) -> f32 {
    return (dot(weights.first, first) + dot(weights.second, second)) / max(weights.total, 1e-6);
}

fn directed_hue_shift(value: f32, backward_span: f32, forward_span: f32) -> f32 {
    let amount = clamp(value / 100.0, -1.0, 1.0);
    let span = select(backward_span, forward_span, amount >= 0.0);
    // At an endpoint, move close to the adjacent named channel without fully
    // collapsing both channels onto the same hue.
    return amount * span * 0.90;
}

fn mixer_hue_shift(weights: MixerBandWeights) -> f32 {
    // Backward/forward spans are distances to the adjacent calibrated
    // OKLab anchors. This makes each slider endpoint move toward the color
    // shown next to it in the Lightroom-style channel order.
    let first = vec4<f32>(
        directed_hue_shift(params.hsl_hue_0.x, 0.1690846, 0.0653940),
        directed_hue_shift(params.hsl_hue_0.y, 0.0653940, 0.1583152),
        directed_hue_shift(params.hsl_hue_0.z, 0.1583152, 0.0909059),
        directed_hue_shift(params.hsl_hue_0.w, 0.0909059, 0.1452044),
    );
    let second = vec4<f32>(
        directed_hue_shift(params.hsl_hue_1.x, 0.1452044, 0.1924530),
        directed_hue_shift(params.hsl_hue_1.y, 0.1924530, 0.0825612),
        directed_hue_shift(params.hsl_hue_1.z, 0.0825612, 0.0960816),
        directed_hue_shift(params.hsl_hue_1.w, 0.0960816, 0.1690846),
    );
    return (dot(weights.first, first) + dot(weights.second, second)) / max(weights.total, 1e-6);
}

fn mixer_sample_from_rgb(rgb: vec3<f32>) -> MixerSample {
    let lab = linear_srgb_to_oklab(REC2020_TO_SRGB * max(rgb, vec3<f32>(0.0)));
    let chroma = length(lab.yz);
    var hue_vector = vec2<f32>(1.0, 0.0);
    if chroma > 1e-9 {
        hue_vector = lab.yz / chroma;
    }

    // Chroma is judged relative to perceptual lightness. This makes nearly
    // neutral pixels in shadows and highlights ineligible for arbitrary hue
    // classification, while retaining low-saturation real colours.
    let relative_chroma = chroma / max(0.028 + 0.095 * max(lab.x, 0.0), 0.028);
    let chroma_confidence = smoothstep(0.10, 0.62, relative_chroma);
    let signal_confidence = smoothstep(0.035, 0.115, max(lab.x, 0.0));
    let confidence = chroma_confidence * signal_confidence;
    return MixerSample(lab, chroma, hue_vector, confidence);
}

fn stabilized_mixer_sample(pos: vec2<i32>, center_rgb: vec3<f32>) -> MixerSample {
    let center = mixer_sample_from_rgb(center_rgb);
    if center.confidence < 1e-5 {
        return center;
    }

    // A compact bilateral selector filter is enough to suppress Bayer/X-Trans
    // chroma speckle without softening image detail. Only hue selection is
    // filtered; the actual RGB detail always comes from the center pixel.
    var vector_sum = center.hue_vector * center.confidence * 4.0;
    var weight_sum = center.confidence * 4.0;
    // Desktop/high-quality rendering already uses a wider tone guide, so use
    // a 5x5 selector there. Android preview keeps a 3x3 selector for speed.
    let selector_radius = select(1, 2, params.tone_guide_radius > 3.5);
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            if (dx == 0 && dy == 0) || abs(dx) > selector_radius || abs(dy) > selector_radius {
                continue;
            }
            let neighbour_rgb = textureLoad(final_adjustment_tex, clamp_pos(pos + vec2<i32>(dx, dy)), 0).xyz;
            let neighbour = mixer_sample_from_rgb(neighbour_rgb);
            if neighbour.confidence < 1e-5 {
                continue;
            }

            let distance_squared = f32(dx * dx + dy * dy);
            let spatial = 1.0 / (1.0 + 0.65 * distance_squared);
            let lightness_delta = neighbour.lab.x - center.lab.x;
            let chroma_delta = neighbour.chroma - center.chroma;
            let hue_agreement = clamp(dot(center.hue_vector, neighbour.hue_vector), -1.0, 1.0);
            let range_weight = 1.0 / (
                1.0
                + 72.0 * lightness_delta * lightness_delta
                + 34.0 * chroma_delta * chroma_delta
                + 8.0 * (1.0 - hue_agreement)
            );
            let weight = spatial * range_weight * neighbour.confidence;
            vector_sum = vector_sum + neighbour.hue_vector * weight;
            weight_sum = weight_sum + weight;
        }
    }

    var stable_hue = center.hue_vector;
    let vector_length = length(vector_sum);
    if weight_sum > 1e-5 && vector_length > 1e-5 {
        stable_hue = vector_sum / vector_length;
    }
    return MixerSample(center.lab, center.chroma, stable_hue, center.confidence);
}

fn positive_rec2020_from_oklab(lightness: f32, hue_vector: vec2<f32>, requested_chroma: f32) -> vec3<f32> {
    // Hue/saturation moves can leave the positive Rec.2020 working gamut.
    // Compress chroma at constant OKLab lightness and hue instead of clipping
    // RGB channels, which would visibly change hue and create hard boundaries.
    var low = 0.0;
    var high = max(requested_chroma, 0.0);
    let requested = SRGB_TO_REC2020 * oklab_to_linear_srgb(
        vec3<f32>(lightness, hue_vector * high),
    );
    if min(requested.r, min(requested.g, requested.b)) >= 0.0 {
        return requested;
    }

    // Chroma zero is a valid neutral fallback for every non-negative
    // lightness, so the binary search always starts from a valid candidate.
    var candidate = SRGB_TO_REC2020 * oklab_to_linear_srgb(
        vec3<f32>(lightness, vec2<f32>(0.0)),
    );
    for (var iteration = 0; iteration < 6; iteration = iteration + 1) {
        let middle = 0.5 * (low + high);
        let probe = SRGB_TO_REC2020 * oklab_to_linear_srgb(
            vec3<f32>(lightness, hue_vector * middle),
        );
        if min(probe.r, min(probe.g, probe.b)) >= 0.0 {
            low = middle;
            candidate = probe;
        } else {
            high = middle;
        }
    }
    return max(candidate, vec3<f32>(0.0));
}

fn mixer_saturation_factor(amount: f32) -> f32 {
    let value = clamp(amount, -1.0, 1.0);
    if value >= 0.0 {
        // Positive saturation has a soft shoulder; +100 is strong but does not
        // produce the brittle 2x channel excursion of a simple RGB multiplier.
        return exp2(value * 0.85);
    }
    return max(1.0 + value, 0.0);
}

fn mixer_luminance_ev(amount: f32, lightness: f32) -> f32 {
    let value = clamp(amount, -1.0, 1.0);
    let endpoint_ev = select(1.45, 1.20, value >= 0.0);
    // Avoid turning barely-exposed chroma noise into bright coloured pixels.
    // This is a smooth signal confidence, not a hard shadow threshold.
    let signal = smoothstep(0.040, 0.135, max(lightness, 0.0));
    let hdr_guard = 1.0 / (1.0 + 0.20 * max(lightness - 1.0, 0.0));
    return value * endpoint_ev * signal * hdr_guard;
}

fn local_curve_block(mask_index: u32, curve: u32, block: u32) -> vec4<f32> {
    if curve == 1u {
        switch block {
            case 0u: { return params.mask_curve_red_0[mask_index]; }
            case 1u: { return params.mask_curve_red_1[mask_index]; }
            case 2u: { return params.mask_curve_red_2[mask_index]; }
            case 3u: { return params.mask_curve_red_3[mask_index]; }
            case 4u: { return params.mask_curve_red_4[mask_index]; }
            case 5u: { return params.mask_curve_red_5[mask_index]; }
            case 6u: { return params.mask_curve_red_6[mask_index]; }
            default: { return params.mask_curve_red_7[mask_index]; }
        }
    }
    if curve == 2u {
        switch block {
            case 0u: { return params.mask_curve_green_0[mask_index]; }
            case 1u: { return params.mask_curve_green_1[mask_index]; }
            case 2u: { return params.mask_curve_green_2[mask_index]; }
            case 3u: { return params.mask_curve_green_3[mask_index]; }
            case 4u: { return params.mask_curve_green_4[mask_index]; }
            case 5u: { return params.mask_curve_green_5[mask_index]; }
            case 6u: { return params.mask_curve_green_6[mask_index]; }
            default: { return params.mask_curve_green_7[mask_index]; }
        }
    }
    if curve == 3u {
        switch block {
            case 0u: { return params.mask_curve_blue_0[mask_index]; }
            case 1u: { return params.mask_curve_blue_1[mask_index]; }
            case 2u: { return params.mask_curve_blue_2[mask_index]; }
            case 3u: { return params.mask_curve_blue_3[mask_index]; }
            case 4u: { return params.mask_curve_blue_4[mask_index]; }
            case 5u: { return params.mask_curve_blue_5[mask_index]; }
            case 6u: { return params.mask_curve_blue_6[mask_index]; }
            default: { return params.mask_curve_blue_7[mask_index]; }
        }
    }
    switch block {
        case 0u: { return params.mask_curve_0[mask_index]; }
        case 1u: { return params.mask_curve_1[mask_index]; }
        case 2u: { return params.mask_curve_2[mask_index]; }
        case 3u: { return params.mask_curve_3[mask_index]; }
        case 4u: { return params.mask_curve_4[mask_index]; }
        case 5u: { return params.mask_curve_5[mask_index]; }
        case 6u: { return params.mask_curve_6[mask_index]; }
        default: { return params.mask_curve_7[mask_index]; }
    }
}

fn local_curve_value(mask_index: u32, curve: u32, input: f32) -> f32 {
    let position = clamp(input, 0.0, 1.0) * 31.0;
    let lower = u32(floor(position));
    let upper = min(lower + 1u, 31u);
    let first = local_curve_block(mask_index, curve, lower / 4u)[lower % 4u];
    let second = local_curve_block(mask_index, curve, upper / 4u)[upper % 4u];
    return mix(first, second, fract(position));
}

fn local_hue_shift(weights: MixerBandWeights, first_values: vec4<f32>, second_values: vec4<f32>) -> f32 {
    let first = vec4<f32>(
        directed_hue_shift(first_values.x, 0.1690846, 0.0653940),
        directed_hue_shift(first_values.y, 0.0653940, 0.1583152),
        directed_hue_shift(first_values.z, 0.1583152, 0.0909059),
        directed_hue_shift(first_values.w, 0.0909059, 0.1452044),
    );
    let second = vec4<f32>(
        directed_hue_shift(second_values.x, 0.1452044, 0.1924530),
        directed_hue_shift(second_values.y, 0.1924530, 0.0825612),
        directed_hue_shift(second_values.z, 0.0825612, 0.0960816),
        directed_hue_shift(second_values.w, 0.0960816, 0.1690846),
    );
    return (dot(weights.first, first) + dot(weights.second, second)) / max(weights.total, 1e-6);
}

fn apply_local_curve_and_hsl(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let full_size = vec2<f32>(f32(max(params.full_width, 1u)), f32(max(params.full_height, 1u)));
    let global_pos = vec2<f32>(pos + tile_origin()) + vec2<f32>(0.5);
    let uv = clamp(global_pos / full_size, vec2<f32>(0.0), vec2<f32>(1.0));
    let count = min(params.mask_counts.x, 8u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = params.mask_meta[index];
        if state.x == 0u || (state.z == 0u && state.w == 0u) { continue; }
        let weight = textureSampleLevel(local_mask_tex, local_mask_sampler, uv, i32(index), 0.0).x;
        if weight <= 1e-5 { continue; }
        var adjusted = rgb;
        if (state.z & 1u) != 0u {
            let luminance = max(dot(adjusted, LUMA), 0.0);
            let curved = scene_curve_decode(local_curve_value(index, 0u, scene_curve_encode(luminance)));
            adjusted = select(vec3<f32>(curved), adjusted * clamp(curved / luminance, 0.0, 256.0), luminance > 1e-9);
        }
        if (state.z & 2u) != 0u && adjusted.r >= 0.0 {
            adjusted.r = scene_curve_decode(local_curve_value(index, 1u, scene_curve_encode(adjusted.r)));
        }
        if (state.z & 4u) != 0u && adjusted.g >= 0.0 {
            adjusted.g = scene_curve_decode(local_curve_value(index, 2u, scene_curve_encode(adjusted.g)));
        }
        if (state.z & 8u) != 0u && adjusted.b >= 0.0 {
            adjusted.b = scene_curve_decode(local_curve_value(index, 3u, scene_curve_encode(adjusted.b)));
        }
        if state.w != 0u {
            let sample = mixer_sample_from_rgb(adjusted);
            if sample.confidence > 1e-5 {
                let hue = fract(atan2(sample.hue_vector.y, sample.hue_vector.x) / (2.0 * 3.14159265359) + 1.0);
                let bands = mixer_band_weights(hue);
                let hue_shift = local_hue_shift(bands, params.mask_hsl_hue_0[index], params.mask_hsl_hue_1[index]) * sample.confidence;
                let saturation = mixer_band_value(bands, params.mask_hsl_saturation_0[index], params.mask_hsl_saturation_1[index]) / 100.0 * sample.confidence;
                let luminance = mixer_band_value(bands, params.mask_hsl_luminance_0[index], params.mask_hsl_luminance_1[index]) / 100.0 * sample.confidence;
                if abs(hue_shift) > 1e-7 || abs(saturation) > 1e-7 {
                    let angle = atan2(sample.lab.z, sample.lab.y) + hue_shift * 2.0 * 3.14159265359;
                    adjusted = positive_rec2020_from_oklab(
                        sample.lab.x,
                        vec2<f32>(cos(angle), sin(angle)),
                        sample.chroma * mixer_saturation_factor(saturation),
                    );
                }
                if abs(luminance) > 1e-7 {
                    adjusted = adjusted * exp2(mixer_luminance_ev(luminance, sample.lab.x));
                }
            }
        }
        rgb = mix(rgb, adjusted, weight);
    }
    return max(rgb, vec3<f32>(0.0));
}

fn apply_color_mixer(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let strengths = color_mixer_strength();
    if max(strengths.x, max(strengths.y, strengths.z)) < 1e-6 {
        // Exact no-op: enabling the mixer with neutral sliders must not change
        // pixels or run the selector filter.
        return rgb;
    }

    let sample = stabilized_mixer_sample(pos, rgb);
    if sample.confidence < 1e-5 {
        return rgb;
    }

    let selector_hue = fract(atan2(sample.hue_vector.y, sample.hue_vector.x) / (2.0 * 3.14159265359) + 1.0);
    let weights = mixer_band_weights(selector_hue);
    let hue_shift = mixer_hue_shift(weights) * sample.confidence;
    let saturation_amount = mixer_band_value(
        weights,
        params.hsl_saturation_0,
        params.hsl_saturation_1,
    ) / 100.0 * sample.confidence;
    let luminance_amount = mixer_band_value(
        weights,
        params.hsl_luminance_0,
        params.hsl_luminance_1,
    ) / 100.0 * sample.confidence;

    if max(abs(hue_shift), max(abs(saturation_amount), abs(luminance_amount))) < 1e-7 {
        return rgb;
    }

    var adjusted = rgb;
    if abs(hue_shift) > 1e-7 || abs(saturation_amount) > 1e-7 {
        let center_hue = sample.lab.yz / max(sample.chroma, 1e-9);
        let center_angle = atan2(center_hue.y, center_hue.x);
        let target_angle = center_angle + hue_shift * 2.0 * 3.14159265359;
        let target_hue = vec2<f32>(cos(target_angle), sin(target_angle));
        let target_chroma = sample.chroma * mixer_saturation_factor(saturation_amount);
        adjusted = positive_rec2020_from_oklab(sample.lab.x, target_hue, target_chroma);
    }

    if abs(luminance_amount) > 1e-7 {
        // A scalar scene-linear gain preserves RGB ratios, hue and saturation.
        // Unlike changing HSL lightness, it cannot expose per-channel extrema
        // as a checkerboard of different brightness values.
        adjusted = adjusted * exp2(mixer_luminance_ev(luminance_amount, sample.lab.x));
    }
    return max(adjusted, vec3<f32>(0.0));
}

@compute @workgroup_size(8, 8, 1)
fn prepare_adjustment_base(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));

    var rgb = scene_working_at(pos);
    // Camera-profile rendering establishes the base rendition. User controls
    // then follow Lightroom's panel order before local Effects.
    rgb = apply_profile_look(rgb);
    rgb = apply_profile_tone_curve(rgb);
    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));
    rgb = apply_lightroom_tone(rgb, pos);

    // Local masks are evaluated in normalized full-image coordinates, so the
    // same feathered mask is used by the preview proxy and every export tile.
    let local = local_adjustment_mix(pos);
    rgb = rgb * exp2(clamp(local.tone0.x, -10.0, 10.0));
    rgb = apply_local_basic_tone_values(
        rgb,
        pos,
        local.tone0.z,
        local.tone0.w,
        local.tone1.x,
        local.tone1.y,
    );
    rgb = apply_basic_contrast_value(rgb, local.tone0.y);
    rgb = apply_temperature_tint_values(rgb, local.tone1.z, local.tone1.w);
    rgb = apply_local_curve_and_hsl(pos, rgb);
    textureStore(adjustment_base_out, pos, vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn apply_lightroom_effects(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = adjustment_base_at(pos);
    let local = local_adjustment_mix(pos);
    rgb = apply_texture_and_clarity_values(
        pos,
        rgb,
        params.presence.x + local.effects.y,
        params.presence.y + local.effects.z,
    );
    rgb = apply_dehaze_value(pos, rgb, params.presence.z + local.effects.w);
    rgb = apply_saturation_vibrance(rgb);
    rgb = apply_saturation_value(rgb, local.effects.x);
    textureStore(local_effects_out, pos, vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn apply_creative_effects(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = local_effects_at(pos);
    rgb = apply_glow(pos, rgb);
    rgb = apply_vignette(pos, rgb);
    textureStore(creative_effects_out, pos, vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn apply_lightroom_adjustments(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let rgb = textureLoad(final_adjustment_tex, pos, 0).xyz;
    let mixed = apply_color_mixer(pos, rgb);
    let display_linear = darktable_sigmoid(mixed);
    textureStore(display_linear_out, pos, vec4<f32>(display_linear, 1.0));
    textureStore(out_tex, pos, vec4<f32>(apply_output_lut(display_linear), 1.0));
}
