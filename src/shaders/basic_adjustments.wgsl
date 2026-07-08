// basic_adjustments.wgsl

fn apply_exposure(rgb: vec3<f32>) -> vec3<f32> {
    let white = exp2(-params.exposure);
    let scale = 1.0 / max(white - params.black, 1e-4);
    return (rgb - vec3<f32>(params.black)) * scale;
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

    // Perceptual luma and chroma
    let lum = safe_luma(rgb);
    let gray = vec3<f32>(lum);
    let chroma = rgb - gray;
    let current_sat = length(chroma) / max(lum, 1e-6);

    // Vibrance: stronger boost for less saturated pixels
    let vibrance_factor = 1.0 - clamp(current_sat, 0.0, 1.0);
    let vibrance_boost = params.vibrance * vibrance_factor;
    let total_boost = 1.0 + params.saturation + vibrance_boost;

    // Skin-tone protection: detect skin-like hue (R > G > B) and reduce
    // the effective boost to avoid orange/red oversaturation in faces.
    let r_above_g = step(rgb.g, rgb.r);   // 1.0 if R ≥ G
    let g_above_b = step(rgb.b, rgb.g);   // 1.0 if G ≥ B
    let is_skin_like = r_above_g * g_above_b;
    let skin_protection = mix(1.0, 0.6, is_skin_like);

    let effective_boost = 1.0 + (total_boost - 1.0) * skin_protection;

    return gray + chroma * effective_boost;
}