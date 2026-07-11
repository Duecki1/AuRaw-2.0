// Bayer RCD stage 2: ratio-corrected green interpolation.
@group(0) @binding(5) var tex1_read: texture_2d<f32>;
@group(0) @binding(6) var tex2_write: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn bayer_rcd_green(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = color_at(pos);

    var g_out = 0.0;
    var pq_dir = 0.0;
    var clip = select(0.0, 1.0, is_raw_clipped(pos));

    if cc == 1u {
        g_out = raw_cfa_at(pos);
    } else {
        let lpfi = textureLoad(tex1_read, pos, 0).y;

        let c   = raw_cfa_at(pos);
        let n   = raw_cfa_at(pos + vec2<i32>(0, -1));
        let s   = raw_cfa_at(pos + vec2<i32>(0, 1));
        let w   = raw_cfa_at(pos + vec2<i32>(-1, 0));
        let e   = raw_cfa_at(pos + vec2<i32>(1, 0));
        let n2  = raw_cfa_at(pos + vec2<i32>(0, -2));
        let s2  = raw_cfa_at(pos + vec2<i32>(0, 2));
        let w2  = raw_cfa_at(pos + vec2<i32>(-2, 0));
        let e2  = raw_cfa_at(pos + vec2<i32>(2, 0));
        let n3  = raw_cfa_at(pos + vec2<i32>(0, -3));
        let s3  = raw_cfa_at(pos + vec2<i32>(0, 3));
        let w3  = raw_cfa_at(pos + vec2<i32>(-3, 0));
        let e3  = raw_cfa_at(pos + vec2<i32>(3, 0));
        let n4  = raw_cfa_at(pos + vec2<i32>(0, -4));
        let s4  = raw_cfa_at(pos + vec2<i32>(0, 4));
        let w4  = raw_cfa_at(pos + vec2<i32>(-4, 0));
        let e4  = raw_cfa_at(pos + vec2<i32>(4, 0));

        let eps = 1e-5;
        let n_grad = eps + abs(n - s) + abs(c - n2) + abs(n - n3) + abs(n2 - n4);
        let s_grad = eps + abs(n - s) + abs(c - s2) + abs(s - s3) + abs(s2 - s4);
        let w_grad = eps + abs(w - e) + abs(c - w2) + abs(w - w3) + abs(w2 - w4);
        let e_grad = eps + abs(w - e) + abs(c - e2) + abs(e - e3) + abs(e2 - e4);

        let lpf_n = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(0, -2)), 0).y;
        let lpf_s = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(0, 2)), 0).y;
        let lpf_w = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(-2, 0)), 0).y;
        let lpf_e = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(2, 0)), 0).y;

        let n_est = n * (lpfi + lpfi) / (eps + lpfi + lpf_n);
        let s_est = s * (lpfi + lpfi) / (eps + lpfi + lpf_s);
        let w_est = w * (lpfi + lpfi) / (eps + lpfi + lpf_w);
        let e_est = e * (lpfi + lpfi) / (eps + lpfi + lpf_e);

        let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
        let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);

        let vh_c = textureLoad(tex1_read, pos, 0).x;
        let vh_nw = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(-1, -1)), 0).x;
        let vh_ne = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(1, -1)), 0).x;
        let vh_sw = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(-1, 1)), 0).x;
        let vh_se = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(1, 1)), 0).x;
        let vh_n = 0.25 * (vh_nw + vh_ne + vh_sw + vh_se);
        let vh_disc = select(vh_c, vh_n, abs(0.5 - vh_c) < abs(0.5 - vh_n));

        g_out = mix(v_est, h_est, vh_disc);
    }

    var p_stat = 0.0;
    var q_stat = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let p = pos + vec2<i32>(dx, dy);
            let c  = raw_cfa_at(p);
            let nw1 = raw_cfa_at(p + vec2<i32>(-1, -1));
            let nw2 = raw_cfa_at(p + vec2<i32>(-2, -2));
            let nw3 = raw_cfa_at(p + vec2<i32>(-3, -3));
            let se1 = raw_cfa_at(p + vec2<i32>(1, 1));
            let se2 = raw_cfa_at(p + vec2<i32>(2, 2));
            let se3 = raw_cfa_at(p + vec2<i32>(3, 3));
            
            let ne1 = raw_cfa_at(p + vec2<i32>(1, -1));
            let ne2 = raw_cfa_at(p + vec2<i32>(2, -2));
            let ne3 = raw_cfa_at(p + vec2<i32>(3, -3));
            let sw1 = raw_cfa_at(p + vec2<i32>(-1, 1));
            let sw2 = raw_cfa_at(p + vec2<i32>(-2, 2));
            let sw3 = raw_cfa_at(p + vec2<i32>(-3, 3));

            let is_p_diag = (dx == dy);
            let is_q_diag = (dx == -dy);
            if is_p_diag {
                let val_p = nw3 - nw1 - se1 + se3 - 3.0 * (nw2 + se2) + 6.0 * c;
                p_stat += val_p * val_p;
            }
            if is_q_diag {
                let val_q = ne3 - ne1 - sw1 + sw3 - 3.0 * (ne2 + sw2) + 6.0 * c;
                q_stat += val_q * val_q;
            }
        }
    }
    pq_dir = max(1e-10, p_stat) / (max(1e-10, p_stat) + max(1e-10, q_stat));

    let vh_dir = textureLoad(tex1_read, pos, 0).x;
    textureStore(tex2_write, pos, vec4<f32>(g_out, vh_dir, pq_dir, clip));
}
