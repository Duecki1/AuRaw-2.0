// Post-demosaic scene-linear controls. Keeping this in its own pass lets
// local operations sample neighbouring RGB pixels. Global tone controls are
// evaluated once, at the end, by display_render().

@group(0) @binding(11) var scene_tex: texture_2d<f32>;
@group(0) @binding(12) var out_tex: texture_storage_2d<rgba8unorm, write>;

const SRGB_TO_REC2020: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>(0.6274039, 0.0690973, 0.0163914),
    vec3<f32>(0.3292830, 0.9195404, 0.0880133),
    vec3<f32>(0.0433131, 0.0113623, 0.8955953),
);

fn scene_working_at(pos: vec2<i32>) -> vec3<f32> {
    let camera_rgb = textureLoad(scene_tex, clamp_pos(pos), 0).xyz;
    return map_negative_gamut(cam_to_working(camera_rgb));
}

fn blur_luminance(pos: vec2<i32>, radius: i32) -> f32 {
    let center = safe_luma(max(scene_working_at(pos), vec3<f32>(0.0)));
    var sum = 0.0;
    var sum_w = 0.0;
    for (var dy = -radius; dy <= radius; dy = dy + 1) {
        for (var dx = -radius; dx <= radius; dx = dx + 1) {
            let sample_lum = safe_luma(max(scene_working_at(pos + vec2<i32>(dx, dy)), vec3<f32>(0.0)));
            let distance = f32(dx * dx + dy * dy);
            let spatial = 1.0 / (1.0 + distance);
            // Edge-aware weighting keeps detail controls from making halos.
            let range = 1.0 / (1.0 + 12.0 * abs(sample_lum - center));
            let weight = spatial * range;
            sum = sum + sample_lum * weight;
            sum_w = sum_w + weight;
        }
    }
    return sum / max(sum_w, 1e-6);
}

fn apply_texture_and_clarity(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let texture = params.presence.x / 100.0;
    let clarity = params.presence.y / 100.0;
    if abs(texture) < 1e-6 && abs(clarity) < 1e-6 {
        return rgb;
    }

    let lum = safe_luma(max(rgb, vec3<f32>(0.0)));
    let fine_blur = blur_luminance(pos, 1);
    let broad_blur = blur_luminance(pos, 2);
    let fine_detail = lum - fine_blur;
    let mid_detail = lum - broad_blur;
    let midtone_gate = smoothstep(0.015, 0.20, lum) * (1.0 - smoothstep(1.0, 4.0, lum));
    let adjusted_lum = max(
        lum + fine_detail * texture * 0.75 + mid_detail * clarity * 0.60 * midtone_gate,
        0.0,
    );
    return rgb * clamp(adjusted_lum / max(lum, 1e-6), 0.0, 4.0);
}

fn dark_channel(pos: vec2<i32>) -> f32 {
    var dark = 1e20;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let sample = max(scene_working_at(pos + vec2<i32>(dx, dy)), vec3<f32>(0.0));
            dark = min(dark, min(sample.r, min(sample.g, sample.b)));
        }
    }
    return dark;
}

fn apply_dehaze(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = params.presence.z / 100.0;
    if abs(amount) < 1e-6 {
        return rgb;
    }

    // A deliberately conservative dark-channel transmission estimate.  The
    // full Ansel haze module also has global reductions and a guided filter;
    // this local form keeps an interactive mobile preview stable.
    let dark = clamp(dark_channel(pos), 0.0, 1.0);
    let local_lum = safe_luma(max(rgb, vec3<f32>(0.0)));
    let airlight = vec3<f32>(max(0.20, min(1.0, local_lum + 0.20)));
    let transmission = clamp(1.0 - amount * (1.0 - dark) * 0.45, 0.35, 1.65);
    return max((rgb - airlight * (1.0 - transmission)) / transmission, vec3<f32>(0.0));
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let hi = max(max(rgb.r, rgb.g), rgb.b);
    let lo = min(min(rgb.r, rgb.g), rgb.b);
    let delta = hi - lo;
    let lightness = 0.5 * (hi + lo);
    if delta < 1e-6 {
        return vec3<f32>(0.0, 0.0, lightness);
    }

    let saturation = delta / max(1.0 - abs(2.0 * lightness - 1.0), 1e-6);
    var hue = 0.0;
    if hi == rgb.r {
        hue = (rgb.g - rgb.b) / delta;
        if hue < 0.0 { hue = hue + 6.0; }
    } else if hi == rgb.g {
        hue = (rgb.b - rgb.r) / delta + 2.0;
    } else {
        hue = (rgb.r - rgb.g) / delta + 4.0;
    }
    return vec3<f32>(hue / 6.0, saturation, lightness);
}

fn hsl_hue_to_rgb(p_in: f32, q_in: f32, hue_in: f32) -> f32 {
    var hue = hue_in;
    if hue < 0.0 { hue = hue + 1.0; }
    if hue > 1.0 { hue = hue - 1.0; }
    if hue < 1.0 / 6.0 { return p_in + (q_in - p_in) * 6.0 * hue; }
    if hue < 0.5 { return q_in; }
    if hue < 2.0 / 3.0 { return p_in + (q_in - p_in) * (2.0 / 3.0 - hue) * 6.0; }
    return p_in;
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    if hsl.y < 1e-6 {
        return vec3<f32>(hsl.z);
    }
    let q = select(hsl.z * (1.0 + hsl.y), hsl.z + hsl.y - hsl.z * hsl.y, hsl.z >= 0.5);
    let p = 2.0 * hsl.z - q;
    return vec3<f32>(
        hsl_hue_to_rgb(p, q, hsl.x + 1.0 / 3.0),
        hsl_hue_to_rgb(p, q, hsl.x),
        hsl_hue_to_rgb(p, q, hsl.x - 1.0 / 3.0),
    );
}

fn circular_distance(a: f32, b: f32) -> f32 {
    let d = abs(a - b);
    return min(d, 1.0 - d);
}

fn hsl_band_value(hue: f32, a: vec4<f32>, b: vec4<f32>) -> f32 {
    // Feather neighbouring colour bands instead of switching at arbitrary
    // hue boundaries. The anchors are Red, Orange, Yellow, Green, Aqua,
    // Blue, Purple, Magenta.
    let w0 = max(0.0, 1.0 - circular_distance(hue, 0.00) / 0.12);
    let w1 = max(0.0, 1.0 - circular_distance(hue, 0.08) / 0.12);
    let w2 = max(0.0, 1.0 - circular_distance(hue, 0.16) / 0.14);
    let w3 = max(0.0, 1.0 - circular_distance(hue, 0.33) / 0.18);
    let w4 = max(0.0, 1.0 - circular_distance(hue, 0.50) / 0.18);
    let w5 = max(0.0, 1.0 - circular_distance(hue, 0.66) / 0.16);
    let w6 = max(0.0, 1.0 - circular_distance(hue, 0.75) / 0.13);
    let w7 = max(0.0, 1.0 - circular_distance(hue, 0.89) / 0.14);
    let total = w0 + w1 + w2 + w3 + w4 + w5 + w6 + w7;
    return (w0 * a.x + w1 * a.y + w2 * a.z + w3 * a.w
        + w4 * b.x + w5 * b.y + w6 * b.z + w7 * b.w) / max(total, 1e-6);
}

fn apply_hsl_mixer(rgb: vec3<f32>) -> vec3<f32> {
    let linear_srgb = REC2020_TO_SRGB * rgb;
    let peak = max(max(linear_srgb.r, linear_srgb.g), max(linear_srgb.b, 1.0));
    let hsl = rgb_to_hsl(clamp(linear_srgb / peak, vec3<f32>(0.0), vec3<f32>(1.0)));
    if hsl.y < 1e-5 {
        return rgb;
    }

    let hue_adjust = hsl_band_value(hsl.x, params.hsl_hue_0, params.hsl_hue_1);
    let saturation_adjust = hsl_band_value(hsl.x, params.hsl_saturation_0, params.hsl_saturation_1);
    let luminance_adjust = hsl_band_value(hsl.x, params.hsl_luminance_0, params.hsl_luminance_1);
    let adjusted = vec3<f32>(
        fract(hsl.x + hue_adjust / 400.0),
        clamp(hsl.y * (1.0 + saturation_adjust / 100.0), 0.0, 1.0),
        clamp(hsl.z + luminance_adjust / 200.0, 0.0, 1.0),
    );
    return SRGB_TO_REC2020 * hsl_to_rgb(adjusted) * peak;
}

@compute @workgroup_size(8, 8, 1)
fn apply_lightroom_adjustments(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));

    var rgb = scene_working_at(pos);
    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));
    rgb = apply_texture_and_clarity(pos, rgb);
    rgb = apply_dehaze(pos, rgb);
    rgb = apply_saturation_vibrance(rgb);
    rgb = apply_hsl_mixer(rgb);

    textureStore(out_tex, pos, vec4<f32>(display_render(max(rgb, vec3<f32>(0.0)), pos), 1.0));
}
