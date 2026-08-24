#import auraw::common as Common
#import auraw::raw_sampling as RawSampling

@group(0) @binding(9) var markesteijn_base_read: texture_2d<f32>;

const MARKESTEIJN3_MARGIN: i32 = 17;

fn mark_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(Common::camera_uniforms.width) && pos.y < i32(Common::camera_uniforms.height);
}

fn mark_has_margin(pos: vec2<i32>) -> bool {
    return pos.x >= MARKESTEIJN3_MARGIN && pos.y >= MARKESTEIJN3_MARGIN
        && pos.x < i32(Common::camera_uniforms.width) - MARKESTEIJN3_MARGIN
        && pos.y < i32(Common::camera_uniforms.height) - MARKESTEIJN3_MARGIN;
}

fn mark_load(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(markesteijn_base_read, Common::clamp_pos(pos), 0).rgb;
}

fn mark_direction(index: u32) -> vec2<i32> {
    switch index {
        case 0u: { return vec2<i32>( 1,  0); }
        case 1u: { return vec2<i32>( 0,  1); }
        case 2u: { return vec2<i32>( 1,  1); }
        case 3u: { return vec2<i32>( 1, -1); }
        case 4u: { return vec2<i32>(-1,  0); }
        case 5u: { return vec2<i32>( 0, -1); }
        case 6u: { return vec2<i32>(-1, -1); }
        default: { return vec2<i32>(-1,  1); }
    }
}

fn mark_axis(index: u32) -> vec2<i32> {
    return mark_direction(index & 3u);
}

fn mark_component(rgb: vec3<f32>, channel: u32) -> f32 {
    if channel == 0u { return rgb.r; }
    if channel == 1u { return rgb.g; }
    return rgb.b;
}

fn mark_set_component(rgb: vec3<f32>, channel: u32, value: f32) -> vec3<f32> {
    var out = rgb;
    if channel == 0u { out.r = value; }
    if channel == 1u { out.g = value; }
    if channel == 2u { out.b = value; }
    return out;
}

fn mark_local_bounds(pos: vec2<i32>) -> mat2x3<f32> {
    var lo = vec3<f32>(1e20);
    var hi = vec3<f32>(-1e20);
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let rgb = mark_load(pos + vec2<i32>(dx, dy));
            lo = min(lo, rgb);
            hi = max(hi, rgb);
        }
    }
    return mat2x3<f32>(lo, hi);
}

fn mark_candidate(pos: vec2<i32>, index: u32) -> vec3<f32> {
    let direction = mark_direction(index);
    let center = mark_load(pos);
    let forward = mark_load(pos + direction);
    let forward2 = mark_load(pos + 2 * direction);
    let backward = mark_load(pos - direction);
    let backward2 = mark_load(pos - 2 * direction);

    let one_sided = forward + 0.5 * (center - forward2);
    let opposite_anchor = backward + 0.5 * (center - backward2);
    var candidate = 0.75 * one_sided + 0.25 * opposite_anchor;

    let g = candidate.g;
    let grad_forward = vec3<f32>(1e-5) + abs(forward - forward2);
    let grad_backward = vec3<f32>(1e-5) + abs(backward - backward2);
    for (var channel = 0u; channel < 3u; channel = channel + 1u) {
        if channel == 1u { continue; }
        let df = mark_component(forward, channel) - forward.g;
        let db = mark_component(backward, channel) - backward.g;
        let wf = mark_component(grad_backward, channel);
        let wb = mark_component(grad_forward, channel);
        let difference = (wf * df + wb * db) / max(wf + wb, 1e-6);
        candidate = mark_set_component(candidate, channel, g + difference);
    }

    let bounds = mark_local_bounds(pos);
    let span = max(bounds[1] - bounds[0], vec3<f32>(1e-5));
    candidate = clamp(candidate, bounds[0] - 0.125 * span, bounds[1] + 0.125 * span);

    let measured_channel = RawSampling::color_at(pos);
    candidate = mark_set_component(
        candidate,
        measured_channel,
        mark_component(center, measured_channel),
    );
    return candidate;
}

fn mark_yuv(rgb: vec3<f32>) -> vec3<f32> {
    let y = 0.2627 * rgb.r + 0.6780 * rgb.g + 0.0593 * rgb.b;
    return vec3<f32>(y, 0.56433 * (rgb.b - y), 0.67815 * (rgb.r - y));
}

fn mark_border_rgb(pos: vec2<i32>) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    var weight_sum = vec3<f32>(0.0);
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let q = pos + vec2<i32>(dx, dy);
            if !mark_in_bounds(q) { continue; }
            let channel = RawSampling::color_at(q);
            let weight = 1.0 / (1.0 + f32(dx * dx + dy * dy));
            let value = max(RawSampling::raw_cfa_at(q), 0.0);
            if channel == 0u { sum.r += value * weight; weight_sum.r += weight; }
            if channel == 1u { sum.g += value * weight; weight_sum.g += weight; }
            if channel == 2u { sum.b += value * weight; weight_sum.b += weight; }
        }
    }
    var rgb = sum / max(weight_sum, vec3<f32>(1e-6));
    let measured_channel = RawSampling::color_at(pos);
    rgb = mark_set_component(rgb, measured_channel, RawSampling::raw_cfa_at(pos));
    return rgb;
}
