// Bayer RCD stage 4. The 9-pixel exterior is deliberately produced by PPG,
// matching darktable's RCD margin policy; RCD samples never rely on clamped
// coordinates. The same reference result feeds optional FDC and dual modes.
@group(0) @binding(7) var tex2_read: texture_2d<f32>;
@group(0) @binding(9) var tex3_read: texture_2d<f32>;
@group(0) @binding(10) var scene_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

const RCD_MARGIN: i32 = 9;

fn demosaic_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(params.width) && pos.y < i32(params.height);
}

fn rcd_has_reference_margin(pos: vec2<i32>) -> bool {
    return pos.x >= RCD_MARGIN && pos.y >= RCD_MARGIN
        && pos.x < i32(params.width) - RCD_MARGIN
        && pos.y < i32(params.height) - RCD_MARGIN;
}

fn green_plane_at(pos: vec2<i32>) -> f32 {
    return textureLoad(tex2_read, clamp_pos(pos), 0).x;
}

// PPG fallback used for the complete exterior margin and for small images.
// It preserves measured samples and only clamps where the border algorithm
// itself requires unavailable sensor samples.
fn ppg_green_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    if color_at(p) == 1u { return raw_cfa_at(p); }

    let c = raw_cfa_at(p);
    let gm = raw_cfa_at(p + vec2<i32>(-1, 0));
    let gp = raw_cfa_at(p + vec2<i32>( 1, 0));
    let gv_m = raw_cfa_at(p + vec2<i32>(0, -1));
    let gv_p = raw_cfa_at(p + vec2<i32>(0,  1));
    let cm2 = raw_cfa_at(p + vec2<i32>(-2, 0));
    let cp2 = raw_cfa_at(p + vec2<i32>( 2, 0));
    let cv_m2 = raw_cfa_at(p + vec2<i32>(0, -2));
    let cv_p2 = raw_cfa_at(p + vec2<i32>(0,  2));

    let dh = abs(cm2 - cp2) + abs(gm - gp);
    let dv = abs(cv_m2 - cv_p2) + abs(gv_m - gv_p);
    let gh = 0.5 * (gm + gp) + 0.25 * (2.0 * c - cm2 - cp2);
    let gv = 0.5 * (gv_m + gv_p) + 0.25 * (2.0 * c - cv_m2 - cv_p2);
    var g = select(gv, gh, dh < dv);
    if abs(dh - dv) < 1e-6 { g = 0.5 * (gh + gv); }
    let lo = min(min(gm, gp), min(gv_m, gv_p));
    let hi = max(max(gm, gp), max(gv_m, gv_p));
    return clamp(g, lo, hi);
}

fn ppg_difference_pair(pos: vec2<i32>, channel: u32, axis: vec2<i32>) -> f32 {
    let a = clamp_pos(pos - axis);
    let b = clamp_pos(pos + axis);
    var sum = 0.0;
    var count = 0.0;
    if color_at(a) == channel {
        sum += raw_cfa_at(a) - ppg_green_at(a);
        count += 1.0;
    }
    if color_at(b) == channel {
        sum += raw_cfa_at(b) - ppg_green_at(b);
        count += 1.0;
    }
    return sum / max(count, 1.0);
}

fn ppg_rgb_at(pos: vec2<i32>) -> vec3<f32> {
    let p = clamp_pos(pos);
    let cc = color_at(p);
    let g = ppg_green_at(p);
    var r = g;
    var b = g;
    if cc == 0u {
        r = raw_cfa_at(p);
        var d = 0.0;
        for (var sy = -1; sy <= 1; sy = sy + 2) {
            for (var sx = -1; sx <= 1; sx = sx + 2) {
                let q = clamp_pos(p + vec2<i32>(sx, sy));
                d += raw_cfa_at(q) - ppg_green_at(q);
            }
        }
        b = g + 0.25 * d;
    } else if cc == 2u {
        b = raw_cfa_at(p);
        var d = 0.0;
        for (var sy = -1; sy <= 1; sy = sy + 2) {
            for (var sx = -1; sx <= 1; sx = sx + 2) {
                let q = clamp_pos(p + vec2<i32>(sx, sy));
                d += raw_cfa_at(q) - ppg_green_at(q);
            }
        }
        r = g + 0.25 * d;
    } else {
        let horizontal = vec2<i32>(1, 0);
        let vertical = vec2<i32>(0, 1);
        if color_at(clamp_pos(p + horizontal)) == 0u {
            r = g + ppg_difference_pair(p, 0u, horizontal);
            b = g + ppg_difference_pair(p, 2u, vertical);
        } else {
            r = g + ppg_difference_pair(p, 0u, vertical);
            b = g + ppg_difference_pair(p, 2u, horizontal);
        }
    }
    return vec3<f32>(r, g, b);
}

fn rcd_green_channel(pos: vec2<i32>, channel: u32) -> f32 {
    let eps = 1e-5;
    let g0 = green_plane_at(pos);
    let n = pos + vec2<i32>(0, -1);
    let s = pos + vec2<i32>(0,  1);
    let w = pos + vec2<i32>(-1, 0);
    let e = pos + vec2<i32>( 1, 0);
    let n2 = pos + vec2<i32>(0, -2);
    let s2 = pos + vec2<i32>(0,  2);
    let w2 = pos + vec2<i32>(-2, 0);
    let e2 = pos + vec2<i32>( 2, 0);
    let n3 = pos + vec2<i32>(0, -3);
    let s3 = pos + vec2<i32>(0,  3);
    let w3 = pos + vec2<i32>(-3, 0);
    let e3 = pos + vec2<i32>( 3, 0);

    let rn = textureLoad(tex3_read, n, 0).rgb;
    let rs = textureLoad(tex3_read, s, 0).rgb;
    let rw = textureLoad(tex3_read, w, 0).rgb;
    let re = textureLoad(tex3_read, e, 0).rgb;
    let rn3 = textureLoad(tex3_read, n3, 0).rgb;
    let rs3 = textureLoad(tex3_read, s3, 0).rgb;
    let rw3 = textureLoad(tex3_read, w3, 0).rgb;
    let re3 = textureLoad(tex3_read, e3, 0).rgb;
    let cn = select(rn.r, rn.b, channel == 2u);
    let cs = select(rs.r, rs.b, channel == 2u);
    let cw = select(rw.r, rw.b, channel == 2u);
    let ce = select(re.r, re.b, channel == 2u);
    let cn3 = select(rn3.r, rn3.b, channel == 2u);
    let cs3 = select(rs3.r, rs3.b, channel == 2u);
    let cw3 = select(rw3.r, rw3.b, channel == 2u);
    let ce3 = select(re3.r, re3.b, channel == 2u);

    let sn_abs = abs(cn - cs);
    let ew_abs = abs(cw - ce);
    let n_grad = eps + abs(g0 - green_plane_at(n2)) + sn_abs + abs(cn - cn3);
    let s_grad = eps + abs(g0 - green_plane_at(s2)) + sn_abs + abs(cs - cs3);
    let w_grad = eps + abs(g0 - green_plane_at(w2)) + ew_abs + abs(cw - cw3);
    let e_grad = eps + abs(g0 - green_plane_at(e2)) + ew_abs + abs(ce - ce3);
    let n_est = cn - green_plane_at(n);
    let s_est = cs - green_plane_at(s);
    let w_est = cw - green_plane_at(w);
    let e_est = ce - green_plane_at(e);
    let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
    let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);

    let vh_center = textureLoad(tex2_read, pos, 0).y;
    let vh_neighbours = 0.25 * (
        textureLoad(tex2_read, pos + vec2<i32>(-1, -1), 0).y
      + textureLoad(tex2_read, pos + vec2<i32>( 1, -1), 0).y
      + textureLoad(tex2_read, pos + vec2<i32>(-1,  1), 0).y
      + textureLoad(tex2_read, pos + vec2<i32>( 1,  1), 0).y
    );
    let vh = clamp(select(vh_center, vh_neighbours,
        abs(0.5 - vh_center) < abs(0.5 - vh_neighbours)), 0.0, 1.0);
    return g0 + mix(v_est, h_est, vh);
}

fn rcd_reference_at(pos: vec2<i32>) -> vec3<f32> {
    if !rcd_has_reference_margin(pos) { return ppg_rgb_at(pos); }
    let cc = color_at(pos);
    if cc != 1u { return textureLoad(tex3_read, pos, 0).rgb; }
    let g = green_plane_at(pos);
    let r = rcd_green_channel(pos, 0u);
    let b = rcd_green_channel(pos, 2u);
    return vec3<f32>(r, g, b);
}

fn bayer_uv(rgb: vec3<f32>) -> vec2<f32> {
    let y = dot(rgb, vec3<f32>(0.2627, 0.6780, 0.0593));
    return vec2<f32>(0.56433 * (rgb.b - y), 0.67815 * (rgb.r - y));
}

fn bayer_from_yuv(y: f32, uv: vec2<f32>) -> vec3<f32> {
    let b = y + uv.x / 0.56433;
    let r = y + uv.y / 0.67815;
    let g = (y - 0.2627 * r - 0.0593 * b) / 0.6780;
    return vec3<f32>(r, g, b);
}

fn bayer_median5(a: f32, b: f32, c: f32, d: f32, e: f32) -> f32 {
    var v0 = a;
    var v1 = b;
    var v2 = c;
    var v3 = d;
    var v4 = e;
    var t = 0.0;
    if v0 > v1 { t = v0; v0 = v1; v1 = t; }
    if v3 > v4 { t = v3; v3 = v4; v4 = t; }
    if v0 > v2 { t = v0; v0 = v2; v2 = t; }
    if v1 > v2 { t = v1; v1 = v2; v2 = t; }
    if v0 > v3 { t = v0; v0 = v3; v3 = t; }
    if v2 > v3 { t = v2; v2 = v3; v3 = t; }
    if v1 > v4 { t = v1; v1 = v4; v4 = t; }
    if v1 > v2 { t = v1; v1 = v2; v2 = t; }
    if v3 > v4 { t = v3; v3 = v4; v4 = t; }
    if v2 > v3 { t = v2; v2 = v3; v3 = t; }
    return v2;
}

fn bayer_reference_false_color_guard(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    // RCD is intentionally detail preserving, but one-pixel chroma outliers can
    // survive on hair, fabric, and other near-Nyquist detail. A five-sample
    // cross median removes only isolated U/V excursions while keeping the
    // center luminance untouched. The adaptive blend is zero for coherent real
    // color and rises only when the center chroma disagrees strongly with all
    // four immediate neighbors.
    let uv0 = bayer_uv(rgb);
    let uvn = bayer_uv(rcd_reference_at(clamp_pos(pos + vec2<i32>(0, -1))));
    let uvs = bayer_uv(rcd_reference_at(clamp_pos(pos + vec2<i32>(0,  1))));
    let uvw = bayer_uv(rcd_reference_at(clamp_pos(pos + vec2<i32>(-1, 0))));
    let uve = bayer_uv(rcd_reference_at(clamp_pos(pos + vec2<i32>( 1, 0))));
    let median = vec2<f32>(
        bayer_median5(uv0.x, uvn.x, uvs.x, uvw.x, uve.x),
        bayer_median5(uv0.y, uvn.y, uvs.y, uvw.y, uve.y),
    );
    let disagreement = length(uv0 - median);
    let strength = 0.55 * smoothstep(0.006, 0.055, disagreement);
    if strength <= 1e-6 { return rgb; }
    let y = dot(rgb, vec3<f32>(0.2627, 0.6780, 0.0593));
    return bayer_from_yuv(y, mix(uv0, median, strength));
}

fn bayer_phase2(offset: i32) -> f32 {
    return select(1.0, -1.0, (abs(offset) & 1) == 1);
}

fn frequency_chroma_at(pos: vec2<i32>, center: vec3<f32>) -> vec3<f32> {
    let center_uv = bayer_uv(center);
    var carrier_x = vec2<f32>(0.0);
    var carrier_y = vec2<f32>(0.0);
    var carrier_xy = vec2<f32>(0.0);
    var carrier_weight = 0.0;

    // Analyze the three Bayer chroma carriers over a 13x13 apodized window.
    // Removing only coherent Nyquist-period chroma protects real low-frequency
    // colour while suppressing zippering and maze aliases.
    for (var dy = -6; dy <= 6; dy = dy + 1) {
        let wy = f32(7 - abs(dy));
        let py = bayer_phase2(dy);
        for (var dx = -6; dx <= 6; dx = dx + 1) {
            let weight = wy * f32(7 - abs(dx));
            let px = bayer_phase2(dx);
            let uv = bayer_uv(rcd_reference_at(pos + vec2<i32>(dx, dy)));
            let delta = uv - center_uv;
            carrier_x += weight * px * delta;
            carrier_y += weight * py * delta;
            carrier_xy += weight * px * py * delta;
            carrier_weight += weight;
        }
    }

    let carrier_alias = (carrier_x + carrier_y + carrier_xy)
        / max(3.0 * carrier_weight, 1e-6);
    let n = rcd_reference_at(pos + vec2<i32>(0, -1));
    let s = rcd_reference_at(pos + vec2<i32>(0,  1));
    let w = rcd_reference_at(pos + vec2<i32>(-1, 0));
    let e = rcd_reference_at(pos + vec2<i32>( 1, 0));
    let center_y = dot(center, vec3<f32>(0.2627, 0.6780, 0.0593));
    let luma_high = abs(4.0 * center_y
        - dot(n + s + w + e, vec3<f32>(0.2627, 0.6780, 0.0593)));
    let spectral_energy = max(length(carrier_alias) - 0.25 * luma_high, 0.0);
    let reject = smoothstep(0.0015, 0.030, spectral_energy)
        * clamp(params.frequency_chroma, 0.0, 1.0);
    return bayer_from_yuv(center_y, center_uv - reject * carrier_alias);
}

fn low_detail_rgb_at(pos: vec2<i32>) -> vec3<f32> {
    // VNG-style low-detail reconstruction: measured samples are accumulated
    // per channel with a green-edge range weight, then colour-smoothed.
    let center_g = ppg_green_at(pos);
    var sum = vec3<f32>(0.0);
    var weights = vec3<f32>(0.0);
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let q = pos + vec2<i32>(dx, dy);
            if !demosaic_in_bounds(q) { continue; }
            let channel = color_at(q);
            let spatial = 1.0 / (1.0 + f32(dx * dx + dy * dy));
            let range = 1.0 / (1.0 + 16.0 * abs(ppg_green_at(q) - center_g));
            let weight = spatial * range;
            if channel == 0u { sum.r += raw_cfa_at(q) * weight; weights.r += weight; }
            if channel == 1u { sum.g += raw_cfa_at(q) * weight; weights.g += weight; }
            if channel == 2u { sum.b += raw_cfa_at(q) * weight; weights.b += weight; }
        }
    }
    return sum / max(weights, vec3<f32>(1e-6));
}

fn reference_luma_at(pos: vec2<i32>) -> f32 {
    return dot(rcd_reference_at(clamp_pos(pos)), vec3<f32>(0.25, 0.50, 0.25));
}

fn scharr_detail_at(pos: vec2<i32>) -> f32 {
    let nw = reference_luma_at(pos + vec2<i32>(-1, -1));
    let n  = reference_luma_at(pos + vec2<i32>( 0, -1));
    let ne = reference_luma_at(pos + vec2<i32>( 1, -1));
    let w  = reference_luma_at(pos + vec2<i32>(-1,  0));
    let e  = reference_luma_at(pos + vec2<i32>( 1,  0));
    let sw = reference_luma_at(pos + vec2<i32>(-1,  1));
    let s  = reference_luma_at(pos + vec2<i32>( 0,  1));
    let se = reference_luma_at(pos + vec2<i32>( 1,  1));
    let gx = 3.0 * (ne - nw) + 10.0 * (e - w) + 3.0 * (se - sw);
    let gy = 3.0 * (sw - nw) + 10.0 * (s - n) + 3.0 * (se - ne);
    return sqrt(gx * gx + gy * gy) / 32.0;
}

fn gaussian5_weight(offset: i32) -> f32 {
    let a = abs(offset);
    if a == 0 { return 6.0; }
    if a == 1 { return 4.0; }
    return 1.0;
}

fn dual_high_weight(pos: vec2<i32>) -> f32 {
    var detail = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        let wy = gaussian5_weight(dy);
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            detail += wy * gaussian5_weight(dx)
                * scharr_detail_at(clamp_pos(pos + vec2<i32>(dx, dy)));
        }
    }
    detail /= 256.0;
    let threshold = 0.005 * pow(max(params.dual_threshold, 0.0), 1.1);
    if threshold <= 1e-7 { return 1.0; }
    return smoothstep(threshold, max(4.0 * threshold, threshold + 1e-5), detail);
}

fn warped_pos(pos: vec2<i32>, amount: f32) -> vec2<f32> {
    let local_extent = vec2<f32>(f32(params.width - 1u), f32(params.height - 1u));
    let origin = vec2<f32>(f32(params.tile_origin_x), f32(params.tile_origin_y));
    let full_extent = vec2<f32>(f32(params.full_width - 1u), f32(params.full_height - 1u));
    let center = 0.5 * full_extent;
    let global_pos = vec2<f32>(pos) + origin;
    let rel = global_pos - center;
    let norm = rel / max(center, vec2<f32>(1.0));
    let scale = 1.0 + amount * 0.001 * dot(norm, norm);
    let warped_global = clamp(center + rel * scale, vec2<f32>(0.0), full_extent);
    return clamp(warped_global - origin, vec2<f32>(0.0), local_extent);
}

fn reference_bilinear(pos: vec2<f32>) -> vec3<f32> {
    let base = floor(pos);
    let p0 = vec2<i32>(i32(base.x), i32(base.y));
    let p1 = p0 + vec2<i32>(1, 1);
    let f = fract(pos);
    let a = rcd_reference_at(clamp_pos(p0));
    let b = rcd_reference_at(clamp_pos(vec2<i32>(p1.x, p0.y)));
    let c = rcd_reference_at(clamp_pos(vec2<i32>(p0.x, p1.y)));
    let d = rcd_reference_at(clamp_pos(p1));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

fn apply_ca(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    var out = rgb;
    if abs(params.ca_red) > 1e-6 { out.r = reference_bilinear(warped_pos(pos, params.ca_red)).r; }
    if abs(params.ca_blue) > 1e-6 { out.b = reference_bilinear(warped_pos(pos, params.ca_blue)).b; }
    return out;
}

fn apply_chroma_denoise(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let strength = clamp(params.chroma_denoise, 0.0, 1.0);
    if strength <= 1e-6 { return rgb; }
    var sum = vec2<f32>(0.0);
    var weight_sum = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let sample = rcd_reference_at(clamp_pos(pos + vec2<i32>(dx, dy)));
            let spatial = 1.0 / (1.0 + f32(dx * dx + dy * dy));
            let range = 1.0 / (1.0 + 24.0 * abs(sample.g - rgb.g));
            let weight = spatial * range;
            sum += vec2<f32>(sample.r - sample.g, sample.b - sample.g) * weight;
            weight_sum += weight;
        }
    }
    let center = vec2<f32>(rgb.r - rgb.g, rgb.b - rgb.g);
    let chroma = mix(center, sum / max(weight_sum, 1e-6), strength);
    return vec3<f32>(rgb.g + chroma.x, rgb.g, rgb.g + chroma.y);
}

@compute @workgroup_size(8, 8, 1)
fn bayer_rcd_output(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let reference = rcd_reference_at(pos);
    var camera_rgb = reference;
    if params.demosaic_mode >= 1.5 {
        camera_rgb = mix(low_detail_rgb_at(pos), reference, dual_high_weight(pos));
    } else if params.demosaic_mode >= 0.5 {
        camera_rgb = frequency_chroma_at(pos, reference);
    } else {
        camera_rgb = bayer_reference_false_color_guard(pos, reference);
    }
    camera_rgb = apply_ca(pos, camera_rgb);
    camera_rgb = apply_chroma_denoise(pos, camera_rgb);
    textureStore(scene_write, pos, vec4<f32>(camera_rgb, 1.0));
}
