@group(0) @binding(6) var tex2_read: texture_2d<f32>;
@group(0) @binding(7) var tex3_write: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn pass3_rb_opposite(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = color_at(pos);

    let g0 = textureLoad(tex2_read, pos, 0).x;
    let clip = textureLoad(tex2_read, pos, 0).w;

    var diffR = 0.0;
    var diffB = 0.0;

    if cc == 0u {
        diffR = raw_cfa_at(pos) - g0;
        
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
        let pq_disc = select(pq_c, pq_n, abs(0.5 - pq_c) < abs(0.5 - pq_n));

        diffB = mix(p_est, q_est, pq_disc);
        
    } else if cc == 2u {
        diffB = raw_cfa_at(pos) - g0;
        
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
        let pq_disc = select(pq_c, pq_n, abs(0.5 - pq_c) < abs(0.5 - pq_n));

        diffR = mix(p_est, q_est, pq_disc);
    }

    textureStore(tex3_write, pos, vec4<f32>(diffR, diffB, 0.0, clip));
}


