#import auraw::common as Common


const NR_SIGNAL_WEIGHTS: vec3<f32> = vec3<f32>(0.25, 0.50, 0.25);
const NR_DIRECTIONS: array<vec2<i32>, 8> = array<vec2<i32>, 8>(
    vec2<i32>( 1,  0), vec2<i32>(-1,  0),
    vec2<i32>( 0,  1), vec2<i32>( 0, -1),
    vec2<i32>( 1,  1), vec2<i32>(-1,  1),
    vec2<i32>( 1, -1), vec2<i32>(-1, -1),
);
const NR_KNIGHT_DIRECTIONS: array<vec2<i32>, 8> = array<vec2<i32>, 8>(
    vec2<i32>( 1,  2), vec2<i32>(-1,  2),
    vec2<i32>( 1, -2), vec2<i32>(-1, -2),
    vec2<i32>( 2,  1), vec2<i32>(-2,  1),
    vec2<i32>( 2, -1), vec2<i32>(-2, -1),
);

fn nr_signal(rgb: vec3<f32>) -> f32 {
    return dot(rgb, NR_SIGNAL_WEIGHTS);
}

fn nr_opponents(rgb: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(
        0.5 * (rgb.r - rgb.b),
        0.25 * rgb.r - 0.5 * rgb.g + 0.25 * rgb.b,
    );
}

fn nr_from_signal_opponents(signal: f32, opponents: vec2<f32>) -> vec3<f32> {
    return vec3<f32>(
        signal + opponents.y + opponents.x,
        signal - opponents.y,
        signal + opponents.y - opponents.x,
    );
}

fn nr_rgb_variance(signal_rgb: vec3<f32>) -> vec3<f32> {
    return max(
        Common::camera_uniforms.noise_read.rgb + Common::camera_uniforms.noise_shot.rgb * max(signal_rgb, vec3<f32>(0.0)),
        vec3<f32>(1e-10),
    );
}

fn nr_component_variance(rgb: vec3<f32>) -> vec2<f32> {
    let variance = nr_rgb_variance(rgb);
    let squared_weights = NR_SIGNAL_WEIGHTS * NR_SIGNAL_WEIGHTS;
    let signal_variance = max(dot(variance, squared_weights), 1e-10);
    let opponent_variance = nr_opponent_variance(rgb);
    return vec2<f32>(signal_variance, max(max(opponent_variance.x, opponent_variance.y), 1e-10));
}

fn nr_opponent_variance(rgb: vec3<f32>) -> vec2<f32> {
    let variance = nr_rgb_variance(rgb);
    return max(
        vec2<f32>(
            0.25 * (variance.r + variance.b),
            0.0625 * variance.r + 0.25 * variance.g + 0.0625 * variance.b,
        ),
        vec2<f32>(1e-10),
    );
}

fn nr_signal_range_weight(
    center_signal: f32,
    center_variance: vec2<f32>,
    sample_signal: f32,
    sample_variance: vec2<f32>,
    spatial: f32,
) -> f32 {
    let detail = clamp(Common::camera_uniforms.noise_options.y, 0.0, 1.0);
    let signal_sigma = mix(3.4, 1.7, detail);
    let signal_delta = sample_signal - center_signal;
    let signal_variance = center_variance.x + sample_variance.x;
    let signal_distance = signal_delta * signal_delta
        / max(signal_variance * signal_sigma * signal_sigma, 1e-10);
    return spatial * exp(-0.5 * signal_distance);
}

fn nr_scale_radius(scale_index: i32) -> i32 {
    if scale_index <= 0 { return 1; }
    if scale_index == 1 { return 2; }
    if scale_index == 2 { return 4; }
    if scale_index == 3 { return 8; }
    return 16;
}

fn nr_scale_count() -> i32 {
    let quality = Common::camera_uniforms.noise_options.z;
    if quality < 0.5 { return 1; }
    if quality < 1.5 { return 2; }
    return 5;
}

fn nr_scale_spatial_weight(radius: i32, direction: vec2<i32>) -> f32 {
    let diagonal = select(1.0, 0.72, abs(direction.x) + abs(direction.y) == 2);
    let radius_weight = 1.0 / (1.0 + 0.55 * f32(radius * radius));
    return diagonal * radius_weight;
}

fn nr_offset_spatial_weight(offset: vec2<i32>) -> f32 {
    let distance_squared = f32(offset.x * offset.x + offset.y * offset.y);
    return 1.0 / (1.0 + 0.55 * distance_squared);
}

fn nr_finish_signal(
    center: vec3<f32>,
    signal_sum: f32,
    signal_weight_sum: f32,
) -> vec3<f32> {
    let center_signal = nr_signal(center);
    let center_opponents = nr_opponents(center);
    let filtered_signal = signal_sum / max(signal_weight_sum, 1e-6);

    let signal_strength = nr_perceptual_strength(Common::camera_uniforms.noise_options.x, 3.2);
    if signal_strength <= 1e-6 { return center; }

    let profile_trust = mix(0.72, 1.0, clamp(Common::camera_uniforms.noise_options.w, 0.0, 1.0));
    let out_signal = mix(center_signal, filtered_signal, signal_strength * profile_trust);
    return nr_from_signal_opponents(out_signal, center_opponents);
}

fn nr_perceptual_strength(requested: f32, response: f32) -> f32 {
    let x = clamp(requested, 0.0, 1.0);
    if x <= 1e-6 { return 0.0; }
    return (1.0 - exp(-response * x)) / (1.0 - exp(-response));
}
