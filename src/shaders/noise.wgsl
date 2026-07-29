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
    return vec2<f32>(rgb.r - rgb.g, rgb.b - rgb.g);
}

fn nr_from_signal_opponents(signal: f32, opponents: vec2<f32>) -> vec3<f32> {
    let g = signal - 0.25 * (opponents.x + opponents.y);
    return vec3<f32>(g + opponents.x, g, g + opponents.y);
}

fn nr_rgb_variance(signal_rgb: vec3<f32>) -> vec3<f32> {
    return max(
        params.noise_read.rgb + params.noise_shot.rgb * max(signal_rgb, vec3<f32>(0.0)),
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
// signal-dependent variance. Chroma uses the signal edge as its primary guide:
// using the noisy opponent difference to decide whether opponent samples are
// allowed through makes false-color speckles protect themselves from filtering.
// A signal guide still prevents color from crossing ordinary luminance edges.
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
    // Chroma needs a broader signal tolerance because a false-color speckle
    // often carries a simultaneous signal error after demosaic. Real
    // luminance edges remain many sensor sigmas farther away.
    let opponent_signal_sigma = mix(10.0, 6.0, detail);
    let signal_delta = sample_signal - center_signal;
    let signal_variance = center_variance.x + sample_variance.x;
    let signal_distance = signal_delta * signal_delta
        / max(signal_variance * signal_sigma * signal_sigma, 1e-10);
    let opponent_guide_distance = signal_delta * signal_delta
        / max(signal_variance * opponent_signal_sigma * opponent_signal_sigma, 1e-10);

    // Only reject a large, coherent color discontinuity. The deliberately wide
    // noise-relative knee keeps ordinary false-color speckles from
    // self-protecting while preventing wide-radius support from crossing a
    // strongly isoluminant subject boundary.
    let opponent_delta = length(sample_opponents - center_opponents);
    let opponent_sigma = sqrt(max(center_variance.y + sample_variance.y, 1e-10));
    let color_edge_gate = 1.0 - smoothstep(
        0.16 + 4.0 * opponent_sigma,
        0.32 + 8.0 * opponent_sigma,
        opponent_delta,
    );
    let signal_weight = exp(-0.5 * signal_distance);
    let opponent_weight = exp(-0.5 * opponent_guide_distance) * color_edge_gate;
    return spatial * vec2<f32>(signal_weight, opponent_weight);
}

fn nr_scale_radius(scale_index: i32) -> i32 {
    if scale_index <= 0 { return 1; }
    if scale_index == 1 { return 2; }
    if scale_index == 2 { return 4; }
    if scale_index == 3 { return 8; }
    return 16;
}

fn nr_scale_count() -> i32 {
    let quality = params.noise_options.z;
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

fn nr_chroma_spatial_boost(offset: vec2<i32>) -> f32 {
    let distance_squared = f32(offset.x * offset.x + offset.y * offset.y);
    // Chroma noise is spatially correlated by demosaic and therefore needs
    // meaningful wide support. Luminance keeps the tighter base weights.
    return 1.0 + 0.25 * distance_squared;
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

    let signal_strength = nr_perceptual_strength(params.noise_options.x, 3.2);
    let chroma_strength = nr_perceptual_strength(params.chroma_denoise, 16.0);
    if signal_strength <= 1e-6 && chroma_strength <= 1e-6 { return center; }

    // Avoid turning weak/failed profile fits into generic blur. The fallback
    // remains usable, but a measured profile earns the full requested blend.
    let profile_trust = mix(0.72, 1.0, clamp(params.noise_options.w, 0.0, 1.0));
    let chroma_trust = mix(0.97, 1.0, clamp(params.noise_options.w, 0.0, 1.0));
    let out_signal = mix(center_signal, filtered_signal, signal_strength * profile_trust);
    let out_opponents = mix(center_opponents, filtered_opponents, chroma_strength * chroma_trust);
    return nr_from_signal_opponents(out_signal, out_opponents);
}

fn nr_perceptual_strength(requested: f32, response: f32) -> f32 {
    let x = clamp(requested, 0.0, 1.0);
    if x <= 1e-6 { return 0.0; }
    // Normalized exponential response: low Lightroom-style values are useful,
    // the transition is continuous, and 100 remains an exact full blend.
    return (1.0 - exp(-response * x)) / (1.0 - exp(-response));
}
