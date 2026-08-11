// A scale-aware Gaussian approximation for the non-destructive mask Blur
// effect. It samples the completed local-effects image and returns a temporary
// result; mask coverage performs the final blend in the creative pass.

fn mask_blur_kernel_weight(offset: i32) -> f32 {
    let distance = abs(offset);
    if distance == 0 { return 6.0; }
    if distance == 1 { return 4.0; }
    return 1.0;
}

fn mask_blurred_source_at(pos: vec2<i32>, radius: i32) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    var total_weight = 0.0;
    for (var y = -2; y <= 2; y = y + 1) {
        for (var x = -2; x <= 2; x = x + 1) {
            let weight = mask_blur_kernel_weight(x) * mask_blur_kernel_weight(y);
            let offset = vec2<i32>(
                i32(round(f32(x * radius) * 0.5)),
                i32(round(f32(y * radius) * 0.5)),
            );
            sum = sum + SceneAdjustments::local_effects_at(pos + offset) * weight;
            total_weight = total_weight + weight;
        }
    }
    return sum / max(total_weight, 1e-6);
}

fn apply_mask_blur(
    pos: vec2<i32>,
    source_rgb: vec3<f32>,
    primary: vec4<f32>,
) -> vec3<f32> {
    let amount = clamp(primary.x / 100.0, 0.0, 1.0);
    if amount <= 1e-6 || primary.y <= 1e-6 {
        return source_rgb;
    }
    let radius = SceneAdjustments::presence_step(primary.y, 48);
    return mix(source_rgb, mask_blurred_source_at(pos, radius), amount);
}
