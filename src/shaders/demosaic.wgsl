// demosaic.wgsl
//
// True 4-pass RCD (Ratio of Color Differences) demosaicing.
// Pass 1: VH_Dir (vertical/horizontal discrimination) and Low-Pass Filter
// Pass 2: Green channel reconstruction + PQ_Dir (diagonal discrimination)
// Pass 3: Red/Blue at opposite color sites (R at B, B at R)
// Pass 4: Red/Blue at Green sites + output pipeline

// ---------------------------------------------------------------------------
//  Pass 1 — VH_Dir and LPF
// ---------------------------------------------------------------------------

@compute @workgroup_size(8, 8, 1)
fn pass1_vh_lpf(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = color_at(pos);

    // 1. VH_Dir
    var v_stat = 0.0;
    var h_stat = 0.0;
    
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        let p = pos + vec2<i32>(0, dy);
        let c  = raw_cfa_at(p);
        let m1 = raw_cfa_at(p + vec2<i32>(0, -1));
        let m2 = raw_cfa_at(p + vec2<i32>(0, -2));
        let m3 = raw_cfa_at(p + vec2<i32>(0, -3));
        let p1 = raw_cfa_at(p + vec2<i32>(0, 1));
        let p2 = raw_cfa_at(p + vec2<i32>(0, 2));
        let p3 = raw_cfa_at(p + vec2<i32>(0, 3));
        let val = m3 - m1 - p1 + p3 - 3.0 * (m2 + p2) + 6.0 * c;
        v_stat += val * val;
    }
    for (var dx = -1; dx <= 1; dx = dx + 1) {
        let p = pos + vec2<i32>(dx, 0);
        let c  = raw_cfa_at(p);
        let m1 = raw_cfa_at(p + vec2<i32>(-1, 0));
        let m2 = raw_cfa_at(p + vec2<i32>(-2, 0));
        let m3 = raw_cfa_at(p + vec2<i32>(-3, 0));
        let p1 = raw_cfa_at(p + vec2<i32>(1, 0));
        let p2 = raw_cfa_at(p + vec2<i32>(2, 0));
        let p3 = raw_cfa_at(p + vec2<i32>(3, 0));
        let val = m3 - m1 - p1 + p3 - 3.0 * (m2 + p2) + 6.0 * c;
        h_stat += val * val;
    }

    let vh_dir = max(1e-10, v_stat) / (max(1e-10, v_stat) + max(1e-10, h_stat));

    // 2. LPF (only for R/B sites)
    var lpf = 0.0;
    if cc != 1u {
        let c  = raw_cfa_at(pos);
        let n  = raw_cfa_at(pos + vec2<i32>(0, -1));
        let s  = raw_cfa_at(pos + vec2<i32>(0, 1));
        let w  = raw_cfa_at(pos + vec2<i32>(-1, 0));
        let e  = raw_cfa_at(pos + vec2<i32>(1, 0));
        let nw = raw_cfa_at(pos + vec2<i32>(-1, -1));
        let ne = raw_cfa_at(pos + vec2<i32>(1, -1));
        let sw = raw_cfa_at(pos + vec2<i32>(-1, 1));
        let se = raw_cfa_at(pos + vec2<i32>(1, 1));
        lpf = c + 0.5 * (n + s + w + e) + 0.25 * (nw + ne + sw + se);
    }

    textureStore(tex1_write, pos, vec4<f32>(vh_dir, lpf, 0.0, 0.0));
}

// ---------------------------------------------------------------------------
//  Pass 2 — Green at R/B + PQ_Dir
// ---------------------------------------------------------------------------

@compute @workgroup_size(8, 8, 1)
fn pass2_green_pq(@builtin(global_invocation_id) gid: vec3<u32>) {
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
        let vh_disc = select(vh_n, vh_c, abs(0.5 - vh_c) < abs(0.5 - vh_n));

        g_out = mix(v_est, h_est, vh_disc);
    }

    // PQ_Dir (computed for all pixels, but only strictly needed at R/B sites)
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

            if (dx + dy) % 2 == 0 { // P diagonal (NW, 0, SE)
                let val_p = nw3 - nw1 - se1 + se3 - 3.0 * (nw2 + se2) + 6.0 * c;
                p_stat += val_p * val_p;
            }
            if (dx - dy) % 2 == 0 { // Q diagonal (NE, 0, SW)
                let val_q = ne3 - ne1 - sw1 + sw3 - 3.0 * (ne2 + sw2) + 6.0 * c;
                q_stat += val_q * val_q;
            }
        }
    }
    pq_dir = max(1e-10, p_stat) / (max(1e-10, p_stat) + max(1e-10, q_stat));

    textureStore(tex2_write, pos, vec4<f32>(g_out, 0.0, pq_dir, clip));
}

// ---------------------------------------------------------------------------
//  Pass 3 — R/B at B/R sites
// ---------------------------------------------------------------------------

@compute @workgroup_size(8, 8, 1)
fn pass3_rb_opposite(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = color_at(pos);

    let g0 = textureLoad(tex2_read, pos, 0).x;
    let clip = textureLoad(tex2_read, pos, 0).w;

    if cc == 1u {
        textureStore(tex3_write, pos, vec4<f32>(0.0, g0, 0.0, clip));
        return;
    }

    let c = 2 - cc; // if R (0), compute B (2). if B (2), compute R (0).
    let eps = 1e-5;

    let nw = pos + vec2<i32>(-1, -1);
    let ne = pos + vec2<i32>(1, -1);
    let sw = pos + vec2<i32>(-1, 1);
    let se = pos + vec2<i32>(1, 1);
    let nw2 = pos + vec2<i32>(-2, -2);
    let ne2 = pos + vec2<i32>(2, -2);
    let sw2 = pos + vec2<i32>(-2, 2);
    let se2 = pos + vec2<i32>(2, 2);
    let nw3 = pos + vec2<i32>(-3, -3);
    let ne3 = pos + vec2<i32>(3, -3);
    let sw3 = pos + vec2<i32>(-3, 3);
    let se3 = pos + vec2<i32>(3, 3);

    let raw_c_nw = raw_cfa_at(nw);
    let raw_c_ne = raw_cfa_at(ne);
    let raw_c_sw = raw_cfa_at(sw);
    let raw_c_se = raw_cfa_at(se);
    let raw_c_nw3 = raw_cfa_at(nw3);
    let raw_c_ne3 = raw_cfa_at(ne3);
    let raw_c_sw3 = raw_cfa_at(sw3);
    let raw_c_se3 = raw_cfa_at(se3);

    let g_nw = textureLoad(tex2_read, clamp_pos(nw), 0).x;
    let g_ne = textureLoad(tex2_read, clamp_pos(ne), 0).x;
    let g_sw = textureLoad(tex2_read, clamp_pos(sw), 0).x;
    let g_se = textureLoad(tex2_read, clamp_pos(se), 0).x;
    let g_nw2 = textureLoad(tex2_read, clamp_pos(nw2), 0).x;
    let g_ne2 = textureLoad(tex2_read, clamp_pos(ne2), 0).x;
    let g_sw2 = textureLoad(tex2_read, clamp_pos(sw2), 0).x;
    let g_se2 = textureLoad(tex2_read, clamp_pos(se2), 0).x;

    let nw_grad = eps + abs(raw_c_nw - raw_c_se) + abs(raw_c_nw - raw_c_nw3) + abs(g0 - g_nw2);
    let ne_grad = eps + abs(raw_c_ne - raw_c_sw) + abs(raw_c_ne - raw_c_ne3) + abs(g0 - g_ne2);
    let sw_grad = eps + abs(raw_c_ne - raw_c_sw) + abs(raw_c_sw - raw_c_sw3) + abs(g0 - g_sw2);
    let se_grad = eps + abs(raw_c_nw - raw_c_se) + abs(raw_c_se - raw_c_se3) + abs(g0 - g_se2);

    let nw_est = raw_c_nw - g_nw;
    let ne_est = raw_c_ne - g_ne;
    let sw_est = raw_c_sw - g_sw;
    let se_est = raw_c_se - g_se;

    let p_est = (nw_grad * se_est + se_grad * nw_est) / (nw_grad + se_grad);
    let q_est = (ne_grad * sw_est + sw_grad * ne_est) / (ne_grad + sw_grad);

    let pq_c = textureLoad(tex2_read, pos, 0).z;
    let pq_nw = textureLoad(tex2_read, clamp_pos(nw), 0).z;
    let pq_ne = textureLoad(tex2_read, clamp_pos(ne), 0).z;
    let pq_sw = textureLoad(tex2_read, clamp_pos(sw), 0).z;
    let pq_se = textureLoad(tex2_read, clamp_pos(se), 0).z;
    let pq_n = 0.25 * (pq_nw + pq_ne + pq_sw + pq_se);
    let pq_disc = select(pq_n, pq_c, abs(0.5 - pq_c) < abs(0.5 - pq_n));

    let val = g0 + mix(p_est, q_est, pq_disc);

    if cc == 0u { // Red site, computed Blue
        textureStore(tex3_write, pos, vec4<f32>(0.0, g0, val, clip));
    } else { // Blue site, computed Red
        textureStore(tex3_write, pos, vec4<f32>(val, g0, 0.0, clip));
    }
}

// ---------------------------------------------------------------------------
//  Pass 4 — R/B at Green sites + Output
// ---------------------------------------------------------------------------

@compute @workgroup_size(8, 8, 1)
fn pass4_rb_green_output(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = color_at(pos);

    let g0 = textureLoad(tex2_read, pos, 0).x;
    let clip = textureLoad(tex2_read, pos, 0).w;

    var r_val = 0.0;
    var b_val = 0.0;

    if cc == 0u {
        r_val = raw_cfa_at(pos);
        b_val = textureLoad(tex3_read, pos, 0).z;
    } else if cc == 2u {
        r_val = textureLoad(tex3_read, pos, 0).x;
        b_val = raw_cfa_at(pos);
    } else {
        let vh_c = textureLoad(tex1_read, pos, 0).x;
        let vh_nw = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(-1, -1)), 0).x;
        let vh_ne = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(1, -1)), 0).x;
        let vh_sw = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(-1, 1)), 0).x;
        let vh_se = textureLoad(tex1_read, clamp_pos(pos + vec2<i32>(1, 1)), 0).x;
        let vh_n = 0.25 * (vh_nw + vh_ne + vh_sw + vh_se);
        let vh_disc = select(vh_n, vh_c, abs(0.5 - vh_c) < abs(0.5 - vh_n));

        let eps = 1e-5;
        let n  = pos + vec2<i32>(0, -1);
        let s  = pos + vec2<i32>(0, 1);
        let w  = pos + vec2<i32>(-1, 0);
        let e  = pos + vec2<i32>(1, 0);
        let n2 = pos + vec2<i32>(0, -2);
        let s2 = pos + vec2<i32>(0, 2);
        let w2 = pos + vec2<i32>(-2, 0);
        let e2 = pos + vec2<i32>(2, 0);
        let n3 = pos + vec2<i32>(0, -3);
        let s3 = pos + vec2<i32>(0, 3);
        let w3 = pos + vec2<i32>(-3, 0);
        let e3 = pos + vec2<i32>(3, 0);

        let g_n2 = textureLoad(tex2_read, clamp_pos(n2), 0).x;
        let g_s2 = textureLoad(tex2_read, clamp_pos(s2), 0).x;
        let g_w2 = textureLoad(tex2_read, clamp_pos(w2), 0).x;
        let g_e2 = textureLoad(tex2_read, clamp_pos(e2), 0).x;
        
        let n1 = eps + abs(g0 - g_n2);
        let s1 = eps + abs(g0 - g_s2);
        let w1 = eps + abs(g0 - g_w2);
        let e1 = eps + abs(g0 - g_e2);

        for (var c = 0u; c <= 2u; c = c + 2u) {
            let val_n = textureLoad(tex3_read, clamp_pos(n), 0);
            let val_s = textureLoad(tex3_read, clamp_pos(s), 0);
            let val_w = textureLoad(tex3_read, clamp_pos(w), 0);
            let val_e = textureLoad(tex3_read, clamp_pos(e), 0);
            let val_n3 = textureLoad(tex3_read, clamp_pos(n3), 0);
            let val_s3 = textureLoad(tex3_read, clamp_pos(s3), 0);
            let val_w3 = textureLoad(tex3_read, clamp_pos(w3), 0);
            let val_e3 = textureLoad(tex3_read, clamp_pos(e3), 0);

            let c_n = select(val_n.x, val_n.z, c == 2u);
            let c_s = select(val_s.x, val_s.z, c == 2u);
            let c_w = select(val_w.x, val_w.z, c == 2u);
            let c_e = select(val_e.x, val_e.z, c == 2u);
            let c_n3 = select(val_n3.x, val_n3.z, c == 2u);
            let c_s3 = select(val_s3.x, val_s3.z, c == 2u);
            let c_w3 = select(val_w3.x, val_w3.z, c == 2u);
            let c_e3 = select(val_e3.x, val_e3.z, c == 2u);

            let g_n = textureLoad(tex2_read, clamp_pos(n), 0).x;
            let g_s = textureLoad(tex2_read, clamp_pos(s), 0).x;
            let g_w = textureLoad(tex2_read, clamp_pos(w), 0).x;
            let g_e = textureLoad(tex2_read, clamp_pos(e), 0).x;

            let sn_abs = abs(c_n - c_s);
            let ew_abs = abs(c_w - c_e);

            let n_grad = n1 + sn_abs + abs(c_n - c_n3);
            let s_grad = s1 + sn_abs + abs(c_s - c_s3);
            let w_grad = w1 + ew_abs + abs(c_w - c_w3);
            let e_grad = e1 + ew_abs + abs(c_e - c_e3);

            let n_est = c_n - g_n;
            let s_est = c_s - g_s;
            let w_est = c_w - g_w;
            let e_est = c_e - g_e;

            let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
            let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);

            let val = g0 + mix(v_est, h_est, vh_disc);
            if c == 0u { r_val = val; } else { b_val = val; }
        }
    }

    var camera_rgb = vec3<f32>(r_val, g0, b_val);
    let r_clip = select(0.0, 1.0, is_raw_clipped(pos));
    let b_clip = select(0.0, 1.0, is_raw_clipped(pos));
    let final_clip = clip * 10.0 + r_clip * 1.0 + b_clip * 100.0;

    camera_rgb = apply_wb(camera_rgb);
    camera_rgb = reconstruct_sensor_highlights(camera_rgb, final_clip);

    var rgb = cam_to_working(camera_rgb);
    rgb = map_negative_gamut(rgb);

    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));

    rgb = apply_contrast(rgb);
    rgb = apply_saturation_vibrance(rgb);

    textureStore(out_tex, pos, vec4<f32>(display_render(rgb), 1.0));
}