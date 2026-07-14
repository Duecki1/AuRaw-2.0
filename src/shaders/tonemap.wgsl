// darktable sigmoid display transform, ported from darktable 5.6.0
// src/iop/sigmoid.c and data/kernels/sigmoid.cl.
// Copyright (C) 2020-2026 darktable developers.
// Copyright (C) 2026 AuRaw contributors (WGSL port).
// GPL-3.0-or-later.
//
// AuRaw's Highlights/Shadows/Whites/Blacks remain a separate scene-linear,
// edge-aware exposure-shaping stage. The final scene-to-display transform below
// is darktable's generalized log-logistic sigmoid and color processing.

@group(0) @binding(16) var<storage, read> tone_stats: ToneStats;
@group(0) @binding(17) var tone_guide_tex: texture_2d<f32>;

fn sample_tone_guide_ev(pos: vec2<i32>) -> f32 {
    let guide_size_i = vec2<i32>(textureDimensions(tone_guide_tex));
    let guide_max = guide_size_i - vec2<i32>(1);
    let full_size = vec2<f32>(f32(params.width), f32(params.height));
    let guide_size = vec2<f32>(guide_size_i);
    let coordinate = (vec2<f32>(pos) + vec2<f32>(0.5)) * guide_size / full_size
        - vec2<f32>(0.5);
    let base = vec2<i32>(floor(coordinate));
    let fraction = fract(coordinate);

    let p00 = clamp(base, vec2<i32>(0), guide_max);
    let p10 = clamp(base + vec2<i32>(1, 0), vec2<i32>(0), guide_max);
    let p01 = clamp(base + vec2<i32>(0, 1), vec2<i32>(0), guide_max);
    let p11 = clamp(base + vec2<i32>(1, 1), vec2<i32>(0), guide_max);

    let a = mix(textureLoad(tone_guide_tex, p00, 0).x,
                textureLoad(tone_guide_tex, p10, 0).x, fraction.x);
    let b = mix(textureLoad(tone_guide_tex, p01, 0).x,
                textureLoad(tone_guide_tex, p11, 0).x, fraction.x);
    return mix(a, b, fraction.y);
}

fn tone_percentiles() -> TonePercentiles {
    let p0 = tone_stats.percentiles_0;
    let p1 = tone_stats.percentiles_1;
    return TonePercentiles(p0.x, p0.y, p0.z, p0.w, p1.x);
}

fn adaptive_tone_masks(local_ev: f32, percentiles: TonePercentiles) -> vec4<f32> {
    // Lightroom's Blacks control reaches well beyond the darkest five percent.
    // Fade it through the lower quarter of the tonal range so it visibly moves
    // the toe/black endpoint without becoming a duplicate Shadows control.
    let black_fade_end = min(percentiles.p50 - 0.35, percentiles.p05 + 3.00);
    let black_mask = 1.0 - tone_smoothstep(
        percentiles.p005 - 0.55,
        max(black_fade_end, percentiles.p05 + 0.45),
        local_ev,
    );
    let shadow_mask = 1.0 - tone_smoothstep(
        percentiles.p05 - 0.60,
        percentiles.p50 + 0.45,
        local_ev,
    );
    let highlight_mask = tone_smoothstep(
        percentiles.p50 - 0.45,
        percentiles.p95 + 0.60,
        local_ev,
    );
    let white_mask = tone_smoothstep(
        percentiles.p95 - 0.30,
        percentiles.p995 + 0.45,
        local_ev,
    );
    return vec4<f32>(black_mask, shadow_mask, highlight_mask, white_mask);
}

fn signed_tone_range(value: f32, negative_ev: f32, positive_ev: f32) -> f32 {
    return select(value * negative_ev, value * positive_ev, value >= 0.0);
}

fn apply_local_basic_tone_values(
    rgb: vec3<f32>,
    pos: vec2<i32>,
    highlights_value: f32,
    shadows_value: f32,
    whites_value: f32,
    blacks_value: f32,
) -> vec3<f32> {
    let highlights = clamp(highlights_value / 100.0, -1.0, 1.0);
    let shadows = clamp(shadows_value / 100.0, -1.0, 1.0);
    let whites = clamp(whites_value / 100.0, -1.0, 1.0);
    let blacks = clamp(blacks_value / 100.0, -1.0, 1.0);
    if max(max(abs(highlights), abs(shadows)), max(abs(whites), abs(blacks))) < 1e-6 {
        return rgb;
    }

    let masks = adaptive_tone_masks(sample_tone_guide_ev(pos), tone_percentiles());
    // Lightroom-like asymmetric ranges: highlight recovery and shadow lift are
    // deliberately stronger than their opposite directions, while Whites and
    // Blacks primarily move the endpoints. All four remain exposure-like in
    // scene space, preserving hue and avoiding channel clipping.
    let offset_ev = signed_tone_range(blacks, 2.35, 1.90) * masks.x
        + signed_tone_range(shadows, 1.20, 1.90) * masks.y
        + signed_tone_range(highlights, 1.90, 1.15) * masks.z
        + signed_tone_range(whites, 1.25, 1.40) * masks.w;
    return rgb * exp2(offset_ev);
}

fn apply_local_basic_tone(rgb: vec3<f32>, pos: vec2<i32>) -> vec3<f32> {
    return apply_local_basic_tone_values(
        rgb,
        pos,
        params.basic_tone.x,
        params.basic_tone.y,
        params.basic_tone.z,
        params.basic_tone.w,
    );
}

fn apply_basic_contrast_value(rgb: vec3<f32>, value: f32) -> vec3<f32> {
    let amount = clamp(value / 100.0, -1.0, 1.0);
    if abs(amount) < 1e-6 {
        return rgb;
    }

    let luminance = safe_luma(max(rgb, vec3<f32>(0.0)));
    let contrast_power = exp2(amount);
    let scene_ev = log2(luminance / SCENE_MIDDLE_GREY);
    let adjusted_luminance = SCENE_MIDDLE_GREY * exp2(scene_ev * contrast_power);
    return rgb * clamp(adjusted_luminance / luminance, 0.0, 64.0);
}

fn apply_basic_contrast(rgb: vec3<f32>) -> vec3<f32> {
    return apply_basic_contrast_value(rgb, params.presence.w);
}

// Curve 0 is the composite luminance curve; 1, 2 and 3 are R, G and B.
// The point curves are evaluated with monotone cubic Hermite interpolation,
// preventing ringing around steep user edits while retaining endpoint control.
fn tone_curve_point(curve: u32, index: u32) -> vec2<f32> {
    if curve == 1u {
        switch index {
            case 0u: { return params.tone_curve_red_0.xy; }
            case 1u: { return params.tone_curve_red_0.zw; }
            case 2u: { return params.tone_curve_red_1.xy; }
            case 3u: { return params.tone_curve_red_1.zw; }
            case 4u: { return params.tone_curve_red_2.xy; }
            case 5u: { return params.tone_curve_red_2.zw; }
            case 6u: { return params.tone_curve_red_3.xy; }
            default: { return params.tone_curve_red_3.zw; }
        }
    }
    if curve == 2u {
        switch index {
            case 0u: { return params.tone_curve_green_0.xy; }
            case 1u: { return params.tone_curve_green_0.zw; }
            case 2u: { return params.tone_curve_green_1.xy; }
            case 3u: { return params.tone_curve_green_1.zw; }
            case 4u: { return params.tone_curve_green_2.xy; }
            case 5u: { return params.tone_curve_green_2.zw; }
            case 6u: { return params.tone_curve_green_3.xy; }
            default: { return params.tone_curve_green_3.zw; }
        }
    }
    if curve == 3u {
        switch index {
            case 0u: { return params.tone_curve_blue_0.xy; }
            case 1u: { return params.tone_curve_blue_0.zw; }
            case 2u: { return params.tone_curve_blue_1.xy; }
            case 3u: { return params.tone_curve_blue_1.zw; }
            case 4u: { return params.tone_curve_blue_2.xy; }
            case 5u: { return params.tone_curve_blue_2.zw; }
            case 6u: { return params.tone_curve_blue_3.xy; }
            default: { return params.tone_curve_blue_3.zw; }
        }
    }
    switch index {
        case 0u: { return params.tone_curve_0.xy; }
        case 1u: { return params.tone_curve_0.zw; }
        case 2u: { return params.tone_curve_1.xy; }
        case 3u: { return params.tone_curve_1.zw; }
        case 4u: { return params.tone_curve_2.xy; }
        case 5u: { return params.tone_curve_2.zw; }
        case 6u: { return params.tone_curve_3.xy; }
        default: { return params.tone_curve_3.zw; }
    }
}

fn tone_curve_count(curve: u32) -> u32 {
    if curve == 1u { return u32(clamp(params.tone_curve_red_meta.x, 2.0, 8.0)); }
    if curve == 2u { return u32(clamp(params.tone_curve_green_meta.x, 2.0, 8.0)); }
    if curve == 3u { return u32(clamp(params.tone_curve_blue_meta.x, 2.0, 8.0)); }
    return u32(clamp(params.tone_curve_meta.x, 2.0, 8.0));
}

fn tone_curve_is_identity(curve: u32) -> bool {
    if curve == 1u { return params.tone_curve_red_meta.y > 0.5; }
    if curve == 2u { return params.tone_curve_green_meta.y > 0.5; }
    if curve == 3u { return params.tone_curve_blue_meta.y > 0.5; }
    return params.tone_curve_meta.y > 0.5;
}

fn tone_curve_secant(a: vec2<f32>, b: vec2<f32>) -> f32 {
    return (b.y - a.y) / max(b.x - a.x, 1e-5);
}

fn tone_curve_tangent(curve: u32, index: u32, count: u32) -> f32 {
    if index == 0u {
        return tone_curve_secant(tone_curve_point(curve, 0u), tone_curve_point(curve, 1u));
    }
    if index + 1u >= count {
        return tone_curve_secant(
            tone_curve_point(curve, count - 2u),
            tone_curve_point(curve, count - 1u),
        );
    }

    let previous = tone_curve_secant(
        tone_curve_point(curve, index - 1u),
        tone_curve_point(curve, index),
    );
    let next = tone_curve_secant(
        tone_curve_point(curve, index),
        tone_curve_point(curve, index + 1u),
    );
    if previous * next <= 0.0 {
        return 0.0;
    }
    return 2.0 * previous * next / max(abs(previous + next), 1e-6) * sign(previous + next);
}

fn point_curve_value(curve: u32, input: f32) -> f32 {
    let count = tone_curve_count(curve);
    let x = clamp(input, 0.0, 1.0);
    var segment = count - 2u;
    for (var index = 0u; index + 1u < count; index = index + 1u) {
        if x <= tone_curve_point(curve, index + 1u).x {
            segment = index;
            break;
        }
    }

    let p0 = tone_curve_point(curve, segment);
    let p1 = tone_curve_point(curve, segment + 1u);
    let width = max(p1.x - p0.x, 1e-5);
    let t = clamp((x - p0.x) / width, 0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    let m0 = tone_curve_tangent(curve, segment, count) * width;
    let m1 = tone_curve_tangent(curve, segment + 1u, count) * width;
    let hermite = (2.0 * t3 - 3.0 * t2 + 1.0) * p0.y
        + (t3 - 2.0 * t2 + t) * m0
        + (-2.0 * t3 + 3.0 * t2) * p1.y
        + (t3 - t2) * m1;
    return clamp(hermite, min(p0.y, p1.y), max(p0.y, p1.y));
}

fn scene_curve_encode(value: f32) -> f32 {
    let positive = max(value, 0.0);
    return positive / (positive + SCENE_MIDDLE_GREY);
}

fn scene_curve_decode(value: f32) -> f32 {
    let bounded = clamp(value, 0.0, 0.999999);
    return SCENE_MIDDLE_GREY * bounded / max(1.0 - bounded, 1e-6);
}

fn apply_point_tone_curve(rgb: vec3<f32>) -> vec3<f32> {
    if tone_curve_is_identity(0u) {
        return rgb;
    }
    let luminance = max(dot(rgb, LUMA), 0.0);
    let adjusted_luminance = scene_curve_decode(
        point_curve_value(0u, scene_curve_encode(luminance)),
    );
    if luminance <= 1e-9 {
        return vec3<f32>(adjusted_luminance);
    }
    return rgb * clamp(adjusted_luminance / luminance, 0.0, 256.0);
}

fn apply_rgb_point_curves(rgb: vec3<f32>) -> vec3<f32> {
    var result = rgb;
    if !tone_curve_is_identity(1u) && result.r >= 0.0 {
        result.r = scene_curve_decode(point_curve_value(1u, scene_curve_encode(result.r)));
    }
    if !tone_curve_is_identity(2u) && result.g >= 0.0 {
        result.g = scene_curve_decode(point_curve_value(2u, scene_curve_encode(result.g)));
    }
    if !tone_curve_is_identity(3u) && result.b >= 0.0 {
        result.b = scene_curve_decode(point_curve_value(3u, scene_curve_encode(result.b)));
    }
    return result;
}

fn apply_lightroom_tone(rgb: vec3<f32>, pos: vec2<i32>) -> vec3<f32> {
    let basic = apply_basic_contrast(apply_local_basic_tone(rgb, pos));
    return apply_rgb_point_curves(apply_point_tone_curve(basic));
}

fn finite_scalar(value: f32) -> bool {
    return value == value && abs(value) < 3.0e38;
}

fn generalized_loglogistic_sigmoid(value: f32) -> f32 {
    let white_target = params.sigmoid_curve.x;
    let paper_exposure = params.sigmoid_curve.z;
    let film_fog = params.sigmoid_curve.w;
    let film_power = params.sigmoid_power.x;
    let paper_power = params.sigmoid_power.y;
    let fallback = clamp(max(value, 0.0), 0.0, 1.0);

    if !finite_scalar(white_target) || white_target <= 0.0
        || !finite_scalar(paper_exposure) || paper_exposure <= 0.0
        || !finite_scalar(film_fog) || film_fog < 0.0
        || !finite_scalar(film_power) || film_power <= 0.0
        || !finite_scalar(paper_power) || paper_power <= 0.0 {
        return fallback;
    }

    let film_base = film_fog + max(value, 0.0);
    if !finite_scalar(film_base) || film_base < 0.0 {
        return fallback;
    }
    let film_response = pow(film_base, film_power);
    let denominator = paper_exposure + film_response;
    if !finite_scalar(film_response) || !finite_scalar(denominator) || denominator <= 0.0 {
        return fallback;
    }
    let paper_response = white_target
        * pow(clamp(film_response / denominator, 0.0, 1.0), paper_power);
    return select(fallback, paper_response, finite_scalar(paper_response));
}

fn desaturate_negative_values(rgb: vec3<f32>) -> vec3<f32> {
    let pixel_average = max((rgb.r + rgb.g + rgb.b) / 3.0, 0.0);
    let min_value = min(rgb.r, min(rgb.g, rgb.b));
    let saturation_factor = select(
        1.0,
        -pixel_average / (min_value - pixel_average),
        min_value < 0.0,
    );
    return vec3<f32>(pixel_average)
        + saturation_factor * (rgb - vec3<f32>(pixel_average));
}

// Returns min, mid, max channel indices, matching darktable's seven cases.
fn pixel_channel_order(rgb: vec3<f32>) -> vec3<u32> {
    if rgb.r >= rgb.g {
        if rgb.g > rgb.b {
            return vec3<u32>(2u, 1u, 0u);
        }
        if rgb.b > rgb.r {
            return vec3<u32>(1u, 0u, 2u);
        }
        if rgb.b > rgb.g {
            return vec3<u32>(1u, 2u, 0u);
        }
        return vec3<u32>(2u, 1u, 0u);
    }
    if rgb.r >= rgb.b {
        return vec3<u32>(2u, 0u, 1u);
    }
    if rgb.b > rgb.g {
        return vec3<u32>(0u, 1u, 2u);
    }
    return vec3<u32>(0u, 2u, 1u);
}

fn preserve_hue_and_energy(
    pix_in: vec3<f32>,
    per_channel: vec3<f32>,
    order: vec3<u32>,
    hue_preservation: f32,
) -> vec3<f32> {
    let min_index = order.x;
    let mid_index = order.y;
    let max_index = order.z;
    let chroma = pix_in[max_index] - pix_in[min_index];
    let midscale = select(
        0.0,
        (pix_in[mid_index] - pix_in[min_index]) / chroma,
        chroma != 0.0,
    );
    let full_hue_correction = per_channel[min_index]
        + (per_channel[max_index] - per_channel[min_index]) * midscale;
    let naive_hue_mid = (1.0 - hue_preservation) * per_channel[mid_index]
        + hue_preservation * full_hue_correction;
    let per_channel_energy = per_channel.r + per_channel.g + per_channel.b;
    let naive_hue_energy = per_channel[min_index] + naive_hue_mid + per_channel[max_index];
    let pix_in_min_plus_mid = pix_in[min_index] + pix_in[mid_index];
    let blend_factor = select(
        0.0,
        2.0 * pix_in[min_index] / pix_in_min_plus_mid,
        pix_in_min_plus_mid != 0.0,
    );
    let energy_target = blend_factor * per_channel_energy
        + (1.0 - blend_factor) * naive_hue_energy;

    var result = per_channel;
    if naive_hue_mid <= per_channel[mid_index] {
        let corrected_mid = ((1.0 - hue_preservation) * per_channel[mid_index]
            + hue_preservation
                * (midscale * per_channel[max_index]
                    + (1.0 - midscale) * (energy_target - per_channel[max_index])))
            / (1.0 + hue_preservation * (1.0 - midscale));
        result[min_index] = energy_target - per_channel[max_index] - corrected_mid;
        result[mid_index] = corrected_mid;
        result[max_index] = per_channel[max_index];
    } else {
        let corrected_mid = ((1.0 - hue_preservation) * per_channel[mid_index]
            + hue_preservation
                * (per_channel[min_index] * (1.0 - midscale)
                    + midscale * (energy_target - per_channel[min_index])))
            / (1.0 + hue_preservation * midscale);
        result[min_index] = per_channel[min_index];
        result[mid_index] = corrected_mid;
        result[max_index] = energy_target - per_channel[min_index] - corrected_mid;
    }
    return result;
}

fn sigmoid_per_channel(rgb: vec3<f32>) -> vec3<f32> {
    let positive = desaturate_negative_values(rgb);
    let per_channel = vec3<f32>(
        generalized_loglogistic_sigmoid(positive.r),
        generalized_loglogistic_sigmoid(positive.g),
        generalized_loglogistic_sigmoid(positive.b),
    );
    let order = pixel_channel_order(positive);
    return preserve_hue_and_energy(
        positive,
        per_channel,
        order,
        clamp(params.sigmoid_power.z, 0.0, 1.0),
    );
}

fn sigmoid_rgb_ratio(rgb: vec3<f32>) -> vec3<f32> {
    let white_target = params.sigmoid_curve.x;
    let black_target = params.sigmoid_curve.y;
    let positive = desaturate_negative_values(rgb);
    let luma = (positive.r + positive.g + positive.b) / 3.0;
    let mapped_luma = generalized_loglogistic_sigmoid(luma);

    var pre_out = vec3<f32>(mapped_luma);
    if luma > 1e-9 {
        pre_out = positive * (mapped_luma / luma);
    }

    let pixel_min = min(pre_out.r, min(pre_out.g, pre_out.b));
    let pixel_max = max(pre_out.r, max(pre_out.g, pre_out.b));
    let epsilon = 1e-6;
    let display_border_vs_chroma_white =
        (white_target - mapped_luma) / (pixel_max - mapped_luma + epsilon);
    let display_border_vs_chroma_black =
        (black_target - mapped_luma) / (pixel_min - mapped_luma - epsilon);
    let display_border_vs_chroma = min(
        display_border_vs_chroma_white,
        display_border_vs_chroma_black,
    );
    let chroma_vs_mapping_border =
        (mapped_luma - pixel_min) / (mapped_luma + epsilon);
    let pixel_chroma_adjustment = 1.0
        / (chroma_vs_mapping_border * display_border_vs_chroma + epsilon);
    let hyperbolic_chroma = 2.0 * chroma_vs_mapping_border
        / (1.0 - chroma_vs_mapping_border * chroma_vs_mapping_border + epsilon)
        * pixel_chroma_adjustment;
    let hyperbolic_z = sqrt(hyperbolic_chroma * hyperbolic_chroma + 1.0);
    let chroma_factor = hyperbolic_chroma / (1.0 + hyperbolic_z)
        * display_border_vs_chroma;
    return vec3<f32>(mapped_luma)
        + chroma_factor * (pre_out - vec3<f32>(mapped_luma));
}

fn darktable_sigmoid(rgb: vec3<f32>) -> vec3<f32> {
    if params.sigmoid_power.w < 0.5 {
        return sigmoid_per_channel(rgb);
    }
    return sigmoid_rgb_ratio(rgb);
}

