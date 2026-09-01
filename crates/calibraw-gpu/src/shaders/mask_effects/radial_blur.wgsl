
const MASK_RADIAL_BLUR_SAMPLE_COUNT: u32 = 25u;

fn mask_radial_blur_at(
    pos: vec2<i32>,
    primary: vec4<f32>,
    secondary: vec4<f32>,
) -> vec3<f32> {
    let full_size = vec2<f32>(
        f32(max(Common::camera_uniforms.full_width, 1u)),
        f32(max(Common::camera_uniforms.full_height, 1u)),
    );
    let center = primary.zw / 100.0 * full_size;
    let global_pos = vec2<f32>(pos + Common::tile_origin()) + vec2<f32>(0.5);
    let delta = global_pos - center;
    let center_distance = length(delta);
    let radial_direction = delta / max(center_distance, 1e-4);
    let direction = select(
        radial_direction,
        vec2<f32>(-radial_direction.y, radial_direction.x),
        secondary.x >= 0.5,
    );
    let edge_factor = clamp(center_distance / max(length(full_size) * 0.5, 1.0), 0.0, 1.0);
    let extent = f32(SceneAdjustments::presence_step(primary.y, 288)) * edge_factor;
    var sum = vec3<f32>(0.0);
    var total_weight = 0.0;

    for (var index = 0u; index < MASK_RADIAL_BLUR_SAMPLE_COUNT; index = index + 1u) {
        let unit = f32(index) / f32(MASK_RADIAL_BLUR_SAMPLE_COUNT - 1u) - 0.5;
        let offset = direction * (unit * extent);
        let weight = 0.65 + 0.35 * (1.0 - abs(unit) * 2.0);
        sum = sum + SceneAdjustments::local_effects_at(
            pos + vec2<i32>(round(offset)),
        ) * weight;
        total_weight = total_weight + weight;
    }
    return sum / max(total_weight, 1e-6);
}
