// Ratio-corrected green refinement for the irregular X-Trans lattice.
@group(0) @binding(5) var xtrans_seed_read: texture_2d<f32>;
@group(0) @binding(6) var xtrans_green_write: texture_storage_2d<rgba16float, write>;

struct XTransGreenEstimate {
    value: f32,
    gradient: f32,
    valid: f32,
}

fn xtrans2_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(params.width) && pos.y < i32(params.height);
}

// Find the nearest photosite carrying the center's measured red/blue channel.
// The returned vector is (measured channel, interpolated green, distance).
fn xtrans_green_neighbour(
    pos: vec2<i32>,
    channel: u32,
    direction: vec2<i32>,
) -> vec3<f32> {
    for (var step = 1; step <= 6; step = step + 1) {
        let sample_pos = pos + direction * step;
        if !xtrans2_in_bounds(sample_pos) { continue; }
        if color_at(sample_pos) == channel {
            let seed = textureLoad(xtrans_seed_read, sample_pos, 0);
            return vec3<f32>(raw_cfa_at(sample_pos), seed.g, f32(step));
        }
    }
    return vec3<f32>(0.0, 0.0, -1.0);
}

fn xtrans_green_direction(
    pos: vec2<i32>,
    channel: u32,
    axis: vec2<i32>,
) -> XTransGreenEstimate {
    let negative = xtrans_green_neighbour(pos, channel, -axis);
    let positive = xtrans_green_neighbour(pos, channel, axis);
    if negative.z <= 0.0 || positive.z <= 0.0 {
        return XTransGreenEstimate(0.0, 1e6, 0.0);
    }

    let measured_center = max(raw_cfa_at(pos), 1e-6);
    let distance = negative.z + positive.z;
    let base_green = (negative.y * positive.z + positive.y * negative.z)
        / max(distance, 1e-6);

    // Ratio correction limits the colour overshoot that ordinary difference
    // interpolation creates at hard edges. The clamp prevents unstable ratios
    // in near-black data while leaving normal scene ratios untouched.
    let neighbour_signal = max(negative.x + positive.x, 1e-6);
    let correction = clamp(2.0 * measured_center / neighbour_signal, 0.25, 4.0);
    let candidate = base_green * correction;
    let chroma_negative = negative.x - negative.y;
    let chroma_positive = positive.x - positive.y;
    let gradient = abs(negative.x - positive.x)
        + abs(negative.y - positive.y)
        + 0.5 * abs(chroma_negative - chroma_positive);
    return XTransGreenEstimate(candidate, gradient, 1.0);
}

fn xtrans_accumulate_green(
    estimate: XTransGreenEstimate,
    weighted_sum: ptr<function, f32>,
    weight_sum: ptr<function, f32>,
    lower: ptr<function, f32>,
    upper: ptr<function, f32>,
) {
    if estimate.valid <= 0.0 { return; }
    let weight = 1.0 / (1e-5 + estimate.gradient * estimate.gradient);
    *weighted_sum = *weighted_sum + estimate.value * weight;
    *weight_sum = *weight_sum + weight;
    *lower = min(*lower, estimate.value);
    *upper = max(*upper, estimate.value);
}

fn xtrans_refined_green(pos: vec2<i32>, seed: vec4<f32>) -> vec2<f32> {
    let channel = color_at(pos);
    if channel == 1u {
        return vec2<f32>(raw_cfa_at(pos), 1.0);
    }

    var weighted_sum = 0.0;
    var weight_sum = 0.0;
    var lower = 1e20;
    var upper = -1e20;
    xtrans_accumulate_green(
        xtrans_green_direction(pos, channel, vec2<i32>(1, 0)),
        &weighted_sum, &weight_sum, &lower, &upper,
    );
    xtrans_accumulate_green(
        xtrans_green_direction(pos, channel, vec2<i32>(0, 1)),
        &weighted_sum, &weight_sum, &lower, &upper,
    );
    xtrans_accumulate_green(
        xtrans_green_direction(pos, channel, vec2<i32>(1, 1)),
        &weighted_sum, &weight_sum, &lower, &upper,
    );
    xtrans_accumulate_green(
        xtrans_green_direction(pos, channel, vec2<i32>(1, -1)),
        &weighted_sum, &weight_sum, &lower, &upper,
    );

    if weight_sum <= 0.0 {
        return vec2<f32>(seed.g, 0.0);
    }

    let candidate = weighted_sum / weight_sum;
    let range = max(upper - lower, 1e-4);
    let bounded = clamp(candidate, lower - 0.20 * range, upper + 0.20 * range);
    let edge_confidence = clamp(weight_sum / (weight_sum + 2.0), 0.0, 1.0);
    return vec2<f32>(max(mix(seed.g, bounded, 0.85), 0.0), edge_confidence);
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_refine_green(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let seed = textureLoad(xtrans_seed_read, pos, 0);
    let green = xtrans_refined_green(pos, seed);
    textureStore(
        xtrans_green_write,
        pos,
        vec4<f32>(seed.r, green.x, seed.b, green.y),
    );
}
