// X-Trans finishing stage. Mode 0 keeps the Markesteijn-3 result. Mode 1
// preserves its luminance while rejecting chroma energy at the 6x6 X-Trans
// carrier frequencies. Mode 2 blends a low-detail interpolation through a
// radius-2 Gaussian-smoothed Scharr mask, matching darktable's dual-demosaic
// threshold mapping.
@group(0) @binding(26) var mark_high_read: texture_2d<f32>;
@group(0) @binding(10) var xtrans_scene_write: texture_storage_2d<rgba16float, write>;

fn xt_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(params.width) && pos.y < i32(params.height);
}

fn xt_high(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(mark_high_read, clamp_pos(pos), 0).rgb;
}

fn xt_uv(rgb: vec3<f32>) -> vec2<f32> {
    let y = 0.2627 * rgb.r + 0.6780 * rgb.g + 0.0593 * rgb.b;
    return vec2<f32>(0.56433 * (rgb.b - y), 0.67815 * (rgb.r - y));
}

fn xt_from_yuv(y: f32, uv: vec2<f32>) -> vec3<f32> {
    let b = y + uv.x / 0.56433;
    let r = y + uv.y / 0.67815;
    let g = (y - 0.2627 * r - 0.0593 * b) / 0.6780;
    return max(vec3<f32>(r, g, b), vec3<f32>(0.0));
}

fn xt_phase6(offset: i32) -> vec2<f32> {
    let phase = ((offset % 6) + 6) % 6;
    switch phase {
        case 0: { return vec2<f32>( 1.0,  0.0); }
        case 1: { return vec2<f32>( 0.5,  0.8660254); }
        case 2: { return vec2<f32>(-0.5,  0.8660254); }
        case 3: { return vec2<f32>(-1.0,  0.0); }
        case 4: { return vec2<f32>(-0.5, -0.8660254); }
        default: { return vec2<f32>(0.5, -0.8660254); }
    }
}

fn xt_complex_mul(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x * b.x - a.y * b.y, a.x * b.y + a.y * b.x);
}

fn xt_complex_conj(a: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x, -a.y);
}

fn xt_carrier_term(delta: vec2<f32>, phase: vec2<f32>, weight: f32) -> vec4<f32> {
    return weight * vec4<f32>(
        delta.x * phase.x,
        delta.x * phase.y,
        delta.y * phase.x,
        delta.y * phase.y,
    );
}

fn xt_frequency_uv(pos: vec2<i32>) -> vec2<f32> {
    let center_rgb = xt_high(pos);
    let center_uv = xt_uv(center_rgb);
    var low_sum = vec2<f32>(0.0);
    var low_weight = 0.0;
    var carrier_x = vec4<f32>(0.0);
    var carrier_y = vec4<f32>(0.0);
    var carrier_diag = vec4<f32>(0.0);
    var carrier_antidiag = vec4<f32>(0.0);
    var carrier_weight = 0.0;

    // A 13x13 analysis window matches the support of darktable's FDC filters.
    // Triangular apodization suppresses ringing at the window boundary.
    for (var dy = -6; dy <= 6; dy = dy + 1) {
        let wy = f32(7 - abs(dy));
        let py = xt_phase6(dy);
        for (var dx = -6; dx <= 6; dx = dx + 1) {
            let wx = f32(7 - abs(dx));
            let weight = wx * wy;
            let uv = xt_uv(xt_high(pos + vec2<i32>(dx, dy)));
            let delta = uv - center_uv;
            let px = xt_phase6(dx);
            let pdiag = xt_complex_mul(px, py);
            let panti = xt_complex_mul(px, xt_complex_conj(py));
            low_sum += uv * weight;
            low_weight += weight;
            carrier_x += xt_carrier_term(delta, px, weight);
            carrier_y += xt_carrier_term(delta, py, weight);
            carrier_diag += xt_carrier_term(delta, pdiag, weight);
            carrier_antidiag += xt_carrier_term(delta, panti, weight);
            carrier_weight += weight;
        }
    }

    let low = low_sum / max(low_weight, 1e-6);
    let inv = 1.0 / max(carrier_weight, 1e-6);
    let carrier_alias = 0.25 * inv * vec2<f32>(
        carrier_x.x + carrier_y.x + carrier_diag.x + carrier_antidiag.x,
        carrier_x.z + carrier_y.z + carrier_diag.z + carrier_antidiag.z,
    );
    let center_y = dot(center_rgb, vec3<f32>(0.2627, 0.6780, 0.0593));
    let low_rgb = xt_from_yuv(center_y, low);
    let luma_support = abs(center_y - dot(low_rgb, vec3<f32>(0.2627, 0.6780, 0.0593)));
    let spectral_energy = length(carrier_alias);
    let reject = smoothstep(0.0015, 0.030, max(spectral_energy - 0.35 * luma_support, 0.0));
    let corrected = center_uv - reject * carrier_alias;
    return mix(center_uv, corrected, clamp(params.frequency_chroma, 0.0, 1.0));
}

fn xt_median5(a0: f32, a1: f32, a2: f32, a3: f32, a4: f32) -> f32 {
    var a = a0;
    var b = a1;
    var c = a2;
    var d = a3;
    var e = a4;
    var t = 0.0;
    if a > b { t = a; a = b; b = t; }
    if d > e { t = d; d = e; e = t; }
    if a > d { t = a; a = d; d = t; }
    if b > e { t = b; b = e; e = t; }
    if b > c { t = b; b = c; c = t; }
    if c > d { t = c; c = d; d = t; }
    if b > c { t = b; b = c; c = t; }
    return c;
}

fn xt_frequency_chroma(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let uv0 = xt_frequency_uv(pos);
    // The reference FDC path applies a five-sample cross median to remove
    // textile outliers. Neighbor samples use the high-detail chroma to avoid
    // repeating five 13x13 transforms per output pixel.
    let uvn = xt_uv(xt_high(pos + vec2<i32>(0, -1)));
    let uvs = xt_uv(xt_high(pos + vec2<i32>(0,  1)));
    let uvw = xt_uv(xt_high(pos + vec2<i32>(-1, 0)));
    let uve = xt_uv(xt_high(pos + vec2<i32>( 1, 0)));
    let median = vec2<f32>(
        xt_median5(uv0.x, uvn.x, uvs.x, uvw.x, uve.x),
        xt_median5(uv0.y, uvn.y, uvs.y, uvw.y, uve.y),
    );
    let strength = clamp(params.frequency_chroma, 0.0, 1.0);
    let uv = mix(xt_uv(rgb), mix(uv0, median, 0.35), strength);
    let y = dot(rgb, vec3<f32>(0.2627, 0.6780, 0.0593));
    return xt_from_yuv(y, uv);
}

fn xt_low_detail(pos: vec2<i32>) -> vec3<f32> {
    var sum = vec3<f32>(0.0);
    var weights = vec3<f32>(0.0);
    let center_green = xt_high(pos).g;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let q = pos + vec2<i32>(dx, dy);
            if !xt_in_bounds(q) { continue; }
            let channel = color_at(q);
            let spatial = 1.0 / (1.0 + f32(dx * dx + dy * dy));
            let edge = 1.0 / (1.0 + 16.0 * abs(xt_high(q).g - center_green));
            let weight = spatial * edge;
            let value = raw_cfa_at(q);
            if channel == 0u { sum.r += value * weight; weights.r += weight; }
            if channel == 1u { sum.g += value * weight; weights.g += weight; }
            if channel == 2u { sum.b += value * weight; weights.b += weight; }
        }
    }
    return max(sum / max(weights, vec3<f32>(1e-6)), vec3<f32>(0.0));
}

fn xt_luma(pos: vec2<i32>) -> f32 {
    return dot(xt_high(pos), vec3<f32>(0.25, 0.50, 0.25));
}

fn xt_scharr(pos: vec2<i32>) -> f32 {
    let nw = xt_luma(pos + vec2<i32>(-1, -1));
    let n  = xt_luma(pos + vec2<i32>( 0, -1));
    let ne = xt_luma(pos + vec2<i32>( 1, -1));
    let w  = xt_luma(pos + vec2<i32>(-1,  0));
    let e  = xt_luma(pos + vec2<i32>( 1,  0));
    let sw = xt_luma(pos + vec2<i32>(-1,  1));
    let s  = xt_luma(pos + vec2<i32>( 0,  1));
    let se = xt_luma(pos + vec2<i32>( 1,  1));
    let gx = 3.0 * (ne - nw) + 10.0 * (e - w) + 3.0 * (se - sw);
    let gy = 3.0 * (sw - nw) + 10.0 * (s - n) + 3.0 * (se - ne);
    return sqrt(gx * gx + gy * gy) / 32.0;
}

fn xt_gaussian5_weight(offset: i32) -> f32 {
    let a = abs(offset);
    if a == 0 { return 6.0; }
    if a == 1 { return 4.0; }
    return 1.0;
}

fn xt_dual_weight(pos: vec2<i32>) -> f32 {
    var detail = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        let wy = xt_gaussian5_weight(dy);
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            detail += wy * xt_gaussian5_weight(dx)
                * xt_scharr(clamp_pos(pos + vec2<i32>(dx, dy)));
        }
    }
    detail /= 256.0;
    let threshold = 0.005 * pow(max(params.dual_threshold, 0.0), 1.1);
    if threshold <= 1e-7 { return 1.0; }
    return smoothstep(threshold, max(4.0 * threshold, threshold + 1e-5), detail);
}

fn xt_warped_pos(pos: vec2<i32>, amount: f32) -> vec2<f32> {
    let local_extent = vec2<f32>(f32(params.width - 1u), f32(params.height - 1u));
    let origin = vec2<f32>(f32(params.tile_origin_x), f32(params.tile_origin_y));
    let full_extent = vec2<f32>(f32(params.full_width - 1u), f32(params.full_height - 1u));
    let center = 0.5 * full_extent;
    let global_pos = vec2<f32>(pos) + origin;
    let rel = global_pos - center;
    let norm = rel / max(center, vec2<f32>(1.0));
    let warped_global = clamp(
        center + rel * (1.0 + amount * 0.001 * dot(norm, norm)),
        vec2<f32>(0.0),
        full_extent,
    );
    return clamp(warped_global - origin, vec2<f32>(0.0), local_extent);
}

fn xt_bilinear(pos: vec2<f32>) -> vec3<f32> {
    let base = floor(pos);
    let p0 = vec2<i32>(i32(base.x), i32(base.y));
    let p1 = p0 + vec2<i32>(1, 1);
    let f = fract(pos);
    let a = xt_high(p0);
    let b = xt_high(vec2<i32>(p1.x, p0.y));
    let c = xt_high(vec2<i32>(p0.x, p1.y));
    let d = xt_high(p1);
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

fn xt_apply_ca(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    var out = rgb;
    if abs(params.ca_red) > 1e-6 { out.r = xt_bilinear(xt_warped_pos(pos, params.ca_red)).r; }
    if abs(params.ca_blue) > 1e-6 { out.b = xt_bilinear(xt_warped_pos(pos, params.ca_blue)).b; }
    return out;
}

fn xt_chroma_denoise(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let strength = clamp(params.chroma_denoise, 0.0, 1.0);
    if strength <= 1e-6 { return rgb; }
    var sum = vec2<f32>(0.0);
    var weights = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let sample = xt_high(pos + vec2<i32>(dx, dy));
            let spatial = 1.0 / (1.0 + f32(dx * dx + dy * dy));
            let range = 1.0 / (1.0 + 24.0 * abs(sample.g - rgb.g));
            let weight = spatial * range;
            sum += vec2<f32>(sample.r - sample.g, sample.b - sample.g) * weight;
            weights += weight;
        }
    }
    let center = vec2<f32>(rgb.r - rgb.g, rgb.b - rgb.g);
    let chroma = mix(center, sum / max(weights, 1e-6), strength);
    return max(vec3<f32>(rgb.g + chroma.x, rgb.g, rgb.g + chroma.y), vec3<f32>(0.0));
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_demosaic_finish(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let reference = xt_high(pos);
    var camera_rgb = reference;
    if params.demosaic_mode >= 1.5 {
        camera_rgb = mix(xt_low_detail(pos), reference, xt_dual_weight(pos));
    } else if params.demosaic_mode >= 0.5 {
        camera_rgb = xt_frequency_chroma(pos, reference);
    }
    camera_rgb = xt_apply_ca(pos, camera_rgb);
    camera_rgb = xt_chroma_denoise(pos, camera_rgb);
    textureStore(xtrans_scene_write, pos, vec4<f32>(max(camera_rgb, vec3<f32>(0.0)), 1.0));
}
