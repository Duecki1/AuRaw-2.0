fn cam_to_working(rgb: vec3<f32>) -> vec3<f32> {
    let r = dot(params.cam_to_srgb_0.xyz, rgb);
    let g = dot(params.cam_to_srgb_1.xyz, rgb);
    let b = dot(params.cam_to_srgb_2.xyz, rgb);
    return vec3<f32>(r, g, b);
}

// Compresses only the chroma excursion required to enter a non-negative RGB
// domain. The neutral anchor carries the requested lightness, so the operation
// preserves lightness and RGB hue direction instead of independently flooring
// channels. Use this only at stages whose mathematical domain requires
// positive RGB; scene-linear storage and signed-safe transforms should retain
// signed tristimulus values unchanged.
fn gamut_project_nonnegative(rgb: vec3<f32>, lightness: f32) -> vec3<f32> {
    let neutral_value = max(lightness, 0.0);
    let neutral = vec3<f32>(neutral_value);
    let chroma = rgb - neutral;
    var scale = 1.0;
    if chroma.r < 0.0 { scale = min(scale, neutral_value / max(-chroma.r, 1e-20)); }
    if chroma.g < 0.0 { scale = min(scale, neutral_value / max(-chroma.g, 1e-20)); }
    if chroma.b < 0.0 { scale = min(scale, neutral_value / max(-chroma.b, 1e-20)); }
    return neutral + chroma * clamp(scale, 0.0, 1.0);
}

fn gamut_project_nonnegative_rec2020(rgb: vec3<f32>) -> vec3<f32> {
    return gamut_project_nonnegative(rgb, dot(rgb, LUMA));
}

// Display/output counterpart of gamut_project_nonnegative. The view transform
// is expected to place lightness in [0, 1]; this projection compresses chroma
// toward the neutral axis only as far as required to fit the unit RGB cube.
fn gamut_project_unit(rgb: vec3<f32>, lightness: f32) -> vec3<f32> {
    let neutral_value = clamp(lightness, 0.0, 1.0);
    let neutral = vec3<f32>(neutral_value);
    let chroma = rgb - neutral;
    var scale = 1.0;

    if chroma.r < 0.0 {
        scale = min(scale, neutral_value / max(-chroma.r, 1e-20));
    } else if chroma.r > 0.0 {
        scale = min(scale, (1.0 - neutral_value) / max(chroma.r, 1e-20));
    }
    if chroma.g < 0.0 {
        scale = min(scale, neutral_value / max(-chroma.g, 1e-20));
    } else if chroma.g > 0.0 {
        scale = min(scale, (1.0 - neutral_value) / max(chroma.g, 1e-20));
    }
    if chroma.b < 0.0 {
        scale = min(scale, neutral_value / max(-chroma.b, 1e-20));
    } else if chroma.b > 0.0 {
        scale = min(scale, (1.0 - neutral_value) / max(chroma.b, 1e-20));
    }
    return neutral + chroma * clamp(scale, 0.0, 1.0);
}

fn gamut_project_unit_rec2020(rgb: vec3<f32>) -> vec3<f32> {
    return gamut_project_unit(rgb, dot(rgb, LUMA));
}

// Kept as the historical call-site name, but now implemented as the shared
// hue/lightness-preserving projection rather than channel flooring.
fn map_negative_gamut(rgb: vec3<f32>) -> vec3<f32> {
    return gamut_project_nonnegative_rec2020(rgb);
}

// Extended sRGB transfer: sign-preserving so diagnostic/intermediate callers
// cannot accidentally hide a negative component before explicit gamut mapping.
fn srgb_oetf(c: vec3<f32>) -> vec3<f32> {
    let magnitude = abs(c);
    let lo = c * 12.92;
    let hi = sign(c) * (1.055 * pow(magnitude, vec3<f32>(1.0 / 2.4)) - 0.055);
    let cutoff = step(vec3<f32>(0.0031308), magnitude);
    return mix(lo, hi, cutoff);
}
