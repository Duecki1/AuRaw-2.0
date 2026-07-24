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

// This file implements a complete selectable scene-to-display view transform.
// Process 13+ never stacks DCP ProfileToneCurve ahead of this sigmoid path.

fn adaptive_tone_user_exposure_ev() -> f32 {
    // The histogram/guide remain cached in pre-user-exposure scene space so
    // moving Exposure stays cheap. Reintroduce the user-facing edit here. The
    // camera/DNG default rendering exposure is deliberately excluded here.
    return clamp(bitcast<f32>(params.process_info.z), -5.0, 5.0);
}

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
    // Evaluate the local guide at the post-Exposure brightness. This makes a
    // large positive Exposure naturally move more pixels into Highlights and
    // Whites, while a negative Exposure moves them toward Shadows/Blacks.
    return mix(a, b, fraction.y) + adaptive_tone_user_exposure_ev();
}

fn tone_percentiles() -> TonePercentiles {
    let p0 = tone_stats.percentiles_0;
    let p1 = tone_stats.percentiles_1;
    // Partially follow base Exposure instead of shifting the entire analysis
    // by the same amount. A full equal shift would cancel out and reproduce
    // the old pre-exposure masks; keeping 65% of the relative movement gives
    // recovery controls a more photographic, exposure-aware response without
    // making their target ranges jump abruptly after large edits.
    let guide_follow = adaptive_tone_user_exposure_ev() * 0.35;
    return TonePercentiles(
        p0.x + guide_follow,
        p0.y + guide_follow,
        p0.z + guide_follow,
        p0.w + guide_follow,
        p1.x + guide_follow,
    );
}

const BASIC_TONE_RESPONSE_PROCESS_VERSION: u32 = 16u;

fn basic_low_tone_control(value: f32) -> f32 {
    let normalized = clamp(value / 100.0, -1.0, 1.0);
    if params.process_info.x < BASIC_TONE_RESPONSE_PROCESS_VERSION {
        return normalized;
    }
    let magnitude = abs(normalized);
    // The former linear mapping made the useful centre of Shadows/Blacks feel
    // almost inert after the view transform. This concave response preserves
    // exact endpoints and fine zero control while giving +/-25..60 materially
    // more authority without a discontinuity.
    let shaped = magnitude * (1.45 - 0.45 * magnitude);
    return sign(normalized) * shaped;
}

fn adaptive_low_tone_ev(rgb: vec3<f32>, pos: vec2<i32>, guide_ev: f32) -> f32 {
    if params.process_info.x < BASIC_TONE_RESPONSE_PROCESS_VERSION {
        return guide_ev;
    }
    let pixel_ev = clamp(
        log2(safe_luma(rgb) / SCENE_MIDDLE_GREY),
        TONE_EV_MIN,
        TONE_EV_MAX,
    );
    // The old low-tone masks were selected only from the reduced bilateral
    // guide. A small dark subject inside a brighter guide cell could therefore
    // miss Shadows/Blacks almost entirely. Keep the guide as a halo suppressor,
    // but let actual pixel luminance dominate, especially across strong edges.
    let mismatch = abs(pixel_ev - guide_ev);
    let guide_weight = mix(0.38, 0.16, smoothstep(0.75, 2.75, mismatch));
    return mix(pixel_ev, guide_ev, guide_weight);
}

fn adaptive_tone_masks(
    low_ev: f32,
    high_ev: f32,
    percentiles: TonePercentiles,
) -> vec4<f32> {
    if params.process_info.x < BASIC_TONE_RESPONSE_PROCESS_VERSION {
        let black_fade_end = min(percentiles.p50 - 0.35, percentiles.p05 + 3.00);
        let black_mask = 1.0 - tone_smoothstep(
            percentiles.p005 - 0.55,
            max(black_fade_end, percentiles.p05 + 0.45),
            low_ev,
        );
        let shadow_mask = 1.0 - tone_smoothstep(
            percentiles.p05 - 0.60,
            percentiles.p50 + 0.45,
            low_ev,
        );
        let highlight_mask = tone_smoothstep(
            percentiles.p50 - 0.45,
            percentiles.p95 + 0.60,
            high_ev,
        );
        let white_mask = tone_smoothstep(
            percentiles.p95 - 0.30,
            percentiles.p995 + 0.45,
            high_ev,
        );
        return vec4<f32>(black_mask, shadow_mask, highlight_mask, white_mask);
    }

    // Process 16: Blacks is implemented by a dedicated monotone toe below, so
    // its mask is retained only for diagnostics/future use. Shadows reaches
    // farther into lower midtones while still rolling out before the bright
    // half of the image. Highlight/White semantics remain unchanged.
    let black_fade_end = min(percentiles.p50 - 0.55, percentiles.p05 + 3.35);
    let black_mask = 1.0 - tone_smoothstep(
        percentiles.p005 - 0.75,
        max(black_fade_end, percentiles.p05 + 0.90),
        low_ev,
    );
    let shadow_mask = 1.0 - tone_smoothstep(
        percentiles.p05 - 0.90,
        percentiles.p50 + 1.35,
        low_ev,
    );
    let highlight_mask = tone_smoothstep(
        percentiles.p50 - 0.45,
        percentiles.p95 + 0.60,
        high_ev,
    );
    let white_mask = tone_smoothstep(
        percentiles.p95 - 0.30,
        percentiles.p995 + 0.45,
        high_ev,
    );
    return vec4<f32>(black_mask, shadow_mask, highlight_mask, white_mask);
}

fn signed_tone_range(value: f32, negative_ev: f32, positive_ev: f32) -> f32 {
    return select(value * negative_ev, value * positive_ev, value >= 0.0);
}

fn apply_blacks_toe_v2(
    rgb: vec3<f32>,
    blacks: f32,
    percentiles: TonePercentiles,
) -> vec3<f32> {
    if params.process_info.x < BASIC_TONE_RESPONSE_PROCESS_VERSION || abs(blacks) < 1e-6 {
        return rgb;
    }

    // Blacks is an endpoint/toe control, not just another shadow exposure mask.
    // Remap luminance monotonically around an image-adaptive lower-tone pivot.
    // A power curve guarantees dy/dx > 0 for positive luminance, so even +/-100
    // cannot introduce tonal reversals or posterized plateaus.
    let pivot_ev = max(
        min(percentiles.p50 - 0.55, percentiles.p05 + 3.35),
        percentiles.p05 + 0.90,
    );
    let pivot_luminance = SCENE_MIDDLE_GREY * exp2(pivot_ev);
    let luminance = dot(rgb, LUMA);
    // Preserve signed/out-of-gamut scene values for later gamut handling. A
    // toe operator has no meaningful logarithmic endpoint for non-positive Y.
    if luminance <= 1e-8 || luminance >= pivot_luminance || pivot_luminance <= 1e-8 {
        return rgb;
    }

    let normalized = clamp(luminance / pivot_luminance, 0.0, 1.0);
    var gamma = 1.0;
    if blacks >= 0.0 {
        // +100 visibly opens the deepest toe (~0.42 power) without adding an
        // arbitrary RGB pedestal; zero stays black and hue is ratio-preserved.
        gamma = exp2(-1.25 * blacks);
    } else {
        // -100 decisively anchors/deepens blacks (~2.38 power) while the soft
        // pivot transition protects lower midtones from abrupt crushing.
        gamma = exp2(1.25 * (-blacks));
    }
    let mapped = pivot_luminance * pow(max(normalized, 1e-6), gamma);
    let pivot_feather = 1.0 - smoothstep(0.72, 1.0, normalized);
    let target_luminance = mix(luminance, mapped, pivot_feather);
    return rgb * clamp(target_luminance / luminance, 0.0, 64.0);
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
    let shadows = basic_low_tone_control(shadows_value);
    let whites = clamp(whites_value / 100.0, -1.0, 1.0);
    let blacks = basic_low_tone_control(blacks_value);
    if max(max(abs(highlights), abs(shadows)), max(abs(whites), abs(blacks))) < 1e-6 {
        return rgb;
    }

    let percentiles = tone_percentiles();
    let guide_ev = sample_tone_guide_ev(pos);
    let low_ev = adaptive_low_tone_ev(rgb, pos, guide_ev);
    let masks = adaptive_tone_masks(low_ev, guide_ev, percentiles);

    if params.process_info.x < BASIC_TONE_RESPONSE_PROCESS_VERSION {
        let offset_ev = signed_tone_range(blacks, 2.35, 1.90) * masks.x
            + signed_tone_range(shadows, 1.20, 1.90) * masks.y
            + signed_tone_range(highlights, 1.90, 1.15) * masks.z
            + signed_tone_range(whites, 1.25, 1.40) * masks.w;
        return rgb * exp2(offset_ev);
    }

    var adjusted = apply_blacks_toe_v2(rgb, blacks, percentiles);

    // Positive Shadows should open usable dark detail rather than simply lift
    // the absolute black endpoint. Protect the deepest few percent and let the
    // dedicated Blacks control own that endpoint. Negative Shadows may still
    // deepen the whole lower range for a deliberately dramatic rendering.
    var shadow_mask = masks.y;
    if shadows > 0.0 {
        let toe_guard = tone_smoothstep(
            percentiles.p005 - 0.35,
            percentiles.p05 + 0.90,
            low_ev,
        );
        shadow_mask = shadow_mask * mix(0.28, 1.0, toe_guard);
    }

    // Stronger but bounded scene-EV authority. At +50 the shaped Shadows value
    // is ~0.61, yielding roughly +1.5 EV around the 5th percentile instead of
    // the former sub-1-EV response that was mostly compressed by the view node.
    let offset_ev = signed_tone_range(shadows, 2.35, 3.20) * shadow_mask
        + signed_tone_range(highlights, 1.90, 1.15) * masks.z
        + signed_tone_range(whites, 1.25, 1.40) * masks.w;
    return adjusted * exp2(clamp(offset_ev, -6.5, 6.5));
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

    let luminance = safe_luma(rgb);
    let scene_ev = log2(luminance / SCENE_MIDDLE_GREY);

    // Contrast is a protected S-curve in scene EV rather than a global EV
    // multiplier. Near middle grey the exponential responses are steep, so
    // the slider changes midtone slope decisively. Toward the ends they
    // asymptotically cap the displacement: the toe cannot be driven down by
    // more than ~0.85 EV and the shoulder cannot be driven up by more than
    // ~1.0 EV at +100. This keeps deep texture and highlight separation alive
    // while still making the centre of the histogram visibly punchier.
    let toe_distance_ev = max(-scene_ev, 0.0);
    let shoulder_distance_ev = max(scene_ev, 0.0);
    let toe_midtone_width_ev = 1.65;
    let shoulder_midtone_width_ev = 1.85;
    let toe_response = 1.0 - exp2(-toe_distance_ev / toe_midtone_width_ev);
    let shoulder_response = 1.0 - exp2(-shoulder_distance_ev / shoulder_midtone_width_ev);
    let signed_protected_shape = shoulder_response * 1.00 - toe_response * 0.85;

    // Negative contrast is deliberately gentler. Keeping its maximum response
    // below the response widths preserves monotonicity through middle grey and
    // avoids the flat/reversed tonal patches that an over-strong inverse S can
    // create. Positive +100 remains the more assertive photographic endpoint.
    let contrast_strength = select(0.72, 1.0, amount >= 0.0);
    let adjusted_ev = scene_ev + amount * contrast_strength * signed_protected_shape;
    let adjusted_luminance = SCENE_MIDDLE_GREY * exp2(adjusted_ev);
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

fn remap_scene_luminance(rgb: vec3<f32>, adjusted_luminance: f32) -> vec3<f32> {
    let luminance = dot(rgb, LUMA);
    let mapped_luminance = max(adjusted_luminance, 0.0);
    if luminance <= 1e-7 {
        return vec3<f32>(mapped_luminance);
    }
    // One scalar gain keeps signed RGB ratios intact. Near zero luminance the
    // neutral fallback avoids unstable ratios without flooring channels.
    return rgb * (mapped_luminance / luminance);
}

fn apply_point_tone_curve(rgb: vec3<f32>) -> vec3<f32> {
    if tone_curve_is_identity(0u) {
        return rgb;
    }
    let luminance = max(dot(rgb, LUMA), 0.0);
    let adjusted_luminance = scene_curve_decode(
        point_curve_value(0u, scene_curve_encode(luminance)),
    );
    return remap_scene_luminance(rgb, adjusted_luminance);
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
    // The ABI slot stores log2(paper_exposure). Steep but valid curves can
    // overflow both the film response and paper exposure in linear form even
    // though their ratio remains perfectly well behaved.
    let log2_paper_exposure = params.sigmoid_curve.z;
    let film_fog = params.sigmoid_curve.w;
    let film_power = params.sigmoid_power.x;
    let paper_power = params.sigmoid_power.y;
    let fallback = clamp(max(value, 0.0), 0.0, 1.0);

    if !finite_scalar(white_target) || white_target <= 0.0
        || !finite_scalar(log2_paper_exposure)
        || !finite_scalar(film_fog) || film_fog < 0.0
        || !finite_scalar(film_power) || film_power <= 0.0
        || !finite_scalar(paper_power) || paper_power <= 0.0 {
        return fallback;
    }

    let film_base = film_fog + max(value, 0.0);
    if !finite_scalar(film_base) || film_base < 0.0 {
        return fallback;
    }
    if film_base == 0.0 {
        return 0.0;
    }

    // Stable base-2 logistic for F / (P + F), evaluated from log2(F/P).
    // Both exp2 calls receive a non-positive argument and therefore cannot
    // overflow. The exact zero/zero-fog case was handled above.
    let log2_film_response = film_power * log2(film_base);
    let log2_ratio = log2_film_response - log2_paper_exposure;
    var ratio = 0.0;
    if log2_ratio >= 0.0 {
        ratio = 1.0 / (1.0 + exp2(-log2_ratio));
    } else {
        let scaled = exp2(log2_ratio);
        ratio = scaled / (1.0 + scaled);
    }
    let paper_response = white_target
        * pow(clamp(ratio, 0.0, 1.0), paper_power);
    return select(fallback, paper_response, finite_scalar(paper_response));
}

fn desaturate_negative_values(rgb: vec3<f32>) -> vec3<f32> {
    // Sigmoid variants require a positive domain. Use the same lightness- and
    // hue-direction-preserving projection as the rest of the render graph so
    // this safety boundary is explicit and consistent.
    return gamut_project_nonnegative_rec2020(rgb);
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

fn apply_sigmoid_view_transform(scene_rgb: vec3<f32>) -> vec3<f32> {
    return darktable_sigmoid(scene_rgb);
}
