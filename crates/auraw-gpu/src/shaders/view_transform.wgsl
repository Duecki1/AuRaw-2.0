#import auraw::common as Common
#import auraw::color as Color
#import auraw::profile as Profile
#import auraw::scene_adjustments as SceneAdjustments
#import auraw::creative_effects as CreativeEffects
#import auraw::tone_common as ToneCommon
#import auraw::tonemap as Tonemap

// Display/view-domain processing: perceptual color mixer, color grading, the
// profile/sigmoid view transform, display black toe, and output encoding.

// Lightroom's named HSL channels are a UI model, not a reason to process in
// mathematical HSL. HSL lightness is based on per-pixel RGB extrema and turns
// demosaic/chroma noise into luminance noise. The mixer below instead selects
// hues in perceptual OKLab, stabilizes only the selector with an edge-aware
// neighbourhood, and applies luminance as a scene-linear exposure gain.

// Perceptual hue-selector anchors, expressed in turns on the OKLab a/b plane:
// hue = wrap(atan2(b, a) / (2*pi), 0, 1). The values were obtained by converting
// fully saturated sRGB primary/secondary swatches plus the orange and purple
// intermediates at HSL 0, 30, 60, 120, 180, 240, 270, and 300 degrees to
// OKLab. They therefore do not equal the source HSL angles: OKLab deliberately
// redistributes hue around its perceptual plane. The paired bell widths are
// empirical overlap widths chosen to bridge the unequal OKLab spacing while
// retaining distinct centers for Lightroom's eight named mixer ranges.
const MIXER_HUE_RED: f32 = 0.0812052;
const MIXER_HUE_ORANGE: f32 = 0.1465993;
const MIXER_HUE_YELLOW: f32 = 0.3049145;
const MIXER_HUE_GREEN: f32 = 0.3958204;
const MIXER_HUE_AQUA: f32 = 0.5410248;
const MIXER_HUE_BLUE: f32 = 0.7334778;
const MIXER_HUE_PURPLE: f32 = 0.8160390;
const MIXER_HUE_MAGENTA: f32 = 0.9121206;

const MIXER_WIDTH_RED: f32 = 0.160;
const MIXER_WIDTH_ORANGE: f32 = 0.150;
const MIXER_WIDTH_YELLOW: f32 = 0.150;
const MIXER_WIDTH_GREEN: f32 = 0.140;
const MIXER_WIDTH_AQUA: f32 = 0.180;
const MIXER_WIDTH_BLUE: f32 = 0.180;
const MIXER_WIDTH_PURPLE: f32 = 0.100;
const MIXER_WIDTH_MAGENTA: f32 = 0.160;

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
    // Red, orange, yellow, green, aqua, blue, purple, magenta. Use the OKLab
    // swatch-derived anchors above rather than treating HSL degrees as uniform
    // angles in the perceptual a/b plane.
    let first = vec4<f32>(
        smooth_hue_bell(hue, MIXER_HUE_RED, MIXER_WIDTH_RED),
        smooth_hue_bell(hue, MIXER_HUE_ORANGE, MIXER_WIDTH_ORANGE),
        smooth_hue_bell(hue, MIXER_HUE_YELLOW, MIXER_WIDTH_YELLOW),
        smooth_hue_bell(hue, MIXER_HUE_GREEN, MIXER_WIDTH_GREEN),
    );
    let second = vec4<f32>(
        smooth_hue_bell(hue, MIXER_HUE_AQUA, MIXER_WIDTH_AQUA),
        smooth_hue_bell(hue, MIXER_HUE_BLUE, MIXER_WIDTH_BLUE),
        smooth_hue_bell(hue, MIXER_HUE_PURPLE, MIXER_WIDTH_PURPLE),
        smooth_hue_bell(hue, MIXER_HUE_MAGENTA, MIXER_WIDTH_MAGENTA),
    );
    return MixerBandWeights(first, second, dot(first, vec4<f32>(1.0)) + dot(second, vec4<f32>(1.0)));
}

fn mixer_band_value(weights: MixerBandWeights, first: vec4<f32>, second: vec4<f32>) -> f32 {
    return (dot(weights.first, first) + dot(weights.second, second)) / max(weights.total, 1e-6);
}

fn directed_hue_shift(value: f32, backward_span: f32, forward_span: f32) -> f32 {
    // Preserve the historical +/-100 response exactly, but permit the extended
    // UI range to continue linearly up to twice that shift. This lets a named
    // channel travel beyond its immediate neighbour without changing existing
    // sidecars or reducing fine control around zero.
    let amount = clamp(value / 100.0, -2.0, 2.0);
    let span = select(backward_span, forward_span, amount >= 0.0);
    return amount * span * 0.90;
}

fn mixer_hue_shift_values(weights: MixerBandWeights, first_values: vec4<f32>, second_values: vec4<f32>) -> f32 {
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

fn mixer_sample_from_rgb(rgb: vec3<f32>) -> MixerSample {
    let lab = Color::linear_srgb_to_oklab(Common::REC2020_TO_SRGB * rgb);
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
    let selector_radius = select(1, 2, Common::camera_uniforms.tone_guide_radius > 3.5);
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            if (dx == 0 && dy == 0) || abs(dx) > selector_radius || abs(dy) > selector_radius {
                continue;
            }
            let neighbour_rgb = textureLoad(SceneAdjustments::final_adjustment_tex, Common::clamp_pos(pos + vec2<i32>(dx, dy)), 0).xyz;
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

fn color_grade_strength(
    shadows: vec4<f32>,
    midtones: vec4<f32>,
    highlights: vec4<f32>,
    global: vec4<f32>,
) -> f32 {
    return max(
        max(max(abs(shadows.y), abs(shadows.z)), max(abs(midtones.y), abs(midtones.z))),
        max(max(abs(highlights.y), abs(highlights.z)), max(abs(global.y), abs(global.z))),
    );
}

fn color_grade_vector(wheel: vec4<f32>) -> vec2<f32> {
    let angle = wheel.x * 2.0 * 3.14159265359;
    return vec2<f32>(cos(angle), sin(angle)) * clamp(wheel.y, 0.0, 1.0);
}

fn color_grade_tonal_weights(luminance: f32, options: vec4<f32>) -> vec3<f32> {
    // Evaluate ranges in exposure space around photographic middle gray.
    // Wider blending produces Lightroom-like overlap without hard range
    // boundaries. Balance shifts the shared pivot by up to 1.5 stops.
    let ev = log2(max(luminance, 1e-7) / ToneCommon::SCENE_MIDDLE_GREY);
    let width = mix(0.60, 2.80, clamp(options.x, 0.0, 1.0));
    let pivot = -clamp(options.y, -1.0, 1.0) * 1.5;
    let shadows = 1.0 - smoothstep(
        -1.25 + pivot - 0.5 * width,
        -1.25 + pivot + 0.5 * width,
        ev,
    );
    let highlights = smoothstep(
        1.25 + pivot - 0.5 * width,
        1.25 + pivot + 0.5 * width,
        ev,
    );
    let midtones = max(1.0 - shadows - highlights, 0.0);
    let total = max(shadows + midtones + highlights, 1e-6);
    return vec3<f32>(shadows, midtones, highlights) / total;
}

fn apply_color_grading_wheels(
    input_rgb: vec3<f32>,
    shadows: vec4<f32>,
    midtones: vec4<f32>,
    highlights: vec4<f32>,
    global: vec4<f32>,
    options: vec4<f32>,
) -> vec3<f32> {
    if color_grade_strength(shadows, midtones, highlights, global) < 1e-7 {
        return input_rgb;
    }

    let rgb = input_rgb;
    let luminance = max(dot(rgb, Common::LUMA), 0.0);
    let weights = color_grade_tonal_weights(luminance, options);
    let lab = Color::linear_srgb_to_oklab(Common::REC2020_TO_SRGB * rgb);

    let grade_vector = color_grade_vector(shadows) * weights.x
        + color_grade_vector(midtones) * weights.y
        + color_grade_vector(highlights) * weights.z
        + color_grade_vector(global);

    var adjusted = rgb;
    if dot(grade_vector, grade_vector) > 1e-12 {
        // Deep-shadow confidence and a soft saturation guard prevent grading
        // from amplifying demosaic chroma noise or forcing already vivid
        // colors against the gamut boundary. Gamut compression then holds
        // OKLab lightness and hue instead of clipping individual RGB channels.
        let signal = smoothstep(0.025, 0.115, max(lab.x, 0.0));
        let hdr_guard = 1.0 / (1.0 + 0.25 * max(lab.x - 1.0, 0.0));
        let existing_chroma = length(lab.yz);
        let saturation_guard = 1.0 / (1.0 + 1.8 * existing_chroma);
        let target_ab = lab.yz + grade_vector * (0.135 * signal * hdr_guard * saturation_guard);
        let target_chroma = length(target_ab);
        if target_chroma > 1e-8 {
            adjusted = Color::perceptual_rec2020_from_oklab_nonnegative(
                lab.x,
                target_ab / target_chroma,
                target_chroma,
            );
        }
    }

    let luminance_grade = shadows.z * weights.x
        + midtones.z * weights.y
        + highlights.z * weights.z
        + global.z;
    if abs(luminance_grade) > 1e-7 {
        // Scene-linear scalar gain preserves the graded hue and RGB ratios.
        adjusted = adjusted * exp2(mixer_luminance_ev(luminance_grade, lab.x));
    }
    return adjusted;
}

fn apply_local_color_grading(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || (state.w & 2u) == 0u { continue; }
        let weight = SceneAdjustments::local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }
        let adjusted = apply_color_grading_wheels(
            rgb,
            Common::mask_data[index].grade_shadows,
            Common::mask_data[index].grade_midtones,
            Common::mask_data[index].grade_highlights,
            Common::mask_data[index].grade_global,
            Common::mask_data[index].grade_options,
        );
        rgb = mix(rgb, adjusted, weight);
    }
    return rgb;
}

fn apply_color_mixer_values(
    pos: vec2<i32>,
    rgb: vec3<f32>,
    hue_0: vec4<f32>,
    hue_1: vec4<f32>,
    saturation_0: vec4<f32>,
    saturation_1: vec4<f32>,
    luminance_0: vec4<f32>,
    luminance_1: vec4<f32>,
) -> vec3<f32> {
    let strengths = vec3<f32>(
        max(max_abs_vec4(hue_0), max_abs_vec4(hue_1)),
        max(max_abs_vec4(saturation_0), max_abs_vec4(saturation_1)),
        max(max_abs_vec4(luminance_0), max_abs_vec4(luminance_1)),
    );
    if max(strengths.x, max(strengths.y, strengths.z)) < 1e-6 {
        return rgb;
    }

    let sample = stabilized_mixer_sample(pos, rgb);
    if sample.confidence < 1e-5 {
        return rgb;
    }

    let selector_hue = fract(atan2(sample.hue_vector.y, sample.hue_vector.x) / (2.0 * 3.14159265359) + 1.0);
    let weights = mixer_band_weights(selector_hue);
    let hue_shift = mixer_hue_shift_values(weights, hue_0, hue_1) * sample.confidence;
    let saturation_amount = mixer_band_value(weights, saturation_0, saturation_1)
        / 100.0 * sample.confidence;
    let luminance_amount = mixer_band_value(weights, luminance_0, luminance_1)
        / 100.0 * sample.confidence;

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
        adjusted = Color::perceptual_rec2020_from_oklab_nonnegative(
            sample.lab.x,
            target_hue,
            target_chroma,
        );
    }

    if abs(luminance_amount) > 1e-7 {
        // A scalar scene-linear gain preserves RGB ratios, hue and saturation.
        adjusted = adjusted * exp2(mixer_luminance_ev(luminance_amount, sample.lab.x));
        adjusted = Color::perceptual_gamut_compress_nonnegative_rec2020(adjusted);
    }
    return adjusted;
}

fn apply_color_mixer(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    return apply_color_mixer_values(
        pos,
        rgb,
        Common::scene_tone_uniforms.hsl_hue_0,
        Common::scene_tone_uniforms.hsl_hue_1,
        Common::scene_tone_uniforms.hsl_saturation_0,
        Common::scene_tone_uniforms.hsl_saturation_1,
        Common::scene_tone_uniforms.hsl_luminance_0,
        Common::scene_tone_uniforms.hsl_luminance_1,
    );
}

fn apply_local_color_mixer(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || (state.w & 1u) == 0u { continue; }
        let weight = SceneAdjustments::local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }

        // A local HSL/mixer edit is exactly the global mixer node with mask-
        // supplied parameters, followed by one masked blend. Keeping one node
        // implementation prevents local/global selector or gamut drift.
        let adjusted = apply_color_mixer_values(
            pos,
            rgb,
            Common::mask_data[index].hsl_hue_0,
            Common::mask_data[index].hsl_hue_1,
            Common::mask_data[index].hsl_saturation_0,
            Common::mask_data[index].hsl_saturation_1,
            Common::mask_data[index].hsl_luminance_0,
            Common::mask_data[index].hsl_luminance_1,
        );
        rgb = mix(rgb, adjusted, weight);
    }
    return rgb;
}

fn apply_view_transform(scene_rgb: vec3<f32>) -> vec3<f32> {
    // Optional creative profile look is the final scene-domain operation. It is
    // deliberately downstream of H/S/W/B, Contrast, curves, presence, mixer,
    // and grading so those controls mean the same thing across camera profiles.
    let looked = Profile::apply_optional_profile_look(scene_rgb);
    let view_input = Color::gamut_project_nonnegative_rec2020(looked);

    return Tonemap::apply_sigmoid_view_transform(view_input);
}

fn apply_local_display_blacks(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u { continue; }
        let value = Common::mask_data[index].adjust_1.y;
        if abs(value) < 1e-7 { continue; }
        let weight = SceneAdjustments::local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }
        let amount = Tonemap::basic_low_tone_control(value) * weight;
        rgb = Tonemap::apply_display_blacks_toe_amount(rgb, amount);
    }
    return rgb;
}

// ShaderManager specializes this default literal together with the matching
// ceiling-divided dispatch dimensions when benchmarking alternate tile shapes.
@compute @workgroup_size(8, 8, 1)
fn apply_view_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let rgb = textureLoad(SceneAdjustments::final_adjustment_tex, pos, 0).xyz;
    let globally_mixed = apply_color_mixer(pos, rgb);
    let mixed = apply_local_color_mixer(pos, globally_mixed);
    let globally_graded = apply_color_grading_wheels(
        mixed,
        Common::scene_tone_uniforms.grade_shadows,
        Common::scene_tone_uniforms.grade_midtones,
        Common::scene_tone_uniforms.grade_highlights,
        Common::scene_tone_uniforms.grade_global,
        Common::scene_tone_uniforms.grade_options,
    );
    let graded = apply_local_color_grading(pos, globally_graded);
    var display_linear = apply_view_transform(graded);
    display_linear = Tonemap::apply_display_blacks_toe_value(display_linear, Common::scene_tone_uniforms.basic_tone.w);
    display_linear = apply_local_display_blacks(pos, display_linear);
    display_linear = CreativeEffects::apply_vignette(pos, display_linear);
    textureStore(SceneAdjustments::display_linear_out, pos, vec4<f32>(display_linear, 1.0));
    // Output ICC/device encoding is a separate display-domain operation, not a
    // second view transform. It receives already display-referred linear RGB.
    textureStore(SceneAdjustments::out_tex, pos, vec4<f32>(Profile::apply_output_lut(display_linear), 1.0));
}
