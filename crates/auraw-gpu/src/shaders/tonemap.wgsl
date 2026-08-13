#import auraw::common as Common
#import auraw::tone_common as ToneCommon

// darktable sigmoid display transform, ported from darktable 5.6.0
// src/iop/sigmoid.c and data/kernels/sigmoid.cl.
// Copyright (C) 2020-2026 darktable developers.
// Copyright (C) 2026 AuRaw contributors (WGSL port).
// GPL-3.0-or-later.

@group(0) @binding(16) var<storage, read> tone_stats: ToneCommon::ToneStats;
@group(0) @binding(17) var tone_guide_tex: texture_2d<f32>;

// Sigmoid

fn adaptive_tone_user_exposure_ev() -> f32 {
    // Exposure
    return clamp(bitcast<f32>(Common::camera_uniforms.user_exposure_bits), -5.0, 5.0);
}

fn sample_tone_guide_ev(pos: vec2<i32>) -> f32 {
    let guide_size_i = vec2<i32>(textureDimensions(tone_guide_tex));
    let guide_max = guide_size_i - vec2<i32>(1);
    let full_size = vec2<f32>(f32(Common::camera_uniforms.width), f32(Common::camera_uniforms.height));
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
    // Guide
    return mix(a, b, fraction.y) + adaptive_tone_user_exposure_ev();
}

fn tone_percentiles() -> ToneCommon::TonePercentiles {
    let p0 = tone_stats.percentiles_0_field;
    let p1 = tone_stats.percentiles_1_field;
    // Follow
    let guide_follow = adaptive_tone_user_exposure_ev() * 0.35;
    return ToneCommon::TonePercentiles(
        p0.x + guide_follow,
        p0.y + guide_follow,
        p0.z + guide_follow,
        p0.w + guide_follow,
        p1.x + guide_follow,
    );
}


fn basic_low_tone_control(value: f32) -> f32 {
    let normalized = clamp(value / 100.0, -1.0, 1.0);
    let magnitude = abs(normalized);
    // Shape
    let shaped = magnitude * (1.45 - 0.45 * magnitude);
    return sign(normalized) * shaped;
}

fn adaptive_low_tone_ev(rgb: vec3<f32>, pos: vec2<i32>, guide_ev: f32) -> f32 {
    let pixel_ev = clamp(
        log2(Common::safe_luma(rgb) / ToneCommon::SCENE_MIDDLE_GREY),
        ToneCommon::TONE_EV_MIN,
        ToneCommon::TONE_EV_MAX,
    );
    let mismatch = abs(pixel_ev - guide_ev);
    // Bound
    let bounded_guide_ev = pixel_ev + clamp(guide_ev - pixel_ev, -1.25, 0.75);
    let guide_weight = mix(0.42, 0.22, smoothstep(0.50, 3.00, mismatch));
    return mix(pixel_ev, bounded_guide_ev, guide_weight);
}

fn adaptive_tone_masks(
    low_ev: f32,
    high_ev: f32,
    percentiles: ToneCommon::TonePercentiles,
) -> vec4<f32> {
    // Masks
    let black_fade_end = min(percentiles.p50_field - 0.55, percentiles.p05_field + 3.35);
    let black_mask = 1.0 - ToneCommon::tone_smoothstep(
        percentiles.p005_field - 0.75,
        max(black_fade_end, percentiles.p05_field + 0.90),
        low_ev,
    );
    let shadow_mask = 1.0 - ToneCommon::tone_smoothstep(
        percentiles.p05_field - 0.90,
        percentiles.p50_field + 1.35,
        low_ev,
    );
    let highlight_mask = ToneCommon::tone_smoothstep(
        percentiles.p005_field - 0.40,
        percentiles.p50_field - 0.30,
        high_ev,
    );
    let white_mask = ToneCommon::tone_smoothstep(
        percentiles.p05_field - 0.10,
        percentiles.p50_field + 0.50,
        high_ev,
    );
    return vec4<f32>(black_mask, shadow_mask, highlight_mask, white_mask);
}

fn lightroom_shadow_offset_ev(
    shadows: f32,
    mask: f32,
    percentiles: ToneCommon::TonePercentiles,
) -> f32 {
    if abs(shadows) < 1e-7 || mask <= 0.0 {
        return 0.0;
    }

    // Shadows
    let lower = percentiles.p05_field - 0.90;
    let upper = percentiles.p50_field + 1.35;
    let monotone_limit = 0.64 * max(upper - lower, 0.25);
    let requested = abs(shadows) * 2.20;
    return sign(shadows) * min(requested, monotone_limit) * mask;
}

fn lightroom_positive_whites_offset_ev(
    whites: f32,
    low_ev: f32,
    percentiles: ToneCommon::TonePercentiles,
) -> f32 {
    if whites <= 0.0 {
        return 0.0;
    }

    // The 16-bit Lightroom endpoint is a broad but restrained hump: nearly
    // neutral in the bottom decile, strongest from upper midtones into diffuse
    // white, then rolling away from the clipped endpoint. The previous 20%
    // floor and 2.35 EV request turned Whites into a second Exposure control.
    let rise = ToneCommon::tone_smoothstep(
        percentiles.p05_field - 0.15,
        percentiles.p50_field + 0.55,
        low_ev,
    );
    let fall_start = percentiles.p50_field + 0.10;
    let fall_end = percentiles.p995_field + 0.60;
    let fall = 1.0 - 0.35 * ToneCommon::tone_smoothstep(fall_start, fall_end, low_ev);
    let mask = (0.025 + 0.975 * rise) * fall;
    let monotone_limit = 0.90 * max(fall_end - fall_start, 0.25) / (1.5 * 0.35);
    return min(whites * 0.95, monotone_limit) * mask;
}

fn signed_tone_range(value: f32, negative_ev: f32, positive_ev: f32) -> f32 {
    return select(value * negative_ev, value * positive_ev, value >= 0.0);
}

fn apply_local_basic_tone_values_with_low_strength(
    rgb: vec3<f32>,
    pos: vec2<i32>,
    highlights_value: f32,
    shadows_value: f32,
    whites_value: f32,
    _blacks_value: f32,
    low_tone_strength: f32,
) -> vec3<f32> {
    let highlights = clamp(highlights_value / 100.0, -1.0, 1.0);
    let shadows = basic_low_tone_control(shadows_value)
        * clamp(low_tone_strength, 0.0, 1.0);
    let whites = clamp(whites_value / 100.0, -1.0, 1.0);
    // Blacks is a display-linear toe applied after the view transform.
    // Never spend Blacks authority in scene-linear tone processing.
    if max(max(abs(highlights), abs(shadows)), abs(whites)) < 1e-6 {
        return rgb;
    }

    let percentiles = tone_percentiles();
    let guide_ev = sample_tone_guide_ev(pos);
    let low_ev = adaptive_low_tone_ev(rgb, pos, guide_ev);
    let masks = adaptive_tone_masks(low_ev, guide_ev, percentiles);

    let shadow_ev = lightroom_shadow_offset_ev(shadows, masks.y, percentiles);
    // Highlights peak in the top decile while staying gentle below the median.
    let highlight_mask = 0.10 + 0.90 * ToneCommon::tone_smoothstep(
        percentiles.p50_field - 0.35,
        percentiles.p95_field + 0.45,
        guide_ev,
    );
    let highlight_ev = signed_tone_range(highlights, 1.35, 1.00) * highlight_mask;
    let white_ev = select(
        signed_tone_range(whites, 0.30, 1.40) * masks.w,
        lightroom_positive_whites_offset_ev(whites, low_ev, percentiles),
        whites >= 0.0,
    );
    return rgb * exp2(clamp(shadow_ev + highlight_ev + white_ev, -6.5, 6.5));
}

fn apply_local_basic_tone_values(
    rgb: vec3<f32>,
    pos: vec2<i32>,
    highlights_value: f32,
    shadows_value: f32,
    whites_value: f32,
    blacks_value: f32,
) -> vec3<f32> {
    return apply_local_basic_tone_values_with_low_strength(
        rgb,
        pos,
        highlights_value,
        shadows_value,
        whites_value,
        blacks_value,
        1.0,
    );
}

fn apply_local_basic_tone(rgb: vec3<f32>, pos: vec2<i32>) -> vec3<f32> {
    return apply_local_basic_tone_values(
        rgb,
        pos,
        Common::scene_tone_uniforms.basic_tone.x,
        Common::scene_tone_uniforms.basic_tone.y,
        Common::scene_tone_uniforms.basic_tone.z,
        Common::scene_tone_uniforms.basic_tone.w,
    );
}

// Local masks retain a bounded scene-EV contrast operator because a view
// transform cannot be varied per pixel without breaking display mapping.
// Global Basic Contrast is the actual sigmoid middle-grey slope instead.
fn apply_mask_contrast_value(rgb: vec3<f32>, value: f32) -> vec3<f32> {
    let amount = clamp(value / 100.0, -1.0, 1.0);
    if abs(amount) < 1e-6 {
        return rgb;
    }

    let luminance = Common::safe_luma(rgb);
    let scene_ev = log2(luminance / ToneCommon::SCENE_MIDDLE_GREY);
    let contrast_pivot_ev = tone_percentiles().p50_field + 0.12;
    let relative_ev = scene_ev - contrast_pivot_ev;

    // Contrast is a protected S-curve in scene EV rather than a global EV
    // multiplier. Near middle grey the exponential responses are steep, so
    // the slider changes midtone slope decisively. Toward the ends they
    // asymptotically cap the displacement. The 16-bit endpoint has a decisive
    // black-end anchor but a protected highlight shoulder; the inverse endpoint
    // remains gentler on both sides to retain a strict monotonicity margin.
    let toe_distance_ev = max(-relative_ev, 0.0);
    let shoulder_distance_ev = max(relative_ev, 0.0);
    let toe_midtone_width_ev = 1.65;
    let shoulder_midtone_width_ev = 1.85;
    let toe_response = 1.0 - exp2(-toe_distance_ev / toe_midtone_width_ev);
    let shoulder_response = 1.0 - exp2(-shoulder_distance_ev / shoulder_midtone_width_ev);
    let toe_endpoint = select(1.70, 5.80, amount >= 0.0);
    let shoulder_endpoint = select(0.95, 0.85, amount >= 0.0);
    let signed_protected_shape =
        shoulder_response * shoulder_endpoint - toe_response * toe_endpoint;

    // At -100 the minimum derivative is still positive on both sides of the
    // pivot: 1 - 1.70*ln(2)/1.65 = 0.286 in the toe and
    // 1 - 0.95*ln(2)/1.85 = 0.644 in the shoulder.
    let adjusted_ev = scene_ev + amount * signed_protected_shape;
    let adjusted_luminance = ToneCommon::SCENE_MIDDLE_GREY * exp2(adjusted_ev);
    return rgb * clamp(adjusted_luminance / luminance, 0.0, 64.0);
}

// Curve 0 is the composite luminance curve; 1, 2 and 3 are R, G and B.
// The point curves are evaluated with monotone cubic Hermite interpolation,
// preventing ringing around steep user edits while retaining endpoint control.
fn tone_curve_point(curve: u32, index: u32) -> vec2<f32> {
    if curve == 1u {
        switch index {
            case 0u: { return Common::scene_tone_uniforms.tone_curve_red_0_field.xy; }
            case 1u: { return Common::scene_tone_uniforms.tone_curve_red_0_field.zw; }
            case 2u: { return Common::scene_tone_uniforms.tone_curve_red_1_field.xy; }
            case 3u: { return Common::scene_tone_uniforms.tone_curve_red_1_field.zw; }
            case 4u: { return Common::scene_tone_uniforms.tone_curve_red_2_field.xy; }
            case 5u: { return Common::scene_tone_uniforms.tone_curve_red_2_field.zw; }
            case 6u: { return Common::scene_tone_uniforms.tone_curve_red_3_field.xy; }
            default: { return Common::scene_tone_uniforms.tone_curve_red_3_field.zw; }
        }
    }
    if curve == 2u {
        switch index {
            case 0u: { return Common::scene_tone_uniforms.tone_curve_green_0_field.xy; }
            case 1u: { return Common::scene_tone_uniforms.tone_curve_green_0_field.zw; }
            case 2u: { return Common::scene_tone_uniforms.tone_curve_green_1_field.xy; }
            case 3u: { return Common::scene_tone_uniforms.tone_curve_green_1_field.zw; }
            case 4u: { return Common::scene_tone_uniforms.tone_curve_green_2_field.xy; }
            case 5u: { return Common::scene_tone_uniforms.tone_curve_green_2_field.zw; }
            case 6u: { return Common::scene_tone_uniforms.tone_curve_green_3_field.xy; }
            default: { return Common::scene_tone_uniforms.tone_curve_green_3_field.zw; }
        }
    }
    if curve == 3u {
        switch index {
            case 0u: { return Common::scene_tone_uniforms.tone_curve_blue_0_field.xy; }
            case 1u: { return Common::scene_tone_uniforms.tone_curve_blue_0_field.zw; }
            case 2u: { return Common::scene_tone_uniforms.tone_curve_blue_1_field.xy; }
            case 3u: { return Common::scene_tone_uniforms.tone_curve_blue_1_field.zw; }
            case 4u: { return Common::scene_tone_uniforms.tone_curve_blue_2_field.xy; }
            case 5u: { return Common::scene_tone_uniforms.tone_curve_blue_2_field.zw; }
            case 6u: { return Common::scene_tone_uniforms.tone_curve_blue_3_field.xy; }
            default: { return Common::scene_tone_uniforms.tone_curve_blue_3_field.zw; }
        }
    }
    switch index {
        case 0u: { return Common::scene_tone_uniforms.tone_curve_0_field.xy; }
        case 1u: { return Common::scene_tone_uniforms.tone_curve_0_field.zw; }
        case 2u: { return Common::scene_tone_uniforms.tone_curve_1_field.xy; }
        case 3u: { return Common::scene_tone_uniforms.tone_curve_1_field.zw; }
        case 4u: { return Common::scene_tone_uniforms.tone_curve_2_field.xy; }
        case 5u: { return Common::scene_tone_uniforms.tone_curve_2_field.zw; }
        case 6u: { return Common::scene_tone_uniforms.tone_curve_3_field.xy; }
        default: { return Common::scene_tone_uniforms.tone_curve_3_field.zw; }
    }
}

fn tone_curve_count(curve: u32) -> u32 {
    if curve == 1u { return u32(clamp(Common::scene_tone_uniforms.tone_curve_red_meta.x, 2.0, 8.0)); }
    if curve == 2u { return u32(clamp(Common::scene_tone_uniforms.tone_curve_green_meta.x, 2.0, 8.0)); }
    if curve == 3u { return u32(clamp(Common::scene_tone_uniforms.tone_curve_blue_meta.x, 2.0, 8.0)); }
    return u32(clamp(Common::scene_tone_uniforms.tone_curve_meta.x, 2.0, 8.0));
}

fn tone_curve_is_identity(curve: u32) -> bool {
    if curve == 1u { return Common::scene_tone_uniforms.tone_curve_red_meta.y > 0.5; }
    if curve == 2u { return Common::scene_tone_uniforms.tone_curve_green_meta.y > 0.5; }
    if curve == 3u { return Common::scene_tone_uniforms.tone_curve_blue_meta.y > 0.5; }
    return Common::scene_tone_uniforms.tone_curve_meta.y > 0.5;
}

fn tone_curve_secant(a: vec2<f32>, b: vec2<f32>) -> f32 {
    return (b.y - a.y) / max(b.x - a.x, 1e-5);
}

fn tone_curve_tangent(curve: u32, index: u32, count: u32) -> f32 {
    if index == 0u {
        let endpoint = tone_curve_point(curve, 0u);
        let raw_slope = tone_curve_secant(endpoint, tone_curve_point(curve, 1u));
        return limit_scene_curve_endpoint_tangent(endpoint.y, raw_slope);
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

const SCENE_CURVE_DECODE_MAX: f32 = 32768.0;
const SCENE_CURVE_WORK_MAX: f32 = 60000.0;
// Bound the scene-domain derivative at a curve's black endpoint. The value is
// deliberately generous for creative curves, but it keeps the first
// representable half-float signed input from becoming a multi-thousand-unit
// false-colour jump near the upper endpoint.
const SCENE_CURVE_ZERO_SLOPE_MAX: f32 = 1048576.0;
// Begin a C1 Hermite shoulder at an exactly representable f32 coordinate. The
// scene start and tangent are derived from the rational branch using the same
// f32 constants, so the join matches in both value and first derivative.
const SCENE_CURVE_SHOULDER_ENCODE_START: f32 = 0.9999915361404419;
const SCENE_CURVE_SHOULDER_WIDTH: f32 =
    1.0 - SCENE_CURVE_SHOULDER_ENCODE_START;
const SCENE_CURVE_SHOULDER_START: f32 =
    ToneCommon::SCENE_MIDDLE_GREY * SCENE_CURVE_SHOULDER_ENCODE_START
        / SCENE_CURVE_SHOULDER_WIDTH;
const SCENE_CURVE_SHOULDER_TANGENT: f32 =
    ToneCommon::SCENE_MIDDLE_GREY / SCENE_CURVE_SHOULDER_WIDTH;

fn scene_curve_shoulder_decode(t: f32) -> f32 {
    let bounded_t = clamp(t, 0.0, 1.0);
    let t2 = bounded_t * bounded_t;
    let t3 = t2 * bounded_t;
    return (2.0 * t3 - 3.0 * t2 + 1.0) * SCENE_CURVE_SHOULDER_START
        + (t3 - 2.0 * t2 + bounded_t) * SCENE_CURVE_SHOULDER_TANGENT
        + (-2.0 * t3 + 3.0 * t2) * SCENE_CURVE_DECODE_MAX;
}

fn scene_curve_shoulder_derivative(t: f32) -> f32 {
    let bounded_t = clamp(t, 0.0, 1.0);
    let t2 = bounded_t * bounded_t;
    return (6.0 * t2 - 6.0 * bounded_t) * SCENE_CURVE_SHOULDER_START
        + (3.0 * t2 - 4.0 * bounded_t + 1.0)
            * SCENE_CURVE_SHOULDER_TANGENT
        + (-6.0 * t2 + 6.0 * bounded_t) * SCENE_CURVE_DECODE_MAX;
}

fn scene_curve_decode(value: f32) -> f32 {
    let bounded = clamp(value, 0.0, 1.0);
    if bounded <= SCENE_CURVE_SHOULDER_ENCODE_START {
        return ToneCommon::SCENE_MIDDLE_GREY * bounded / max(1.0 - bounded, 1e-6);
    }
    let t = (bounded - SCENE_CURVE_SHOULDER_ENCODE_START)
        / SCENE_CURVE_SHOULDER_WIDTH;
    return clamp(scene_curve_shoulder_decode(t), 0.0, SCENE_CURVE_DECODE_MAX);
}

fn scene_curve_encode(value: f32) -> f32 {
    // Invert the finite shoulder only for extreme scene values. Eight bisection
    // steps resolve more finely than one encoded f32 step in this interval and
    // preserve the identity curve to the precision available near 1.0.
    let positive = clamp(value, 0.0, SCENE_CURVE_DECODE_MAX);
    if positive <= SCENE_CURVE_SHOULDER_START {
        // Float32 rounding at the scene-domain join can otherwise advance the
        // rational inverse one encoded step into the shoulder. Clamp the
        // branch to the exact shoulder coordinate so decode(encode(y)) remains
        // monotonic across the join.
        return min(
            positive / (positive + ToneCommon::SCENE_MIDDLE_GREY),
            SCENE_CURVE_SHOULDER_ENCODE_START,
        );
    }

    var low = 0.0;
    var high = 1.0;
    for (var iteration = 0u; iteration < 8u; iteration = iteration + 1u) {
        let middle = 0.5 * (low + high);
        if scene_curve_shoulder_decode(middle) < positive {
            low = middle;
        } else {
            high = middle;
        }
    }
    let low_encoded = SCENE_CURVE_SHOULDER_ENCODE_START
        + SCENE_CURVE_SHOULDER_WIDTH * low;
    let high_encoded = SCENE_CURVE_SHOULDER_ENCODE_START
        + SCENE_CURVE_SHOULDER_WIDTH * high;
    let low_error = abs(scene_curve_decode(low_encoded) - positive);
    let high_error = abs(scene_curve_decode(high_encoded) - positive);
    return select(low_encoded, high_encoded, high_error < low_error);
}

fn scene_curve_decode_slope_scale(encoded_endpoint: f32) -> f32 {
    // Return d(scene output) / d(scene input) per unit encoded-curve tangent.
    // scene_curve_encode'(0) is 1 / ToneCommon::SCENE_MIDDLE_GREY. The scale is C1 at the
    // shoulder join and falls smoothly to zero at an endpoint of 1.0.
    let bounded = clamp(encoded_endpoint, 0.0, 1.0);
    if bounded != encoded_endpoint {
        return 0.0;
    }
    if bounded <= SCENE_CURVE_SHOULDER_ENCODE_START {
        let denominator = max(1.0 - bounded, 1e-6);
        return 1.0 / (denominator * denominator);
    }

    let t = (bounded - SCENE_CURVE_SHOULDER_ENCODE_START)
        / SCENE_CURVE_SHOULDER_WIDTH;
    let decoded_derivative = scene_curve_shoulder_derivative(t)
        / SCENE_CURVE_SHOULDER_WIDTH;
    return max(decoded_derivative / ToneCommon::SCENE_MIDDLE_GREY, 0.0);
}

fn limit_scene_curve_endpoint_tangent(encoded_endpoint: f32, encoded_slope: f32) -> f32 {
    let slope_scale = scene_curve_decode_slope_scale(encoded_endpoint);
    if slope_scale <= 1e-12 {
        return 0.0;
    }
    let encoded_limit = SCENE_CURVE_ZERO_SLOPE_MAX / slope_scale;
    return clamp(encoded_slope, -encoded_limit, encoded_limit);
}

fn decoded_scene_curve_zero_slope(encoded_black: f32, encoded_slope: f32) -> f32 {
    let slope_scale = scene_curve_decode_slope_scale(encoded_black);
    let limited_encoded_slope =
        limit_scene_curve_endpoint_tangent(encoded_black, encoded_slope);
    return clamp(
        limited_encoded_slope * slope_scale,
        -SCENE_CURVE_ZERO_SLOPE_MAX,
        SCENE_CURVE_ZERO_SLOPE_MAX,
    );
}

fn clamp_scene_curve_value(value: f32) -> f32 {
    return clamp(value, -SCENE_CURVE_WORK_MAX, SCENE_CURVE_WORK_MAX);
}

fn limit_scene_curve_rgb_ratio_preserving(value: vec3<f32>) -> vec3<f32> {
    // Composite curves are luminance controls, so an extreme headroom limit
    // must not clip one channel before the others. Uniformly scale the whole
    // signed triplet and preserve RGB ratios, chromaticity, hue, and normalized
    // chroma. Per-channel curves intentionally keep scalar component bounds.
    let peak = max(max(abs(value.r), abs(value.g)), abs(value.b));
    if peak <= SCENE_CURVE_WORK_MAX {
        return value;
    }
    let scale = SCENE_CURVE_WORK_MAX / max(peak, 1e-12);
    return clamp(
        value * scale,
        vec3<f32>(-SCENE_CURVE_WORK_MAX),
        vec3<f32>(SCENE_CURVE_WORK_MAX),
    );
}

fn remap_scene_luminance(
    rgb: vec3<f32>,
    adjusted_luminance: f32,
    black_luminance: f32,
    zero_slope: f32,
) -> vec3<f32> {
    let luminance = dot(rgb, Common::LUMA);
    let black = max(black_luminance, 0.0);
    if luminance <= 0.0 {
        // Continue the scene-domain master curve linearly through non-positive
        // luminance. Using the same endpoint slope as the positive branch keeps
        // signed opponent colors continuous across the zero-luminance plane.
        return limit_scene_curve_rgb_ratio_preserving(vec3<f32>(black) + rgb * zero_slope);
    }
    // Separate the neutral black offset from the chromatic signal. A lifted
    // endpoint intentionally reduces shadow colorfulness as pixels approach the
    // neutral black floor; the endpoint slope controls both sides of zero.
    let mapped_luminance = max(adjusted_luminance, black);
    let chromatic_luminance = mapped_luminance - black;
    return limit_scene_curve_rgb_ratio_preserving(
        vec3<f32>(black) + rgb * (chromatic_luminance / luminance),
    );
}

fn scene_curve_zero_slope(curve: u32) -> f32 {
    let count = tone_curve_count(curve);
    let first = tone_curve_point(curve, 0u);
    let encoded_black = point_curve_value(curve, 0.0);
    // point_curve_value() holds the first endpoint constant to the left of its
    // X coordinate. If that endpoint was moved right, the effective slope at
    // scene zero is therefore flat rather than the first segment's secant.
    if first.x > 0.0 {
        return 0.0;
    }
    let encoded_slope = tone_curve_tangent(curve, 0u, count);
    return decoded_scene_curve_zero_slope(encoded_black, encoded_slope);
}

fn apply_scene_channel_curve(curve: u32, value: f32) -> f32 {
    let encoded_black = point_curve_value(curve, 0.0);
    let black = scene_curve_decode(encoded_black);
    if value < 0.0 {
        // Extend the nonnegative curve with its scene-domain slope at zero.
        // This preserves signed intermediate detail and removes the jump from
        // an untouched negative epsilon to a lifted zero endpoint. Bound the
        // result to finite half-float-safe scene headroom.
        return clamp_scene_curve_value(
            black + value * scene_curve_zero_slope(curve),
        );
    }
    return scene_curve_decode(
        point_curve_value(curve, scene_curve_encode(value)),
    );
}

fn apply_point_tone_curve(rgb: vec3<f32>) -> vec3<f32> {
    if tone_curve_is_identity(0u) {
        return rgb;
    }
    let luminance = max(dot(rgb, Common::LUMA), 0.0);
    let encoded_black = point_curve_value(0u, 0.0);
    let black_luminance = scene_curve_decode(encoded_black);
    let adjusted_luminance = scene_curve_decode(
        point_curve_value(0u, scene_curve_encode(luminance)),
    );
    return remap_scene_luminance(
        rgb,
        adjusted_luminance,
        black_luminance,
        // The composite curve treats its decoded black endpoint as a floor.
        // A descending first segment is therefore flat at the endpoint; use
        // that effective nonnegative slope on the signed side as well.
        max(scene_curve_zero_slope(0u), 0.0),
    );
}

fn apply_rgb_point_curves(rgb: vec3<f32>) -> vec3<f32> {
    var result = rgb;
    if !tone_curve_is_identity(1u) {
        result.r = apply_scene_channel_curve(1u, result.r);
    }
    if !tone_curve_is_identity(2u) {
        result.g = apply_scene_channel_curve(2u, result.g);
    }
    if !tone_curve_is_identity(3u) {
        result.b = apply_scene_channel_curve(3u, result.b);
    }
    return result;
}

// Display-linear Blacks-toe calibration. The two decay scales are denominators
// of exp2(-luminance / scale), so each scale is the display-linear luminance
// interval over which that tail halves. 0.035 gives positive Blacks a compact
// black lift with a long low-amplitude tail; 0.070 makes the negative tail fall
// off more slowly. The 10.50 coefficient is the fitted EV-domain strength of
// the deepest negative toe inside the 0.012..0.030 luminance mask. Together
// these values reproduce the measured black-end response without tying the
// control to scene exposure or the profile/sigmoid view transform.
const DISPLAY_BLACKS_LIFT_DECAY: f32 = 0.035;
const DISPLAY_BLACKS_CRUSH_TAIL_DECAY: f32 = 0.070;
const DISPLAY_BLACKS_DEEP_CRUSH_EV: f32 = 10.50;

fn apply_display_blacks_toe_amount(rgb: vec3<f32>, amount: f32) -> vec3<f32> {
    if abs(amount) < 1e-7 {
        return rgb;
    }

    // Apply Blacks after the single view transform so profile/sigmoid compression
    // cannot erase its endpoint authority. The fixed display-linear response also
    // keeps low/high-key histogram statistics from changing the control's reach.
    let luminance = dot(rgb, Common::LUMA);
    if luminance <= 0.0 {
        return rgb;
    }
    let hdr_guard = 1.0 - ToneCommon::tone_smoothstep(0.35, 1.0, luminance);
    if hdr_guard <= 0.0 {
        return rgb;
    }

    var offset_ev = 0.0;
    if amount >= 0.0 {
        // Positive Blacks is a long, smooth toe with a small upper-tone tail.
        let weight = 0.08 + 0.92 * exp2(-luminance / DISPLAY_BLACKS_LIFT_DECAY);
        offset_ev = amount * 1.75 * weight * hdr_guard;
    } else {
        // Negative Blacks crushes the darkest region, then falls to a restrained tail.
        let deep = 1.0 - ToneCommon::tone_smoothstep(0.012, 0.030, luminance);
        let tail = 0.10 + 2.35 * exp2(-luminance / DISPLAY_BLACKS_CRUSH_TAIL_DECAY);
        offset_ev = -(-amount) * (DISPLAY_BLACKS_DEEP_CRUSH_EV * deep + tail) * hdr_guard;
    }
    let target_luminance = luminance * exp2(offset_ev);
    return rgb * (target_luminance / luminance);
}

fn apply_display_blacks_toe_value(rgb: vec3<f32>, value: f32) -> vec3<f32> {
    return apply_display_blacks_toe_amount(rgb, basic_low_tone_control(value));
}

fn apply_lightroom_tone(rgb: vec3<f32>, pos: vec2<i32>) -> vec3<f32> {
    let basic = apply_local_basic_tone(rgb, pos);
    return apply_rgb_point_curves(apply_point_tone_curve(basic));
}

fn finite_scalar(value: f32) -> bool {
    return value == value && abs(value) < 3.0e38;
}

fn generalized_loglogistic_sigmoid(value: f32) -> f32 {
    let white_target = Common::scene_tone_uniforms.sigmoid_curve.x;
    // The ABI slot stores log2(paper_exposure). Steep but valid curves can
    // overflow both the film response and paper exposure in linear form even
    // though their ratio remains perfectly well behaved.
    let log2_paper_exposure = Common::scene_tone_uniforms.sigmoid_curve.z;
    let film_fog = Common::scene_tone_uniforms.sigmoid_curve.w;
    let film_power = Common::scene_tone_uniforms.sigmoid_power.x;
    let paper_power = Common::scene_tone_uniforms.sigmoid_power.y;
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
    // Exact darktable sigmoid projection: move a triplet with negative
    // channels toward its non-negative arithmetic mean just far enough for the
    // minimum channel to reach zero.
    let pixel_average = max((rgb.r + rgb.g + rgb.b) / 3.0, 0.0);
    let minimum = min(rgb.r, min(rgb.g, rgb.b));
    let saturation_factor = select(
        1.0,
        -pixel_average / (minimum - pixel_average),
        minimum < 0.0,
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
        clamp(Common::scene_tone_uniforms.sigmoid_power.z, 0.0, 1.0),
    );
}

fn sigmoid_rgb_ratio(rgb: vec3<f32>) -> vec3<f32> {
    let white_target = Common::scene_tone_uniforms.sigmoid_curve.x;
    let black_target = Common::scene_tone_uniforms.sigmoid_curve.y;
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
    if Common::scene_tone_uniforms.sigmoid_power.w < 0.5 {
        return sigmoid_per_channel(rgb);
    }
    return sigmoid_rgb_ratio(rgb);
}

fn apply_sigmoid_view_transform(scene_rgb: vec3<f32>) -> vec3<f32> {
    return darktable_sigmoid(scene_rgb);
}
