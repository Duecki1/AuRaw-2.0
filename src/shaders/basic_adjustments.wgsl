fn apply_exposure(rgb: vec3<f32>) -> vec3<f32> {
    let white = exp2(-params.exposure);
    let scale = 1.0 / max(white - params.black, 1e-4);
    return (rgb - vec3<f32>(params.black)) * scale;
}

fn apply_brightness(rgb: vec3<f32>) -> vec3<f32> {
    if abs(params.brightness) < 1e-6 {
        return rgb;
    }
    let b = params.brightness * 2.0;
    let gamma = select(1.0 - b, 1.0 / max(1.0 + b, 1e-3), b >= 0.0);
    return pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(gamma));
}

fn apply_contrast(rgb: vec3<f32>) -> vec3<f32> {
    if abs(params.contrast) < 1e-6 {
        return rgb;
    }
    let contrast = params.contrast + 1.0;
    let middle = max(params.middle_grey / 100.0, 1e-4);
    let lum = safe_luma(rgb);
    let contrast_lum = pow(max(lum / middle, 0.0), contrast) * middle;
    return rgb * (contrast_lum / lum);
}

fn apply_saturation_vibrance(rgb: vec3<f32>) -> vec3<f32> {
    if abs(params.saturation) < 1e-6 && abs(params.vibrance) < 1e-6 {
        return rgb;
    }

    let average = (rgb.r + rgb.g + rgb.b) / 3.0;
    let delta = length(vec3<f32>(average) - rgb);
    let vibrance = params.vibrance / 1.4;
    let power = pow(max(delta, 0.0), max(abs(vibrance), 1e-6));
    let protection = vibrance * (1.0 - power);
    return vec3<f32>(average) + (1.0 + params.saturation + protection) * (rgb - vec3<f32>(average));
}

