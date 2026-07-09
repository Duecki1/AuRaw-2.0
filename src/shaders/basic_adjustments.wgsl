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

const SRGB_TO_REC2020: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.6272773, -0.0652639,  0.0473710),
    vec3<f32>(0.3292671,  1.0613264, -0.0398610),
    vec3<f32>(0.0432929,  0.0038440,  0.9923853),
);

fn signed_cbrt(v: vec3<f32>) -> vec3<f32> {
    return sign(v) * pow(abs(v), vec3<f32>(1.0 / 3.0));
}

fn linear_srgb_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = mat3x3<f32>(
        vec3<f32>(0.4122215, 0.2119035, 0.0883025),
        vec3<f32>(0.5363325, 0.6806995, 0.2817188),
        vec3<f32>(0.0514460, 0.1073970, 0.6299787),
    ) * rgb;
    let lms_cbrt = signed_cbrt(lms);
    return mat3x3<f32>(
        vec3<f32>(0.2104543,  1.9780,     0.0259040),
        vec3<f32>(0.7936178, -2.4285922,  0.7827718),
        vec3<f32>(-0.0040720, 0.4505937, -0.8086758),
    ) * lms_cbrt;
}

fn oklab_to_linear_srgb(lab: vec3<f32>) -> vec3<f32> {
    let lms_ = mat3x3<f32>(
        vec3<f32>(1.0, 1.0, 1.0),
        vec3<f32>(0.3963378, -0.1055613, -0.0894842),
        vec3<f32>(0.2158038, -0.0638542, -1.2914855),
    ) * lab;
    let lms = lms_ * lms_ * lms_;
    return mat3x3<f32>(
        vec3<f32>( 4.0767417, -1.2684380, -0.0041961),
        vec3<f32>(-3.3077116,  2.6097574, -0.7034186),
        vec3<f32>( 0.2309699, -0.3413194,  1.7076147),
    ) * lms;
}

fn apply_saturation_vibrance(rgb: vec3<f32>) -> vec3<f32> {
    if abs(params.saturation) < 1e-6 && abs(params.vibrance) < 1e-6 {
        return rgb;
    }

    let linear_srgb = REC2020_TO_SRGB * rgb;
    var lab = linear_srgb_to_oklab(linear_srgb);
    let chroma = length(lab.yz);
    let current_sat = chroma / max(lab.x, 1e-4);

    let vibrance_factor = 1.0 - clamp(current_sat, 0.0, 1.0);
    let vibrance_boost = params.vibrance * vibrance_factor;
    let total_boost = max(0.0, 1.0 + params.saturation + vibrance_boost);

    let r_above_g = step(rgb.g, rgb.r); 
    let g_above_b = step(rgb.b, rgb.g); 
    let is_skin_like = r_above_g * g_above_b;
    let skin_protection = mix(1.0, 0.6, is_skin_like);

    let effective_boost = 1.0 + (total_boost - 1.0) * skin_protection;

    lab.y *= effective_boost;
    lab.z *= effective_boost;

    return SRGB_TO_REC2020 * oklab_to_linear_srgb(lab);
}
