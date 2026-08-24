#import auraw::common as Common
#import auraw::raw_sampling as RawSampling
#import auraw::xtrans::markesteijn_candidates::{
    mark_border_rgb,
    mark_candidate,
    mark_component,
    mark_has_margin,
    mark_load,
    mark_set_component,
}

@group(0) @binding(29) var mark_homo_0_3_read: texture_2d<f32>;
@group(0) @binding(30) var mark_homo_4_7_read: texture_2d<f32>;

fn mark_homo(pos: vec2<i32>, index: u32) -> f32 {
    let p = Common::clamp_pos(pos);
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

fn mark_accumulate(pos: vec2<i32>) -> vec3<f32> {
    if !mark_has_margin(pos) {
        return mark_border_rgb(pos);
    }

    var hm: array<f32, 8>;
    var maximum = 0.0;
    for (var index = 0u; index < 8u; index = index + 1u) {
        hm[index] = mark_homo_sum5(pos, index);
        maximum = max(maximum, hm[index]);
    }
    let cutoff = maximum - floor(maximum / 8.0);

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
    let measured_channel = RawSampling::color_at(pos);
    rgb = mark_set_component(
        rgb,
        measured_channel,
        mark_component(mark_load(pos), measured_channel),
    );
    return rgb;
}
