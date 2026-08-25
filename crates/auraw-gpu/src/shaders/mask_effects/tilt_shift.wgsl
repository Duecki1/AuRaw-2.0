
const MASK_TILT_SHIFT_SAMPLE_COUNT: u32 = 24u;
const MASK_TILT_SHIFT_GOLDEN_ANGLE: f32 = 2.39996323;
const MASK_TILT_SHIFT_PI: f32 = 3.14159265;

fn mask_tilt_shift_weight(
    pos: vec2<i32>,
    primary: vec4<f32>,
    secondary: vec4<f32>,
) -> f32 {
    let full_size = vec2<f32>(
        f32(max(Common::camera_uniforms.full_width, 1u)),
        f32(max(Common::camera_uniforms.full_height, 1u)),
    );
    let short_edge = max(min(full_size.x, full_size.y), 1.0);
    let center = primary.zw / 100.0 * full_size;
    let global_pos = vec2<f32>(pos + Common::tile_origin()) + vec2<f32>(0.5);
    let angle = secondary.x * MASK_TILT_SHIFT_PI / 180.0;
    let normal = vec2<f32>(-sin(angle), cos(angle));
    let distance_percent = abs(dot(global_pos - center, normal)) / short_edge * 100.0;
    let sharp_half_width = secondary.y * 0.5;
    return smoothstep(
        sharp_half_width,
        sharp_half_width + max(secondary.z, 0.1),
        distance_percent,
    );
}

fn mask_tilt_shift_at(pos: vec2<i32>, primary: vec4<f32>) -> vec3<f32> {
    let radius = f32(SceneAdjustments::presence_step(primary.y, 144));
    var sum = vec3<f32>(0.0);
    for (var index = 0u; index < MASK_TILT_SHIFT_SAMPLE_COUNT; index = index + 1u) {
        let unit = (f32(index) + 0.5) / f32(MASK_TILT_SHIFT_SAMPLE_COUNT);
        let angle = f32(index) * MASK_TILT_SHIFT_GOLDEN_ANGLE;
        let offset = vec2<f32>(cos(angle), sin(angle)) * (radius * sqrt(unit));
        sum = sum + SceneAdjustments::local_effects_at(
            pos + vec2<i32>(round(offset)),
        );
    }
    return sum / f32(MASK_TILT_SHIFT_SAMPLE_COUNT);
}
