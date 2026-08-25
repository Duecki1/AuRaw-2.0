#import auraw::common as Common
#import auraw::color as Color


fn apply_temperature_tint_values(
    rgb: vec3<f32>,
    temperature_value: f32,
    tint_value: f32,
) -> vec3<f32> {
    let temperature = clamp(temperature_value / 100.0, -1.0, 1.0);
    let tint = clamp(tint_value / 100.0, -1.0, 1.0);
    if abs(temperature) < 1e-6 && abs(tint) < 1e-6 {
        return rgb;
    }

    let gains = exp2(vec3<f32>(
        0.22 * temperature + 0.08 * tint,
        -0.24 * tint,
        -0.34 * temperature + 0.08 * tint,
    ));
    let d65_xyz = vec3<f32>(0.9504559, 1.0, 1.0890578);
    let adapted_white = Common::scene_tone_uniforms.bradford_to_xyz
        * ((Common::scene_tone_uniforms.xyz_to_bradford * d65_xyz) * gains);
    let normalization = 1.0 / max(adapted_white.y, 1e-6);

    let xyz = Common::scene_tone_uniforms.rec2020_to_xyz * rgb;
    let adapted_xyz = Common::scene_tone_uniforms.bradford_to_xyz
        * ((Common::scene_tone_uniforms.xyz_to_bradford * xyz) * gains);
    return Common::scene_tone_uniforms.xyz_to_rec2020_field * adapted_xyz * normalization;
}

fn apply_exposure(rgb: vec3<f32>) -> vec3<f32> {
    return rgb * exp2(Common::scene_tone_uniforms.exposure);
}

fn circular_hue_distance(a: f32, b: f32) -> f32 {
    let d = abs(a - b);
    return min(d, 1.0 - d);
}

fn perceptual_control(value: f32) -> f32 {
    let normalized = clamp(value / 100.0, -1.0, 1.0);
    let magnitude = abs(normalized);
    return sign(normalized) * (0.78 * magnitude + 0.22 * magnitude * magnitude);
}

fn apply_saturation_vibrance(rgb: vec3<f32>) -> vec3<f32> {
    let saturation = perceptual_control(Common::scene_tone_uniforms.saturation);
    let vibrance = perceptual_control(Common::scene_tone_uniforms.vibrance);
    if abs(saturation) < 1e-6 && abs(vibrance) < 1e-6 {
        return rgb;
    }

    let lab = Color::linear_srgb_to_oklab(Common::REC2020_TO_SRGB * rgb);
    let chroma = length(lab.yz);
    if chroma < 1e-9 {
        return rgb;
    }

    let hue = fract(atan2(lab.z, lab.y) / (2.0 * 3.14159265359) + 1.0);
    let skin_distance = circular_hue_distance(hue, 0.12);
    let skin_protection = 1.0 - smoothstep(0.032, 0.145, skin_distance);
    let relative_chroma = chroma / max(0.030 + 0.40 * max(lab.x, 0.0), 0.045);
    let content_saturation = clamp(relative_chroma, 0.0, 1.0);

    var saturation_factor = max(1.0 + saturation, 0.0);
    if saturation > 0.0 {
        saturation_factor = exp2(saturation * 0.72);
    }

    var vibrance_factor = 1.0;
    if vibrance >= 0.0 {
        let muted_weight = pow(1.0 - content_saturation, 0.72);
        let neutral_guard = smoothstep(0.0025, 0.014, chroma);
        let tonal_guard = smoothstep(0.018, 0.13, max(lab.x, 0.0))
            * (1.0 - 0.18 * smoothstep(0.92, 1.28, lab.x));
        let boost = vibrance
            * (0.34 + 1.02 * muted_weight)
            * neutral_guard
            * tonal_guard
            * (1.0 - 0.46 * skin_protection);
        vibrance_factor = exp2(boost * 2.25);
    } else {
        let reduction = (-vibrance)
            * mix(0.90, 0.97, pow(content_saturation, 0.68));
        vibrance_factor = max(1.0 - reduction, 0.0);
    }

    let chroma_factor = clamp(saturation_factor * vibrance_factor, 0.0, 4.0);
    let adjusted = vec3<f32>(lab.x, lab.yz * chroma_factor);
    return Color::perceptual_gamut_compress_nonnegative_rec2020(
        Common::SRGB_TO_REC2020 * Color::oklab_to_linear_srgb(adjusted),
    );
}

fn apply_saturation_value(rgb: vec3<f32>, value: f32) -> vec3<f32> {
    let saturation = perceptual_control(value);
    if abs(saturation) < 1e-6 {
        return rgb;
    }
    let lab = Color::linear_srgb_to_oklab(Common::REC2020_TO_SRGB * rgb);
    var factor = max(1.0 + saturation, 0.0);
    if saturation > 0.0 {
        factor = exp2(saturation * 0.72);
    }
    let adjusted = vec3<f32>(lab.x, lab.yz * factor);
    return Color::perceptual_gamut_compress_nonnegative_rec2020(
        Common::SRGB_TO_REC2020 * Color::oklab_to_linear_srgb(adjusted),
    );
}
