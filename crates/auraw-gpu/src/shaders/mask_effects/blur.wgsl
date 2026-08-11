// Progressive, scale-aware diffusion for the non-destructive mask Blur
// effect. Each stage reads the previous stage's complete image, so every
// source pixel reaches its neighbors through a continuous B3-spline kernel.
// This avoids the repeated detail and woven pattern produced by sampling a
// handful of isolated points at a large radius.

const MASK_BLUR_STAGE_COUNT: u32 = 5u;

fn mask_blur_stage_step(stage: u32) -> i32 {
    var reference_step = 1.0;
    switch stage {
        case 2u: { reference_step = 2.0; }
        case 3u: { reference_step = 3.0; }
        case 4u: { reference_step = 5.0; }
        default: {}
    }
    return SceneAdjustments::presence_step(reference_step, 15);
}

fn mask_blur_stage_mix(radius: f32, stage: u32) -> f32 {
    let normalized_radius = clamp(radius / 16.0, 0.0, 1.0);
    switch stage {
        case 0u: { return smoothstep(0.0, 0.12, normalized_radius); }
        case 1u: { return smoothstep(0.08, 0.30, normalized_radius); }
        case 2u: { return smoothstep(0.22, 0.50, normalized_radius); }
        case 3u: { return smoothstep(0.42, 0.75, normalized_radius); }
        default: { return smoothstep(0.68, 1.0, normalized_radius); }
    }
}

fn mask_blur_stage_mix_sum(radius: f32) -> f32 {
    var sum = 0.0;
    for (var stage = 0u; stage < MASK_BLUR_STAGE_COUNT; stage = stage + 1u) {
        sum = sum + mask_blur_stage_mix(radius, stage);
    }
    return sum;
}

fn mask_blur_diffused_at(pos: vec2<i32>, stage: u32) -> vec3<f32> {
    let step = mask_blur_stage_step(stage);
    var sum = vec3<f32>(0.0);
    var total_weight = 0.0;
    for (var y = -2; y <= 2; y = y + 1) {
        for (var x = -2; x <= 2; x = x + 1) {
            let weight = SceneAdjustments::atrous_kernel_weight(x)
                * SceneAdjustments::atrous_kernel_weight(y);
            sum = sum + SceneAdjustments::local_effects_at(
                pos + vec2<i32>(x * step, y * step),
            ) * weight;
            total_weight = total_weight + weight;
        }
    }
    return sum / max(total_weight, 1e-6);
}

fn apply_mask_blur_stage(
    pos: vec2<i32>,
    source_rgb: vec3<f32>,
    stage: u32,
) -> vec3<f32> {
    var retained_source = 1.0;
    let count = min(Common::scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = Common::mask_data[index].metadata;
        if state.x == 0u || state.y == 0u
            || Common::mask_effect_id(state) != MASK_EFFECT_BLUR_ID {
            continue;
        }

        let primary = Common::mask_data[index].adjust_0_field;
        let stage_mix = mask_blur_stage_mix(primary.y, stage);
        if stage_mix <= 1e-6 { continue; }
        let coverage = SceneAdjustments::local_mask_weight(pos, index);
        let amount = clamp(primary.x / 100.0, 0.0, 1.0) * coverage;
        if amount <= 1e-6 { continue; }

        // Allocate the requested final Amount across only the active stages.
        // Their retained-source products therefore remain 1 - Amount instead
        // of applying the full slider value five times. Keep a tiny retained
        // fraction at the endpoint so a newly entering radius stage fades in
        // continuously instead of 0^epsilon making it instantly opaque.
        let mix_sum = mask_blur_stage_mix_sum(primary.y);
        let stage_share = stage_mix / max(mix_sum, 1e-6);
        let distributed_amount = min(amount, 0.995);
        let stage_amount = 1.0 - pow(1.0 - distributed_amount, stage_share);
        retained_source = retained_source * (1.0 - stage_amount);
    }
    let combined_amount = 1.0 - retained_source;
    if combined_amount <= 1e-6 {
        return source_rgb;
    }
    return mix(source_rgb, mask_blur_diffused_at(pos, stage), combined_amount);
}

fn store_mask_blur_stage(gid: vec3<u32>, stage: u32) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let source = SceneAdjustments::local_effects_at(pos);
    textureStore(
        SceneAdjustments::creative_effects_out,
        pos,
        vec4<f32>(apply_mask_blur_stage(pos, source, stage), 1.0),
    );
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_mask_blur_0(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_mask_blur_stage(gid, 0u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_mask_blur_1(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_mask_blur_stage(gid, 1u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_mask_blur_2(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_mask_blur_stage(gid, 2u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_mask_blur_3(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_mask_blur_stage(gid, 3u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_mask_blur_4(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_mask_blur_stage(gid, 4u);
}
