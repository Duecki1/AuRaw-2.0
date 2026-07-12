// Post-demosaic scene-linear controls. Keeping this in its own pass lets
// local operations sample neighbouring RGB pixels. Global tone controls are
// evaluated once, at the end, by display_render().

@group(0) @binding(11) var scene_tex: texture_2d<f32>;
@group(0) @binding(12) var out_tex: texture_storage_2d<rgba8unorm, write>;

fn scene_working_at(pos: vec2<i32>) -> vec3<f32> {
    let camera_rgb = textureLoad(scene_tex, clamp_pos(pos), 0).xyz;
    let working = map_negative_gamut(cam_to_working(camera_rgb));
    let white_balanced = map_negative_gamut(apply_temperature_tint(working));
    let profile_corrected = map_negative_gamut(apply_profile_hue_sat(white_balanced));
    let profile_exposure_ev = bitcast<f32>(params.profile_flags.z);
    return profile_corrected * exp2(profile_exposure_ev);
}

fn blur_luminance(pos: vec2<i32>, radius: i32) -> f32 {
    let center = safe_luma(max(scene_working_at(pos), vec3<f32>(0.0)));
    var sum = 0.0;
    var sum_w = 0.0;
    for (var dy = -radius; dy <= radius; dy = dy + 1) {
        for (var dx = -radius; dx <= radius; dx = dx + 1) {
            let sample_lum = safe_luma(max(scene_working_at(pos + vec2<i32>(dx, dy)), vec3<f32>(0.0)));
            let distance = f32(dx * dx + dy * dy);
            let spatial = 1.0 / (1.0 + distance);
            // Edge-aware weighting keeps detail controls from making halos.
            let range = 1.0 / (1.0 + 12.0 * abs(sample_lum - center));
            let weight = spatial * range;
            sum = sum + sample_lum * weight;
            sum_w = sum_w + weight;
        }
    }
    return sum / max(sum_w, 1e-6);
}

fn apply_texture_and_clarity(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let texture = params.presence.x / 100.0;
    let clarity = params.presence.y / 100.0;
    if abs(texture) < 1e-6 && abs(clarity) < 1e-6 {
        return rgb;
    }

    let lum = safe_luma(max(rgb, vec3<f32>(0.0)));
    let fine_blur = blur_luminance(pos, 1);
    let broad_blur = blur_luminance(pos, 2);
    let fine_detail = lum - fine_blur;
    let mid_detail = lum - broad_blur;
    let midtone_gate = smoothstep(0.015, 0.20, lum) * (1.0 - smoothstep(1.0, 4.0, lum));
    let adjusted_lum = max(
        lum + fine_detail * texture * 0.75 + mid_detail * clarity * 0.60 * midtone_gate,
        0.0,
    );
    return rgb * clamp(adjusted_lum / max(lum, 1e-6), 0.0, 4.0);
}

fn dark_channel(pos: vec2<i32>) -> f32 {
    var dark = 1e20;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let sample = max(scene_working_at(pos + vec2<i32>(dx, dy)), vec3<f32>(0.0));
            dark = min(dark, min(sample.r, min(sample.g, sample.b)));
        }
    }
    return dark;
}

fn apply_dehaze(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = params.presence.z / 100.0;
    if abs(amount) < 1e-6 {
        return rgb;
    }

    // A deliberately conservative dark-channel transmission estimate.  The
    // full Ansel haze module also has global reductions and a guided filter;
    // this local form keeps an interactive mobile preview stable.
    let dark = clamp(dark_channel(pos), 0.0, 1.0);
    let local_lum = safe_luma(max(rgb, vec3<f32>(0.0)));
    let airlight = vec3<f32>(max(0.20, min(1.0, local_lum + 0.20)));
    let transmission = clamp(1.0 - amount * (1.0 - dark) * 0.45, 0.35, 1.65);
    return max((rgb - airlight * (1.0 - transmission)) / transmission, vec3<f32>(0.0));
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
            let neighbour_rgb = textureLoad(color_mixer_tex, clamp_pos(pos + vec2<i32>(dx, dy)), 0).xyz;
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

@group(0) @binding(21) var color_mixer_out: texture_storage_2d<rgba16float, write>;
@group(0) @binding(22) var color_mixer_tex: texture_2d<f32>;

@compute @workgroup_size(8, 8, 1)
fn prepare_color_mixer(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));

    var rgb = scene_working_at(pos);
    // Camera-profile rendering establishes the base rendition. User controls
    // then follow the Lightroom panel order before the selective colour pass.
    rgb = apply_profile_look(rgb);
    rgb = apply_profile_tone_curve(rgb);
    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));
    rgb = apply_lightroom_tone(rgb, pos);
    rgb = apply_texture_and_clarity(pos, rgb);
    rgb = apply_dehaze(pos, rgb);
    rgb = apply_saturation_vibrance(rgb);
    textureStore(color_mixer_out, pos, vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn apply_lightroom_adjustments(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let rgb = textureLoad(color_mixer_tex, pos, 0).xyz;
    let mixed = apply_color_mixer(pos, rgb);
    textureStore(out_tex, pos, vec4<f32>(display_render(mixed), 1.0));
}
