// highlights.wgsl

const SQRT3: f32 = 1.7320508075688772;
const INV_SQRT12: f32 = 0.28867513459481287; // 1 / (2*sqrt(3))

fn reconstruct_sensor_highlights(rgb: vec3<f32>, clip_mask: f32) -> vec3<f32> {
    // Decode clip mask (1 = R, 10 = G, 100 = B)
    let r_clipped = (clip_mask % 10.0) >= 1.0;
    let g_clipped = ((clip_mask / 10.0) % 10.0) >= 1.0;
    let b_clipped = (clip_mask / 100.0) >= 1.0;

    if (!r_clipped && !g_clipped && !b_clipped) {
        return rgb;
    }

    // Undo white balance so all channels share the same clipping point.
    let wb = params.wb.xyz;

    let r = rgb.r / max(wb.x, 1e-8);
    let g = rgb.g / max(wb.y, 1e-8);
    let b = rgb.b / max(wb.z, 1e-8);

    let clip = 0.995 * params.clip;

    // Safe (unclipped) values
    let Rc = min(r, clip);
    let Gc = min(g, clip);
    let Bc = min(b, clip);

    // Forward transform
    let L = (r + g + b) / 3.0;

    var C = SQRT3 * (r - g);
    var H = 2.0 * b - r - g;

    let Cc = SQRT3 * (Rc - Gc);
    let Hc = 2.0 * Bc - Rc - Gc;

    let denom = C * C + H * H;

    if (denom > 1e-12) {
        let chroma = sqrt(denom);
        let safe_chroma = sqrt(Cc * Cc + Hc * Hc);

        // Never increase chroma.
        let ratio = min(1.0, safe_chroma / chroma);

        C *= ratio;
        H *= ratio;
    }

    // Inverse transform
    let r_out = L - H / 6.0 + C * INV_SQRT12;
    let g_out = L - H / 6.0 - C * INV_SQRT12;
    let b_out = L + H / 3.0;

    return max(
        vec3<f32>(
            r_out * wb.x,
            g_out * wb.y,
            b_out * wb.z,
        ),
        vec3<f32>(0.0),
    );
}