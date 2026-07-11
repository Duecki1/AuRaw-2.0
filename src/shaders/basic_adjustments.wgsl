// Scene-linear operations that intentionally happen before the single
// scene-to-display curve. Global contrast/highlight/shadow/white/black shaping
// lives exclusively in tonemap.wgsl.

fn apply_exposure(rgb: vec3<f32>) -> vec3<f32> {
    // Sensor black calibration is applied while normalizing each CFA plane in
    // raw_sampling.wgsl/highlights.wgsl. Exposure is therefore a pure
    // scene-linear gain here; subtracting black in working RGB changes hue and
    // destroys near-black channel relationships.
    return rgb * exp2(params.exposure);
}

fn apply_saturation_vibrance(rgb: vec3<f32>) -> vec3<f32> {
    let saturation = params.saturation / 100.0;
    let vibrance = params.vibrance / 140.0;
    if abs(saturation) < 1e-6 && abs(vibrance) < 1e-6 {
        return rgb;
    }

    // Normalize the content-saturation estimate by signal magnitude so
    // vibrance is invariant to exposure and remains in [0, 1] for HDR values.
    // The previous unnormalized RGB distance exceeded one in bright pixels,
    // causing positive vibrance to reverse sign.
    let positive = max(rgb, vec3<f32>(0.0));
    let hi = max(max(positive.r, positive.g), positive.b);
    let lo = min(min(positive.r, positive.g), positive.b);
    let content_saturation = clamp((hi - lo) / max(hi, 1e-6), 0.0, 1.0);

    let average = (rgb.r + rgb.g + rgb.b) / 3.0;
    let chroma = rgb - vec3<f32>(average);
    let vibrance_term = vibrance * (1.0 - content_saturation);
    let factor = max(0.0, 1.0 + saturation + vibrance_term);
    return vec3<f32>(average) + factor * chroma;
}
