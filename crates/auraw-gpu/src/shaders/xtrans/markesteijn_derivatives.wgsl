// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from darktable 5.6.0 Markesteijn X-Trans demosaicing.
// Copyright (C) 2010-2026 darktable developers.
// Markesteijn algorithm credit: Frank Markesteijn (via dcraw and darktable).
// Copyright (C) 2026 AuRaw contributors (WGSL adaptation).

#import auraw::xtrans::markesteijn_candidates::{mark_axis, mark_candidate, mark_yuv}

fn mark_derivative(pos: vec2<i32>, index: u32) -> f32 {
    let axis = mark_axis(index);
    let center = mark_yuv(mark_candidate(pos, index));
    let forward = mark_yuv(mark_candidate(pos + axis, index));
    let backward = mark_yuv(mark_candidate(pos - axis, index));
    let second = 2.0 * center - forward - backward;
    return dot(second, second);
}
