// Shared post-demosaic noise reduction and lateral chromatic-aberration
// correction. Each finishing shader supplies finish_reference_at() so the
// algorithms stay identical while sampling their native reference texture.

fn finish_warped_pos(pos: vec2<i32>, amount: f32) -> vec2<f32> {
    let local_extent = vec2<f32>(f32(params.width - 1u), f32(params.height - 1u));
    let origin = vec2<f32>(f32(params.tile_origin_x), f32(params.tile_origin_y));
    let full_extent = vec2<f32>(f32(params.full_width - 1u), f32(params.full_height - 1u));
    let center = 0.5 * full_extent;
    let global_pos = vec2<f32>(pos) + origin;
    let rel = global_pos - center;
    let norm = rel / max(center, vec2<f32>(1.0));
    let scale = 1.0 + amount * 0.001 * dot(norm, norm);
    let warped_global = clamp(center + rel * scale, vec2<f32>(0.0), full_extent);
    return clamp(warped_global - origin, vec2<f32>(0.0), local_extent);
}

fn finish_reference_bilinear(pos: vec2<f32>) -> vec3<f32> {
    let base = floor(pos);
    let p0 = vec2<i32>(i32(base.x), i32(base.y));
    let p1 = p0 + vec2<i32>(1, 1);
    let f = fract(pos);
    let a = finish_reference_at(p0);
    let b = finish_reference_at(vec2<i32>(p1.x, p0.y));
    let c = finish_reference_at(vec2<i32>(p0.x, p1.y));
    let d = finish_reference_at(p1);
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

fn finish_apply_ca(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    var out = rgb;
    if abs(params.ca_red) > 1e-6 {
        out.r = finish_reference_bilinear(finish_warped_pos(pos, params.ca_red)).r;
    }
    if abs(params.ca_blue) > 1e-6 {
        out.b = finish_reference_bilinear(finish_warped_pos(pos, params.ca_blue)).b;
    }
    return out;
}

fn finish_apply_legacy_chroma_denoise(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let strength = clamp(params.chroma_denoise, 0.0, 1.0);
    if strength <= 1e-6 { return rgb; }
    var sum = vec2<f32>(0.0);
    var weight_sum = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let sample = finish_reference_at(pos + vec2<i32>(dx, dy));
            let spatial = 1.0 / (1.0 + f32(dx * dx + dy * dy));
            let range = 1.0 / (1.0 + 24.0 * abs(sample.g - rgb.g));
            let weight = spatial * range;
            sum += vec2<f32>(sample.r - sample.g, sample.b - sample.g) * weight;
            weight_sum += weight;
        }
    }
    let center = vec2<f32>(rgb.r - rgb.g, rgb.b - rgb.g);
    let chroma = mix(center, sum / max(weight_sum, 1e-6), strength);
    return vec3<f32>(rgb.g + chroma.x, rgb.g, rgb.g + chroma.y);
}

fn finish_apply_sensor_denoise(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let signal_strength = clamp(params.noise_options.x, 0.0, 1.0);
    let chroma_strength = clamp(params.chroma_denoise, 0.0, 1.0);
    if signal_strength <= 1e-6 && chroma_strength <= 1e-6 { return rgb; }

    let center_signal = nr_signal(rgb);
    let center_opponents = nr_opponents(rgb);
    let center_variance = nr_component_variance(rgb);
    var signal_sum = center_signal;
    var signal_weights = 1.0;
    var opponent_sum = center_opponents;
    var opponent_weights = 1.0;

    let scale_count = nr_scale_count();
    for (var scale = 0; scale < 3; scale = scale + 1) {
        if scale >= scale_count { break; }
        let radius = nr_scale_radius(scale);
        for (var direction_index = 0; direction_index < 8; direction_index = direction_index + 1) {
            let direction = NR_DIRECTIONS[direction_index];
            let sample = finish_reference_at(pos + direction * radius);
            let spatial = nr_scale_spatial_weight(radius, direction);
            let sample_signal = nr_signal(sample);
            let sample_opponents = nr_opponents(sample);
            let sample_variance = nr_component_variance(sample);
            let range_weights = nr_range_weights(
                center_signal,
                center_opponents,
                center_variance,
                sample_signal,
                sample_opponents,
                sample_variance,
                spatial,
            );
            signal_sum += sample_signal * range_weights.x;
            signal_weights += range_weights.x;
            opponent_sum += sample_opponents * range_weights.y;
            opponent_weights += range_weights.y;
        }
    }
    return nr_finish(rgb, signal_sum, signal_weights, opponent_sum, opponent_weights);
}
