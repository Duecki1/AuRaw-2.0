// Robust low-frequency branch for dual demosaic. This is a clean-room,
// gradient/noise-aware reconstruction rather than a port of VNG/LMMSE code.
// Pass 1 builds a full-resolution green guide from symmetric same-colour
// support. Pass 2 reconstructs red/blue as robust colour differences against
// that guide. The alpha channel stores confidence for the final high/low blend.
@group(0) @binding(20) var dual_green_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(21) var dual_green_read: texture_2d<f32>;
@group(0) @binding(22) var dual_low_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

struct DualEstimate {
    value: f32,
    confidence: f32,
}

fn dual_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(params.width) && pos.y < i32(params.height);
}

fn dual_plane_variance(channel: u32, signal: f32) -> f32 {
    let c = min(channel, 3u);
    return max(params.noise_read[c] + params.noise_shot[c] * max(signal, 0.0), 1e-10);
}

fn dual_green_variance(signal: f32) -> f32 {
    let read = 0.5 * (params.noise_read.g + params.noise_read.a);
    let shot = 0.5 * (params.noise_shot.g + params.noise_shot.a);
    return max(read + shot * max(signal, 0.0), 1e-10);
}

fn dual_spatial_weight(dx: i32, dy: i32) -> f32 {
    return 1.0 / (1.0 + 0.30 * f32(dx * dx + dy * dy));
}

fn dual_green_estimate(pos: vec2<i32>) -> DualEstimate {
    let p = clamp_pos(pos);
    if color_at(p) == 1u {
        return DualEstimate(raw_cfa_at(p), 1.0);
    }

    var fallback_sum = 0.0;
    var fallback_weight = 0.0;
    var pair_sum = 0.0;
    var pair_weight = 0.0;
    var pair_support = 0.0;
    var pair_gradient = 0.0;

    // Symmetric green pairs act as directional estimates. Low-gradient pairs
    // receive more weight, which is VNG-like in spirit while remaining a
    // separate clean-room formulation. The fallback guarantees coverage for
    // irregular X-Trans neighbourhoods and image borders.
    for (var dy = -4; dy <= 4; dy = dy + 1) {
        for (var dx = -4; dx <= 4; dx = dx + 1) {
            if dx == 0 && dy == 0 { continue; }
            let q = pos + vec2<i32>(dx, dy);
            if dual_in_bounds(q) && color_at(q) == 1u {
                let spatial = dual_spatial_weight(dx, dy);
                fallback_sum += raw_cfa_at(q) * spatial;
                fallback_weight += spatial;
            }

            if dy < 0 || (dy == 0 && dx <= 0) { continue; }
            let q0 = pos + vec2<i32>(dx, dy);
            let q1 = pos - vec2<i32>(dx, dy);
            if !dual_in_bounds(q0) || !dual_in_bounds(q1) { continue; }
            if color_at(q0) != 1u || color_at(q1) != 1u { continue; }

            let v0 = raw_cfa_at(q0);
            let v1 = raw_cfa_at(q1);
            let sigma = sqrt(dual_green_variance(v0) + dual_green_variance(v1));
            let normalized_gradient = abs(v0 - v1) / max(3.0 * sigma, 0.0020);
            let spatial = dual_spatial_weight(dx, dy);
            let weight = spatial / (1.0 + normalized_gradient * normalized_gradient);
            pair_sum += 0.5 * (v0 + v1) * weight;
            pair_weight += weight;
            pair_support += spatial;
            pair_gradient += normalized_gradient * weight;
        }
    }

    let fallback = fallback_sum / max(fallback_weight, 1e-6);
    if pair_weight <= 1e-6 {
        let support = 1.0 - exp(-0.20 * fallback_weight);
        return DualEstimate(fallback, 0.45 * support);
    }

    let directional = pair_sum / pair_weight;
    let support = 1.0 - exp(-0.35 * pair_support);
    let coherence = 1.0 / (1.0 + pair_gradient / pair_weight);
    let confidence = clamp(support * coherence, 0.0, 1.0);
    return DualEstimate(mix(fallback, directional, 0.35 + 0.65 * confidence), confidence);
}

fn dual_channel_estimate(pos: vec2<i32>, channel: u32, center_green: f32) -> DualEstimate {
    let p = clamp_pos(pos);
    if color_at(p) == channel {
        return DualEstimate(raw_cfa_at(p), 1.0);
    }

    var first_sum = 0.0;
    var first_weight = 0.0;
    var first_support = 0.0;
    for (var dy = -4; dy <= 4; dy = dy + 1) {
        for (var dx = -4; dx <= 4; dx = dx + 1) {
            let q = pos + vec2<i32>(dx, dy);
            if !dual_in_bounds(q) || color_at(q) != channel { continue; }
            let sample = raw_cfa_at(q);
            let sample_green = textureLoad(dual_green_read, q, 0).x;
            let sigma = sqrt(dual_green_variance(center_green) + dual_green_variance(sample_green));
            let edge = abs(sample_green - center_green) / max(3.5 * sigma, 0.0030);
            let spatial = dual_spatial_weight(dx, dy);
            let weight = spatial / (1.0 + edge * edge);
            first_sum += (sample - sample_green) * weight;
            first_weight += weight;
            first_support += spatial;
        }
    }

    if first_weight <= 1e-6 {
        return DualEstimate(center_green, 0.0);
    }
    let mean_difference = first_sum / first_weight;

    var robust_sum = 0.0;
    var robust_weight = 0.0;
    var residual_sum = 0.0;
    for (var dy = -4; dy <= 4; dy = dy + 1) {
        for (var dx = -4; dx <= 4; dx = dx + 1) {
            let q = pos + vec2<i32>(dx, dy);
            if !dual_in_bounds(q) || color_at(q) != channel { continue; }
            let sample = raw_cfa_at(q);
            let sample_green = textureLoad(dual_green_read, q, 0).x;
            let difference = sample - sample_green;
            let green_sigma = sqrt(dual_green_variance(center_green) + dual_green_variance(sample_green));
            let edge = abs(sample_green - center_green) / max(3.5 * green_sigma, 0.0030);
            let spatial = dual_spatial_weight(dx, dy);
            let base_weight = spatial / (1.0 + edge * edge);

            let plane = cfa_channel_at(q);
            let chroma_sigma = sqrt(
                dual_plane_variance(plane, sample)
                + dual_green_variance(sample_green)
            );
            let residual = difference - mean_difference;
            let normalized_residual = abs(residual) / max(4.0 * chroma_sigma, 0.0040);
            let robust = 1.0 / (1.0 + normalized_residual * normalized_residual);
            let weight = base_weight * robust;
            robust_sum += difference * weight;
            robust_weight += weight;
            residual_sum += normalized_residual * weight;
        }
    }

    let difference = robust_sum / max(robust_weight, 1e-6);
    let support = 1.0 - exp(-0.18 * first_support);
    let coherence = 1.0 / (1.0 + residual_sum / max(robust_weight, 1e-6));
    return DualEstimate(center_green + difference, clamp(support * coherence, 0.0, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn dual_green_reconstruct(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let estimate = dual_green_estimate(pos);
    textureStore(
        dual_green_write,
        pos,
        vec4<f32>(estimate.value, estimate.value, estimate.value, estimate.confidence),
    );
}

@compute @workgroup_size(8, 8, 1)
fn dual_rgb_reconstruct(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let green_sample = textureLoad(dual_green_read, pos, 0);
    let red = dual_channel_estimate(pos, 0u, green_sample.x);
    let blue = dual_channel_estimate(pos, 2u, green_sample.x);
    let confidence = clamp(
        0.40 * green_sample.a + 0.30 * red.confidence + 0.30 * blue.confidence,
        0.0,
        1.0,
    );
    textureStore(
        dual_low_write,
        pos,
        vec4<f32>(red.value, green_sample.x, blue.value, confidence),
    );
}
