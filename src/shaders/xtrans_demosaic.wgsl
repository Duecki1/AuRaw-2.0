// Grouped X-Trans seed, Markesteijn interpolation, directional analysis,
// homogeneity, and accumulation passes. Bindings are unique across the module
// so all entry points compile once while retaining their existing dispatch order.

// BEGIN merged source: seed and initial interpolation
// Dedicated Fuji X-Trans seed pass. Unlike the Bayer RCD path, this pass does
// not assume a 2x2 repeating mosaic. It searches the actual per-pixel CFA map
// supplied by LibRaw and combines same-colour estimates from four axes.
@group(0) @binding(4) var xtrans_seed_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

struct XTransDirectionalEstimate {
    value: f32,
    gradient: f32,
    valid: f32,
}

fn xtrans_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(params.width) && pos.y < i32(params.height);
}

// Returns (sample value, distance). A negative distance means no sample was
// found in this direction within the X-Trans 6x6 neighbourhood.
fn xtrans_nearest_in_direction(
    pos: vec2<i32>,
    channel: u32,
    direction: vec2<i32>,
) -> vec2<f32> {
    for (var step = 1; step <= 6; step = step + 1) {
        let sample_pos = pos + direction * step;
        if !xtrans_in_bounds(sample_pos) { continue; }
        if color_at(sample_pos) == channel {
            return vec2<f32>(raw_cfa_at(sample_pos), f32(step));
        }
    }
    return vec2<f32>(0.0, -1.0);
}

fn xtrans_directional_estimate(
    pos: vec2<i32>,
    channel: u32,
    axis: vec2<i32>,
) -> XTransDirectionalEstimate {
    let negative = xtrans_nearest_in_direction(pos, channel, -axis);
    let positive = xtrans_nearest_in_direction(pos, channel, axis);
    let have_negative = negative.y > 0.0;
    let have_positive = positive.y > 0.0;

    if have_negative && have_positive {
        let distance = negative.y + positive.y;
        let value = (negative.x * positive.y + positive.x * negative.y)
            / max(distance, 1e-6);
        let gradient = abs(negative.x - positive.x) / max(distance, 1.0);
        return XTransDirectionalEstimate(value, gradient, 1.0);
    }
    if have_negative {
        return XTransDirectionalEstimate(
            negative.x,
            0.25 + 0.08 * negative.y,
            0.55,
        );
    }
    if have_positive {
        return XTransDirectionalEstimate(
            positive.x,
            0.25 + 0.08 * positive.y,
            0.55,
        );
    }
    return XTransDirectionalEstimate(0.0, 1e6, 0.0);
}

fn xtrans_add_direction(
    estimate: XTransDirectionalEstimate,
    weighted_sum: ptr<function, f32>,
    weight_sum: ptr<function, f32>,
) {
    if estimate.valid <= 0.0 { return; }
    let weight = estimate.valid / (1e-5 + estimate.gradient * estimate.gradient);
    *weighted_sum = *weighted_sum + estimate.value * weight;
    *weight_sum = *weight_sum + weight;
}

fn xtrans_radial_fallback(pos: vec2<i32>, channel: u32) -> f32 {
    var sum = 0.0;
    var weight_sum = 0.0;
    for (var dy = -4; dy <= 4; dy = dy + 1) {
        for (var dx = -4; dx <= 4; dx = dx + 1) {
            if dx == 0 && dy == 0 { continue; }
            let sample_pos = pos + vec2<i32>(dx, dy);
            if !xtrans_in_bounds(sample_pos) { continue; }
            if color_at(sample_pos) != channel { continue; }
            let distance_squared = f32(dx * dx + dy * dy);
            let weight = 1.0 / max(distance_squared, 1.0);
            sum = sum + raw_cfa_at(sample_pos) * weight;
            weight_sum = weight_sum + weight;
        }
    }
    if weight_sum > 0.0 {
        return sum / weight_sum;
    }
    return raw_cfa_at(pos);
}

fn xtrans_seed_channel(pos: vec2<i32>, channel: u32) -> vec2<f32> {
    if color_at(pos) == channel {
        return vec2<f32>(raw_cfa_at(pos), 1.0);
    }

    var weighted_sum = 0.0;
    var weight_sum = 0.0;
    xtrans_add_direction(
        xtrans_directional_estimate(pos, channel, vec2<i32>(1, 0)),
        &weighted_sum,
        &weight_sum,
    );
    xtrans_add_direction(
        xtrans_directional_estimate(pos, channel, vec2<i32>(0, 1)),
        &weighted_sum,
        &weight_sum,
    );
    xtrans_add_direction(
        xtrans_directional_estimate(pos, channel, vec2<i32>(1, 1)),
        &weighted_sum,
        &weight_sum,
    );
    xtrans_add_direction(
        xtrans_directional_estimate(pos, channel, vec2<i32>(1, -1)),
        &weighted_sum,
        &weight_sum,
    );

    if weight_sum > 0.0 {
        let confidence = clamp(weight_sum / (weight_sum + 4.0), 0.15, 0.95);
        return vec2<f32>(max(weighted_sum / weight_sum, 0.0), confidence);
    }
    return vec2<f32>(max(xtrans_radial_fallback(pos, channel), 0.0), 0.1);
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_seed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let red = xtrans_seed_channel(pos, 0u);
    let green = xtrans_seed_channel(pos, 1u);
    let blue = xtrans_seed_channel(pos, 2u);
    let confidence = min(red.y, min(green.y, blue.y));
    textureStore(
        xtrans_seed_write,
        pos,
        vec4<f32>(red.x, green.x, blue.x, confidence),
    );
}
// END merged source: seed and initial interpolation

// BEGIN merged source: directional candidate interpolation
// Markesteijn X-Trans passes 1 and 3. Pass 1 performs bounded high-order green
// interpolation at red/blue sites. Pass 3 refines missing red/blue components
// as directional color differences. The ping-pong layout intentionally keeps
// these operations separate from the homogeneity-selection stage.
@group(0) @binding(5) var markesteijn_read_13: texture_2d<f32>;
@group(0) @binding(6) var markesteijn_write_13: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn mark13_load(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(markesteijn_read_13, clamp_pos(pos), 0).rgb;
}

fn mark13_component(rgb: vec3<f32>, channel: u32) -> f32 {
    if channel == 0u { return rgb.r; }
    if channel == 1u { return rgb.g; }
    return rgb.b;
}

fn mark13_set(rgb: vec3<f32>, channel: u32, value: f32) -> vec3<f32> {
    var out = rgb;
    if channel == 0u { out.r = value; }
    if channel == 1u { out.g = value; }
    if channel == 2u { out.b = value; }
    return out;
}

fn mark13_green_bounds(pos: vec2<i32>) -> vec2<f32> {
    var lo = 1e20;
    var hi = -1e20;
    for (var dy = -3; dy <= 3; dy = dy + 1) {
        for (var dx = -3; dx <= 3; dx = dx + 1) {
            let q = pos + vec2<i32>(dx, dy);
            if q.x < 0 || q.y < 0 || q.x >= i32(params.width) || q.y >= i32(params.height) {
                continue;
            }
            if color_at(q) == 1u {
                let value = raw_cfa_at(q);
                lo = min(lo, value);
                hi = max(hi, value);
            }
        }
    }
    if hi < lo { return vec2<f32>(0.0, 1e20); }
    return vec2<f32>(lo, hi);
}

fn mark13_green_axis(pos: vec2<i32>, axis: vec2<i32>, measured_channel: u32) -> vec2<f32> {
    let center = mark13_load(pos);
    let m1 = mark13_load(pos - axis);
    let p1 = mark13_load(pos + axis);
    let m2 = mark13_load(pos - 2 * axis);
    let p2 = mark13_load(pos + 2 * axis);

    // High-order coefficients are the constants used by Markesteijn's initial
    // green interpolation. The color-channel correction keeps the estimate
    // anchored to the measured red/blue sample.
    let base = 0.6796875 * (m1.g + p1.g) - 0.1796875 * (m2.g + p2.g);
    let c0 = mark13_component(center, measured_channel);
    let cm = mark13_component(m2, measured_channel);
    let cp = mark13_component(p2, measured_channel);
    let estimate = base + 0.12890625 * (2.0 * c0 - cm - cp);
    let gradient = 1e-5
        + abs(m1.g - p1.g)
        + abs(m2.g - p2.g)
        + abs((mark13_component(m1, measured_channel) - m1.g)
            - (mark13_component(p1, measured_channel) - p1.g));
    return vec2<f32>(estimate, gradient);
}

fn mark13_pass1(pos: vec2<i32>) -> vec3<f32> {
    let center = mark13_load(pos);
    let measured_channel = color_at(pos);
    if measured_channel == 1u { return center; }

    let h = mark13_green_axis(pos, vec2<i32>(1, 0), measured_channel);
    let v = mark13_green_axis(pos, vec2<i32>(0, 1), measured_channel);
    let p = mark13_green_axis(pos, vec2<i32>(1, 1), measured_channel);
    let q = mark13_green_axis(pos, vec2<i32>(1, -1), measured_channel);
    let wh = 1.0 / (h.y * h.y);
    let wv = 1.0 / (v.y * v.y);
    let wp = 1.0 / (p.y * p.y);
    let wq = 1.0 / (q.y * q.y);
    let bounds = mark13_green_bounds(pos);
    let green = clamp(
        (h.x * wh + v.x * wv + p.x * wp + q.x * wq) / max(wh + wv + wp + wq, 1e-8),
        bounds.x,
        bounds.y,
    );
    var out = center;
    out.g = green;
    out = mark13_set(out, measured_channel, raw_cfa_at(pos));
    return out;
}

fn mark13_color_axis(pos: vec2<i32>, axis: vec2<i32>, channel: u32) -> vec2<f32> {
    let center = mark13_load(pos);
    let m1 = mark13_load(pos - axis);
    let p1 = mark13_load(pos + axis);
    let m2 = mark13_load(pos - 2 * axis);
    let p2 = mark13_load(pos + 2 * axis);
    let dm = mark13_component(m1, channel) - m1.g;
    let dp = mark13_component(p1, channel) - p1.g;
    let gm = 1e-5 + abs(center.g - m2.g) + abs(m1.g - p1.g)
        + abs(dm - (mark13_component(m2, channel) - m2.g));
    let gp = 1e-5 + abs(center.g - p2.g) + abs(m1.g - p1.g)
        + abs(dp - (mark13_component(p2, channel) - p2.g));
    return vec2<f32>((gm * dp + gp * dm) / (gm + gp), gm + gp);
}

fn mark13_refine_channel(pos: vec2<i32>, channel: u32) -> f32 {
    let h = mark13_color_axis(pos, vec2<i32>(1, 0), channel);
    let v = mark13_color_axis(pos, vec2<i32>(0, 1), channel);
    let p = mark13_color_axis(pos, vec2<i32>(1, 1), channel);
    let q = mark13_color_axis(pos, vec2<i32>(1, -1), channel);
    let wh = 1.0 / (h.y * h.y);
    let wv = 1.0 / (v.y * v.y);
    let wp = 1.0 / (p.y * p.y);
    let wq = 1.0 / (q.y * q.y);
    return (h.x * wh + v.x * wv + p.x * wp + q.x * wq) / max(wh + wv + wp + wq, 1e-8);
}

fn mark13_pass3(pos: vec2<i32>) -> vec3<f32> {
    let center = mark13_load(pos);
    let measured_channel = color_at(pos);
    var out = center;
    if measured_channel != 0u { out.r = center.g + mark13_refine_channel(pos, 0u); }
    if measured_channel != 2u { out.b = center.g + mark13_refine_channel(pos, 2u); }
    out = mark13_set(out, measured_channel, raw_cfa_at(pos));
    return out;
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_pass1(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(markesteijn_write_13, pos, vec4<f32>(mark13_pass1(pos), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_pass3(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(markesteijn_write_13, pos, vec4<f32>(mark13_pass3(pos), 1.0));
}
// END merged source: directional candidate interpolation

// BEGIN merged source: candidate refinement
// Markesteijn X-Trans pass 2: recalculate green from the closer interpolated
// values created by pass 1, then refresh missing chroma around that green.
@group(0) @binding(7) var markesteijn_read_2: texture_2d<f32>;
@group(0) @binding(8) var markesteijn_write_2: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn mark2_load(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(markesteijn_read_2, clamp_pos(pos), 0).rgb;
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

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_pass2(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let measured_channel = color_at(pos);
    var out = mark2_load(pos);
    if measured_channel != 1u {
        out.g = mark2_recalculate_green(pos, measured_channel);
    }
    if measured_channel != 0u { out.r = mark2_chroma(pos, 0u); }
    if measured_channel != 2u { out.b = mark2_chroma(pos, 2u); }
    out = mark2_set(out, measured_channel, raw_cfa_at(pos));
    textureStore(markesteijn_write_2, pos, vec4<f32>(out, 1.0));
}
// END merged source: candidate refinement

// BEGIN merged source: shared candidate sampling helpers
// Markesteijn candidate construction shared by the derivative and accumulation
// passes. This is a memory-bounded GPU port of darktable's eight-direction
// selection: candidates are reconstructed on demand instead of retaining eight
// full-frame RGB buffers. The measured CFA component is never changed.
@group(0) @binding(9) var markesteijn_base_read: texture_2d<f32>;

const MARKESTEIJN3_MARGIN: i32 = 17;

fn mark_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(params.width) && pos.y < i32(params.height);
}

fn mark_has_margin(pos: vec2<i32>) -> bool {
    return pos.x >= MARKESTEIJN3_MARGIN && pos.y >= MARKESTEIJN3_MARGIN
        && pos.x < i32(params.width) - MARKESTEIJN3_MARGIN
        && pos.y < i32(params.height) - MARKESTEIJN3_MARGIN;
}

fn mark_load(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(markesteijn_base_read, clamp_pos(pos), 0).rgb;
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

    // Markesteijn keeps opposite one-sided estimates as separate candidates.
    // A cubic one-sided prediction is anchored by the opposite neighbor to
    // prevent ringing while preserving a meaningful d / d+4 pair.
    let one_sided = forward + 0.5 * (center - forward2);
    let opposite_anchor = backward + 0.5 * (center - backward2);
    var candidate = 0.75 * one_sided + 0.25 * opposite_anchor;

    // Reconstruct missing components as directional color differences around
    // the candidate green. This follows the red/blue interpolation principle
    // in the reference implementation and is less sensitive to luminance edges
    // than interpolating RGB components independently.
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

    // Preserve the sensor sample exactly at every refinement and candidate.
    let measured_channel = color_at(pos);
    candidate = mark_set_component(
        candidate,
        measured_channel,
        mark_component(center, measured_channel),
    );
    return candidate;
}

fn mark_yuv(rgb: vec3<f32>) -> vec3<f32> {
    // darktable uses a perceptual YPbPr/YUV-like space for directional
    // derivatives; these coefficients keep the same luma/chroma separation.
    let y = 0.2627 * rgb.r + 0.6780 * rgb.g + 0.0593 * rgb.b;
    return vec3<f32>(y, 0.56433 * (rgb.b - y), 0.67815 * (rgb.r - y));
}

fn mark_border_rgb(pos: vec2<i32>) -> vec3<f32> {
    // Bounded 5x5 per-channel interpolation for the 17-pixel exterior. The
    // measured photosite value is restored after interpolation.
    var sum = vec3<f32>(0.0);
    var weight_sum = vec3<f32>(0.0);
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let q = pos + vec2<i32>(dx, dy);
            if !mark_in_bounds(q) { continue; }
            let channel = color_at(q);
            let weight = 1.0 / (1.0 + f32(dx * dx + dy * dy));
            let value = max(raw_cfa_at(q), 0.0);
            if channel == 0u { sum.r += value * weight; weight_sum.r += weight; }
            if channel == 1u { sum.g += value * weight; weight_sum.g += weight; }
            if channel == 2u { sum.b += value * weight; weight_sum.b += weight; }
        }
    }
    var rgb = sum / max(weight_sum, vec3<f32>(1e-6));
    let measured_channel = color_at(pos);
    rgb = mark_set_component(rgb, measured_channel, raw_cfa_at(pos));
    return rgb;
}
// END merged source: shared candidate sampling helpers

// BEGIN merged source: directional derivatives
// Markesteijn directional derivative stage. Eight RGB candidates are converted
// to perceptual YUV and differentiated along the four reference axes. The
// scalar derivatives fit in two RGBA scratch textures.
@group(0) @binding(20) var mark_drv_0_3_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(21) var mark_drv_4_7_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn mark_derivative(pos: vec2<i32>, index: u32) -> f32 {
    let axis = mark_axis(index);
    let center = mark_yuv(mark_candidate(pos, index));
    let forward = mark_yuv(mark_candidate(pos + axis, index));
    let backward = mark_yuv(mark_candidate(pos - axis, index));
    let second = 2.0 * center - forward - backward;
    return dot(second, second);
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_derivatives(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    if !mark_has_margin(pos) {
        textureStore(mark_drv_0_3_write, pos, vec4<f32>(0.0));
        textureStore(mark_drv_4_7_write, pos, vec4<f32>(0.0));
        return;
    }
    textureStore(mark_drv_0_3_write, pos, vec4<f32>(
        mark_derivative(pos, 0u),
        mark_derivative(pos, 1u),
        mark_derivative(pos, 2u),
        mark_derivative(pos, 3u),
    ));
    textureStore(mark_drv_4_7_write, pos, vec4<f32>(
        mark_derivative(pos, 4u),
        mark_derivative(pos, 5u),
        mark_derivative(pos, 6u),
        mark_derivative(pos, 7u),
    ));
}
// END merged source: directional derivatives

// BEGIN merged source: homogeneity maps
// Markesteijn homogeneity-map stage. For each direction, count the 3x3
// derivatives below eight times the minimum center derivative. The following
// accumulation pass performs the reference 5x5 sum over these maps.
@group(0) @binding(27) var mark_drv_0_3_read: texture_2d<f32>;
@group(0) @binding(28) var mark_drv_4_7_read: texture_2d<f32>;
@group(0) @binding(24) var mark_homo_0_3_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(25) var mark_homo_4_7_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

const MARK_HOMO_MARGIN: i32 = 15;

fn mark_drv(pos: vec2<i32>, index: u32) -> f32 {
    let p = clamp_pos(pos);
    if index < 4u {
        return textureLoad(mark_drv_0_3_read, p, 0)[index];
    }
    return textureLoad(mark_drv_4_7_read, p, 0)[index - 4u];
}

fn mark_drv_threshold(pos: vec2<i32>) -> f32 {
    let a = textureLoad(mark_drv_0_3_read, pos, 0);
    let b = textureLoad(mark_drv_4_7_read, pos, 0);
    let minimum = min(
        min(min(a.x, a.y), min(a.z, a.w)),
        min(min(b.x, b.y), min(b.z, b.w)),
    );
    return max(minimum * 8.0, 1e-12);
}

fn mark_local_homogeneity(pos: vec2<i32>, index: u32) -> f32 {
    let threshold = mark_drv_threshold(pos);
    var count = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            if mark_drv(pos + vec2<i32>(dx, dy), index) <= threshold {
                count += 1.0;
            }
        }
    }
    return count;
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_homogeneity(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let valid = pos.x >= MARK_HOMO_MARGIN && pos.y >= MARK_HOMO_MARGIN
        && pos.x < i32(params.width) - MARK_HOMO_MARGIN
        && pos.y < i32(params.height) - MARK_HOMO_MARGIN;
    if !valid {
        textureStore(mark_homo_0_3_write, pos, vec4<f32>(0.0));
        textureStore(mark_homo_4_7_write, pos, vec4<f32>(0.0));
        return;
    }
    textureStore(mark_homo_0_3_write, pos, vec4<f32>(
        mark_local_homogeneity(pos, 0u),
        mark_local_homogeneity(pos, 1u),
        mark_local_homogeneity(pos, 2u),
        mark_local_homogeneity(pos, 3u),
    ));
    textureStore(mark_homo_4_7_write, pos, vec4<f32>(
        mark_local_homogeneity(pos, 4u),
        mark_local_homogeneity(pos, 5u),
        mark_local_homogeneity(pos, 6u),
        mark_local_homogeneity(pos, 7u),
    ));
}
// END merged source: homogeneity maps

// BEGIN merged source: candidate accumulation
// Markesteijn final directional selection. Build the 5x5 sum of each 3x3
// homogeneity map, quench the weaker member of opposite-direction pairs, and
// average only candidates within one eighth of the maximum map response.
@group(0) @binding(29) var mark_homo_0_3_read: texture_2d<f32>;
@group(0) @binding(30) var mark_homo_4_7_read: texture_2d<f32>;
@group(0) @binding(26) var mark_high_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn mark_homo(pos: vec2<i32>, index: u32) -> f32 {
    let p = clamp_pos(pos);
    if index < 4u {
        return textureLoad(mark_homo_0_3_read, p, 0)[index];
    }
    return textureLoad(mark_homo_4_7_read, p, 0)[index - 4u];
}

fn mark_homo_sum5(pos: vec2<i32>, index: u32) -> f32 {
    var sum = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            sum += mark_homo(pos + vec2<i32>(dx, dy), index);
        }
    }
    return sum;
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_accumulate(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    if !mark_has_margin(pos) {
        textureStore(mark_high_write, pos, vec4<f32>(mark_border_rgb(pos), 1.0));
        return;
    }

    var hm: array<f32, 8>;
    var maximum = 0.0;
    for (var index = 0u; index < 8u; index = index + 1u) {
        hm[index] = mark_homo_sum5(pos, index);
        maximum = max(maximum, hm[index]);
    }
    let cutoff = maximum - floor(maximum / 8.0);

    // Markesteijn-3 keeps one of each opposing pair when their homogeneity
    // differs, avoiding a blurred average of incompatible directions.
    for (var index = 0u; index < 4u; index = index + 1u) {
        if hm[index] < hm[index + 4u] {
            hm[index] = 0.0;
        } else if hm[index] > hm[index + 4u] {
            hm[index + 4u] = 0.0;
        }
    }

    var sum = vec3<f32>(0.0);
    var count = 0.0;
    for (var index = 0u; index < 8u; index = index + 1u) {
        if hm[index] >= cutoff {
            sum += mark_candidate(pos, index);
            count += 1.0;
        }
    }
    var rgb = select(mark_load(pos), sum / max(count, 1.0), count > 0.0);
    let measured_channel = color_at(pos);
    rgb = mark_set_component(
        rgb,
        measured_channel,
        mark_component(mark_load(pos), measured_channel),
    );
    textureStore(mark_high_write, pos, vec4<f32>(rgb, 1.0));
}
// END merged source: candidate accumulation
