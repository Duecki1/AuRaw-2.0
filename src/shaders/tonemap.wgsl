fn filmic_tonemap(rgb: vec3<f32>) -> vec3<f32> {
    let x = max(rgb, vec3<f32>(0.0));
    let lum = safe_luma(x);
    let scene_middle = 0.1842;
    let display_middle = max(params.middle_grey / 100.0, 0.01);
    let white = max(params.filmic_white, 0.1);
    let black = min(params.filmic_black, -0.1);

    let log_lum = log2(lum / scene_middle);
    let t = clamp((log_lum - black) / max(white - black, 1e-3), 0.0, 1.0);
    let mid_t = clamp((0.0 - black) / max(white - black, 1e-3), 0.05, 0.95);

    // Slope at the midpoint in t-space. 1.0 = locally linear in log space,
    // which preserves midtone contrast (no extra punch, no flattening).
    // The old code had slope 2*display_middle/mid_t on the toe side and
    // 2*(1-display_middle)/(1-mid_t) on the shoulder side — these only
    // match when display_middle == mid_t, which is never true in practice.
    // The result was a visible kink at the midtone join plus over-compressed
    // shadows and over-punched highlights.
    let mid_slope = 1.0;

    var mapped_lum: f32;
    if t < mid_t {
        // Cubic Hermite: (0, 0, slope=0) → (mid_t, display_middle, slope=mid_slope)
        let u = t / max(mid_t, 1e-3);
        let h01 = (-2.0 * u + 3.0) * u * u;   // -2u³ + 3u²
        let h11 = (u - 1.0) * u * u;           // u³ - u²
        mapped_lum = h01 * display_middle + h11 * mid_t * mid_slope;
    } else {
        // Cubic Hermite: (mid_t, display_middle, slope=mid_slope) → (1, 1, slope=0)
        let u = (t - mid_t) / max(1.0 - mid_t, 1e-3);
        let h00 = (2.0 * u - 3.0) * u * u + 1.0;  // 2u³ - 3u² + 1
        let h10 = (u - 2.0) * u * u + u;           // u³ - 2u² + u
        let h01 = (-2.0 * u + 3.0) * u * u;        // -2u³ + 3u²
        mapped_lum = h00 * display_middle + h10 * (1.0 - mid_t) * mid_slope + h01;
    }

    return x * (mapped_lum / lum);
}

fn display_render(rgb: vec3<f32>) -> vec3<f32> {
    let mapped = filmic_tonemap(rgb);
    let encoded = srgb_oetf(mapped);
    return clamp(encoded, vec3<f32>(0.0), vec3<f32>(1.0));
}