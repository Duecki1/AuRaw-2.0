#import auraw::xtrans::markesteijn_candidates::{mark_axis, mark_candidate, mark_yuv}

fn mark_derivative(pos: vec2<i32>, index: u32) -> f32 {
    let axis = mark_axis(index);
    let center = mark_yuv(mark_candidate(pos, index));
    let forward = mark_yuv(mark_candidate(pos + axis, index));
    let backward = mark_yuv(mark_candidate(pos - axis, index));
    let second = 2.0 * center - forward - backward;
    return dot(second, second);
}
