@group(0) @binding(4) var tex1_read: texture_2d<f32>;
@group(0) @binding(6) var tex2_read: texture_2d<f32>;
@group(0) @binding(8) var tex3_read: texture_2d<f32>;
@group(0) @binding(9) var out_tex: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8, 1)
fn pass4_rb_green_output(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = color_at(pos);

    let g0 = textureLoad(tex2_read, pos, 0).x;
    let clip = textureLoad(tex2_read, pos, 0).w;

    var diffR = 0.0;
    var diffB = 0.0;

    if cc == 0u || cc == 2u {
        let diffs = textureLoad(tex3_read, pos, 0);
        diffR = diffs.x;
        diffB = diffs.y;
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

        let g_n  = textureLoad(tex2_read, clamp_pos(n), 0).x;
        let g_s  = textureLoad(tex2_read, clamp_pos(s), 0).x;
        let g_w  = textureLoad(tex2_read, clamp_pos(w), 0).x;
        let g_e  = textureLoad(tex2_read, clamp_pos(e), 0).x;
        let g_n2 = textureLoad(tex2_read, clamp_pos(n2), 0).x;
        let g_s2 = textureLoad(tex2_read, clamp_pos(s2), 0).x;
        let g_w2 = textureLoad(tex2_read, clamp_pos(w2), 0).x;
        let g_e2 = textureLoad(tex2_read, clamp_pos(e2), 0).x;

        let g_n3 = textureLoad(tex2_read, clamp_pos(n3), 0).x;
        let g_s3 = textureLoad(tex2_read, clamp_pos(s3), 0).x;
        let g_w3 = textureLoad(tex2_read, clamp_pos(w3), 0).x;
        let g_e3 = textureLoad(tex2_read, clamp_pos(e3), 0).x;

        let n1 = eps + abs(g0 - g_n2);
        let s1 = eps + abs(g0 - g_s2);
        let w1 = eps + abs(g0 - g_w2);
        let e1 = eps + abs(g0 - g_e2);

        for (var c = 0u; c <= 2u; c = c + 2u) {
            let val_n  = textureLoad(tex3_read, clamp_pos(n), 0);
            let val_s  = textureLoad(tex3_read, clamp_pos(s), 0);
            let val_w  = textureLoad(tex3_read, clamp_pos(w), 0);
            let val_e  = textureLoad(tex3_read, clamp_pos(e), 0);
            let val_n3 = textureLoad(tex3_read, clamp_pos(n3), 0);
            let val_s3 = textureLoad(tex3_read, clamp_pos(s3), 0);
            let val_w3 = textureLoad(tex3_read, clamp_pos(w3), 0);
            let val_e3 = textureLoad(tex3_read, clamp_pos(e3), 0);

            let c_n  = select(val_n.x,  val_n.y,  c == 2u);
            let c_s  = select(val_s.x,  val_s.y,  c == 2u);
            let c_w  = select(val_w.x,  val_w.y,  c == 2u);
            let c_e  = select(val_e.x,  val_e.y,  c == 2u);
            let c_n3 = select(val_n3.x, val_n3.y, c == 2u);
            let c_s3 = select(val_s3.x, val_s3.y, c == 2u);
            let c_w3 = select(val_w3.x, val_w3.y, c == 2u);
            let c_e3 = select(val_e3.x, val_e3.y, c == 2u);

            // reconstruct absolute (non-differenced) channel values
            let rgb_n  = g_n  + c_n;
            let rgb_s  = g_s  + c_s;
            let rgb_w  = g_w  + c_w;
            let rgb_e  = g_e  + c_e;

            let sn_abs = abs(rgb_n - rgb_s);
            let ew_abs = abs(rgb_w - rgb_e);

            let n_grad = n1 + sn_abs + abs(rgb_n - (g_n3 + c_n3));
            let s_grad = s1 + sn_abs + abs(rgb_s - (g_s3 + c_s3));
            let w_grad = w1 + ew_abs + abs(rgb_w - (g_w3 + c_w3));
            let e_grad = e1 + ew_abs + abs(rgb_e - (g_e3 + c_e3));

            let v_est = (n_grad * c_s + s_grad * c_n) / (n_grad + s_grad);
            let h_est = (e_grad * c_w + w_grad * c_e) / (e_grad + w_grad);

            let val = mix(v_est, h_est, vh_disc);
            if c == 0u { diffR = val; } else { diffB = val; }
        }
    }

    let r_val = g0 + diffR;
    let b_val = g0 + diffB;

    var camera_rgb = vec3<f32>(r_val, g0, b_val);

    let r_clip = select(0.0, clip, cc == 0u);
    let g_clip = select(0.0, clip, cc == 1u);
    let b_clip = select(0.0, clip, cc == 2u);
    let final_clip = r_clip * 1.0 + g_clip * 10.0 + b_clip * 100.0;

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