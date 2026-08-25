
const LIGHT_RAY_MIN_TAP_COUNT: u32 = 16u;
const LIGHT_RAY_MAX_TAP_COUNT: u32 = 40u;
const LIGHT_RAY_PI: f32 = 3.141592653589793;

fn light_ray_emission_at(uv: vec2<f32>, mask_index: u32) -> f32 {
    if any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) {
        return 0.0;
    }
    let atlas_size = vec2<f32>(max(
        textureDimensions(SceneAdjustments::light_rays_mask_tex),
        vec2<u32>(1u),
    ));
    let half_texel = vec2<f32>(0.5) / atlas_size;
    return textureSampleLevel(
        SceneAdjustments::light_rays_mask_tex,
        SceneAdjustments::local_mask_sampler,
        clamp(uv, half_texel, vec2<f32>(1.0) - half_texel),
        i32(mask_index),
        0.0,
    ).x;
}

fn light_ray_angular_pattern(
    angle: f32,
    ray_count: f32,
    variation: f32,
    softness: f32,
) -> f32 {
    let primary_frequency = floor(clamp(ray_count, 4.0, 96.0) + 0.5);
    let secondary_frequency = max(floor(primary_frequency * 0.53 + 0.5), 2.0);
    let detail_frequency = primary_frequency * 2.0 + 3.0;
    let wave = sin(angle * primary_frequency + 0.73) * 0.55
        + sin(angle * secondary_frequency + 2.11) * 0.30
        + sin(angle * detail_frequency + 4.37) * 0.15;
    let ridge = pow(
        clamp(0.5 + 0.5 * wave, 0.0, 1.0),
        mix(7.0, 0.72, clamp(softness, 0.0, 1.0)),
    );
    return mix(1.0, 0.20 + 1.80 * ridge, clamp(variation, 0.0, 1.0));
}

fn light_ray_path_energy(
    output_uv: vec2<f32>,
    source_uv: vec2<f32>,
    mask_index: u32,
    spread: f32,
    softness: f32,
) -> f32 {
    let full_size = max(
        vec2<f32>(
            f32(Common::camera_uniforms.full_width),
            f32(Common::camera_uniforms.full_height),
        ),
        vec2<f32>(1.0),
    );
    let radial_pixels = (output_uv - source_uv) * full_size;
    let radial_length = length(radial_pixels);
    let normalized_length = radial_length / max(min(full_size.x, full_size.y), 1.0);
    let tap_count = u32(clamp(
        ceil(f32(LIGHT_RAY_MIN_TAP_COUNT) + normalized_length * 16.0),
        f32(LIGHT_RAY_MIN_TAP_COUNT),
        f32(LIGHT_RAY_MAX_TAP_COUNT),
    ));
    var perpendicular = vec2<f32>(0.0, 1.0);
    if radial_length > 1e-5 {
        perpendicular = vec2<f32>(-radial_pixels.y, radial_pixels.x) / radial_length;
    }
    let cone_slope = tan(radians(clamp(spread, 0.0, 45.0) * 0.5));
    let side_mix = mix(0.08, 0.46, clamp(softness, 0.0, 1.0));

    var energy = 0.0;
    for (var tap = 0u; tap < LIGHT_RAY_MAX_TAP_COUNT; tap = tap + 1u) {
        if tap >= tap_count {
            break;
        }
        let progress = (f32(tap) + 0.5) / f32(tap_count);
        let base_uv = mix(output_uv, source_uv, progress);
        let remaining_radius = radial_length * (1.0 - progress);
        let bow = sin(LIGHT_RAY_PI * progress);
        let side_uv_offset = perpendicular
            * (remaining_radius * cone_slope * bow * 0.42) / full_size;
        let center = light_ray_emission_at(base_uv, mask_index);
        let sides = 0.5 * (
            light_ray_emission_at(base_uv - side_uv_offset, mask_index)
            + light_ray_emission_at(base_uv + side_uv_offset, mask_index)
        );
        energy = energy + mix(center, sides, side_mix);
    }
    return energy / f32(tap_count);
}

fn apply_light_rays(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u
            || Common::mask_effect_id(state) != MASK_EFFECT_LIGHT_RAYS_ID {
            continue;
        }

        let primary = Common::mask_data[index].adjust_0_field;
        let secondary = Common::mask_data[index].adjust_1_field;
        let tertiary = Common::mask_data[index].adjust_2_field;
        let amount = clamp(primary.x / 100.0, 0.0, 1.0);
        let maximum_length = clamp(primary.y / 100.0, 0.0, 2.0);
        if amount <= 1e-6 || maximum_length <= 1e-6 {
            continue;
        }

        let output_uv = full_image_uv(pos);
        let source_uv = primary.zw / 100.0;
        let full_size = max(
            vec2<f32>(
                f32(Common::camera_uniforms.full_width),
                f32(Common::camera_uniforms.full_height),
            ),
            vec2<f32>(1.0),
        );
        let radial_pixels = (output_uv - source_uv) * full_size;
        let short_edge = max(min(full_size.x, full_size.y), 1.0);
        let radial_distance = length(radial_pixels) / short_edge;
        if radial_distance >= maximum_length {
            continue;
        }

        let fade = clamp(secondary.w / 100.0, 0.0, 1.0);
        let softness = clamp(tertiary.w / 100.0, 0.0, 1.0);
        let gathered = light_ray_path_energy(
            output_uv,
            source_uv,
            index,
            tertiary.x,
            softness,
        );
        let reach = max(1.0 - radial_distance / maximum_length, 0.0);
        let distance_falloff = pow(reach, mix(0.35, 2.8, fade));
        let angle = atan2(radial_pixels.y, radial_pixels.x);
        let angular_pattern = light_ray_angular_pattern(
            angle,
            tertiary.y,
            tertiary.z / 100.0,
            softness,
        );
        let shaft = (1.0 - exp(-gathered * 7.0))
            * distance_falloff * angular_pattern;
        let color = mask_effect_picker_color_to_working(secondary.xyz);
        rgb = rgb + color * shaft * amount * 1.8;
    }
    return rgb;
}
