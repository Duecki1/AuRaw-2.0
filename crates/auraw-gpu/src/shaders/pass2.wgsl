// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from darktable 5.6.0 RCD demosaicing.
// Copyright (C) 2010-2026 darktable developers.
// RCD credits: Luis Sanz Rodriguez, Ingo Weyrich, and Hanno Schwalm.
// Copyright (C) 2026 AuRaw contributors (WGSL adaptation).

#import auraw::common as Common
#import auraw::raw_sampling as RawSampling

@group(0) @binding(5) var tex1_read: texture_2d<f32>;
@group(0) @binding(6) var tex2_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn rcd_axis_hpf(pos: vec2<i32>, axis: vec2<i32>) -> f32 {
    let c = RawSampling::raw_cfa_at(pos);
    return RawSampling::raw_cfa_at(pos - 3 * axis)
        - RawSampling::raw_cfa_at(pos - axis)
        - RawSampling::raw_cfa_at(pos + axis)
        + RawSampling::raw_cfa_at(pos + 3 * axis)
        - 3.0 * (RawSampling::raw_cfa_at(pos - 2 * axis) + RawSampling::raw_cfa_at(pos + 2 * axis))
        + 6.0 * c;
}

fn rcd_green_candidate(pos: vec2<i32>, axis: vec2<i32>, lpfi: f32) -> vec2<f32> {
    let eps = 1e-5;
    let c = RawSampling::raw_cfa_at(pos);
    let m1 = RawSampling::raw_cfa_at(pos - axis);
    let p1 = RawSampling::raw_cfa_at(pos + axis);
    let m2 = RawSampling::raw_cfa_at(pos - 2 * axis);
    let p2 = RawSampling::raw_cfa_at(pos + 2 * axis);
    let m3 = RawSampling::raw_cfa_at(pos - 3 * axis);
    let p3 = RawSampling::raw_cfa_at(pos + 3 * axis);
    let m4 = RawSampling::raw_cfa_at(pos - 4 * axis);
    let p4 = RawSampling::raw_cfa_at(pos + 4 * axis);

    let grad_m = eps + abs(m1 - p1) + abs(c - m2) + abs(m1 - m3) + abs(m2 - m4);
    let grad_p = eps + abs(m1 - p1) + abs(c - p2) + abs(p1 - p3) + abs(p2 - p4);
    let lpf_m = textureLoad(tex1_read, Common::clamp_pos(pos - 2 * axis), 0).y;
    let lpf_p = textureLoad(tex1_read, Common::clamp_pos(pos + 2 * axis), 0).y;
    let est_m = m1 * (2.0 * lpfi) / (eps + lpfi + lpf_m);
    let est_p = p1 * (2.0 * lpfi) / (eps + lpfi + lpf_p);
    let estimate = (grad_m * est_p + grad_p * est_m) / (grad_m + grad_p);
    return vec2<f32>(estimate, grad_m + grad_p);
}

fn rcd_diagonal_stat(pos: vec2<i32>, axis: vec2<i32>) -> f32 {
    let a = rcd_axis_hpf(pos - axis, axis);
    let b = rcd_axis_hpf(pos, axis);
    let c = rcd_axis_hpf(pos + axis, axis);
    return a * a + b * b + c * c;
}

@compute @workgroup_size(8, 8, 1)
fn bayer_rcd_green(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = RawSampling::color_at(pos);
    var green = RawSampling::raw_cfa_at(pos);

    if cc != 1u {
        let lpfi = textureLoad(tex1_read, pos, 0).y;
        let vertical = rcd_green_candidate(pos, vec2<i32>(0, 1), lpfi);
        let horizontal = rcd_green_candidate(pos, vec2<i32>(1, 0), lpfi);

        let vh_center = textureLoad(tex1_read, pos, 0).x;
        let vh_neighbours = 0.25 * (
            textureLoad(tex1_read, Common::clamp_pos(pos + vec2<i32>(-1, -1)), 0).x
          + textureLoad(tex1_read, Common::clamp_pos(pos + vec2<i32>( 1, -1)), 0).x
          + textureLoad(tex1_read, Common::clamp_pos(pos + vec2<i32>(-1,  1)), 0).x
          + textureLoad(tex1_read, Common::clamp_pos(pos + vec2<i32>( 1,  1)), 0).x
        );
        let vh = select(vh_center, vh_neighbours,
            abs(0.5 - vh_center) < abs(0.5 - vh_neighbours));
        green = mix(vertical.x, horizontal.x, vh);

        let lo = min(
            min(RawSampling::raw_cfa_at(pos + vec2<i32>(-1, 0)), RawSampling::raw_cfa_at(pos + vec2<i32>(1, 0))),
            min(RawSampling::raw_cfa_at(pos + vec2<i32>(0, -1)), RawSampling::raw_cfa_at(pos + vec2<i32>(0, 1)))
        );
        let hi = max(
            max(RawSampling::raw_cfa_at(pos + vec2<i32>(-1, 0)), RawSampling::raw_cfa_at(pos + vec2<i32>(1, 0))),
            max(RawSampling::raw_cfa_at(pos + vec2<i32>(0, -1)), RawSampling::raw_cfa_at(pos + vec2<i32>(0, 1)))
        );
        green = clamp(green, lo - 0.25 * max(hi - lo, 1e-5), hi + 0.25 * max(hi - lo, 1e-5));
    }

    let p_stat = max(1e-10, rcd_diagonal_stat(pos, vec2<i32>(1, 1)));
    let q_stat = max(1e-10, rcd_diagonal_stat(pos, vec2<i32>(1, -1)));
    let pq_dir = p_stat / (p_stat + q_stat);
    let vh_dir = textureLoad(tex1_read, pos, 0).x;
    let clip = select(0.0, 1.0, RawSampling::is_raw_clipped(pos));
    textureStore(tex2_write, pos, vec4<f32>(green, vh_dir, pq_dir, clip));
}
