// Dense post-demosaic colour-noise reduction.
//
// The old finishing pass averaged isolated samples on sparse radius rings.
// That reduced single-pixel false colour but left the spatially correlated
// clouds created by demosaic. This shader instead performs an edge-aware
// à-trous decomposition in the camera-space Y0U0V0-like basis from noise.wgsl.
// Each pass keeps the camera signal exactly and soft-thresholds only the two
// decorrelated opponent detail bands using the measured sensor variance.

@group(0) @binding(11) var color_denoise_read: texture_2d<f32>;
@group(0) @binding(10) var color_denoise_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn color_denoise_at(pos: vec2<i32>) -> vec3<f32> {
    let maximum = vec2<i32>(i32(params.width) - 1, i32(params.height) - 1);
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
    if params.chroma_denoise <= 1e-6 { return false; }
    let quality = params.noise_options.z;
    if quality < 0.5 { return scale == 0; }
    if quality < 1.5 { return scale <= 3; }
    return true;
}

fn color_denoise_apply(pos: vec2<i32>, radius: i32, scale: i32) -> vec3<f32> {
    let center = color_denoise_at(pos);
    if !color_denoise_enabled(scale) { return center; }

    let center_signal = nr_signal(center);
    let center_variance = nr_component_variance(center);
    let detail = clamp(params.noise_options.y, 0.0, 1.0);
    // Chroma follows luminance edges but tolerates the simultaneous signal
    // error commonly carried by a demosaic false-colour speckle.
    let guide_sigma = mix(10.0, 5.5, detail);
    // The two broadest High-quality scales use a 3x3 binomial kernel. Their
    // large spacing supplies Lightroom-like color smoothness without paying
    // for 25 samples per pixel or extending support beyond the export halo.
    let compact = scale >= 4;
    let extent = select(2, 1, compact);
    var opponent_sum = vec2<f32>(0.0);
    var weight_sum = 0.0;

    for (var y = -extent; y <= extent; y = y + 1) {
        for (var x = -extent; x <= extent; x = x + 1) {
            let sample = color_denoise_at(pos + vec2<i32>(x, y) * radius);
            let sample_signal = nr_signal(sample);
            let sample_variance = nr_component_variance(sample);
            let signal_delta = sample_signal - center_signal;
            let signal_variance = center_variance.x + sample_variance.x;
            let range_distance = signal_delta * signal_delta
                / max(signal_variance * guide_sigma * guide_sigma, 1e-10);
            let spatial = color_denoise_kernel_weight(x, compact)
                * color_denoise_kernel_weight(y, compact);
            let weight = spatial * exp(-0.5 * range_distance);
            opponent_sum += nr_opponents(sample) * weight;
            weight_sum += weight;
        }
    }

    let center_opponents = nr_opponents(center);
    let low_opponents = opponent_sum / max(weight_sum, 1e-6);
    let opponent_detail = center_opponents - low_opponents;
    let opponent_sigma = sqrt(nr_opponent_variance(center));

    // A soft threshold is the key distinction from a blur: noise-sized colour
    // variation disappears, while coherent isoluminant colour edges keep the
    // portion that exceeds the profile-derived threshold.
    // Thresholding already has a perceptual onset, so keep this mapping close
    // to linear. Unlike the old response=16 blend, Color 25 must not be
    // numerically indistinguishable from Color 100.
    let requested = clamp(params.chroma_denoise, 0.0, 1.0);
    let threshold_strength = mix(3.1, 1.35, detail)
        * color_denoise_scale_gain(scale)
        * requested;
    let normalized_detail = opponent_detail / max(opponent_sigma, vec2<f32>(1e-6));
    let normalized_magnitude = length(normalized_detail);
    let soft_retained =
        max(1.0 - threshold_strength / max(normalized_magnitude, 1e-6), 0.0);
    // Use a firm upper knee instead of subtracting the threshold from every
    // coefficient forever. A real, coherent colored feature several sensor
    // sigmas above the noise (LED text, small lights, saturated trim) is
    // restored without bias, while ordinary color speckles remain below it.
    let feature_retention = smoothstep(
        mix(9.0, 5.0, detail),
        mix(18.0, 10.0, detail),
        normalized_magnitude,
    );
    // Profile sigma grows with shot noise in highlights, so a small saturated
    // light can be fewer normalized sigmas than expected even though its
    // camera-space chroma is unquestionably real. Protect such well-exposed,
    // strongly colored structure without giving dark chroma speckles the same
    // exemption.
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
    return nr_from_signal_opponents(center_signal, filtered_opponents);
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_1(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 1, 0), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_2(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 2, 1), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_4(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 4, 2), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_8(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 8, 3), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_16(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 16, 4), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn color_denoise_scale_32(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(color_denoise_write, pos, vec4<f32>(color_denoise_apply(pos, 32, 5), 1.0));
}
