#import auraw::common as Common
#import auraw::raw_sampling as RawSampling

// Markesteijn pass 2 recalculates green from pass 1, then refreshes missing
// chroma around that green.
@group(0) @binding(7) var markesteijn_read_2: texture_2d<f32>;

fn mark2_load(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(markesteijn_read_2, Common::clamp_pos(pos), 0).rgb;
}

fn mark2_component(rgb: vec3<f32>, channel: u32) -> f32 {
    if channel == 0u { return rgb.r; }
    if channel == 1u { return rgb.g; }
    return rgb.b;
}

fn mark2_set(rgb: vec3<f32>, channel: u32, value: f32) -> vec3<f32> {
    var out = rgb;
    if channel == 0u { out.r = value; }
    if channel == 1u { out.g = value; }
    if channel == 2u { out.b = value; }
    return out;
}

fn mark2_recalculate_axis(pos: vec2<i32>, direction: vec2<i32>, channel: u32) -> vec2<f32> {
    let center = mark2_load(pos);
    let near = mark2_load(pos + direction);
    let far = mark2_load(pos - 2 * direction);
    // Direct translation of the reference recalc relation:
    // (G[-2d] + 2G[d] - C[-2d] - 2C[d] + 3C[0]) / 3.
    let estimate = (
        far.g + 2.0 * near.g
        - mark2_component(far, channel)
        - 2.0 * mark2_component(near, channel)
        + 3.0 * mark2_component(center, channel)
    ) / 3.0;
    let gradient = 1e-5 + abs(near.g - far.g)
        + abs((mark2_component(near, channel) - near.g)
            - (mark2_component(far, channel) - far.g));
    return vec2<f32>(estimate, gradient);
}

fn mark2_recalculate_green(pos: vec2<i32>, channel: u32) -> f32 {
    var sum = 0.0;
    var weight_sum = 0.0;
    for (var d = 0u; d < 8u; d = d + 1u) {
        var direction = vec2<i32>(1, 0);
        switch d {
            case 0u: { direction = vec2<i32>( 1,  0); }
            case 1u: { direction = vec2<i32>(-1,  0); }
            case 2u: { direction = vec2<i32>( 0,  1); }
            case 3u: { direction = vec2<i32>( 0, -1); }
            case 4u: { direction = vec2<i32>( 1,  1); }
            case 5u: { direction = vec2<i32>(-1, -1); }
            case 6u: { direction = vec2<i32>( 1, -1); }
            default: { direction = vec2<i32>(-1,  1); }
        }
        let estimate = mark2_recalculate_axis(pos, direction, channel);
        let weight = 1.0 / (estimate.y * estimate.y);
        sum += estimate.x * weight;
        weight_sum += weight;
    }
    return sum / max(weight_sum, 1e-8);
}

fn mark2_chroma(pos: vec2<i32>, channel: u32) -> f32 {
    let center = mark2_load(pos);
    var sum = 0.0;
    var weight_sum = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            if dx == 0 && dy == 0 { continue; }
            let sample = mark2_load(pos + vec2<i32>(dx, dy));
            let spatial = 1.0 / (1.0 + f32(dx * dx + dy * dy));
            let range = 1.0 / (1e-4 + abs(sample.g - center.g));
            let weight = spatial * min(range, 64.0);
            sum += (mark2_component(sample, channel) - sample.g) * weight;
            weight_sum += weight;
        }
    }
    return center.g + sum / max(weight_sum, 1e-8);
}

fn mark2_pass2(pos: vec2<i32>) -> vec3<f32> {
    let measured_channel = RawSampling::color_at(pos);
    var out = mark2_load(pos);
    if measured_channel != 1u {
        out.g = mark2_recalculate_green(pos, measured_channel);
    }
    if measured_channel != 0u { out.r = mark2_chroma(pos, 0u); }
    if measured_channel != 2u { out.b = mark2_chroma(pos, 2u); }
    out = mark2_set(out, measured_channel, RawSampling::raw_cfa_at(pos));
    return out;
}
