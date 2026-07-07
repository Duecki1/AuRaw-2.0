// auraw GPU pipeline
// This shader implements, in order:
// 1. Bayer demosaic (bilinear)
// 2. White balance (per-channel multiply)
// 3. Camera RGB -> sRGB linear (3x3 matrix)
// 4. Highlight desaturation (soft-desaturates pixels above the sensor's
//    clip point, BEFORE exposure -- see comment on desaturate_highlights()
//    below for why the exposure slider must never move this threshold)
// 5. Exposure module (ported 1:1 from darktable/ansel's exposure.c)
// 6. Minimal display tonemap (luminance-based Reinhard roll-off, chroma-
//    preserving -- see comment on tonemap() below)
// 7. sRGB OETF (linear -> gamma-encoded)

struct Params {
    black: f32,
    exposure: f32,
    _pad0: f32,
    _pad1: f32,

    // White balance coefficients (RGGB order)
    wb: vec4<f32>,

    // Camera RGB -> sRGB linear, row-major 3x3 packed into vec4s (w unused)
    cam_to_srgb_0: vec4<f32>,
    cam_to_srgb_1: vec4<f32>,
    cam_to_srgb_2: vec4<f32>,

    // Raw geometry
    width: u32,
    height: u32,
    cfa_pattern: u32,    // 0=RGGB 1=BGGR 2=GRBG 3=GBRG
    black_level: f32,    // sensor black level
    white_level: f32,    // sensor white level
    
    // Explicit padding scalars to align cleanly with Rust on a 112-byte boundary
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
}

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var raw_tex: texture_2d<f32>;
@group(0) @binding(2) var out_tex: texture_storage_2d<rgba8unorm, write>;

// ---- CFA sampling helpers ----------------------------------------------

fn cfa_color(x: i32, y: i32, pattern: u32) -> i32 {
    let ex = x & 1;
    let ey = y & 1;
    
    var tile: array<i32, 4>;
    switch pattern {
        case 1u: { tile = array<i32, 4>(2, 1, 1, 0); } // BGGR
        case 2u: { tile = array<i32, 4>(1, 0, 2, 1); } // GRBG
        case 3u: { tile = array<i32, 4>(1, 2, 0, 1); } // GBRG
        default: { tile = array<i32, 4>(0, 1, 1, 2); } // RGGB (case 0u)
    }
    let idx = ey * 2 + ex;
    return tile[idx];
}

fn load_raw(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    return textureLoad(raw_tex, vec2<i32>(cx, cy), 0).r;
}

fn demosaic(x: i32, y: i32) -> vec3<f32> {
    let c = cfa_color(x, y, params.cfa_pattern);

    var r: f32;
    var g: f32;
    var b: f32;

    if c == 1 {
        g = load_raw(x, y);
        let n1 = load_raw(x - 1, y);
        let n2 = load_raw(x + 1, y);
        let n3 = load_raw(x, y - 1);
        let n4 = load_raw(x, y + 1);
        let horiz_is_r = cfa_color(x - 1, y, params.cfa_pattern) == 0;
        if horiz_is_r {
            r = (n1 + n2) * 0.5;
            b = (n3 + n4) * 0.5;
        } else {
            b = (n1 + n2) * 0.5;
            r = (n3 + n4) * 0.5;
        }
    } else {
        let same = load_raw(x, y);
        let gN = load_raw(x, y - 1);
        let gS = load_raw(x, y + 1);
        let gW = load_raw(x - 1, y);
        let gE = load_raw(x + 1, y);
        let gAvg = (gN + gS + gW + gE) * 0.25;

        let dNW = load_raw(x - 1, y - 1);
        let dNE = load_raw(x + 1, y - 1);
        let dSW = load_raw(x - 1, y + 1);
        let dSE = load_raw(x + 1, y + 1);
        let dAvg = (dNW + dNE + dSW + dSE) * 0.25;

        g = gAvg;
        if c == 0 {
            r = same;
            b = dAvg;
        } else {
            b = same;
            r = dAvg;
        }
    }

    return vec3<f32>(r, g, b);
}

// ---- Color management ---------------------------------------------------

fn apply_wb(rgb: vec3<f32>) -> vec3<f32> {
    return rgb * params.wb.rgb;
}

fn cam_to_srgb(rgb: vec3<f32>) -> vec3<f32> {
    let r = dot(params.cam_to_srgb_0.xyz, rgb);
    let g = dot(params.cam_to_srgb_1.xyz, rgb);
    let b = dot(params.cam_to_srgb_2.xyz, rgb);
    return vec3<f32>(r, g, b);
}

// highlights.c's reconstruction runs on the CPU pre-demosaic, in raw sensor
// space, per-channel -- it can only work with the ratio between the R/G/B
// photosites actually available near a clipped pixel. When a highlight is
// only partially recoverable (e.g. one channel clipped with no good
// same-color neighbor to borrow a ratio from -- darktable's own docs call
// this out explicitly as the case that produces a residual color cast),
// some real chroma survives all the way through WB and the camera matrix.
//
// This MUST run here, on the signal right after cam_to_srgb and BEFORE
// apply_exposure -- i.e. keyed to the sensor's actual clip point (1.0,
// since raw_loader.rs already normalizes white to 1.0 per channel), not to
// the exposure-adjusted display brightness. Exposure is a user-facing
// creative slider; whether a pixel is "clipped" can never depend on it,
// or the desaturation threshold silently moves every time the user drags
// exposure:
//   - pull exposure down -> genuinely-clipped pixels get scaled below
//     whatever threshold you check post-exposure, so their leftover chroma
//     survives uncorrected (pink dots reappear)
//   - push exposure up -> ordinary bright-but-unclipped pixels get pushed
//     past that threshold by the exposure multiply alone and get
//     incorrectly flattened to gray (loses real color that was never
//     actually clipped)
// Checking clip status before exposure is applied avoids both failure
// modes: the threshold is anchored to the sensor, not to the slider.
fn desaturate_highlights(rgb: vec3<f32>) -> vec3<f32> {
    let x = max(rgb, vec3<f32>(0.0));
    let luma = dot(x, vec3<f32>(0.2126, 0.7152, 0.0722));
    // How far above sensor white (1.0, pre-exposure) the brightest channel
    // sits drives how strongly we pull this pixel back toward neutral.
    // Ramps from 0 (no desaturation, at or below white) to fully neutral
    // by 2x white.
    let peak = max(max(x.r, x.g), x.b);
    let desat_amount = clamp(peak - 1.0, 0.0, 1.0);
    return mix(x, vec3<f32>(luma), desat_amount);
}

fn apply_exposure(rgb: vec3<f32>) -> vec3<f32> {
    let white = exp2(-params.exposure);
    let scale = 1.0 / (white - params.black);
    return (rgb - vec3<f32>(params.black)) * scale;
}

// ---- Display tonemap + OETF ---------------------------------------------

// BUG (root cause of pink/magenta highlights): the previous version ran
// Reinhard (`x/(x+1)`) on R, G, and B *independently*. That curve is
// nonlinear, so it compresses three different input values by three
// different ratios. highlights.c's reconstruction (LCH / inpaint mode)
// legitimately leaves some residual color in a recovered highlight -- e.g.
// R=2.2, G=1.3, B=1.9 above white after WB + the camera matrix, for a
// highlight that should read as bright near-white/desaturated. Tonemapping
// each channel on its own hue-shifts that toward magenta: the same failure
// mode darktable/filmic's docs and engineering notes describe when RGB
// channels are tonemapped separately instead of preserving chrominance
// (see darktable's "chroma preservation" in filmic rgb). The channel
// closest to 1.0 (here G) gets compressed the least in absolute terms
// relative to its distance from the curve's knee, while R and B -- further
// out -- get pulled down disproportionately less *relative to each other*,
// widening the R/B vs G gap instead of preserving it.
//
// Fix: compress *luminance* through the curve, then apply the resulting
// scale factor equally to all three channels. This preserves whatever hue
// highlights.c reconstructed (right or wrong) instead of amplifying it.
fn tonemap(rgb: vec3<f32>) -> vec3<f32> {
    let x = max(rgb, vec3<f32>(0.0));
    let luma = max(dot(x, vec3<f32>(0.2126, 0.7152, 0.0722)), 1e-6);
    let luma_mapped = luma / (luma + 1.0) * 1.06;
    return x * (luma_mapped / luma);
}

fn srgb_oetf(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    let cutoff = step(vec3<f32>(0.0031308), c);
    return mix(lo, hi, cutoff);
}

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height {
        return;
    }
    let x = i32(gid.x);
    let y = i32(gid.y);

    var rgb = demosaic(x, y);
    rgb = apply_wb(rgb);
    rgb = cam_to_srgb(rgb);
    rgb = desaturate_highlights(rgb);
    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));
    rgb = tonemap(rgb);
    rgb = srgb_oetf(rgb);

    textureStore(out_tex, vec2<i32>(x, y), vec4<f32>(rgb, 1.0));
}