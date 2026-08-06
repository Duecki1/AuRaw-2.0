#import auraw::common as Common

fn cam_to_working(rgb: vec3<f32>) -> vec3<f32> {
    let r = dot(Common::camera_uniforms.cam_to_srgb_0.xyz, rgb);
    let g = dot(Common::camera_uniforms.cam_to_srgb_1.xyz, rgb);
    let b = dot(Common::camera_uniforms.cam_to_srgb_2.xyz, rgb);
    return vec3<f32>(r, g, b);
}

// Shared perceptual gamut service. Color-edit nodes work in OKLab and ask this
// service for the reachable chroma along one constant-lightness/hue ray. A
// soft knee begins near the boundary so saturation, mixer, grading, the view
// transform, and output encoding all approach gamut in the same way instead of
// independently clipping or inventing their own safety guards.
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

fn rec2020_from_oklab(lab: vec3<f32>) -> vec3<f32> {
    return Common::SRGB_TO_REC2020 * oklab_to_linear_srgb(lab);
}

fn rec2020_to_oklab(rgb: vec3<f32>) -> vec3<f32> {
    return linear_srgb_to_oklab(Common::REC2020_TO_SRGB * rgb);
}

fn rgb_is_nonnegative(rgb: vec3<f32>) -> bool {
    return min(rgb.r, min(rgb.g, rgb.b)) >= -1e-7;
}

fn rgb_is_unit(rgb: vec3<f32>) -> bool {
    return min(rgb.r, min(rgb.g, rgb.b)) >= -1e-7
        && max(rgb.r, max(rgb.g, rgb.b)) <= 1.0000001;
}

fn perceptual_soft_chroma(requested: f32, boundary: f32) -> f32 {
    if boundary <= 1e-8 {
        return 0.0;
    }
    let chroma = max(requested, 0.0);
    let knee = boundary * 0.90;
    if chroma <= knee {
        return chroma;
    }
    let span = max(boundary - knee, 1e-8);
    return min(knee + span * (1.0 - exp(-(chroma - knee) / span)), boundary * 0.99995);
}

fn rec2020_nonnegative_boundary(lightness: f32, hue: vec2<f32>, requested: f32) -> f32 {
    var low = 0.0;
    var high = max(requested, 0.04);
    // Expand until the hue ray exits the positive Rec.2020 domain. Fixed loop
    // bounds keep shader cost deterministic on desktop and mobile.
    for (var iteration = 0; iteration < 8; iteration = iteration + 1) {
        let probe = rec2020_from_oklab(vec3<f32>(max(lightness, 0.0), hue * high));
        if rgb_is_nonnegative(probe) {
            low = high;
            high = high * 2.0;
        }
    }
    for (var iteration = 0; iteration < 11; iteration = iteration + 1) {
        let middle = 0.5 * (low + high);
        let probe = rec2020_from_oklab(vec3<f32>(max(lightness, 0.0), hue * middle));
        if rgb_is_nonnegative(probe) {
            low = middle;
        } else {
            high = middle;
        }
    }
    return low;
}

fn rec2020_unit_boundary(lightness: f32, hue: vec2<f32>, requested: f32) -> f32 {
    let l = clamp(lightness, 0.0, 1.0);
    var low = 0.0;
    var high = max(requested, 0.04);
    for (var iteration = 0; iteration < 8; iteration = iteration + 1) {
        let probe = rec2020_from_oklab(vec3<f32>(l, hue * high));
        if rgb_is_unit(probe) {
            low = high;
            high = high * 2.0;
        }
    }
    for (var iteration = 0; iteration < 11; iteration = iteration + 1) {
        let middle = 0.5 * (low + high);
        let probe = rec2020_from_oklab(vec3<f32>(l, hue * middle));
        if rgb_is_unit(probe) {
            low = middle;
        } else {
            high = middle;
        }
    }
    return low;
}

fn srgb_unit_boundary(lightness: f32, hue: vec2<f32>, requested: f32) -> f32 {
    let l = clamp(lightness, 0.0, 1.0);
    var low = 0.0;
    var high = max(requested, 0.04);
    for (var iteration = 0; iteration < 8; iteration = iteration + 1) {
        let probe = oklab_to_linear_srgb(vec3<f32>(l, hue * high));
        if rgb_is_unit(probe) {
            low = high;
            high = high * 2.0;
        }
    }
    for (var iteration = 0; iteration < 11; iteration = iteration + 1) {
        let middle = 0.5 * (low + high);
        let probe = oklab_to_linear_srgb(vec3<f32>(l, hue * middle));
        if rgb_is_unit(probe) {
            low = middle;
        } else {
            high = middle;
        }
    }
    return low;
}

fn perceptual_rec2020_from_oklab_nonnegative(
    lightness: f32,
    hue: vec2<f32>,
    requested_chroma: f32,
) -> vec3<f32> {
    let l = max(lightness, 0.0);
    let hue_length = length(hue);
    if requested_chroma <= 1e-9 || hue_length <= 1e-9 {
        return rec2020_from_oklab(vec3<f32>(l, vec2<f32>(0.0)));
    }
    let direction = hue / hue_length;
    let candidate = rec2020_from_oklab(vec3<f32>(l, direction * requested_chroma));

    // One 1/0.9 chroma probe exactly tests whether the requested color is below
    // the soft-knee region. Most pixels take this fast path; only near/outside
    // the boundary pay for the binary boundary solve.
    let knee_probe = rec2020_from_oklab(
        vec3<f32>(l, direction * (requested_chroma / 0.90)),
    );
    if rgb_is_nonnegative(candidate) && rgb_is_nonnegative(knee_probe) {
        return candidate;
    }

    let boundary = rec2020_nonnegative_boundary(l, direction, requested_chroma);
    let chroma = perceptual_soft_chroma(requested_chroma, boundary);
    return rec2020_from_oklab(vec3<f32>(l, direction * chroma));
}

fn perceptual_gamut_compress_nonnegative_rec2020(rgb: vec3<f32>) -> vec3<f32> {
    let lab = rec2020_to_oklab(rgb);
    let chroma = length(lab.yz);
    if chroma <= 1e-9 {
        return rec2020_from_oklab(vec3<f32>(max(lab.x, 0.0), vec2<f32>(0.0)));
    }
    let l = max(lab.x, 0.0);
    let hue = lab.yz / chroma;
    let knee_probe = rec2020_from_oklab(vec3<f32>(l, hue * (chroma / 0.90)));
    if lab.x >= 0.0 && rgb_is_nonnegative(rgb) && rgb_is_nonnegative(knee_probe) {
        return rgb;
    }
    return perceptual_rec2020_from_oklab_nonnegative(l, hue, chroma);
}

fn perceptual_gamut_compress_unit_rec2020(rgb: vec3<f32>) -> vec3<f32> {
    let lab = rec2020_to_oklab(rgb);
    let l = clamp(lab.x, 0.0, 1.0);
    let chroma = length(lab.yz);
    if chroma <= 1e-9 {
        return rec2020_from_oklab(vec3<f32>(l, vec2<f32>(0.0)));
    }
    let hue = lab.yz / chroma;
    let knee_probe = rec2020_from_oklab(vec3<f32>(l, hue * (chroma / 0.90)));
    if abs(l - lab.x) <= 1e-7 && rgb_is_unit(rgb) && rgb_is_unit(knee_probe) {
        return rgb;
    }
    let boundary = rec2020_unit_boundary(l, hue, chroma);
    let compressed = perceptual_soft_chroma(chroma, boundary);
    // The boundary solve can leave sub-ULP excursions at saturated edges.
    // Clamp only after out-of-cube projection; valid unit-cube inputs return
    // unchanged through the fast path above.
    return clamp(
        rec2020_from_oklab(vec3<f32>(l, hue * compressed)),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}

fn perceptual_gamut_compress_unit_srgb(rgb: vec3<f32>) -> vec3<f32> {
    let lab = linear_srgb_to_oklab(rgb);
    let l = clamp(lab.x, 0.0, 1.0);
    let chroma = length(lab.yz);
    if chroma <= 1e-9 {
        return oklab_to_linear_srgb(vec3<f32>(l, vec2<f32>(0.0)));
    }
    let hue = lab.yz / chroma;
    let knee_probe = oklab_to_linear_srgb(vec3<f32>(l, hue * (chroma / 0.90)));
    if abs(l - lab.x) <= 1e-7 && rgb_is_unit(rgb) && rgb_is_unit(knee_probe) {
        return rgb;
    }
    let boundary = srgb_unit_boundary(l, hue, chroma);
    let compressed = perceptual_soft_chroma(chroma, boundary);
    return oklab_to_linear_srgb(vec3<f32>(l, hue * compressed));
}

// Generic neutral-axis projectors remain for profile-table mathematical
// domains that are not Rec.2020/OKLab editing nodes (notably ProPhoto DCP LUT
// coordinates). User-facing color tools and output transforms use the shared
// perceptual service above.
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
    if rgb_is_nonnegative(rgb) {
        return rgb;
    }
    return perceptual_gamut_compress_nonnegative_rec2020(rgb);
}

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
    return perceptual_gamut_compress_unit_rec2020(rgb);
}

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
