#import auraw::common as Common
#import auraw::color as Color
#import auraw::basic_adjustments as BasicAdjustments
#import auraw::scene_adjustments as SceneAdjustments
#import auraw::detail_scale_space as DetailScaleSpace
#import auraw::tone_common as ToneCommon
#import auraw::tonemap as Tonemap

struct HazeNeighborhood {
    dark_ratio: f32,
    airlight: vec3<f32>,
    airlight_luma: f32,
}

fn normalized_dark_ratio(rgb: vec3<f32>, airlight_luma: f32) -> f32 {
    let positive = Color::gamut_project_nonnegative_rec2020(rgb);
    let normalized = positive / max(airlight_luma, 1e-6);
    return clamp(min(normalized.r, min(normalized.g, normalized.b)), 0.0, 1.0);
}

fn haze_neighborhood(pos: vec2<i32>, step: i32, airlight_luma: f32) -> HazeNeighborhood {
    var dark_ratio = 1.0;
    var brightest = SceneAdjustments::adjustment_base_at(pos);
    var brightest_luma = Common::safe_luma(brightest);
    var haziest_ratio = normalized_dark_ratio(brightest, airlight_luma);
    for (var ky = -3; ky <= 3; ky = ky + 1) {
        for (var kx = -3; kx <= 3; kx = kx + 1) {
            let sample = SceneAdjustments::adjustment_base_at(pos + vec2<i32>(kx * step, ky * step));
            let sample_dark_ratio = normalized_dark_ratio(sample, airlight_luma);
            dark_ratio = min(dark_ratio, sample_dark_ratio);
            let luminance = Common::safe_luma(sample);
            if sample_dark_ratio > haziest_ratio
                || (abs(sample_dark_ratio - haziest_ratio) < 1e-5
                    && luminance > brightest_luma) {
                brightest = sample;
                brightest_luma = luminance;
                haziest_ratio = sample_dark_ratio;
            }
        }
    }
    return HazeNeighborhood(dark_ratio, brightest, brightest_luma);
}

fn apply_dehaze_value(pos: vec2<i32>, rgb: vec3<f32>, value: f32) -> vec3<f32> {
    let amount = BasicAdjustments::perceptual_control(value);
    if abs(amount) < 1e-6 {
        return rgb;
    }

    let center_lum = Common::safe_luma(rgb);
    let center_ev = log2(center_lum);
    let broad_ev = SceneAdjustments::bilateral_log_luminance(
        pos,
        2,
        SceneAdjustments::presence_step(1.0, 3),
        0.95,
    );
    let ambient_ev = clamp(Tonemap::tone_stats.percentiles_1_field.x + Common::scene_tone_uniforms.exposure, -16.0, 16.0);
    let airlight_luma = max(ToneCommon::SCENE_MIDDLE_GREY * exp2(ambient_ev), 1e-5);
    let haze_step = SceneAdjustments::presence_step(2.0, 6);
    let neighborhood = haze_neighborhood(pos, haze_step, airlight_luma);

    let airlight_colour = neighborhood.airlight
        / max(neighborhood.airlight_luma, 1e-6) * airlight_luma;
    let airlight = max(
        mix(vec3<f32>(airlight_luma), airlight_colour, 0.14),
        vec3<f32>(airlight_luma * 0.28),
    );
    let dark_ratio = neighborhood.dark_ratio;
    let veil = smoothstep(0.025, 0.78, dark_ratio);
    let low_contrast = 1.0 - smoothstep(0.10, 0.72, abs(center_ev - broad_ev));
    let haze_likelihood = clamp(veil * (0.52 + 0.48 * low_contrast), 0.0, 1.0);

    if amount > 0.0 {
        let ambient_position = clamp(center_lum / max(airlight_luma, 1e-6), 0.0, 1.0);
        let shaped_position = pow(ambient_position, 0.33);
        let mid_position_hump = 0.30 * shaped_position * (1.0 - shaped_position);
        let tone_mask = min(
            1.0,
            1.0 - ToneCommon::tone_smoothstep(0.0, 1.0, shaped_position) + mid_position_hump,
        );
        let transmission = 1.0 - amount * mix(0.008, 0.012, haze_likelihood);
        let physical = (rgb - airlight * (1.0 - transmission)) / transmission;
        let physical_lum = Common::safe_luma(physical);
        let luminance_gain = clamp(physical_lum / max(center_lum, 1e-6), 0.0, 2.0);
        let hue_safe = rgb * luminance_gain;
        var restored = mix(hue_safe, physical, 0.30 + 0.16 * haze_likelihood);
        restored = restored * exp2(-amount * 0.90 * tone_mask);

        let local_detail = clamp(center_ev - broad_ev, -1.2, 1.2);
        restored = restored * exp2(amount * local_detail * 0.12);
        let lab = Color::linear_srgb_to_oklab(Common::REC2020_TO_SRGB * restored);
        let chroma = length(lab.yz);
        let content_saturation = clamp(chroma / max(0.045 + 0.38 * lab.x, 0.06), 0.0, 1.0);
        let chroma_boost = 1.0
            + amount * (0.30 + 0.22 * tone_mask)
                * (1.0 - 0.10 * content_saturation);
        return Color::perceptual_gamut_compress_nonnegative_rec2020(
            Common::SRGB_TO_REC2020 * Color::oklab_to_linear_srgb(
                vec3<f32>(lab.x, lab.yz * chroma_boost),
            ),
        );
    }

    let haze = -amount;
    let ambient_position = clamp(center_lum / max(airlight_luma, 1e-6), 0.0, 1.0);
    let position_weight = pow(ambient_position, 0.35);
    let haze_mix = clamp(haze * mix(0.045, 0.23, position_weight), 0.0, 0.30);
    let hazed = mix(rgb, airlight, haze_mix);
    let lab = Color::linear_srgb_to_oklab(Common::REC2020_TO_SRGB * hazed);
    let desaturation = 1.0 - haze * mix(0.32, 0.27, haze_likelihood);
    return Color::perceptual_gamut_compress_nonnegative_rec2020(
        Common::SRGB_TO_REC2020 * Color::oklab_to_linear_srgb(
            vec3<f32>(lab.x, lab.yz * desaturation),
        ),
    );
}

fn extended_perceptual_luminance(linear_luma: f32) -> f32 {
    if linear_luma <= 1.0 {
        return pow(max(linear_luma, 0.0), 1.0 / 2.2);
    }
    return 1.0 + (1.0 / 2.2) * log(linear_luma);
}

fn glow_emission(rgb: vec3<f32>, cutoff: f32) -> vec3<f32> {
    let glow_rgb = Color::gamut_project_nonnegative_rec2020(rgb);
    let linear_luma = Common::safe_luma(glow_rgb);
    let perceptual_luma = extended_perceptual_luminance(linear_luma);
    let cutoff_fade = smoothstep(cutoff, cutoff + 0.16, perceptual_luma);
    let excess = max(perceptual_luma - cutoff, 0.0);
    let range = max(2.25 - cutoff, 0.25);
    let intensity = pow(smoothstep(0.0, range, excess), 0.48);
    let black_gate = pow(smoothstep(0.0, 0.42, linear_luma), 0.5);

    let colour_ratio = clamp(glow_rgb / max(linear_luma, 1e-6), vec3<f32>(0.0), vec3<f32>(3.5));
    let warm_tint = vec3<f32>(1.025, 1.0, 0.975);
    return colour_ratio * warm_tint
        * intensity * pow(linear_luma, 0.62) * cutoff_fade * black_gate;
}

fn glow_cutoff() -> f32 {
    let threshold = clamp(Common::effects_uniforms.creative_effects.z / 100.0, 0.0, 1.0);
    return mix(0.06, 0.92, pow(threshold, 1.12));
}

fn glow_work_at(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(SceneAdjustments::glow_work_tex, Common::clamp_pos(pos), 0).xyz;
}

fn glow_stage_step(stage: u32) -> i32 {
    // The 96-pixel maximum support is mirrored by GLOW_SUPPORT in processing.rs.
    var reference_step = 1.0;
    switch stage {
        case 2u: { reference_step = 2.0; }
        case 3u: { reference_step = 4.0; }
        case 4u: { reference_step = 8.0; }
        default: {}
    }
    let scale = clamp(
        f32(min(Common::camera_uniforms.full_width, Common::camera_uniforms.full_height)) / 1080.0,
        0.45,
        3.0,
    );
    return max(i32(round(reference_step * scale)), 1);
}

fn glow_stage_mix(stage: u32) -> f32 {
    let radius = clamp(Common::effects_uniforms.creative_effects.y / 100.0, 0.0, 1.0);
    switch stage {
        case 0u: { return 1.0; }
        case 1u: { return smoothstep(0.0, 0.20, radius); }
        case 2u: { return smoothstep(0.15, 0.45, radius); }
        case 3u: { return smoothstep(0.40, 0.75, radius); }
        default: { return smoothstep(0.70, 1.0, radius); }
    }
}

fn glow_diffuse_at(pos: vec2<i32>, stage: u32) -> vec3<f32> {
    let center = glow_work_at(pos);
    let stage_mix = glow_stage_mix(stage);
    if stage_mix < 1e-6 {
        return center;
    }

    let step = glow_stage_step(stage);
    var sum = vec3<f32>(0.0);
    var sum_weight = 0.0;
    for (var ky = -2; ky <= 2; ky = ky + 1) {
        for (var kx = -2; kx <= 2; kx = kx + 1) {
            let weight = SceneAdjustments::atrous_kernel_weight(kx) * SceneAdjustments::atrous_kernel_weight(ky);
            let sample_pos = pos + vec2<i32>(kx * step, ky * step);
            sum = sum + glow_work_at(sample_pos) * weight;
            sum_weight = sum_weight + weight;
        }
    }
    return mix(center, sum / max(sum_weight, 1e-6), stage_mix);
}

fn apply_glow(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let global_amount = clamp(Common::effects_uniforms.creative_effects.x / 100.0, 0.0, 1.0);
    if global_amount < 1e-6 && !mask_glow_active() {
        return rgb;
    }

    let bloom = glow_work_at(pos);

    let current_luma = Common::safe_luma(rgb);
    let core_protection = 1.0 - 0.72 * smoothstep(1.0, 3.2, current_luma);
    return rgb + bloom * 2.8 * core_protection;
}

fn full_image_uv(pos: vec2<i32>) -> vec2<f32> {
    let dimensions = max(
        vec2<f32>(f32(Common::camera_uniforms.full_width), f32(Common::camera_uniforms.full_height)),
        vec2<f32>(1.0),
    );
    let global_pos = clamp(pos + Common::tile_origin(), vec2<i32>(0), Common::full_image_max());
    return (vec2<f32>(global_pos) + vec2<f32>(0.5)) / dimensions;
}

fn vignette_distance(pos: vec2<i32>, roundness: f32) -> f32 {
    let dimensions = max(Common::effects_uniforms.vignette_frame.zw, vec2<f32>(1.0));
    let source_delta = full_image_uv(pos) - Common::effects_uniforms.vignette_frame.xy;
    let transform = Common::effects_uniforms.vignette_transform;
    let frame_uv = vec2<f32>(
        0.5 + transform.x * source_delta.x + transform.y * source_delta.y,
        0.5 + transform.z * source_delta.x + transform.w * source_delta.y,
    );
    let p = abs(frame_uv * 2.0 - vec2<f32>(1.0));
    let frame_ellipse = length(p);
    let frame_rectangle = pow(pow(p.x, 8.0) + pow(p.y, 8.0), 1.0 / 8.0);
    let short_dimension = max(min(dimensions.x, dimensions.y), 1.0);
    let image_circle = length(vec2<f32>(
        p.x * dimensions.x / short_dimension,
        p.y * dimensions.y / short_dimension,
    ));

    if abs(roundness) < 1e-6 {
        return frame_ellipse;
    }
    if roundness < 0.0 {
        return mix(frame_ellipse, frame_rectangle, -roundness);
    }
    return mix(frame_ellipse, image_circle, roundness);
}

fn calibrated_vignette_anchor(
    distance: f32,
    start: f32,
    end: f32,
    power: f32,
    edge_opacity: f32,
) -> f32 {
    return edge_opacity * pow(smoothstep(start, end, distance), power);
}

fn calibrated_vignette_opacity(
    distance: f32,
    amount: f32,
    midpoint: f32,
    feather: f32,
) -> f32 {
    let magnitude = clamp(abs(amount), 0.0, 1.0);
    let dark_half_fit = Common::effects_uniforms.vignette_dark_half_fit;
    let dark_full_fit = Common::effects_uniforms.vignette_dark_full_fit;
    let light_half_fit = Common::effects_uniforms.vignette_light_half_fit;
    let light_full_fit = Common::effects_uniforms.vignette_light_full_fit;
    var shaped_distance = distance;
    if abs(midpoint - 0.5) >= 1e-6 {
        let midpoint_power = exp2((midpoint - 0.5) * 1.4);
        let corner_distance = sqrt(2.0);
        shaped_distance = corner_distance
            * pow(max(distance / corner_distance, 0.0), midpoint_power);
    }
    var half_amount = 0.0;
    var full_amount = 0.0;

    if amount < 0.0 {
        half_amount = calibrated_vignette_anchor(
            shaped_distance,
            dark_half_fit.x,
            dark_half_fit.y,
            dark_half_fit.z,
            dark_half_fit.w
        );
        full_amount = calibrated_vignette_anchor(
            shaped_distance,
            dark_full_fit.x,
            dark_full_fit.y,
            dark_full_fit.z,
            dark_full_fit.w
        );
    } else {
        half_amount = calibrated_vignette_anchor(
            shaped_distance,
            light_half_fit.x,
            light_half_fit.y,
            light_half_fit.z,
            light_half_fit.w
        );
        full_amount = calibrated_vignette_anchor(
            shaped_distance,
            light_full_fit.x,
            light_full_fit.y,
            light_full_fit.z,
            light_full_fit.w
        );
    }

    var opacity = 0.0;
    if magnitude <= 0.5 {
        opacity = half_amount * (magnitude * 2.0);
    } else {
        opacity = mix(half_amount, full_amount, (magnitude - 0.5) * 2.0);
    }
    if abs(feather - 0.5) < 1e-6 {
        return opacity;
    }
    let feather_power = exp2((0.5 - feather) * 1.3);
    return pow(clamp(opacity, 0.0, 1.0), feather_power);
}

fn apply_vignette(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = clamp(Common::effects_uniforms.vignette.x / 100.0, -1.0, 1.0);
    if abs(amount) < 1e-6 {
        return rgb;
    }

    let midpoint = clamp(Common::effects_uniforms.vignette.y / 100.0, 0.0, 1.0);
    let roundness = clamp(Common::effects_uniforms.vignette.z / 100.0, -1.0, 1.0);
    let feather = clamp(Common::effects_uniforms.vignette.w / 100.0, 0.0, 1.0);
    var opacity = calibrated_vignette_opacity(
        vignette_distance(pos, roundness),
        amount,
        midpoint,
        feather,
    );
    if amount < 0.0 {
        let highlights = clamp(Common::effects_uniforms.vignette_options.x / 100.0, 0.0, 1.0);
        let highlight_protection = 1.0
            - highlights * smoothstep(0.35, 1.0, Common::safe_luma(rgb));
        opacity = opacity * highlight_protection;
        return rgb * (1.0 - opacity);
    }
    return mix(rgb, vec3<f32>(1.0), opacity);
}

fn apply_local_scene_effect_nodes(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u || Common::mask_effect_id(state) != 0u { continue; }
        let local = Common::mask_data[index].adjust_2_field;
        if max(max(abs(local.x), abs(local.y)), max(abs(local.z), abs(local.w))) <= 1e-7 {
            continue;
        }
        let weight = SceneAdjustments::local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }
        var adjusted = rgb;
        adjusted = DetailScaleSpace::apply_texture_and_clarity_values(pos, adjusted, local.y, local.z);
        adjusted = apply_dehaze_value(pos, adjusted, local.w);
        adjusted = BasicAdjustments::apply_saturation_value(adjusted, local.x);
        rgb = mix(rgb, adjusted, weight);
    }
    return rgb;
}

fn apply_local_mask_effect_nodes(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u || Common::mask_effect_id(state) != MASK_EFFECT_NEON_ID { continue; }
        let weight = SceneAdjustments::local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }
        let adjusted = apply_neon(
            pos,
            rgb,
            Common::mask_data[index].adjust_0_field,
            Common::mask_data[index].adjust_1_field,
        );
        rgb = mix(rgb, adjusted, weight);
    }
    return rgb;
}

fn apply_local_creative_mask_effect_nodes(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u { continue; }
        let effect_id = Common::mask_effect_id(state);
        if effect_id != MASK_EFFECT_EDGE_GLOW_ID
            && effect_id != MASK_EFFECT_PIXELATE_ID
            && effect_id != MASK_EFFECT_FOG_ID
            && effect_id != MASK_EFFECT_SMOKE_ID {
            continue;
        }
        let weight = SceneAdjustments::local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }

        let primary = Common::mask_data[index].adjust_0_field;
        let secondary = Common::mask_data[index].adjust_1_field;
        var adjusted = rgb;
        if effect_id == MASK_EFFECT_EDGE_GLOW_ID {
            adjusted = apply_edge_glow(pos, rgb, primary, secondary);
        } else if effect_id == MASK_EFFECT_PIXELATE_ID {
            adjusted = apply_pixelate(pos, rgb, primary);
        } else if effect_id == MASK_EFFECT_FOG_ID {
            adjusted = apply_fog(pos, rgb, primary, secondary, Common::mask_data[index].adjust_2_field);
        } else if effect_id == MASK_EFFECT_SMOKE_ID {
            adjusted = apply_smoke(pos, rgb, primary, secondary, Common::mask_data[index].adjust_2_field);
        }
        rgb = mix(rgb, adjusted, weight);
    }
    return rgb;
}

@compute @workgroup_size(8, 8, 1)
fn apply_scene_effects_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = SceneAdjustments::adjustment_base_at(pos);
    rgb = DetailScaleSpace::apply_texture_and_clarity_values(pos, rgb, Common::effects_uniforms.presence.x, Common::effects_uniforms.presence.y);
    rgb = apply_dehaze_value(pos, rgb, Common::effects_uniforms.presence.z);
    rgb = BasicAdjustments::apply_saturation_vibrance(rgb);
    rgb = apply_local_scene_effect_nodes(pos, rgb);
    rgb = apply_local_mask_effect_nodes(pos, rgb);
    textureStore(SceneAdjustments::local_effects_out, pos, vec4<f32>(rgb, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn copy_scene_effects_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(SceneAdjustments::local_effects_out, pos, vec4<f32>(SceneAdjustments::adjustment_base_at(pos), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn prepare_glow_source(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let global_amount = clamp(Common::effects_uniforms.creative_effects.x / 100.0, 0.0, 1.0);
    let emission = glow_emission(SceneAdjustments::local_effects_at(pos), glow_cutoff())
        * global_amount
        + mask_glow_source_at(pos);
    textureStore(SceneAdjustments::glow_work_out, pos, vec4<f32>(emission, 1.0));
}

fn store_glow_stage(gid: vec3<u32>, stage: u32) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(SceneAdjustments::glow_work_out, pos, vec4<f32>(glow_diffuse_at(pos, stage), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_0(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 0u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_1(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 1u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_2(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 2u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_3(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 3u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_4(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 4u);
}

@compute @workgroup_size(8, 8, 1)
fn apply_creative_effects(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = SceneAdjustments::local_effects_at(pos);
    rgb = apply_local_creative_mask_effect_nodes(pos, rgb);
    rgb = apply_glow(pos, rgb);
    rgb = apply_mask_glow_cores(pos, rgb);
    rgb = apply_light_rays(pos, rgb);
    textureStore(SceneAdjustments::creative_effects_out, pos, vec4<f32>(rgb, 1.0));
}
