#import auraw::common as Common
#import auraw::noise as Noise
#import auraw::noise_ca_finish as NoiseCaFinish

@group(0) @binding(26) var mark_high_read: texture_2d<f32>;
@group(0) @binding(10) var xtrans_scene_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(23) var xtrans_dual_low_read: texture_2d<f32>;

fn xt_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(Common::camera_uniforms.width) && pos.y < i32(Common::camera_uniforms.height);
}

fn xt_high(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(mark_high_read, Common::clamp_pos(pos), 0).rgb;
}

fn xt_uv(rgb: vec3<f32>) -> vec2<f32> {
    let y = 0.2627 * rgb.r + 0.6780 * rgb.g + 0.0593 * rgb.b;
    return vec2<f32>(0.56433 * (rgb.b - y), 0.67815 * (rgb.r - y));
}

fn xt_from_yuv(y: f32, uv: vec2<f32>) -> vec3<f32> {
    let b = y + uv.x / 0.56433;
    let r = y + uv.y / 0.67815;
    let g = (y - 0.2627 * r - 0.0593 * b) / 0.6780;
    return vec3<f32>(r, g, b);
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
    let center_opponents = xt_uv(center_rgb);
    var low_sum = vec2<f32>(0.0);
    var low_weight = 0.0;
    var carrier_x = vec4<f32>(0.0);
    var carrier_y = vec4<f32>(0.0);
    var carrier_diag = vec4<f32>(0.0);
    var carrier_antidiag = vec4<f32>(0.0);
    var carrier_weight = 0.0;

    for (var dy = -6; dy <= 6; dy = dy + 1) {
        let wy = f32(7 - abs(dy));
        let py = xt_phase6(dy);
        for (var dx = -6; dx <= 6; dx = dx + 1) {
            let wx = f32(7 - abs(dx));
            let weight = wx * wy;
            let uv = xt_uv(xt_high(pos + vec2<i32>(dx, dy)));
            let delta = uv - center_opponents;
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
    let center_signal = dot(center_rgb, vec3<f32>(0.2627, 0.6780, 0.0593));
    let low_rgb = xt_from_yuv(center_signal, low);
    let luma_support = abs(center_signal - dot(low_rgb, vec3<f32>(0.2627, 0.6780, 0.0593)));
    let spectral_energy = length(carrier_alias);
    let reject = smoothstep(0.0015, 0.030, max(spectral_energy - 0.35 * luma_support, 0.0));
    let corrected = center_opponents - reject * carrier_alias;
    return mix(center_opponents, corrected, clamp(Common::camera_uniforms.frequency_chroma, 0.0, 1.0));
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

fn xt_reference_false_color_guard(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let uv0 = xt_uv(rgb);
    let uvn = xt_uv(xt_high(pos + vec2<i32>(0, -1)));
    let uvs = xt_uv(xt_high(pos + vec2<i32>(0,  1)));
    let uvw = xt_uv(xt_high(pos + vec2<i32>(-1, 0)));
    let uve = xt_uv(xt_high(pos + vec2<i32>( 1, 0)));
    let median = vec2<f32>(
        xt_median5(uv0.x, uvn.x, uvs.x, uvw.x, uve.x),
        xt_median5(uv0.y, uvn.y, uvs.y, uvw.y, uve.y),
    );
    let disagreement = length(uv0 - median);
    let strength = 0.50 * smoothstep(0.006, 0.055, disagreement);
    if strength <= 1e-6 { return rgb; }
    let y = dot(rgb, vec3<f32>(0.2627, 0.6780, 0.0593));
    return xt_from_yuv(y, mix(uv0, median, strength));
}

fn xt_frequency_chroma(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let uv0 = xt_frequency_uv(pos);
    let uvn = xt_uv(xt_high(pos + vec2<i32>(0, -1)));
    let uvs = xt_uv(xt_high(pos + vec2<i32>(0,  1)));
    let uvw = xt_uv(xt_high(pos + vec2<i32>(-1, 0)));
    let uve = xt_uv(xt_high(pos + vec2<i32>( 1, 0)));
    let median = vec2<f32>(
        xt_median5(uv0.x, uvn.x, uvs.x, uvw.x, uve.x),
        xt_median5(uv0.y, uvn.y, uvs.y, uvw.y, uve.y),
    );
    let strength = clamp(Common::camera_uniforms.frequency_chroma, 0.0, 1.0);
    let uv = mix(xt_uv(rgb), mix(uv0, median, 0.35), strength);
    let y = dot(rgb, vec3<f32>(0.2627, 0.6780, 0.0593));
    return xt_from_yuv(y, uv);
}

fn xt_dual_low(pos: vec2<i32>) -> vec4<f32> {
    return textureLoad(xtrans_dual_low_read, Common::clamp_pos(pos), 0);
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
    let ss = xt_luma(pos + vec2<i32>( 0,  1));
    let se = xt_luma(pos + vec2<i32>( 1,  1));
    let gx = 3.0 * (ne - nw) + 10.0 * (e - w) + 3.0 * (se - sw);
    let gy = 3.0 * (sw - nw) + 10.0 * (ss - n) + 3.0 * (se - ne);
    return sqrt(gx * gx + gy * gy) / 32.0;
}

fn xt_gaussian5_weight(offset: i32) -> f32 {
    let a = abs(offset);
    if a == 0 { return 6.0; }
    if a == 1 { return 4.0; }
    return 1.0;
}

fn xt_dual_weight(pos: vec2<i32>, reference: vec3<f32>, low: vec4<f32>) -> f32 {
    var detail = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        let wy = xt_gaussian5_weight(dy);
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            detail += wy * xt_gaussian5_weight(dx)
                * xt_scharr(Common::clamp_pos(pos + vec2<i32>(dx, dy)));
        }
    }
    detail /= 256.0;
    let threshold = 0.005 * pow(max(Common::camera_uniforms.dual_threshold, 0.0), 1.1);
    if threshold <= 1e-7 { return 1.0; }

    let variance = Noise::nr_component_variance(0.5 * (reference + low.rgb));
    let noise_floor = 2.25 * sqrt(max(variance.x, 1e-10));
    let detail_signal = max(detail - noise_floor, 0.0);
    let edge_confidence = smoothstep(
        threshold,
        max(4.0 * threshold, threshold + 1e-5),
        detail_signal,
    );
    let opponent_delta = length(xt_uv(reference) - xt_uv(low.rgb));
    let opponent_sigma = max(sqrt(max(variance.y, 1e-10)), 0.0015);
    let disagreement = smoothstep(3.0 * opponent_sigma, 8.0 * opponent_sigma, opponent_delta);
    let low_confidence = clamp(low.a, 0.0, 1.0);
    let alias_penalty = 0.45 * disagreement * (1.0 - 0.35 * edge_confidence);
    let high_confidence = clamp(edge_confidence * (1.0 - alias_penalty), 0.0, 1.0);
    return clamp(1.0 - low_confidence * (1.0 - high_confidence), 0.0, 1.0);
}

override fn NoiseCaFinish::finish_reference_at(pos: vec2<i32>) -> vec3<f32> {
    return xt_high(pos);
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_demosaic_finish(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let reference = xt_high(pos);
    var camera_rgb = reference;
    if Common::camera_uniforms.demosaic_mode >= 1.5 {
        let low = xt_dual_low(pos);
        camera_rgb = mix(low.rgb, reference, xt_dual_weight(pos, reference, low));
    } else if Common::camera_uniforms.demosaic_mode >= 0.5 {
        camera_rgb = xt_frequency_chroma(pos, reference);
    } else {
        camera_rgb = xt_reference_false_color_guard(pos, reference);
    }
    camera_rgb = NoiseCaFinish::finish_apply_sensor_denoise(pos, camera_rgb);
    camera_rgb = NoiseCaFinish::finish_apply_ca(pos, camera_rgb);
    textureStore(xtrans_scene_write, pos, vec4<f32>(camera_rgb, 1.0));
}
