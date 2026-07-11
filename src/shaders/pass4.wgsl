// Bayer RCD stage 4: missing chroma at green sites, refinement and output.
@group(0) @binding(7) var tex2_read: texture_2d<f32>;
@group(0) @binding(9) var tex3_read: texture_2d<f32>;
@group(0) @binding(10) var scene_write: texture_storage_2d<rgba16float, write>;

fn chroma_filter_weight(dx: i32, dy: i32) -> f32 {
    if dx == 0 && dy == 0 {
        return 4.0;
    }
    if dx == 0 || dy == 0 {
        return 2.0;
    }
    return 1.0;
}

fn smoothed_chroma_diffs(pos: vec2<i32>) -> vec2<f32> {
    let center = clamp_pos(pos);
    let g_center = textureLoad(tex2_read, center, 0).x;
    var sum = vec2<f32>(0.0);
    var sum_w = 0.0;

    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let p = clamp_pos(pos + vec2<i32>(dx, dy));
            let cc = color_at(p);

            if cc == 1u {
                continue;
            }

            let diffs = textureLoad(tex3_read, p, 0).xy;
            let g = textureLoad(tex2_read, p, 0).x;
            let spatial_w = chroma_filter_weight(dx, dy);
            let edge_w = 1.0 / (1.0 + 8.0 * abs(g_center - g));
            let w = spatial_w * edge_w;

            sum += diffs * w;
            sum_w += w;
        }
    }

    if sum_w > 0.0 {
        return sum / sum_w;
    }
    return textureLoad(tex3_read, center, 0).xy;
}

fn ca_warped_pos(pos: vec2<i32>, amount: f32) -> vec2<f32> {
    let extent = vec2<f32>(f32(params.width - 1u), f32(params.height - 1u));
    let center = 0.5 * extent;
    let p = vec2<f32>(pos);
    let rel = p - center;
    let norm = rel / max(center, vec2<f32>(1.0));
    let r2 = dot(norm, norm);
    let scale = 1.0 + amount * 0.001 * r2;
    return clamp(center + rel * scale, vec2<f32>(0.0), extent);
}

fn reconstructed_channel_at(pos: vec2<i32>, channel: u32) -> f32 {
    let p = clamp_pos(pos);
    let g = textureLoad(tex2_read, p, 0).x;
    let diffs = smoothed_chroma_diffs(p);
    return select(g + diffs.x, g + diffs.y, channel == 2u);
}

fn reconstructed_channel_bilinear(pos: vec2<f32>, channel: u32) -> f32 {
    let pf = floor(pos);
    let p0 = vec2<i32>(i32(pf.x), i32(pf.y));
    let p1 = p0 + vec2<i32>(1, 1);
    let f = fract(pos);

    let v00 = reconstructed_channel_at(p0, channel);
    let v10 = reconstructed_channel_at(vec2<i32>(p1.x, p0.y), channel);
    let v01 = reconstructed_channel_at(vec2<i32>(p0.x, p1.y), channel);
    let v11 = reconstructed_channel_at(p1, channel);

    return mix(mix(v00, v10, f.x), mix(v01, v11, f.x), f.y);
}

fn apply_lateral_ca(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let use_r = abs(params.ca_red) >= 1e-6;
    let use_b = abs(params.ca_blue) >= 1e-6;
    let r = select(rgb.r, reconstructed_channel_bilinear(ca_warped_pos(pos, params.ca_red), 0u), use_r);
    let b = select(rgb.b, reconstructed_channel_bilinear(ca_warped_pos(pos, params.ca_blue), 2u), use_b);
    return vec3<f32>(
        r,
        rgb.g,
        b,
    );
}

fn apply_chroma_denoise(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let strength = clamp(params.chroma_denoise, 0.0, 1.0);
    if strength <= 1e-6 {
        return rgb;
    }

    let center_g = max(rgb.g, 0.0);
    let center_chroma = vec2<f32>(rgb.r - rgb.g, rgb.b - rgb.g);
    var sum = vec2<f32>(0.0);
    var sum_w = 0.0;

    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let p = clamp_pos(pos + vec2<i32>(dx, dy));
            let g = textureLoad(tex2_read, p, 0).x;
            let diffs = smoothed_chroma_diffs(p);
            let dist = f32(dx * dx + dy * dy);
            let spatial_w = 1.0 / (1.0 + dist);
            let range_w = 1.0 / (1.0 + 32.0 * abs(center_g - g));
            let w = spatial_w * range_w;
            sum += diffs * w;
            sum_w += w;
        }
    }

    if sum_w <= 0.0 {
        return rgb;
    }

    let shadow = 1.0 - smoothstep(0.04, 0.35, center_g);
    let effective = strength * mix(0.35, 1.0, shadow);
    let denoised_chroma = mix(center_chroma, sum / sum_w, effective);
    return vec3<f32>(rgb.g + denoised_chroma.x, rgb.g, rgb.g + denoised_chroma.y);
}

fn refine_green_with_chroma(pos: vec2<i32>, cc: u32, clip: f32, g0: f32, diffR: f32, diffB: f32) -> f32 {
    if cc == 1u || clip > 0.5 {
        return g0;
    }

    let raw = raw_cfa_at(pos);
    let chroma = select(diffR, diffB, cc == 2u);
    let candidate = raw - chroma;

    let g_n = textureLoad(tex2_read, clamp_pos(pos + vec2<i32>(0, -1)), 0).x;
    let g_s = textureLoad(tex2_read, clamp_pos(pos + vec2<i32>(0, 1)), 0).x;
    let g_w = textureLoad(tex2_read, clamp_pos(pos + vec2<i32>(-1, 0)), 0).x;
    let g_e = textureLoad(tex2_read, clamp_pos(pos + vec2<i32>(1, 0)), 0).x;
    let lo = min(min(g_n, g_s), min(g_w, g_e));
    let hi = max(max(g_n, g_s), max(g_w, g_e));
    let range = max(hi - lo, 1e-4);
    let bounded = clamp(candidate, lo - 0.25 * range - 0.02, hi + 0.25 * range + 0.02);

    return mix(g0, bounded, 0.65);
}

@compute @workgroup_size(8, 8, 1)
fn bayer_rcd_output(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let cc = color_at(pos);

    let g0 = textureLoad(tex2_read, pos, 0).x;
    let clip = textureLoad(tex2_read, pos, 0).w;

    var diffR = 0.0;
    var diffB = 0.0;

    if cc == 0u || cc == 2u {
        let diffs = smoothed_chroma_diffs(pos);
        diffR = diffs.x;
        diffB = diffs.y;
    } else {
        let vh_c = textureLoad(tex2_read, pos, 0).y;
        let vh_nw = textureLoad(tex2_read, clamp_pos(pos + vec2<i32>(-1, -1)), 0).y;
        let vh_ne = textureLoad(tex2_read, clamp_pos(pos + vec2<i32>(1, -1)), 0).y;
        let vh_sw = textureLoad(tex2_read, clamp_pos(pos + vec2<i32>(-1, 1)), 0).y;
        let vh_se = textureLoad(tex2_read, clamp_pos(pos + vec2<i32>(1, 1)), 0).y;
        let vh_n = 0.25 * (vh_nw + vh_ne + vh_sw + vh_se);
        let vh_disc = select(vh_c, vh_n, abs(0.5 - vh_c) < abs(0.5 - vh_n));

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

        let val_n  = smoothed_chroma_diffs(n);
        let val_s  = smoothed_chroma_diffs(s);
        let val_w  = smoothed_chroma_diffs(w);
        let val_e  = smoothed_chroma_diffs(e);
        let val_n3 = smoothed_chroma_diffs(n3);
        let val_s3 = smoothed_chroma_diffs(s3);
        let val_w3 = smoothed_chroma_diffs(w3);
        let val_e3 = smoothed_chroma_diffs(e3);

        for (var c = 0u; c <= 2u; c = c + 2u) {
            let c_n  = select(val_n.x,  val_n.y,  c == 2u);
            let c_s  = select(val_s.x,  val_s.y,  c == 2u);
            let c_w  = select(val_w.x,  val_w.y,  c == 2u);
            let c_e  = select(val_e.x,  val_e.y,  c == 2u);
            let c_n3 = select(val_n3.x, val_n3.y, c == 2u);
            let c_s3 = select(val_s3.x, val_s3.y, c == 2u);
            let c_w3 = select(val_w3.x, val_w3.y, c == 2u);
            let c_e3 = select(val_e3.x, val_e3.y, c == 2u);

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

    let g_refined = refine_green_with_chroma(pos, cc, clip, g0, diffR, diffB);
    let r_val = g_refined + diffR;
    let b_val = g_refined + diffB;

    // Highlight reconstruction already happened on the raw CFA texture before
    // this demosaic.  Reconstructing a second time from RGB here was the old
    // source of coloured halos and grey patches around clipped highlights.
    var camera_rgb = apply_lateral_ca(pos, vec3<f32>(r_val, g_refined, b_val));
    camera_rgb = apply_chroma_denoise(pos, camera_rgb);
    textureStore(scene_write, pos, vec4<f32>(max(camera_rgb, vec3<f32>(0.0)), 1.0));
}
