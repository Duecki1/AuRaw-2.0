// Non-destructive mask Glow helpers. The mask is sampled only while building
// the emission source and its hot core. The shared creative Glow pass diffuses
// that source and composites the result without masking it again, allowing the
// halo to spread naturally into surrounding pixels.

fn mask_glow_active() -> bool {
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x != 0u && state.y != 0u && Common::mask_effect_id(state) == MASK_EFFECT_GLOW_ID {
            return true;
        }
    }
    return false;
}

fn mask_glow_source_at(pos: vec2<i32>) -> vec3<f32> {
    var emission = vec3<f32>(0.0);
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u || Common::mask_effect_id(state) != MASK_EFFECT_GLOW_ID { continue; }
        let weight = SceneAdjustments::local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }

        let amount = clamp(Common::mask_data[index].adjust_0_field.x / 100.0, 0.0, 1.0);
        let color = mask_effect_picker_color_to_working(
            Common::mask_data[index].adjust_1_field.xyz,
        );
        emission = emission + color * weight * amount * 0.8;
    }
    return emission;
}

fn apply_mask_glow_cores(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u || Common::mask_effect_id(state) != MASK_EFFECT_GLOW_ID { continue; }
        let weight = SceneAdjustments::local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }

        let primary = Common::mask_data[index].adjust_0_field;
        let amount = clamp(primary.x / 100.0, 0.0, 1.0);
        let core = clamp(primary.z / 100.0, 0.0, 1.0);
        let color = mask_effect_picker_color_to_working(
            Common::mask_data[index].adjust_1_field.xyz,
        );
        let hot_color = mix(color, vec3<f32>(1.0), smoothstep(0.15, 1.0, core));
        rgb = rgb + hot_color * weight * amount * core * 2.1;
    }
    return rgb;
}
