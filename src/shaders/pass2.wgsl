// Bayer RCD stage 2. This follows darktable's ratio-corrected green stage:
// directional HPF discrimination, low-pass ratios, then diagonal P/Q HPFs.
@group(0) @binding(5) var tex1_read: texture_2d<f32>;
@group(0) @binding(6) var tex2_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn rcd_axis_hpf(pos: vec2<i32>, axis: vec2<i32>) -> f32 {
    let c = raw_cfa_at(pos);
    return raw_cfa_at(pos - 3 * axis)
        - raw_cfa_at(pos - axis)
        - raw_cfa_at(pos + axis)
        + raw_cfa_at(pos + 3 * axis)
        - 3.0 * (raw_cfa_at(pos - 2 * axis) + raw_cfa_at(pos + 2 * axis))
        + 6.0 * c;
}

fn rcd_green_candidate(pos: vec2<i32>, axis: vec2<i32>, lpfi: f32) -> vec2<f32> {
    let eps = 1e-5;
    let c = raw_cfa_at(pos);
    let m1 = raw_cfa_at(pos - axis);
    let p1 = raw_cfa_at(pos + axis);
    let m2 = raw_cfa_at(pos - 2 * axis);
    let p2 = raw_cfa_at(pos + 2 * axis);
    let m3 = raw_cfa_at(pos - 3 * axis);
    let p3 = raw_cfa_at(pos + 3 * axis);
    let m4 = raw_cfa_at(pos - 4 * axis);
    let p4 = raw_cfa_at(pos + 4 * axis);

    let grad_m = eps + abs(m1 - p1) + abs(c - m2) + abs(m1 - m3) + abs(m2 - m4);
    let grad_p = eps + abs(m1 - p1) + abs(c - p2) + abs(p1 - p3) + abs(p2 - p4);
    let lpf_m = textureLoad(tex1_read, clamp_pos(pos - 2 * axis), 0).y;
    let lpf_p = textureLoad(tex1_read, clamp_pos(pos + 2 * axis), 0).y;
    let est_m = m1 * (2.0 * lpfi) / (eps + lpfi + lpf_m);
    let est_p = p1 * (2.0 * lpfi) / (eps + lpfi + lpf_p);
    let estimate = (grad_m * est_p + grad_p * est_m) / (grad_m + grad_p);
    return vec2<f32>(estimate, grad_m + grad_p);
}

fn rcd_diagonal_stat(pos: vec2<i32>, axis: vec2<i32>) -> f32 {
    // darktable sums three squared high-pass responses on each diagonal.
    let a = rcd_axis_hpf(pos - axis, axis);
    let b = rcd_axis_hpf(pos, axis);
    let c = rcd_axis_hpf(pos + axis, axis);
    return a * a + b * b + c * c;
}

@compute @workgroup_size(8, 8, 1)
fn bayer_rcd_green(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = color_at(pos);
    var green = raw_cfa_at(pos);

    if cc != 1u {
        let lpfi = textureLoad(tex1_read, pos, 0).y;
        let vertical = rcd_green_candidate(pos, vec2<i32>(0, 1), lpfi);
        let horizontal = rcd_green_candidate(pos, vec2<i32>(1, 0), lpfi);

        let vh_center = textureLoad(tex1_read, pos, 0).x;
        let vh_neighbours = 0.25 * (
            textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(-1, -1)), 0).x
          + textureLoad(tex1_read, clamp_pos(pos + vec2<i32>( 1, -1)), 0).x
          + textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(-1,  1)), 0).x
          + textureLoad(tex1_read, clamp_pos(pos + vec2<i32>( 1,  1)), 0).x
        );
        // Use the more decisive discriminator, as in the reference code.
        let vh = select(vh_center, vh_neighbours,
            abs(0.5 - vh_center) < abs(0.5 - vh_neighbours));
        green = mix(vertical.x, horizontal.x, vh);

        // Keep the ratio correction bounded by the immediate measured greens.
        let lo = min(
            min(raw_cfa_at(pos + vec2<i32>(-1, 0)), raw_cfa_at(pos + vec2<i32>(1, 0))),
            min(raw_cfa_at(pos + vec2<i32>(0, -1)), raw_cfa_at(pos + vec2<i32>(0, 1)))
        );
        let hi = max(
            max(raw_cfa_at(pos + vec2<i32>(-1, 0)), raw_cfa_at(pos + vec2<i32>(1, 0))),
            max(raw_cfa_at(pos + vec2<i32>(0, -1)), raw_cfa_at(pos + vec2<i32>(0, 1)))
        );
        green = clamp(green, lo - 0.25 * max(hi - lo, 1e-5), hi + 0.25 * max(hi - lo, 1e-5));
    }

    let p_stat = max(1e-10, rcd_diagonal_stat(pos, vec2<i32>(1, 1)));
    let q_stat = max(1e-10, rcd_diagonal_stat(pos, vec2<i32>(1, -1)));
    let pq_dir = p_stat / (p_stat + q_stat);
    let vh_dir = textureLoad(tex1_read, pos, 0).x;
    let clip = select(0.0, 1.0, is_raw_clipped(pos));
    textureStore(tex2_write, pos, vec4<f32>(max(green, 0.0), vh_dir, pq_dir, clip));
}
