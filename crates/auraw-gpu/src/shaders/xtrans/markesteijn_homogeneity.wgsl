// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from darktable 5.6.0 Markesteijn X-Trans demosaicing.
// Copyright (C) 2010-2026 darktable developers.
// Markesteijn algorithm credit: Frank Markesteijn (via dcraw and darktable).
// Copyright (C) 2026 AuRaw contributors (WGSL adaptation).

#import auraw::common as Common

@group(0) @binding(27) var mark_drv_0_3_read: texture_2d<f32>;
@group(0) @binding(28) var mark_drv_4_7_read: texture_2d<f32>;

const MARK_HOMO_MARGIN: i32 = 15;

fn mark_drv(pos: vec2<i32>, index: u32) -> f32 {
    let p = Common::clamp_pos(pos);
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
