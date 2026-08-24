// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from darktable 5.6.0 Markesteijn X-Trans demosaicing.
// Copyright (C) 2010-2026 darktable developers.
// Markesteijn algorithm credit: Frank Markesteijn (via dcraw and darktable).
// Copyright (C) 2026 AuRaw contributors (WGSL adaptation).

#import auraw::common as Common
#import auraw::raw_sampling as RawSampling

struct XTransDirectionalEstimate {
    value: f32,
    gradient: f32,
    valid: f32,
}

fn xtrans_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(Common::camera_uniforms.width) && pos.y < i32(Common::camera_uniforms.height);
}

fn xtrans_nearest_in_direction(
    pos: vec2<i32>,
    channel: u32,
    direction: vec2<i32>,
) -> vec2<f32> {
    for (var step = 1; step <= 6; step = step + 1) {
        let sample_pos = pos + direction * step;
        if !xtrans_in_bounds(sample_pos) { continue; }
        if RawSampling::color_at(sample_pos) == channel {
            return vec2<f32>(RawSampling::raw_cfa_at(sample_pos), f32(step));
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
            if RawSampling::color_at(sample_pos) != channel { continue; }
            let distance_squared = f32(dx * dx + dy * dy);
            let weight = 1.0 / max(distance_squared, 1.0);
            sum = sum + RawSampling::raw_cfa_at(sample_pos) * weight;
            weight_sum = weight_sum + weight;
        }
    }
    if weight_sum > 0.0 {
        return sum / weight_sum;
    }
    return RawSampling::raw_cfa_at(pos);
}

fn xtrans_seed_channel(pos: vec2<i32>, channel: u32) -> vec2<f32> {
    if RawSampling::color_at(pos) == channel {
        return vec2<f32>(RawSampling::raw_cfa_at(pos), 1.0);
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
