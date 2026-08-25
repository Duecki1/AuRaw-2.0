#import auraw::common as Common
#import auraw::color as Color
#import auraw::basic_adjustments as BasicAdjustments
#import auraw::scene_adjustments as SceneAdjustments
#import auraw::tone_common as ToneCommon
#import auraw::tonemap as Tonemap


fn creative_fine_base_ev(pos: vec2<i32>) -> f32 {
    let step = SceneAdjustments::presence_step(1.65, 5);
    return SceneAdjustments::bilateral_log_luminance(pos, 2, step, 10.5);
}

fn creative_broad_base_ev(pos: vec2<i32>) -> f32 {
    let clarity_reference = select(4.5, 5.5, Common::camera_uniforms.tone_guide_radius > 3.5);
    let step = SceneAdjustments::presence_step(clarity_reference, 14);
    return SceneAdjustments::atrous_log_luminance(pos, step, 1.05);
}

fn creative_edge_guard(pos: vec2<i32>) -> f32 {
    let step = SceneAdjustments::presence_step(1.0, 3);
    let left = SceneAdjustments::log_luminance(SceneAdjustments::adjustment_base_at(pos + vec2<i32>(-step, 0)));
    let right = SceneAdjustments::log_luminance(SceneAdjustments::adjustment_base_at(pos + vec2<i32>(step, 0)));
    let up = SceneAdjustments::log_luminance(SceneAdjustments::adjustment_base_at(pos + vec2<i32>(0, -step)));
    let down = SceneAdjustments::log_luminance(SceneAdjustments::adjustment_base_at(pos + vec2<i32>(0, step)));
    let gradient = length(vec2<f32>(right - left, down - up));
    return 1.0 - 0.78 * smoothstep(0.48, 1.25, gradient);
}

fn apply_texture_and_clarity_values(
    pos: vec2<i32>,
    rgb: vec3<f32>,
    texture_value: f32,
    clarity_value: f32,
) -> vec3<f32> {
    let texture = BasicAdjustments::perceptual_control(texture_value);
    let clarity = BasicAdjustments::perceptual_control(clarity_value);
    if abs(texture) < 1e-6 && abs(clarity) < 1e-6 {
        return rgb;
    }

    let center_ev = SceneAdjustments::log_luminance(rgb);
    let fine_base_ev = creative_fine_base_ev(pos);
    var broad_base_ev = fine_base_ev;
    if abs(clarity) >= 1e-6 {
        broad_base_ev = creative_broad_base_ev(pos);
    }

    let texture_band_ev = center_ev - fine_base_ev;
    let clarity_band_ev = fine_base_ev - broad_base_ev;

    let signal_gate = smoothstep(-7.4, -2.35, center_ev);
    let shadow_noise = 1.0 - signal_gate;
    let texture_threshold = mix(0.028, 0.006, signal_gate) * mix(1.0, 1.65, shadow_noise);
    let positive_texture = SceneAdjustments::soft_detail_threshold(texture_band_ev, texture_threshold);
    var negative_texture_base_ev = fine_base_ev;
    if texture < 0.0 {
        negative_texture_base_ev = SceneAdjustments::bilateral_log_luminance(
            pos,
            3,
            SceneAdjustments::presence_step(1.65, 5),
            3.0,
        );
    }
    let negative_texture = clamp(center_ev - negative_texture_base_ev, -0.32, 0.32);

    let midtone_gate = smoothstep(-7.0, -2.25, center_ev)
        * (1.0 - 0.74 * smoothstep(0.9, 3.6, center_ev));
    let selected_clarity = SceneAdjustments::soft_detail_threshold(clarity_band_ev, 0.0065);
    let halo_guard = creative_edge_guard(pos);
    let percentiles = Tonemap::tone_percentiles();
    let center_relative_ev = center_ev - log2(ToneCommon::SCENE_MIDDLE_GREY);
    let clarity_scene_gate = ToneCommon::tone_smoothstep(
        1.0,
        2.5,
        percentiles.p995_field - percentiles.p005_field,
    );
    let clarity_tone_position = ToneCommon::tone_smoothstep(
        percentiles.p05_field + 0.50,
        percentiles.p50_field - 0.10,
        center_relative_ev,
    );
    let positive_clarity_tone = select(
        0.0,
        clarity * mix(-1.25, 0.36, clarity_tone_position) * clarity_scene_gate,
        clarity > 0.0,
    );

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
    return Color::gamut_project_nonnegative_rec2020(rgb * exp2(delta_ev));
}
