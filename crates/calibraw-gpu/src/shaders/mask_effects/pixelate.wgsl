
fn mask_pixelated_source_at(pos: vec2<i32>, reference_block_size: f32) -> vec3<f32> {
    let block_size = SceneAdjustments::presence_step(reference_block_size, 96);
    let global_pos = clamp(
        pos + Common::tile_origin(),
        vec2<i32>(0),
        Common::full_image_max(),
    );
    let cell_min = (global_pos / vec2<i32>(block_size)) * vec2<i32>(block_size);
    var sum = vec3<f32>(0.0);
    for (var y = 0; y < 3; y = y + 1) {
        for (var x = 0; x < 3; x = x + 1) {
            let sample_global = cell_min + vec2<i32>(
                ((2 * x + 1) * block_size) / 6,
                ((2 * y + 1) * block_size) / 6,
            );
            let sample_pos = sample_global - Common::tile_origin();
            sum = sum + SceneAdjustments::local_effects_at(sample_pos);
        }
    }
    return sum / 9.0;
}

fn apply_pixelate(
    pos: vec2<i32>,
    source_rgb: vec3<f32>,
    primary: vec4<f32>,
) -> vec3<f32> {
    let amount = clamp(primary.x / 100.0, 0.0, 1.0);
    if amount <= 1e-6 || primary.y <= 1.0 {
        return source_rgb;
    }
    let pixelated = mask_pixelated_source_at(pos, primary.y);
    return mix(source_rgb, pixelated, amount);
}
