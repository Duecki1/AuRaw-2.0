#import calibraw::common as Common
#import calibraw::noise as Noise


@group(0) @binding(11) var color_denoise_read: texture_2d<f32>;
@group(0) @binding(10) var color_denoise_write: texture_storage_2d<rgba16float /* CALIBRAW_WORK_FORMAT */, write>;

fn color_denoise_at(pos: vec2<i32>) -> vec3<f32> {
    let maximum = vec2<i32>(i32(Common::camera_uniforms.width) - 1, i32(Common::camera_uniforms.height) - 1);
    return textureLoad(color_denoise_read, clamp(pos, vec2<i32>(0), maximum), 0).rgb;
}

fn color_denoise_kernel_weight(index: i32, compact: bool) -> f32 {
    let absolute = abs(index);
    if compact {
        if absolute == 0 { return 2.0; }
        return 1.0;
    }
    if absolute == 0 { return 6.0; }
    if absolute == 1 { return 4.0; }
    return 1.0;
}

fn color_denoise_scale_gain(scale: i32) -> f32 {
    if scale == 0 { return 1.80; }
    if scale == 1 { return 1.35; }
    if scale == 2 { return 1.00; }
    if scale == 3 { return 0.75; }
    if scale == 4 { return 0.55; }
    return 0.40;
}

fn color_denoise_enabled(scale: i32) -> bool {
    if Common::camera_uniforms.chroma_denoise <= 1e-6 { return false; }
    let quality = Common::camera_uniforms.noise_options.z;
    if quality < 0.5 { return scale == 0; }
    if quality < 1.5 { return scale <= 3; }
    return true;
}

fn color_denoise_apply(pos: vec2<i32>, radius: i32, scale: i32) -> vec3<f32> {
    let center = color_denoise_at(pos);
    if !color_denoise_enabled(scale) { return center; }

    let center_signal = Noise::nr_signal(center);
    let center_variance = Noise::nr_component_variance(center);
    let detail = clamp(Common::camera_uniforms.noise_options.y, 0.0, 1.0);
    let requested = clamp(Common::camera_uniforms.chroma_denoise, 0.0, 1.0);
    let guide_variance_scale = exp2(-f32(scale));
    let signal_guide_sigma = mix(10.0, 5.5, detail);
    let opponent_noise_deadzone = mix(12.0, 6.0, detail);
    let opponent_edge_slope = mix(0.28, 0.52, detail);
    let center_opponents = Noise::nr_opponents(center);
    let center_opponent_variance = Noise::nr_opponent_variance(center);
    let compact = scale >= 4;
    let extent = select(2, 1, compact);
    var opponent_sum = vec2<f32>(0.0);
    var weight_sum = 0.0;

    for (var y = -extent; y <= extent; y = y + 1) {
        for (var x = -extent; x <= extent; x = x + 1) {
            let sample = color_denoise_at(pos + vec2<i32>(x, y) * radius);
            let sample_signal = Noise::nr_signal(sample);
            let sample_variance = Noise::nr_component_variance(sample);
            let signal_delta = sample_signal - center_signal;
            let signal_variance = center_variance.x + sample_variance.x;
            let signal_distance = signal_delta * signal_delta
                / max(
                    signal_variance
                        * guide_variance_scale
                        * signal_guide_sigma
                        * signal_guide_sigma,
                    1e-10,
                );
            let sample_opponents = Noise::nr_opponents(sample);
            let opponent_delta = sample_opponents - center_opponents;
            let opponent_variance =
                center_opponent_variance + Noise::nr_opponent_variance(sample);
            let normalized_opponent_distance = dot(
                opponent_delta * opponent_delta
                    / max(
                        opponent_variance * guide_variance_scale,
                        vec2<f32>(1e-10),
                    ),
                vec2<f32>(1.0),
            );
            let opponent_distance = max(
                normalized_opponent_distance - opponent_noise_deadzone,
                0.0,
            ) * opponent_edge_slope;
            let spatial = color_denoise_kernel_weight(x, compact)
                * color_denoise_kernel_weight(y, compact);
            let weight =
                spatial * exp(-0.5 * (signal_distance + opponent_distance));
            opponent_sum += sample_opponents * weight;
            weight_sum += weight;
        }
    }

    let low_opponents = opponent_sum / max(weight_sum, 1e-6);
    let opponent_detail = center_opponents - low_opponents;
    let opponent_sigma = sqrt(
        Noise::nr_opponent_variance(center) * guide_variance_scale,
    );

    let threshold_strength = mix(3.1, 1.35, detail)
        * color_denoise_scale_gain(scale)
        * requested;
    let normalized_detail = opponent_detail / max(opponent_sigma, vec2<f32>(1e-6));
    let normalized_magnitude = length(normalized_detail);
    let soft_retained =
        max(1.0 - threshold_strength / max(normalized_magnitude, 1e-6), 0.0);
    let feature_retention = smoothstep(
        mix(9.0, 5.0, detail),
        mix(18.0, 10.0, detail),
        normalized_magnitude,
    );
    let relative_chroma =
        length(center_opponents) / max(abs(center_signal), 0.02);
    let saturated_feature = smoothstep(0.15, 0.35, relative_chroma)
        * smoothstep(0.08, 0.20, center_signal);
    let retained = mix(
        soft_retained,
        1.0,
        max(feature_retention, saturated_feature),
    );
    let filtered_opponents = low_opponents + opponent_detail * retained;
    return Noise::nr_from_signal_opponents(center_signal, filtered_opponents);
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_1(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 1, 0), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_2(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 2, 1), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_4(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 4, 2), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_8(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 8, 3), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_16(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 16, 4), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_32(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 32, 5), 1.0));
}
