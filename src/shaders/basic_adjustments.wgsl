// Scene-linear operations that intentionally happen before the single
// scene-to-display curve. Global contrast/highlight/shadow/white/black shaping
// lives exclusively in tonemap.wgsl.

fn apply_exposure(rgb: vec3<f32>) -> vec3<f32> {
    let white = exp2(-params.exposure);
    let scale = 1.0 / max(white - params.black_point, 1e-4);
    return (rgb - vec3<f32>(params.black_point)) * scale;
}

fn apply_saturation_vibrance(rgb: vec3<f32>) -> vec3<f32> {
    let saturation = params.saturation / 100.0;
    let vibrance = params.vibrance / 140.0;
    if abs(saturation) < 1e-6 && abs(vibrance) < 1e-6 {
        return rgb;
    }

    // RGB-average/delta saturation with content-adaptive vibrance. This keeps
    // the operation scene-linear and leaves all luminance shaping to the final
    // monotonic display curve.
    let average = (rgb.r + rgb.g + rgb.b) / 3.0;
    let delta = length(rgb - vec3<f32>(average));
    let vibrance_term = vibrance * (1.0 - pow(max(delta, 0.0), abs(vibrance)));
    let factor = max(0.0, 1.0 + saturation + vibrance_term);
    return vec3<f32>(average) + factor * (rgb - vec3<f32>(average));
}
