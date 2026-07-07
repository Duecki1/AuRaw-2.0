// rcd_demosaic.wgsl
//
// RCD (Ratio of Convolution Differences) demosaic for Bayer CFA sensors.
//
// Five compute passes, dispatched in order within a single compute pass:
//   1. vh_discrimination  — H/V edge discriminant at every pixel
//   2. lpf                — 5×5 box low-pass of raw (threshold scaling)
//   3. green_fill         — directional green interp at R/B; copy at G
//   4. pq_discrimination  — P/Q diagonal discriminant at R/B sites
//   5. rb_fill            — directional R/B interp; completes RGB
//
// Output: rgb_b_tex holds the fully demosaiced RGBA32Float image.
//
// Border pixels (within 2 of any edge) fall back to bilinear to avoid
// 5-tap stencil artifacts — folded into green_fill and rb_fill via a
// single `if`, not a separate dispatch.

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

// Group 0: Common to all passes
@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var raw_tex: texture_2d<f32>;
@group(0) @binding(2) var vh_dir_tex: texture_storage_2d<r32float, read_write>;
@group(0) @binding(3) var lpf_tex: texture_storage_2d<r32float, read_write>;
@group(0) @binding(4) var pq_dir_tex: texture_storage_2d<r32float, read_write>;

// Group 1: Used only by green_fill (Pass 3)
@group(1) @binding(0) var rgb_a_tex: texture_storage_2d<rgba32float, write>;

// Group 2: Used only by rb_fill (Pass 5)
@group(2) @binding(0) var rgb_a_sampled: texture_2d<f32>;
@group(2) @binding(1) var rgb_b_tex: texture_storage_2d<rgba32float, write>;

const BORDER: i32 = 2;

// ---- CFA helpers -------------------------------------------------------

fn cfa_color(x: i32, y: i32) -> i32 {
    let ex = x & 1;
    let ey = y & 1;
    var tile: array<i32, 4>;
    switch params.cfa_pattern {
        case 1u: { tile = array<i32, 4>(2, 1, 1, 0); } // BGGR
        case 2u: { tile = array<i32, 4>(1, 0, 2, 1); } // GRBG
        case 3u: { tile = array<i32, 4>(1, 2, 0, 1); } // GBRG
        default: { tile = array<i32, 4>(0, 1, 1, 2); } // RGGB
    }
    return tile[ey * 2 + ex];
}

fn load_raw(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return textureLoad(raw_tex, vec2<i32>(cx, cy), 0).r;
}

fn load_rgb_a(x: i32, y: i32) -> vec4<f32> {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return textureLoad(rgb_a_sampled, vec2<i32>(cx, cy), 0);
}

fn is_border(x: i32, y: i32) -> bool {
    return x < BORDER || y < BORDER ||
           x >= i32(params.width) - BORDER ||
           y >= i32(params.height) - BORDER;
}

// =====================================================================
// Pass 1: vh_discrimination
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn vh_discrimination(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    let hm2 = load_raw(x - 2, y);
    let hm1 = load_raw(x - 1, y);
    let h0  = load_raw(x, y);
    let hp1 = load_raw(x + 1, y);
    let hp2 = load_raw(x + 2, y);

    let vm2 = load_raw(x, y - 2);
    let vm1 = load_raw(x, y - 1);
    let vp1 = load_raw(x, y + 1);
    let vp2 = load_raw(x, y + 2);

    let d1h = hp1 - hm1;
    let d2h = hm2 - 2.0 * h0 + hp2;
    let d1v = vp1 - vm1;
    let d2v = vm2 - 2.0 * h0 + vp2;

    let disc = abs(d1h) + 0.5 * abs(d2h) - abs(d1v) - 0.5 * abs(d2v);

    textureStore(vh_dir_tex, vec2<i32>(x, y), vec4<f32>(disc, 0.0, 0.0, 0.0));
}

// =====================================================================
// Pass 2: lpf
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn lpf(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    var sum = 0.0;
    for (var dy = -2; dy <= 2; dy++) {
        for (var dx = -2; dx <= 2; dx++) {
            sum += load_raw(x + dx, y + dy);
        }
    }
    textureStore(lpf_tex, vec2<i32>(x, y), vec4<f32>(sum / 25.0, 0.0, 0.0, 0.0));
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
    let raw_val = load_raw(x, y);

    var r: f32 = 0.0;
    var g: f32 = 0.0;
    var b: f32 = 0.0;

    if c == 1 {
        g = raw_val;
    } else {
        let gN = load_raw(x, y - 1);
        let gS = load_raw(x, y + 1);
        let gW = load_raw(x - 1, y);
        let gE = load_raw(x + 1, y);

        let pN2 = load_raw(x, y - 2);
        let pS2 = load_raw(x, y + 2);
        let pW2 = load_raw(x - 2, y);
        let pE2 = load_raw(x + 2, y);

        let gH = (gW + gE) * 0.5 + (2.0 * raw_val - pW2 - pE2) * 0.25;
        let gV = (gN + gS) * 0.5 + (2.0 * raw_val - pN2 - pS2) * 0.25;

        let vh = textureLoad(vh_dir_tex, vec2<i32>(x, y)).r;
        let lpf_val = textureLoad(lpf_tex, vec2<i32>(x, y)).r;
        let thresh = 0.03 * max(lpf_val, 1e-4);

        if is_border(x, y) || abs(vh) <= thresh {
            g = (gH + gV) * 0.5;
        } else if vh > thresh {
            g = gV;
        } else {
            g = gH;
        }

        if c == 0 { r = raw_val; } else { b = raw_val; }
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

    let c = cfa_color(x, y);
    var disc = 0.0;

    if c != 1 {
        let dNE = load_raw(x + 1, y - 1);
        let dSW = load_raw(x - 1, y + 1);
        let dNW = load_raw(x - 1, y - 1);
        let dSE = load_raw(x + 1, y + 1);
        disc = abs(dNE - dSW) - abs(dNW - dSE);
    }

    textureStore(pq_dir_tex, vec2<i32>(x, y), vec4<f32>(disc, 0.0, 0.0, 0.0));
}

// =====================================================================
// Pass 5: rb_fill
// =====================================================================

@compute @workgroup_size(8, 8, 1)
fn rb_fill(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let x = i32(gid.x);
    let y = i32(gid.y);

    let c = cfa_color(x, y);
    let here = load_rgb_a(x, y);
    let g_here = here.g;
    let border = is_border(x, y);

    var r: f32 = here.r;
    var g: f32 = g_here;
    var b: f32 = here.b;

    if c == 0 {
        // ---- R site: fill B from diagonals ----
        let bNW = load_raw(x - 1, y - 1);
        let bNE = load_raw(x + 1, y - 1);
        let bSW = load_raw(x - 1, y + 1);
        let bSE = load_raw(x + 1, y + 1);

        let gNW = load_rgb_a(x - 1, y - 1).g;
        let gNE = load_rgb_a(x + 1, y - 1).g;
        let gSW = load_rgb_a(x - 1, y + 1).g;
        let gSE = load_rgb_a(x + 1, y + 1).g;

        let pq = textureLoad(pq_dir_tex, vec2<i32>(x, y)).r;

        if border || abs(pq) < 1e-6 {
            let b_avg = (bNW + bNE + bSW + bSE) * 0.25;
            let g_avg = (gNW + gNE + gSW + gSE) * 0.25;
            b = b_avg + (g_here - g_avg);
        } else if pq > 0.0 {
            b = (bNW + bSE) * 0.5 + (g_here - (gNW + gSE) * 0.5);
        } else {
            b = (bNE + bSW) * 0.5 + (g_here - (gNE + gSW) * 0.5);
        }
        r = here.r;

    } else if c == 2 {
        // ---- B site: fill R from diagonals ----
        let rNW = load_raw(x - 1, y - 1);
        let rNE = load_raw(x + 1, y - 1);
        let rSW = load_raw(x - 1, y + 1);
        let rSE = load_raw(x + 1, y + 1);

        let gNW = load_rgb_a(x - 1, y - 1).g;
        let gNE = load_rgb_a(x + 1, y - 1).g;
        let gSW = load_rgb_a(x - 1, y + 1).g;
        let gSE = load_rgb_a(x + 1, y + 1).g;

        let pq = textureLoad(pq_dir_tex, vec2<i32>(x, y)).r;

        if border || abs(pq) < 1e-6 {
            let r_avg = (rNW + rNE + rSW + rSE) * 0.25;
            let g_avg = (gNW + gNE + gSW + gSE) * 0.25;
            r = r_avg + (g_here - g_avg);
        } else if pq > 0.0 {
            r = (rNW + rSE) * 0.5 + (g_here - (gNW + gSE) * 0.5);
        } else {
            r = (rNE + rSW) * 0.5 + (g_here - (gNE + gSW) * 0.5);
        }
        b = here.b;

    } else {
        // ---- Green site: fill both R and B ----
        let horiz_is_r = cfa_color(x - 1, y) == 0;

        if horiz_is_r {
            let rW = load_raw(x - 1, y);
            let rE = load_raw(x + 1, y);
            let gW = load_rgb_a(x - 1, y).g;
            let gE = load_rgb_a(x + 1, y).g;

            let bN = load_raw(x, y - 1);
            let bS = load_raw(x, y + 1);
            let gN = load_rgb_a(x, y - 1).g;
            let gS = load_rgb_a(x, y + 1).g;

            if border {
                r = (rW + rE) * 0.5;
                b = (bN + bS) * 0.5;
            } else {
                r = (rW + rE) * 0.5 + (2.0 * g_here - gW - gE) * 0.25;
                b = (bN + bS) * 0.5 + (2.0 * g_here - gN - gS) * 0.25;
            }
        } else {
            let bW = load_raw(x - 1, y);
            let bE = load_raw(x + 1, y);
            let gW = load_rgb_a(x - 1, y).g;
            let gE = load_rgb_a(x + 1, y).g;

            let rN = load_raw(x, y - 1);
            let rS = load_raw(x, y + 1);
            let gN = load_rgb_a(x, y - 1).g;
            let gS = load_rgb_a(x, y + 1).g;

            if border {
                b = (bW + bE) * 0.5;
                r = (rN + rS) * 0.5;
            } else {
                b = (bW + bE) * 0.5 + (2.0 * g_here - gW - gE) * 0.25;
                r = (rN + rS) * 0.5 + (2.0 * g_here - gN - gS) * 0.25;
            }
        }
    }

    textureStore(rgb_b_tex, vec2<i32>(x, y), vec4<f32>(r, g, b, 1.0));
}