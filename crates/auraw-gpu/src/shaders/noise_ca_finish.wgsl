#import auraw::common as Common
#import auraw::noise as Noise

// The CFA-specific finishing shader overrides this adapter. The fallback is
// never reachable in a composed entrypoint, but gives the reusable module a
// complete WGSL signature for independent Naga validation.
virtual fn finish_reference_at(_pos: vec2<i32>) -> vec3<f32> {
    return vec3<f32>(0.0);
}

// Shared post-demosaic noise reduction and lateral chromatic-aberration
// correction. Each finishing shader supplies finish_reference_at() so the
// algorithms stay identical while sampling their native reference texture.

fn finish_warped_pos(pos: vec2<i32>, amount: f32) -> vec2<f32> {
    let local_extent = vec2<f32>(f32(Common::camera_uniforms.width - 1u), f32(Common::camera_uniforms.height - 1u));
    let origin = vec2<f32>(f32(Common::camera_uniforms.tile_origin_x), f32(Common::camera_uniforms.tile_origin_y));
    let full_extent = vec2<f32>(f32(Common::camera_uniforms.full_width - 1u), f32(Common::camera_uniforms.full_height - 1u));
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
    if abs(Common::camera_uniforms.ca_red) > 1e-6 {
        out.r = finish_reference_bilinear(finish_warped_pos(pos, Common::camera_uniforms.ca_red)).r;
    }
    if abs(Common::camera_uniforms.ca_blue) > 1e-6 {
        out.b = finish_reference_bilinear(finish_warped_pos(pos, Common::camera_uniforms.ca_blue)).b;
    }
    return out;
}

fn finish_apply_sensor_denoise(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let signal_strength = Noise::nr_perceptual_strength(Common::camera_uniforms.noise_options.x, 3.2);
    if signal_strength <= 1e-6 { return rgb; }

    let center_signal = Noise::nr_signal(rgb);
    let center_variance = Noise::nr_component_variance(rgb);
    var signal_sum = center_signal;
    var signal_weights = 1.0;

    let scale_count = Noise::nr_scale_count();
    for (var scale = 0; scale < 5; scale = scale + 1) {
        if scale >= scale_count { break; }
        let radius = Noise::nr_scale_radius(scale);
        for (var direction_index = 0; direction_index < 8; direction_index = direction_index + 1) {
            let direction = Noise::NR_DIRECTIONS[direction_index];
            let sample = finish_reference_at(pos + direction * radius);
            let spatial = Noise::nr_scale_spatial_weight(radius, direction);
            let sample_signal = Noise::nr_signal(sample);
            let sample_variance = Noise::nr_component_variance(sample);
            let range_weight = Noise::nr_signal_range_weight(
                center_signal,
                center_variance,
                sample_signal,
                sample_variance,
                spatial,
            );
            signal_sum += sample_signal * range_weight;
            signal_weights += range_weight;
        }
        // Balanced/High fill the missing positions of the 5x5 signal support
        // instead of sampling only axes and diagonals.
        if scale == 1 {
            for (var direction_index = 0; direction_index < 8; direction_index = direction_index + 1) {
                let offset = Noise::NR_KNIGHT_DIRECTIONS[direction_index];
                let sample = finish_reference_at(pos + offset);
                let spatial = Noise::nr_offset_spatial_weight(offset);
                let sample_signal = Noise::nr_signal(sample);
                let sample_variance = Noise::nr_component_variance(sample);
                let range_weight = Noise::nr_signal_range_weight(
                    center_signal,
                    center_variance,
                    sample_signal,
                    sample_variance,
                    spatial,
                );
                signal_sum += sample_signal * range_weight;
                signal_weights += range_weight;
            }
        }
    }
    return Noise::nr_finish_signal(rgb, signal_sum, signal_weights);
}
