// rcd_demosaic.wgsl
//
// RCD (Ratio of Convolution Differences) demosaic for Bayer CFA sensors.
// Implements difference-domain (C-G) interpolation with confidence-blended directions,
// local homogeneity detection, adaptive chroma smoothing, and residual refinement.

struct Params {
    black: f32,
    exposure: f32,
    _pad0: f32,
    _pad1: f32,
    wb: vec4<f32>,
    cam_to_srgb_0: vec4<f32>,
    cam_to_srgb_1: vec4<f32>,
    cam_to_srgb_2: vec4<f32>,
    width: u32,
    height: u32,
    cfa_pattern: u32,
    black_level: f32,
    white_level: f32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var raw_tex: texture_2d<f32>;
@group(0) @binding(2) var vh_dir_tex: texture_storage_2d<r32float, read_write>;
@group(0) @binding(3) var lpf_tex: texture_storage_2d<r32float, read_write>;
@group(0) @binding(4) var pq_dir_tex: texture_storage_2d<r32float, read_write>;

// Pass 3 output (Destination for green_fill)
@group(1) @binding(0) var rgb_a_tex: texture_storage_2d<rgba32float, write>;

// Consolidated Ping-Pong Group (Reused by Pass 5, Pass 6, Pass 7, and Pass 8)
@group(2) @binding(0) var rgb_in: texture_2d<f32>;
@group(2) @binding(1) var rgb_out: texture_storage_2d<rgba32float, write>;

const BORDER: i32 = 4;
const GREEN_BORDER: i32 = 3;
const eps: f32 = 1e-5;
const epssq: f32 = 1e-10;

// Auditable color channel indices
const RED: i32 = 0;
const GREEN: i32 = 1;
const BLUE: i32 = 2;

fn sqf(x: f32) -> f32 { return x * x; }

fn cfa_color(x: i32, y: i32) -> i32 {
    let ex = u32(x & 1);
    let ey = u32(y & 1);
    let idx = ey * 2u + ex;
    switch params.cfa_pattern {
        case 1u: { // BGGR
            if idx == 0u { return BLUE; }
            if idx == 1u { return GREEN; }
            if idx == 2u { return GREEN; }
            return RED;
        }
        case 2u: { // GRBG
            if idx == 0u { return GREEN; }
            if idx == 1u { return RED; }
            if idx == 2u { return BLUE; }
            return GREEN;
        }
        case 3u: { // GBRG
            if idx == 0u { return GREEN; }
            if idx == 1u { return BLUE; }
            if idx == 2u { return RED; }
            return GREEN;
        }
        default: { // RGGB
            if idx == 0u { return RED; }
            if idx == 1u { return GREEN; }
            if idx == 2u { return GREEN; }
            return BLUE;
        }
    }
}

fn load_raw(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return textureLoad(raw_tex, vec2<i32>(cx, cy), 0).r;
}

fn load_rgb(x: i32, y: i32) -> vec4<f32> {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return textureLoad(rgb_in, vec2<i32>(cx, cy), 0);
}

fn load_vh(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return textureLoad(vh_dir_tex, vec2<i32>(cx, cy)).r;
}

fn load_pq(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return textureLoad(pq_dir_tex, vec2<i32>(cx, cy)).r;
}

fn load_lpf(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return textureLoad(lpf_tex, vec2<i32>(cx, cy)).r;
}

fn is_border(x: i32, y: i32) -> bool {
    return x < BORDER || y < BORDER ||
           x >= i32(params.width) - BORDER ||
           y >= i32(params.height) - BORDER;
}

fn is_green_border(x: i32, y: i32) -> bool {
    return x < GREEN_BORDER || y < GREEN_BORDER ||
           x >= i32(params.width) - GREEN_BORDER ||
           y >= i32(params.height) - GREEN_BORDER;
}

// =====================================================================
// Pass 1: vh_discrimination
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn vh_discrimination(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    let v0 = sqf((load_raw(x, y-4) - load_raw(x, y-2) - load_raw(x, y) + load_raw(x, y+2)) - 3.0 * (load_raw(x, y-3) + load_raw(x, y+1)) + 6.0 * load_raw(x, y-1));
    let v1 = sqf((load_raw(x, y-3) - load_raw(x, y-1) - load_raw(x, y+1) + load_raw(x, y+3)) - 3.0 * (load_raw(x, y-2) + load_raw(x, y+2)) + 6.0 * load_raw(x, y));
    let v2 = sqf((load_raw(x, y-2) - load_raw(x, y) - load_raw(x, y+2) + load_raw(x, y+4)) - 3.0 * (load_raw(x, y-1) + load_raw(x, y+3)) + 6.0 * load_raw(x, y+1));
    let V_Stat = max(epssq, v0 + v1 + v2);

    let h0 = sqf((load_raw(x-4, y) - load_raw(x-2, y) - load_raw(x, y) + load_raw(x+2, y)) - 3.0 * (load_raw(x-3, y) + load_raw(x+1, y)) + 6.0 * load_raw(x-1, y));
    let h1 = sqf((load_raw(x-3, y) - load_raw(x-1, y) - load_raw(x+1, y) + load_raw(x+3, y)) - 3.0 * (load_raw(x-2, y) + load_raw(x+2, y)) + 6.0 * load_raw(x, y));
    let h2 = sqf((load_raw(x-2, y) - load_raw(x, y) - load_raw(x+2, y) + load_raw(x+4, y)) - 3.0 * (load_raw(x-1, y) + load_raw(x+3, y)) + 6.0 * load_raw(x+1, y));
    let H_Stat = max(epssq, h0 + h1 + h2);

    textureStore(vh_dir_tex, vec2<i32>(x, y), vec4<f32>(V_Stat / (V_Stat + H_Stat), 0.0, 0.0, 0.0));
}

// =====================================================================
// Pass 2: lpf (Normalized Binomial 2D Filter)
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn lpf(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    let lp = 0.25 * load_raw(x, y)
           + 0.125 * (load_raw(x, y-1) + load_raw(x, y+1) + load_raw(x-1, y) + load_raw(x+1, y))
           + 0.0625 * (load_raw(x-1, y-1) + load_raw(x+1, y-1) + load_raw(x-1, y+1) + load_raw(x+1, y+1));

    textureStore(lpf_tex, vec2<i32>(x, y), vec4<f32>(lp, 0.0, 0.0, 0.0));
}

// =====================================================================
// Pass 3: green_fill
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn green_fill(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    let c = cfa_color(x, y);
    let cfai = load_raw(x, y);

    var r: f32 = 0.0;
    var g: f32 = 0.0;
    var b: f32 = 0.0;

    if c == GREEN {
        g = cfai;
    } else {
        if is_border(x, y) {
            g = (load_raw(x, y-1) + load_raw(x, y+1) + load_raw(x-1, y) + load_raw(x+1, y)) * 0.25;
        } else {
            let N_Grad = eps
                + abs(load_raw(x, y-1) - load_raw(x, y+1))
                + 0.5 * abs(cfai - load_raw(x, y-2))
                + 0.5 * abs(load_raw(x, y-1) - load_raw(x, y-3));

            let S_Grad = eps
                + abs(load_raw(x, y-1) - load_raw(x, y+1))
                + 0.5 * abs(cfai - load_raw(x, y+2))
                + 0.5 * abs(load_raw(x, y+1) - load_raw(x, y+3));

            let W_Grad = eps
                + abs(load_raw(x-1, y) - load_raw(x+1, y))
                + 0.5 * abs(cfai - load_raw(x-2, y))
                + 0.5 * abs(load_raw(x-1, y) - load_raw(x-3, y));

            let E_Grad = eps
                + abs(load_raw(x-1, y) - load_raw(x+1, y))
                + 0.5 * abs(cfai - load_raw(x+2, y))
                + 0.5 * abs(load_raw(x+1, y) - load_raw(x+3, y));

            let lpfi = load_lpf(x, y);
            let N_Est = load_raw(x, y-1) * (2.0 * lpfi) / (eps + lpfi + load_lpf(x, y-1));
            let S_Est = load_raw(x, y+1) * (2.0 * lpfi) / (eps + lpfi + load_lpf(x, y+1));
            let W_Est = load_raw(x-1, y) * (2.0 * lpfi) / (eps + lpfi + load_lpf(x-1, y));
            let E_Est = load_raw(x+1, y) * (2.0 * lpfi) / (eps + lpfi + load_lpf(x+1, y));

            let V_Est = (S_Grad * N_Est + N_Grad * S_Est) / (N_Grad + S_Grad + eps);
            let H_Est = (W_Grad * E_Est + E_Grad * W_Est) / (E_Grad + W_Grad + eps);

            let vh_c = load_vh(x, y);
            let vh_n = 0.25 * (load_vh(x-1, y-1) + load_vh(x+1, y-1) + load_vh(x-1, y+1) + load_vh(x+1, y+1));
            let vh_disc = select(vh_c, vh_n, abs(0.5 - vh_c) < abs(0.5 - vh_n));

            // Homogeneity assessment
            let v_err = abs(load_raw(x, y-1) - load_raw(x, y+1)) + abs(cfai - load_raw(x, y-2));
            let h_err = abs(load_raw(x-1, y) - load_raw(x+1, y)) + abs(cfai - load_raw(x-2, y));
            let vh_diff = abs(v_err - h_err);
            let vh_sum = v_err + h_err + eps;
            let vh_conf = vh_diff / vh_sum;

            let directional_est = mix(V_Est, H_Est, vh_disc);
            let neutral_est = 0.5 * (V_Est + H_Est);
            g = mix(neutral_est, directional_est, vh_conf);
        }

        if c == RED { r = cfai; } else { b = cfai; }
    }

    textureStore(rgb_a_tex, vec2<i32>(x, y), vec4<f32>(r, g, b, 1.0));
}

// =====================================================================
// Pass 4: pq_discrimination
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn pq_discrimination(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    let p_nw = sqf((load_raw(x-4, y-4) - load_raw(x-2, y-2) - load_raw(x, y) + load_raw(x+2, y+2)) - 3.0 * (load_raw(x-3, y-3) + load_raw(x+1, y+1)) + 6.0 * load_raw(x-1, y-1));
    let p_c  = sqf((load_raw(x-3, y-3) - load_raw(x-1, y-1) - load_raw(x+1, y+1) + load_raw(x+3, y+3)) - 3.0 * (load_raw(x-2, y-2) + load_raw(x+2, y+2)) + 6.0 * load_raw(x, y));
    let p_se = sqf((load_raw(x-2, y-2) - load_raw(x, y) - load_raw(x+2, y+2) + load_raw(x+4, y+4)) - 3.0 * (load_raw(x-1, y-1) + load_raw(x+3, y+3)) + 6.0 * load_raw(x+1, y+1));
    let P_Stat = max(epssq, p_nw + p_c + p_se);

    let q_ne = sqf((load_raw(x+4, y-4) - load_raw(x+2, y-2) - load_raw(x, y) + load_raw(x-2, y+2)) - 3.0 * (load_raw(x+3, y-3) + load_raw(x-1, y+1)) + 6.0 * load_raw(x+1, y-1));
    let q_c  = sqf((load_raw(x+3, y-3) - load_raw(x+1, y-1) - load_raw(x-1, y+1) + load_raw(x-3, y+3)) - 3.0 * (load_raw(x+2, y-2) + load_raw(x-2, y+2)) + 6.0 * load_raw(x, y));
    let q_sw = sqf((load_raw(x+2, y-2) - load_raw(x, y) - load_raw(x-2, y+2) + load_raw(x-4, y+4)) - 3.0 * (load_raw(x+1, y-1) + load_raw(x-3, y+3)) + 6.0 * load_raw(x-1, y+1));
    let Q_Stat = max(epssq, q_ne + q_c + q_sw);

    textureStore(pq_dir_tex, vec2<i32>(x, y), vec4<f32>(P_Stat / (P_Stat + Q_Stat), 0.0, 0.0, 0.0));
}

// =====================================================================
// Pass 5: rb_at_rb_sites
// Difference-Domain (C - G) Interpolation
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn rb_at_rb_sites(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    let c = cfa_color(x, y);
    let here = load_rgb(x, y);
    let g_here = here.g;

    var r: f32 = here.r;
    var g: f32 = g_here;
    var b: f32 = here.b;

    if c == RED || c == BLUE {
        if is_border(x, y) {
            let avg = (load_raw(x-1, y-1) + load_raw(x+1, y-1) + load_raw(x-1, y+1) + load_raw(x+1, y+1)) * 0.25;
            let g_avg = (load_rgb(x-1, y-1).g + load_rgb(x+1, y-1).g + load_rgb(x-1, y+1).g + load_rgb(x+1, y+1).g) * 0.25;
            let val = g_here + (avg - g_avg); // Stable Difference fallback
            if c == RED { b = val; } else { r = val; }
        } else {
            let NW_raw = load_raw(x-1, y-1);
            let NE_raw = load_raw(x+1, y-1);
            let SW_raw = load_raw(x-1, y+1);
            let SE_raw = load_raw(x+1, y+1);

            let diff_NW = NW_raw - load_rgb(x-1, y-1).g;
            let diff_NE = NE_raw - load_rgb(x+1, y-1).g;
            let diff_SW = SW_raw - load_rgb(x-1, y+1).g;
            let diff_SE = SE_raw - load_rgb(x+1, y+1).g;

            let diff_NW_far = load_raw(x-3, y-3) - load_rgb(x-3, y-3).g;
            let diff_NE_far = load_raw(x+3, y-3) - load_rgb(x+3, y-3).g;
            let diff_SW_far = load_raw(x-3, y+3) - load_rgb(x-3, y+3).g;
            let diff_SE_far = load_raw(x+3, y+3) - load_rgb(x+3, y+3).g;

            let NW_Grad = eps + abs(diff_NW - diff_SE) + abs(diff_NW - diff_NW_far);
            let SE_Grad = eps + abs(diff_NW - diff_SE) + abs(diff_SE - diff_SE_far);
            let NE_Grad = eps + abs(diff_NE - diff_SW) + abs(diff_NE - diff_NE_far);
            let SW_Grad = eps + abs(diff_NE - diff_SW) + abs(diff_SW - diff_SW_far);

            let P_Est = (NW_Grad * diff_SE + SE_Grad * diff_NW) / (NW_Grad + SE_Grad + eps);
            let Q_Est = (NE_Grad * diff_SW + SW_Grad * diff_NE) / (NE_Grad + SW_Grad + eps);

            let pq_c = load_pq(x, y);
            let pq_n = 0.25 * (load_pq(x-1, y-1) + load_pq(x+1, y-1) + load_pq(x-1, y+1) + load_pq(x+1, y+1));
            let pq_disc = select(pq_c, pq_n, abs(0.5 - pq_c) < abs(0.5 - pq_n));

            // Diagonal Homogeneity Assessment
            let p_err = abs(diff_NW - diff_SE) + abs(diff_NW - diff_NW_far);
            let q_err = abs(diff_NE - diff_SW) + abs(diff_NE - diff_NE_far);
            let pq_diff = abs(p_err - q_err);
            let pq_sum = p_err + q_err + eps;
            let pq_conf = pq_diff / pq_sum;

            let directional_est = mix(P_Est, Q_Est, pq_disc);
            let neutral_est = 0.5 * (P_Est + Q_Est);
            let val = g_here + mix(neutral_est, directional_est, pq_conf);

            if c == RED { b = val; } else { r = val; }
        }
    }

    textureStore(rgb_out, vec2<i32>(x, y), vec4<f32>(r, g, b, 1.0));
}

// =====================================================================
// Pass 6: rb_at_green_sites
// Interpolates missing R/B at green sites via difference-domain estimates
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn rb_at_green_sites(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    let c = cfa_color(x, y);
    let here = load_rgb(x, y);

    var r: f32 = here.r;
    var g: f32 = here.g;
    var b: f32 = here.b;

    if c == GREEN {
        let g_here = g;

        if is_green_border(x, y) {
            let left_color = cfa_color(x-1, y);
            let h_avg = (load_raw(x-1, y) + load_raw(x+1, y)) * 0.5;
            let v_avg = (load_raw(x, y-1) + load_raw(x, y+1)) * 0.5;
            if left_color == RED {
                r = h_avg;
                b = v_avg;
            } else {
                r = v_avg;
                b = h_avg;
            }
        } else {
            let vh_c = load_vh(x, y);
            let vh_n = 0.25 * (load_vh(x-1, y-1) + load_vh(x+1, y-1) + load_vh(x-1, y+1) + load_vh(x+1, y+1));
            let vh_disc = select(vh_c, vh_n, abs(0.5 - vh_c) < abs(0.5 - vh_n));

            let N1 = eps + abs(g_here - load_rgb(x, y-2).g);
            let S1 = eps + abs(g_here - load_rgb(x, y+2).g);
            let W1 = eps + abs(g_here - load_rgb(x-2, y).g);
            let E1 = eps + abs(g_here - load_rgb(x+2, y).g);

            // Interpolate R
            {
                let diff_N = load_rgb(x, y-1).r - load_rgb(x, y-1).g;
                let diff_S = load_rgb(x, y+1).r - load_rgb(x, y+1).g;
                let diff_W = load_rgb(x-1, y).r - load_rgb(x-1, y).g;
                let diff_E = load_rgb(x+1, y).r - load_rgb(x+1, y).g;

                let diff_N_far = load_rgb(x, y-3).r - load_rgb(x, y-3).g;
                let diff_S_far = load_rgb(x, y+3).r - load_rgb(x, y+3).g;
                let diff_W_far = load_rgb(x-3, y).r - load_rgb(x-3, y).g;
                let diff_E_far = load_rgb(x+3, y).r - load_rgb(x+3, y).g;

                let SNabs = abs(diff_N - diff_S);
                let EWabs = abs(diff_W - diff_E);

                let N_Grad = N1 + SNabs + abs(diff_N - diff_N_far);
                let S_Grad = S1 + SNabs + abs(diff_S - diff_S_far);
                let W_Grad = W1 + EWabs + abs(diff_W - diff_W_far);
                let E_Grad = E1 + EWabs + abs(diff_E - diff_E_far);

                let V_Est = (N_Grad * diff_S + S_Grad * diff_N) / (N_Grad + S_Grad + eps);
                let H_Est = (E_Grad * diff_W + W_Grad * diff_E) / (E_Grad + W_Grad + eps);

                // Cardinal Homogeneity Assessment
                let v_err = N_Grad + S_Grad;
                let h_err = W_Grad + E_Grad;
                let vh_diff = abs(v_err - h_err);
                let vh_sum = v_err + h_err + eps;
                let vh_conf = vh_diff / vh_sum;

                let directional_est = mix(V_Est, H_Est, vh_disc);
                let neutral_est = 0.5 * (V_Est + H_Est);
                r = g_here + mix(neutral_est, directional_est, vh_conf);
            }

            // Interpolate B
            {
                let diff_N = load_rgb(x, y-1).b - load_rgb(x, y-1).g;
                let diff_S = load_rgb(x, y+1).b - load_rgb(x, y+1).g;
                let diff_W = load_rgb(x-1, y).b - load_rgb(x-1, y).g;
                let diff_E = load_rgb(x+1, y).b - load_rgb(x+1, y).g;

                let diff_N_far = load_rgb(x, y-3).b - load_rgb(x, y-3).g;
                let diff_S_far = load_rgb(x, y+3).b - load_rgb(x, y+3).g;
                let diff_W_far = load_rgb(x-3, y).b - load_rgb(x-3, y).g;
                let diff_E_far = load_rgb(x+3, y).b - load_rgb(x+3, y).g;

                let SNabs = abs(diff_N - diff_S);
                let EWabs = abs(diff_W - diff_E);

                let N_Grad = N1 + SNabs + abs(diff_N - diff_N_far);
                let S_Grad = S1 + SNabs + abs(diff_S - diff_S_far);
                let W_Grad = W1 + EWabs + abs(diff_W - diff_W_far);
                let E_Grad = E1 + EWabs + abs(diff_E - diff_E_far);

                let V_Est = (N_Grad * diff_S + S_Grad * diff_N) / (N_Grad + S_Grad + eps);
                let H_Est = (E_Grad * diff_W + W_Grad * diff_E) / (E_Grad + W_Grad + eps);

                // Cardinal Homogeneity Assessment
                let v_err = N_Grad + S_Grad;
                let h_err = W_Grad + E_Grad;
                let vh_diff = abs(v_err - h_err);
                let vh_sum = v_err + h_err + eps;
                let vh_conf = vh_diff / vh_sum;

                let directional_est = mix(V_Est, H_Est, vh_disc);
                let neutral_est = 0.5 * (V_Est + H_Est);
                b = g_here + mix(neutral_est, directional_est, vh_conf);
            }
        }
    }

    textureStore(rgb_out, vec2<i32>(x, y), vec4<f32>(r, g, b, 1.0));
}

// =====================================================================
// Pass 7: color_smooth (Adaptive Chroma Smoothing)
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn color_smooth(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    if is_border(x, y) {
        textureStore(rgb_out, vec2<i32>(x, y), load_rgb(x, y));
        return;
    }

    var sum_r_g: f32 = 0.0;
    var sum_b_g: f32 = 0.0;

    var min_g: f32 = 1e9;
    var max_g: f32 = -1e9;
    var min_rg: f32 = 1e9;
    var max_rg: f32 = -1e9;
    var min_bg: f32 = 1e9;
    var max_bg: f32 = -1e9;

    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let p = load_rgb(x + dx, y + dy);
            let rg = p.r - p.g;
            let bg = p.b - p.g;

            sum_r_g += rg;
            sum_b_g += bg;

            min_g = min(min_g, p.g);
            max_g = max(max_g, p.g);
            min_rg = min(min_rg, rg);
            max_rg = max(max_rg, rg);
            min_bg = min(min_bg, bg);
            max_bg = max(max_bg, bg);
        }
    }

    let p_here = load_rgb(x, y);
    let g_here = p_here.g;

    // Local edge assessment
    let g_range = max_g - min_g;
    let edge_strength = clamp(g_range / (g_here + eps), 0.0, 1.0);

    // Chroma variance assessment
    let rg_range = max_rg - min_rg;
    let bg_range = max_bg - min_bg;
    let chroma_var = max(rg_range, bg_range);
    let chroma_confidence = clamp(1.0 - (chroma_var / (abs(p_here.r - g_here) + abs(p_here.b - g_here) + eps)), 0.0, 1.0);

    // Compute localized adaptive filter strength
    let strength = clamp(0.80 * (1.0 - edge_strength) * (1.0 - chroma_confidence), 0.02, 0.80);

    let avg_r_g = sum_r_g / 9.0;
    let avg_b_g = sum_b_g / 9.0;

    let rg_here = p_here.r - g_here;
    let bg_here = p_here.b - g_here;

    let rg_filtered = mix(rg_here, avg_r_g, strength);
    let bg_filtered = mix(bg_here, avg_b_g, strength);

    textureStore(rgb_out, vec2<i32>(x, y), vec4<f32>(g_here + rg_filtered, g_here, g_here + bg_filtered, 1.0));
}

// =====================================================================
// Pass 8: chroma_refine (3x3 Register-Based Median Chroma Refinement)
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn chroma_refine(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    if is_border(x, y) {
        textureStore(rgb_out, vec2<i32>(x, y), load_rgb(x, y));
        return;
    }

    var r_g_vals: array<f32, 9>;
    var b_g_vals: array<f32, 9>;
    var idx = 0;

    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let p = load_rgb(x + dx, y + dy);
            r_g_vals[idx] = p.r - p.g;
            b_g_vals[idx] = p.b - p.g;
            idx++;
        }
    }

    // Sort networks for the 9-element arrays (optimized bubble-sort variants)
    for (var i = 0; i < 9; i++) {
        for (var j = i + 1; j < 9; j++) {
            if r_g_vals[i] > r_g_vals[j] {
                let temp = r_g_vals[i];
                r_g_vals[i] = r_g_vals[j];
                r_g_vals[j] = temp;
            }
            if b_g_vals[i] > b_g_vals[j] {
                let temp = b_g_vals[i];
                b_g_vals[i] = b_g_vals[j];
                b_g_vals[j] = temp;
            }
        }
    }

    // Retrieve median-sorted delta values
    let median_r_g = r_g_vals[4];
    let median_b_g = b_g_vals[4];

    let p_here = load_rgb(x, y);
    let g_here = p_here.g;

    textureStore(rgb_out, vec2<i32>(x, y), vec4<f32>(g_here + median_r_g, g_here, g_here + median_b_g, 1.0));
}