
const MASK_MOTION_BLUR_SAMPLE_COUNT: u32 = 25u;
const MASK_MOTION_BLUR_PI: f32 = 3.14159265;

fn mask_motion_blur_at(pos: vec2<i32>, primary: vec4<f32>) -> vec3<f32> {
    let distance = f32(SceneAdjustments::presence_step(primary.y, 288));
    let angle = primary.z * MASK_MOTION_BLUR_PI / 180.0;
    let direction = vec2<f32>(cos(angle), sin(angle));
    var sum = vec3<f32>(0.0);
    var total_weight = 0.0;

    for (var index = 0u; index < MASK_MOTION_BLUR_SAMPLE_COUNT; index = index + 1u) {
        let unit = f32(index) / f32(MASK_MOTION_BLUR_SAMPLE_COUNT - 1u) - 0.5;
        let offset = direction * (unit * distance);
        let weight = 0.65 + 0.35 * (1.0 - abs(unit) * 2.0);
        sum = sum + SceneAdjustments::local_effects_at(
            pos + vec2<i32>(round(offset)),
        ) * weight;
        total_weight = total_weight + weight;
    }
    return sum / max(total_weight, 1e-6);
}
