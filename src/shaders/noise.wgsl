// Shared sensor-profiled denoise math. Sampling stays in the CFA-specific
// finishing shader so Bayer and X-Trans can reuse their native reconstructed
// reference textures without adding another full-frame intermediate.

const SENSOR_DENOISE_PROCESS_VERSION: u32 = 14u;
const NR_LUMA_WEIGHTS: vec3<f32> = vec3<f32>(0.2627, 0.6780, 0.0593);
const NR_DIRECTIONS: array<vec2<i32>, 8> = array<vec2<i32>, 8>(
    vec2<i32>( 1,  0), vec2<i32>(-1,  0),
    vec2<i32>( 0,  1), vec2<i32>( 0, -1),
    vec2<i32>( 1,  1), vec2<i32>(-1,  1),
    vec2<i32>( 1, -1), vec2<i32>(-1, -1),
);

fn nr_luma(rgb: vec3<f32>) -> f32 {
    return dot(rgb, NR_LUMA_WEIGHTS);
}

fn nr_chroma(rgb: vec3<f32>) -> vec2<f32> {
    let y = nr_luma(rgb);
    return vec2<f32>(0.56433 * (rgb.b - y), 0.67815 * (rgb.r - y));
}

fn nr_from_luma_chroma(y: f32, uv: vec2<f32>) -> vec3<f32> {
    let b = y + uv.x / 0.56433;
    let r = y + uv.y / 0.67815;
    let g = (y - NR_LUMA_WEIGHTS.r * r - NR_LUMA_WEIGHTS.b * b) / NR_LUMA_WEIGHTS.g;
    return vec3<f32>(r, g, b);
}

fn nr_rgb_variance(rgb: vec3<f32>) -> vec3<f32> {
    return max(
        params.noise_read.rgb + params.noise_shot.rgb * max(rgb, vec3<f32>(0.0)),
        vec3<f32>(1e-10),
    );
}

// x = luma variance, y = conservative average chroma variance.
fn nr_component_variance(rgb: vec3<f32>) -> vec2<f32> {
    let variance = nr_rgb_variance(rgb);
    let squared_weights = NR_LUMA_WEIGHTS * NR_LUMA_WEIGHTS;
    let y_variance = max(dot(variance, squared_weights), 1e-10);
    // U = 0.56433(B-Y), V = 0.67815(R-Y). Ignore RGB covariance after
    // demosaic, then average both axes into one hue-neutral range variance.
    let u_variance = 0.56433 * 0.56433 * (variance.b + y_variance);
    let v_variance = 0.67815 * 0.67815 * (variance.r + y_variance);
    return vec2<f32>(y_variance, max(0.5 * (u_variance + v_variance), 1e-10));
}

// Returns luma/chroma range weights after normalizing distances by the local
// signal-dependent variance. This is the variance-stabilized NLMeans-like
// part of the filter: equal-sigma differences receive comparable treatment in
// shadows and highlights even though their absolute noise amplitudes differ.
fn nr_range_weights(
    center_y: f32,
    center_uv: vec2<f32>,
    center_variance: vec2<f32>,
    sample_y: f32,
    sample_uv: vec2<f32>,
    sample_variance: vec2<f32>,
    spatial: f32,
) -> vec2<f32> {
    let detail = clamp(params.noise_options.y, 0.0, 1.0);
    // Detail raises selectivity. At 100, cross-edge pooling is rejected more
    // aggressively; at 0, flat noisy regions can pool a wider sigma range.
    let luma_sigma = mix(3.4, 1.7, detail);
    let chroma_sigma = mix(5.0, 2.8, detail);

    let luma_delta = sample_y - center_y;
    let luma_variance = center_variance.x + sample_variance.x;
    let luma_distance = luma_delta * luma_delta
        / max(luma_variance * luma_sigma * luma_sigma, 1e-10);

    let chroma_delta = sample_uv - center_uv;
    let chroma_variance = center_variance.y + sample_variance.y;
    let chroma_distance = dot(chroma_delta, chroma_delta)
        / max(chroma_variance * chroma_sigma * chroma_sigma, 1e-10);

    return spatial * vec2<f32>(exp(-0.5 * luma_distance), exp(-0.5 * chroma_distance));
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
    luma_sum: f32,
    luma_weight_sum: f32,
    chroma_sum: vec2<f32>,
    chroma_weight_sum: f32,
) -> vec3<f32> {
    let center_y = nr_luma(center);
    let center_uv = nr_chroma(center);
    let filtered_y = luma_sum / max(luma_weight_sum, 1e-6);
    let filtered_uv = chroma_sum / max(chroma_weight_sum, 1e-6);

    let luma_strength = clamp(params.noise_options.x, 0.0, 1.0);
    let chroma_strength = clamp(params.chroma_denoise, 0.0, 1.0);
    if luma_strength <= 1e-6 && chroma_strength <= 1e-6 { return center; }

    // Avoid turning weak/failed profile fits into generic blur. The fallback
    // remains usable, but a measured profile earns the full requested blend.
    let profile_trust = mix(0.72, 1.0, clamp(params.noise_options.w, 0.0, 1.0));
    let out_y = mix(center_y, filtered_y, luma_strength * profile_trust);
    let out_uv = mix(center_uv, filtered_uv, chroma_strength * profile_trust);
    return nr_from_luma_chroma(out_y, out_uv);
}
