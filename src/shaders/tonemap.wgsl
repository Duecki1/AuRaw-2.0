// One image-adaptive scene-to-display transform. A robust histogram chooses
// useful scene bounds while a low-resolution bilateral guide gives the Basic
// tonal controls soft, edge-aware masks. All controls are integrated into this
// mapping; no second basic-adjustments or filmic curve is applied.
@group(0) @binding(16) var<storage, read> tone_stats: ToneStats;
@group(0) @binding(17) var tone_guide_tex: texture_2d<f32>;

const DISPLAY_SHOULDER_START: f32 = 0.94;

fn schlick_bias(value: f32, shape: f32) -> f32 {
    let x = clamp(value, 0.0, 1.0);
    let a = clamp(shape, 0.04, 96.0);
    return x / max(a + (1.0 - a) * x, 1e-6);
}

fn highlight_shoulder(adjusted_ev: f32, white_ev: f32, length_ev: f32) -> f32 {
    let distance = max(adjusted_ev - white_ev, 0.0);
    let normalized = distance / max(length_ev, 1e-4);

    // Four exponential half-lives fit inside the requested shoulder length.
    // white_ev is now the shoulder start, not a clipping boundary. Exact
    // display white is approached asymptotically instead of becoming a plateau.
    return 1.0 - (1.0 - DISPLAY_SHOULDER_START) * exp2(-4.0 * normalized);
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
    return mix(a, b, fraction.y);
}

fn tone_percentiles() -> TonePercentiles {
    let p0 = tone_stats.percentiles_0;
    let p1 = tone_stats.percentiles_1;
    return TonePercentiles(p0.x, p0.y, p0.z, p0.w, p1.x);
}

fn adaptive_scene_bounds(percentiles: TonePercentiles) -> vec2<f32> {
    let robust_black = min(percentiles.p005 - 0.25, percentiles.p05 - 0.80);
    let robust_white = max(percentiles.p995 + 0.25, percentiles.p95 + 0.80);

    // Blend histogram bounds with conservative fixed bounds. This avoids
    // unstable auto-levels on unusual images while making the default curve
    // use the photographed range instead of always fitting twelve stops.
    var black_ev = mix(-8.0, clamp(robust_black, -12.0, -2.0), 0.72);
    var white_ev = mix(4.0, clamp(robust_white, 1.5, 9.0), 0.72);

    let minimum_range = 5.5;
    if white_ev - black_ev < minimum_range {
        let center = clamp(percentiles.p50, -1.5, 1.5);
        black_ev = center - minimum_range * 0.58;
        white_ev = center + minimum_range * 0.42;
    }
    return vec2<f32>(black_ev, white_ev);
}

fn adaptive_tone_masks(local_ev: f32, percentiles: TonePercentiles) -> vec4<f32> {
    let black_mask = 1.0 - tone_smoothstep(
        percentiles.p005 - 0.45,
        percentiles.p05 + 0.30,
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

fn scene_to_display_luminance(scene_luminance: f32, local_ev: f32) -> f32 {
    let highlights = clamp(params.basic_tone.x / 100.0, -1.0, 1.0);
    let shadows = clamp(params.basic_tone.y / 100.0, -1.0, 1.0);
    let whites = clamp(params.basic_tone.z / 100.0, -1.0, 1.0);
    let blacks = clamp(params.basic_tone.w / 100.0, -1.0, 1.0);
    let contrast = clamp(params.contrast / 100.0, -1.0, 1.0);

    let percentiles = tone_percentiles();
    let masks = adaptive_tone_masks(local_ev, percentiles);
    let bounds = adaptive_scene_bounds(percentiles);

    // Whites and Blacks move the robust scene endpoints and also receive a
    // narrow end-zone lift/crush. Highlights and Shadows act through broader
    // edge-aware zones. This is still one per-pixel monotonic transform.
    let black_ev = bounds.x - 2.75 * blacks;
    let white_ev = bounds.y - 2.25 * whites;
    let range_ev = max(white_ev - black_ev, 3.5);

    let scene_ev = log2(max(scene_luminance, 1e-8) / SCENE_MIDDLE_GREY);
    let zone_offset =
          0.60 * blacks * masks.x
        + 1.35 * shadows * masks.y
        + 1.20 * highlights * masks.z
        + 0.60 * whites * masks.w;
    let adjusted_ev = scene_ev + zone_offset;

    let position = clamp((adjusted_ev - black_ev) / range_ev, 0.0, 1.0);
    let middle_position = clamp(-black_ev / range_ev, 0.04, 0.96);

    // Stronger than the old one-stop full-range mapping so a Lightroom-style
    // +/-100 Contrast control has an obvious but bounded effect.
    let middle_slope = exp2(1.55 * contrast);
    let shadow_shape = clamp(
        middle_slope * middle_position / DISPLAY_MIDDLE_GREY
            * exp2(-0.70 * shadows),
        0.04,
        96.0,
    );
    let highlight_shape = clamp(
        (DISPLAY_SHOULDER_START - DISPLAY_MIDDLE_GREY)
            / max(middle_slope * (1.0 - middle_position), 1e-4)
            * exp2(-0.70 * highlights),
        0.04,
        96.0,
    );

    if adjusted_ev > white_ev {
        // Positive Whites/Highlights make the shoulder a little firmer, while
        // negative values preserve up to four stops of highlight latitude.
        let shoulder_length_ev = clamp(
            3.0 - 0.5 * whites - 0.5 * highlights,
            2.0,
            4.0,
        );
        return highlight_shoulder(adjusted_ev, white_ev, shoulder_length_ev);
    }

    if position <= middle_position {
        let local = position / max(middle_position, 1e-5);
        return DISPLAY_MIDDLE_GREY * schlick_bias(local, shadow_shape);
    }

    let local = (position - middle_position) / max(1.0 - middle_position, 1e-5);
    return DISPLAY_MIDDLE_GREY
        + (DISPLAY_SHOULDER_START - DISPLAY_MIDDLE_GREY)
            * schlick_bias(local, highlight_shape);
}

fn scene_to_display(rgb: vec3<f32>, pos: vec2<i32>) -> vec3<f32> {
    let positive = max(rgb, vec3<f32>(0.0));
    let scene_luminance = safe_luma(positive);
    let local_ev = sample_tone_guide_ev(pos);
    let display_luminance = scene_to_display_luminance(scene_luminance, local_ev);
    return positive * (display_luminance / scene_luminance);
}

fn compress_display_gamut(rgb: vec3<f32>) -> vec3<f32> {
    let x = max(rgb, vec3<f32>(0.0));
    let peak = max(max(x.r, x.g), x.b);
    if peak <= 1.0 {
        return x;
    }

    let lum = clamp(safe_luma(x), 0.0, 1.0);
    let boundary = vec3<f32>(lum);
    let scale = clamp((1.0 - lum) / max(peak - lum, 1e-6), 0.0, 1.0);
    return mix(boundary, x, scale);
}

fn display_render(rgb: vec3<f32>, pos: vec2<i32>) -> vec3<f32> {
    // The adaptive tone map produces display-referred linear Rec.2020. The
    // final 3D LUT is generated from the selected ICC display/output profile,
    // including its transfer curves and rendering intent.
    let mapped = scene_to_display(rgb, pos);
    return apply_output_lut(mapped);
}
