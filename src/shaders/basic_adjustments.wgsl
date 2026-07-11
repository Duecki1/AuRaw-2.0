// Scene-linear basic adjustments translated from Ansel's basicadj module.
// The UI uses Lightroom-like -100..100 controls where appropriate; the
// Ansel-native controls retain their original units in the Advanced panel.

fn apply_exposure(rgb: vec3<f32>) -> vec3<f32> {
    let white = exp2(-params.exposure);
    let scale = 1.0 / max(white - params.black_point, 1e-4);
    return (rgb - vec3<f32>(params.black_point)) * scale;
}

fn ansel_highlight_curve(level: f32, compression: f32, range: f32) -> f32 {
    if compression <= 0.0 {
        return 1.0;
    }

    // Direct WGSL port of basicadj.c:hlcurve().  The guards keep the curve
    // finite at the UI extremes without changing ordinary Ansel values.
    var value = level + (range - 1.0);
    if abs(value) < 1e-6 {
        value = select(-1e-6, 1e-6, value >= 0.0);
    }
    var y = value / max(range, 1e-6) * compression;
    y = max(y, -0.999999);
    return log(1.0 + y) * range / (value * compression);
}

fn apply_ansel_highlight_compression(rgb: vec3<f32>) -> vec3<f32> {
    if params.hlcompr <= 1e-6 {
        return rgb;
    }
    let lum = safe_luma(max(rgb, vec3<f32>(0.0)));
    if lum <= 0.0 {
        return rgb;
    }
    let compression = params.hlcompr / 100.0;
    let shoulder = params.hlcomprthresh / 800.0 + 0.1;
    let ratio = ansel_highlight_curve(lum, compression, 1.0 - shoulder);
    return rgb * ratio;
}

fn apply_brightness(rgb: vec3<f32>) -> vec3<f32> {
    if abs(params.brightness) <= 1e-6 {
        return rgb;
    }
    let brightness = params.brightness * 2.0;
    let gamma = select(1.0 - brightness, 1.0 / max(1.0 + brightness, 1e-4), brightness >= 0.0);
    return select(rgb, pow(max(rgb, vec3<f32>(0.0)), vec3<f32>(gamma)), rgb > vec3<f32>(0.0));
}

fn apply_contrast(rgb: vec3<f32>) -> vec3<f32> {
    if abs(params.contrast) < 1e-6 {
        return rgb;
    }
    // Preserve colour by applying the Ansel power curve to luminance and
    // scaling RGB as a unit. Contrast's familiar UI range maps to [-1, 1].
    let contrast = 1.0 + params.contrast / 100.0;
    let middle = max(params.middle_grey / 100.0, 1e-4);
    let lum = safe_luma(rgb);
    let contrast_lum = pow(max(lum / middle, 0.0), contrast) * middle;
    return rgb * (contrast_lum / lum);
}

fn apply_saturation_vibrance(rgb: vec3<f32>) -> vec3<f32> {
    let saturation = params.saturation / 100.0;
    let vibrance = params.vibrance / 140.0;
    if abs(saturation) < 1e-6 && abs(vibrance) < 1e-6 {
        return rgb;
    }

    // This is the RGB-average/delta model in Ansel basicadj.c, replacing the
    // previous unrelated OKLab skin heuristic so its slider behaves as Ansel's.
    let average = (rgb.r + rgb.g + rgb.b) / 3.0;
    let delta = length(rgb - vec3<f32>(average));
    let vibrance_term = vibrance * (1.0 - pow(max(delta, 0.0), abs(vibrance)));
    let factor = max(0.0, 1.0 + saturation + vibrance_term);
    return vec3<f32>(average) + factor * (rgb - vec3<f32>(average));
}
