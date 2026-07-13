// Scene-linear operations that intentionally happen before the final
// scene-to-display transform. Global white balance is applied separately in
// camera RGB before the DCP matrix/profile path; the Bradford transform below
// remains useful for local post-profile temperature/tint adjustments.

const REC2020_TO_XYZ: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.6369580, 0.2627002, 0.0000000),
    vec3<f32>(0.1446169, 0.6779981, 0.0280727),
    vec3<f32>(0.1688809, 0.0593017, 1.0609851),
);

const XYZ_TO_REC2020: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 1.7166512, -0.6666844,  0.0176399),
    vec3<f32>(-0.3556708,  1.6164812, -0.0427706),
    vec3<f32>(-0.2533663,  0.0157685,  0.9421031),
);

const XYZ_TO_BRADFORD: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 0.8951000, -0.7502000,  0.0389000),
    vec3<f32>( 0.2664000,  1.7135000, -0.0685000),
    vec3<f32>(-0.1614000,  0.0367000,  1.0296000),
);

const BRADFORD_TO_XYZ: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 0.9869929,  0.4323053, -0.0085287),
    vec3<f32>(-0.1470543,  0.5183603,  0.0400428),
    vec3<f32>( 0.1599627,  0.0492912,  0.9684867),
);

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

    // Work in Bradford cone responses so temperature follows the blue-yellow
    // daylight axis and tint follows the green-magenta axis. The transform is
    // normalized to keep the adapted reference white at Y=1, avoiding an
    // unwanted exposure change while the user adjusts white balance.
    let gains = exp2(vec3<f32>(
        0.22 * temperature + 0.08 * tint,
        -0.24 * tint,
        -0.34 * temperature + 0.08 * tint,
    ));
    let d65_xyz = vec3<f32>(0.9504559, 1.0, 1.0890578);
    let adapted_white = BRADFORD_TO_XYZ * ((XYZ_TO_BRADFORD * d65_xyz) * gains);
    let normalization = 1.0 / max(adapted_white.y, 1e-6);

    let xyz = REC2020_TO_XYZ * rgb;
    let adapted_xyz = BRADFORD_TO_XYZ * ((XYZ_TO_BRADFORD * xyz) * gains);
    return XYZ_TO_REC2020 * adapted_xyz * normalization;
}

fn apply_camera_temperature_tint(camera_rgb: vec3<f32>) -> vec3<f32> {
    let temperature = clamp(params.temperature / 100.0, -1.0, 1.0);
    let tint = clamp(params.tint / 100.0, -1.0, 1.0);
    if abs(temperature) < 1e-6 && abs(tint) < 1e-6 {
        return camera_rgb;
    }

    // White balance is a diagonal gain in the camera's native RGB space.
    // Anchor green so the global control changes chromaticity without adding
    // an arbitrary exposure shift. This must happen before cam_to_working and
    // every DCP HueSat/look/tone operation.
    let gains = exp2(vec3<f32>(
        0.22 * temperature + 0.08 * tint,
        -0.24 * tint,
        -0.34 * temperature + 0.08 * tint,
    ));
    return camera_rgb * (gains / max(gains.y, 1e-6));
}

fn apply_exposure(rgb: vec3<f32>) -> vec3<f32> {
    // Sensor black calibration is applied while normalizing each CFA plane in
    // raw_sampling.wgsl/highlights.wgsl. Exposure is therefore a pure
    // scene-linear gain here; subtracting black in working RGB changes hue and
    // destroys near-black channel relationships.
    return rgb * exp2(params.exposure);
}

fn signed_cuberoot(value: f32) -> f32 {
    return sign(value) * pow(abs(value), 1.0 / 3.0);
}

fn linear_srgb_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    let lms = mat3x3<f32>(
        vec3<f32>(0.41222147, 0.21190350, 0.08830246),
        vec3<f32>(0.53633254, 0.68069955, 0.28171884),
        vec3<f32>(0.05144599, 0.10739696, 0.62997870),
    ) * rgb;
    let root = vec3<f32>(
        signed_cuberoot(lms.x),
        signed_cuberoot(lms.y),
        signed_cuberoot(lms.z),
    );
    return mat3x3<f32>(
        vec3<f32>( 0.21045426,  1.97799850,  0.02590404),
        vec3<f32>( 0.79361779, -2.42859221,  0.78277177),
        vec3<f32>(-0.00407205,  0.45059371, -0.80867577),
    ) * root;
}

fn oklab_to_linear_srgb(lab: vec3<f32>) -> vec3<f32> {
    let root = mat3x3<f32>(
        vec3<f32>(1.0, 1.0, 1.0),
        vec3<f32>(0.39633778, -0.10556135, -0.08948418),
        vec3<f32>(0.21580376, -0.06385417, -1.29148555),
    ) * lab;
    let lms = root * root * root;
    return mat3x3<f32>(
        vec3<f32>( 4.07674166, -1.26843800, -0.00419609),
        vec3<f32>(-3.30771159,  2.60975740, -0.70341861),
        vec3<f32>( 0.23096993, -0.34131940,  1.70761470),
    ) * lms;
}

fn circular_hue_distance(a: f32, b: f32) -> f32 {
    let d = abs(a - b);
    return min(d, 1.0 - d);
}

fn repair_negative_rec2020(rgb: vec3<f32>) -> vec3<f32> {
    let neutral = vec3<f32>(max(dot(rgb, LUMA), 0.0));
    let chroma = rgb - neutral;
    var scale = 1.0;
    if chroma.r < 0.0 { scale = min(scale, neutral.r / max(-chroma.r, 1e-9)); }
    if chroma.g < 0.0 { scale = min(scale, neutral.g / max(-chroma.g, 1e-9)); }
    if chroma.b < 0.0 { scale = min(scale, neutral.b / max(-chroma.b, 1e-9)); }
    return neutral + chroma * clamp(scale, 0.0, 1.0);
}

fn apply_saturation_vibrance(rgb: vec3<f32>) -> vec3<f32> {
    let saturation = clamp(params.saturation / 100.0, -1.0, 1.0);
    let vibrance = clamp(params.vibrance / 100.0, -1.0, 1.0);
    if abs(saturation) < 1e-6 && abs(vibrance) < 1e-6 {
        return rgb;
    }

    let lab = linear_srgb_to_oklab(REC2020_TO_SRGB * rgb);
    let chroma = length(lab.yz);
    if chroma < 1e-9 {
        return rgb;
    }

    let hue = fract(atan2(lab.z, lab.y) / (2.0 * 3.14159265359) + 1.0);
    let skin_distance = circular_hue_distance(hue, 0.12);
    let skin_protection = 1.0 - smoothstep(0.035, 0.14, skin_distance);
    let content_saturation = clamp(chroma / max(0.04 + 0.30 * lab.x, 0.06), 0.0, 1.0);

    let saturation_factor = max(0.0, 1.0 + saturation);
    var vibrance_factor = 1.0 + vibrance * 0.85;
    if vibrance >= 0.0 {
        vibrance_factor = 1.0
            + vibrance * (1.0 - content_saturation) * (1.0 - 0.55 * skin_protection);
    }

    let adjusted = vec3<f32>(lab.x, lab.yz * saturation_factor * max(vibrance_factor, 0.0));
    return repair_negative_rec2020(SRGB_TO_REC2020 * oklab_to_linear_srgb(adjusted));
}

fn apply_saturation_value(rgb: vec3<f32>, value: f32) -> vec3<f32> {
    let saturation = clamp(value / 100.0, -1.0, 1.0);
    if abs(saturation) < 1e-6 {
        return rgb;
    }
    let lab = linear_srgb_to_oklab(REC2020_TO_SRGB * rgb);
    let factor = max(0.0, 1.0 + saturation);
    let adjusted = vec3<f32>(lab.x, lab.yz * factor);
    return repair_negative_rec2020(SRGB_TO_REC2020 * oklab_to_linear_srgb(adjusted));
}
