// Shared sensor-profiled denoise math. Sampling stays in the CFA-specific
// finishing shader so Bayer and X-Trans can reuse their native reconstructed
// reference textures without adding another full-frame intermediate.

const SENSOR_DENOISE_PROCESS_VERSION: u32 = 14u;
// Denoise runs before camera characterization, so these are deliberately
// camera-space signal/opponent weights rather than Rec.2020 luminance. The
// green-heavy signal axis is reversible with the two green-referenced colour
// differences and avoids assigning display primaries to sensor channels.
const NR_SIGNAL_WEIGHTS: vec3<f32> = vec3<f32>(0.25, 0.50, 0.25);
const NR_DIRECTIONS: array<vec2<i32>, 8> = array<vec2<i32>, 8>(
    vec2<i32>( 1,  0), vec2<i32>(-1,  0),
    vec2<i32>( 0,  1), vec2<i32>( 0, -1),
    vec2<i32>( 1,  1), vec2<i32>(-1,  1),
    vec2<i32>( 1, -1), vec2<i32>(-1, -1),
);

fn nr_signal(rgb: vec3<f32>) -> f32 {
    return dot(rgb, NR_SIGNAL_WEIGHTS);
}

fn nr_opponents(rgb: vec3<f32>) -> vec2<f32> {
    return vec2<f32>(rgb.r - rgb.g, rgb.b - rgb.g);
}

fn nr_from_signal_opponents(signal: f32, opponents: vec2<f32>) -> vec3<f32> {
    let g = signal - 0.25 * (opponents.x + opponents.y);
    return vec3<f32>(g + opponents.x, g, g + opponents.y);
}

fn nr_rgb_variance(rgb: vec3<f32>) -> vec3<f32> {
    return max(
        params.noise_read.rgb + params.noise_shot.rgb * max(rgb, vec3<f32>(0.0)),
        vec3<f32>(1e-10),
    );
}

// x = camera-signal variance, y = conservative average opponent variance.
fn nr_component_variance(rgb: vec3<f32>) -> vec2<f32> {
    let variance = nr_rgb_variance(rgb);
    let squared_weights = NR_SIGNAL_WEIGHTS * NR_SIGNAL_WEIGHTS;
    let signal_variance = max(dot(variance, squared_weights), 1e-10);
    // Opponent axes are R-G and B-G. Ignore post-demosaic covariance, then
    // average both axes into one conservative, channel-order-neutral range
    // variance. This is a camera-space noise model, not a display YUV model.
    let rg_variance = variance.r + variance.g;
    let bg_variance = variance.b + variance.g;
    return vec2<f32>(signal_variance, max(0.5 * (rg_variance + bg_variance), 1e-10));
}

// Returns signal/opponent range weights after normalizing distances by the local
// signal-dependent variance. This is the variance-stabilized NLMeans-like
// part of the filter: equal-sigma differences receive comparable treatment in
// shadows and highlights even though their absolute noise amplitudes differ.
fn nr_range_weights(
    center_signal: f32,
    center_opponents: vec2<f32>,
    center_variance: vec2<f32>,
    sample_signal: f32,
    sample_opponents: vec2<f32>,
    sample_variance: vec2<f32>,
    spatial: f32,
) -> vec2<f32> {
    let detail = clamp(params.noise_options.y, 0.0, 1.0);
    // Detail raises selectivity. At 100, cross-edge pooling is rejected more
    // aggressively; at 0, flat noisy regions can pool a wider sigma range.
    let signal_sigma = mix(3.4, 1.7, detail);
    let opponent_sigma = mix(5.0, 2.8, detail);

    let signal_delta = sample_signal - center_signal;
    let signal_variance = center_variance.x + sample_variance.x;
    let signal_distance = signal_delta * signal_delta
        / max(signal_variance * signal_sigma * signal_sigma, 1e-10);

    let opponent_delta = sample_opponents - center_opponents;
    let opponent_variance = center_variance.y + sample_variance.y;
    let opponent_distance = dot(opponent_delta, opponent_delta)
        / max(opponent_variance * opponent_sigma * opponent_sigma, 1e-10);

    return spatial * vec2<f32>(exp(-0.5 * signal_distance), exp(-0.5 * opponent_distance));
}

fn nr_scale_radius(scale_index: i32) -> i32 {
    if scale_index <= 0 { return 1; }
    if scale_index == 1 { return 2; }
    return 4;
}

fn nr_scale_count() -> i32 {
    let quality = params.noise_options.z;
    if quality < 0.5 { return 1; }
    if quality < 1.5 { return 2; }
    return 3;
}

fn nr_scale_spatial_weight(radius: i32, direction: vec2<i32>) -> f32 {
    let diagonal = select(1.0, 0.72, abs(direction.x) + abs(direction.y) == 2);
    let radius_weight = 1.0 / (1.0 + 0.55 * f32(radius * radius));
    return diagonal * radius_weight;
}

fn nr_finish(
    center: vec3<f32>,
    signal_sum: f32,
    signal_weight_sum: f32,
    opponent_sum: vec2<f32>,
    opponent_weight_sum: f32,
) -> vec3<f32> {
    let center_signal = nr_signal(center);
    let center_opponents = nr_opponents(center);
    let filtered_signal = signal_sum / max(signal_weight_sum, 1e-6);
    let filtered_opponents = opponent_sum / max(opponent_weight_sum, 1e-6);

    let signal_strength = clamp(params.noise_options.x, 0.0, 1.0);
    let chroma_strength = clamp(params.chroma_denoise, 0.0, 1.0);
    if signal_strength <= 1e-6 && chroma_strength <= 1e-6 { return center; }

    // Avoid turning weak/failed profile fits into generic blur. The fallback
    // remains usable, but a measured profile earns the full requested blend.
    let profile_trust = mix(0.72, 1.0, clamp(params.noise_options.w, 0.0, 1.0));
    let out_signal = mix(center_signal, filtered_signal, signal_strength * profile_trust);
    let out_opponents = mix(center_opponents, filtered_opponents, chroma_strength * profile_trust);
    return nr_from_signal_opponents(out_signal, out_opponents);
}
