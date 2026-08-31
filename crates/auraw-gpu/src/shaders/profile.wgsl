#import auraw::common as Common
#import auraw::color as Color

@group(0) @binding(20) var<storage, read> profile_data: array<vec4<f32>>;

const REC2020_TO_PROPHOTO: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 0.83528284,  0.05403228, -0.00234171),
    vec3<f32>( 0.04887048,  0.92886970,  0.03632753),
    vec3<f32>( 0.11595392,  0.01705474,  0.96588654),
);

const PROPHOTO_TO_REC2020: mat3x3<f32> = mat3x3<f32>(
    vec3<f32>( 1.20052188, -0.06993601,  0.00554089),
    vec3<f32>(-0.05756611,  1.08067472, -0.04078434),
    vec3<f32>(-0.14310526, -0.01068580,  1.03537324),
);

const PROPHOTO_LUMA: vec3<f32> = vec3<f32>(0.26828944, 0.71515419, 0.01656066);

fn profile_srgb_encode_value(value: f32) -> f32 {
    let magnitude = abs(value);
    if magnitude >= 1.0 {
        return sign(value);
    }
    let encoded = select(
        12.92 * magnitude,
        1.055 * pow(magnitude, 1.0 / 2.4) - 0.055,
        magnitude > 0.0031308,
    );
    return sign(value) * encoded;
}

fn profile_srgb_decode_value(value: f32) -> f32 {
    let magnitude = abs(value);
    let decoded = select(
        magnitude / 12.92,
        pow((magnitude + 0.055) / 1.055, 2.4),
        magnitude > 0.04045,
    );
    return sign(value) * decoded;
}

fn profile_rgb_to_hsv(rgb: vec3<f32>) -> vec3<f32> {
    let hi = max(rgb.r, max(rgb.g, rgb.b));
    let lo = min(rgb.r, min(rgb.g, rgb.b));
    let delta = hi - lo;
    var hue = 0.0;
    if delta > 1e-10 {
        if hi == rgb.r {
            hue = (rgb.g - rgb.b) / delta;
        } else if hi == rgb.g {
            hue = 2.0 + (rgb.b - rgb.r) / delta;
        } else {
            hue = 4.0 + (rgb.r - rgb.g) / delta;
        }
        hue = fract(hue / 6.0 + 1.0);
    }
    let saturation = select(0.0, delta / hi, hi > 1e-10);
    return vec3<f32>(hue, saturation, hi);
}

fn profile_hsv_to_rgb(hsv: vec3<f32>) -> vec3<f32> {
    let h = fract(hsv.x) * 6.0;
    let sector = i32(floor(h));
    let f = h - floor(h);
    let p = hsv.z * (1.0 - hsv.y);
    let q = hsv.z * (1.0 - hsv.y * f);
    let t = hsv.z * (1.0 - hsv.y * (1.0 - f));
    switch sector {
        case 0: { return vec3<f32>(hsv.z, t, p); }
        case 1: { return vec3<f32>(q, hsv.z, p); }
        case 2: { return vec3<f32>(p, hsv.z, t); }
        case 3: { return vec3<f32>(p, q, hsv.z); }
        case 4: { return vec3<f32>(t, p, hsv.z); }
        default: { return vec3<f32>(hsv.z, p, q); }
    }
}

fn profile_map_index(map_info: vec4<u32>, h: u32, s: u32, v: u32) -> u32 {
    return map_info.w + (v * map_info.x + h) * map_info.y + s;
}

fn profile_map_fetch(map_info: vec4<u32>, h: u32, s: u32, v: u32) -> vec3<f32> {
    return profile_data[profile_map_index(map_info, h, s, v)].xyz;
}

fn profile_map_sample(map_info: vec4<u32>, hsv: vec3<f32>) -> vec3<f32> {
    let hue_count = max(map_info.x, 1u);
    let saturation_count = max(map_info.y, 2u);
    let value_count = max(map_info.z, 1u);

    let hue_position = fract(hsv.x) * f32(hue_count);
    let hue_floor = floor(hue_position);
    let h0 = u32(hue_floor) % hue_count;
    let h1 = (h0 + 1u) % hue_count;
    let hf = hue_position - hue_floor;

    let saturation_position = clamp(hsv.y, 0.0, 1.0) * f32(saturation_count - 1u);
    let s0 = u32(floor(saturation_position));
    let s1 = min(s0 + 1u, saturation_count - 1u);
    let sf = saturation_position - f32(s0);

    let value_position = clamp(hsv.z, 0.0, 1.0) * f32(value_count - 1u);
    let v0 = u32(floor(value_position));
    let v1 = min(v0 + 1u, value_count - 1u);
    let vf = value_position - f32(v0);

    let c000 = profile_map_fetch(map_info, h0, s0, v0);
    let c100 = profile_map_fetch(map_info, h1, s0, v0);
    let c010 = profile_map_fetch(map_info, h0, s1, v0);
    let c110 = profile_map_fetch(map_info, h1, s1, v0);
    let c001 = profile_map_fetch(map_info, h0, s0, v1);
    let c101 = profile_map_fetch(map_info, h1, s0, v1);
    let c011 = profile_map_fetch(map_info, h0, s1, v1);
    let c111 = profile_map_fetch(map_info, h1, s1, v1);

    let low_s0 = mix(c000, c100, hf);
    let low_s1 = mix(c010, c110, hf);
    let high_s0 = mix(c001, c101, hf);
    let high_s1 = mix(c011, c111, hf);
    return mix(mix(low_s0, low_s1, sf), mix(high_s0, high_s1, sf), vf);
}

fn apply_profile_hsv_map(rgb_rec2020: vec3<f32>, map_info: vec4<u32>, encoding: u32) -> vec3<f32> {
    if map_info.x == 0u || map_info.y == 0u || map_info.z == 0u {
        return rgb_rec2020;
    }
    let profile_signed = REC2020_TO_PROPHOTO * rgb_rec2020;
    let profile_linear = Color::gamut_project_nonnegative(profile_signed, dot(profile_signed, PROPHOTO_LUMA));
    let profile_headroom = max(
        1.0,
        max(profile_linear.r, max(profile_linear.g, profile_linear.b)),
    );
    let profile_rgb = profile_linear / profile_headroom;
    var hsv = profile_rgb_to_hsv(profile_rgb);

    let encode_value = encoding == 1u && map_info.z > 1u;
    if encode_value {
        hsv.z = profile_srgb_encode_value(hsv.z);
    }
    let adjustment = profile_map_sample(map_info, hsv);
    hsv.x = fract(hsv.x + adjustment.x / 360.0 + 1.0);
    hsv.y = clamp(hsv.y * adjustment.y, 0.0, 1.0);
    hsv.z = clamp(hsv.z * adjustment.z, 0.0, 1.0);
    if encode_value {
        hsv.z = profile_srgb_decode_value(hsv.z);
    }
    return PROPHOTO_TO_REC2020 * profile_hsv_to_rgb(hsv) * profile_headroom;
}

fn apply_profile_hue_sat(rgb: vec3<f32>) -> vec3<f32> {
    let second = bitcast<vec4<u32>>(profile_data[0]);
    if second.x == 0u || second.y == 0u || second.z == 0u {
        return apply_profile_hsv_map(rgb, Common::camera_uniforms.profile_hue_sat, Common::camera_uniforms.profile_flags.x);
    }

    let profile_signed = REC2020_TO_PROPHOTO * rgb;
    let profile_linear = Color::gamut_project_nonnegative(profile_signed, dot(profile_signed, PROPHOTO_LUMA));
    let profile_headroom = max(
        1.0,
        max(profile_linear.r, max(profile_linear.g, profile_linear.b)),
    );
    let profile_rgb = profile_linear / profile_headroom;
    var hsv = profile_rgb_to_hsv(profile_rgb);
    let encode_value = Common::camera_uniforms.profile_flags.x == 1u
        && (Common::camera_uniforms.profile_hue_sat.z > 1u || second.z > 1u);
    if encode_value {
        hsv.z = profile_srgb_encode_value(hsv.z);
    }
    let weight = clamp(bitcast<f32>(Common::camera_uniforms.profile_flags.w), 0.0, 1.0);
    let adjustment = mix(
        profile_map_sample(Common::camera_uniforms.profile_hue_sat, hsv),
        profile_map_sample(second, hsv),
        weight,
    );
    hsv.x = fract(hsv.x + adjustment.x / 360.0 + 1.0);
    hsv.y = clamp(hsv.y * adjustment.y, 0.0, 1.0);
    hsv.z = clamp(hsv.z * adjustment.z, 0.0, 1.0);
    if encode_value {
        hsv.z = profile_srgb_decode_value(hsv.z);
    }
    return PROPHOTO_TO_REC2020 * profile_hsv_to_rgb(hsv) * profile_headroom;
}

fn apply_camera_characterization(rgb: vec3<f32>) -> vec3<f32> {
    return apply_profile_hue_sat(rgb);
}

fn apply_optional_profile_look(rgb: vec3<f32>) -> vec3<f32> {
    return apply_profile_hsv_map(rgb, Common::camera_uniforms.profile_look, Common::camera_uniforms.profile_flags.y);
}

fn profile_curve_value(x: f32) -> f32 {
    let size = Common::camera_uniforms.profile_tone.x;
    if size < 2u {
        return x;
    }
    let offset = Common::camera_uniforms.profile_tone.y;
    let maximum = size - 1u;
    if x <= 0.0 {
        return profile_data[offset].x;
    }
    if x >= 1.0 {
        return profile_data[offset + maximum].x;
    }
    let position = x * f32(maximum);
    let low = u32(floor(position));
    let high = min(low + 1u, maximum);
    return mix(
        profile_data[offset + low].x,
        profile_data[offset + high].x,
        position - f32(low),
    );
}

fn apply_profile_tone_curve(rgb_rec2020: vec3<f32>) -> vec3<f32> {
    if Common::camera_uniforms.profile_tone.x < 2u {
        return rgb_rec2020;
    }

    let prophoto_signed = REC2020_TO_PROPHOTO * rgb_rec2020;
    let prophoto_linear = Color::gamut_project_nonnegative(prophoto_signed, dot(prophoto_signed, PROPHOTO_LUMA));
    let profile_headroom = max(
        1.0,
        max(prophoto_linear.r, max(prophoto_linear.g, prophoto_linear.b)),
    );
    let prophoto = prophoto_linear / profile_headroom;
    let low = min(prophoto.r, min(prophoto.g, prophoto.b));
    let high = max(prophoto.r, max(prophoto.g, prophoto.b));
    if high - low <= 1e-8 {
        let value = profile_curve_value(low);
        return PROPHOTO_TO_REC2020 * vec3<f32>(value) * profile_headroom;
    }

    let mapped_low = profile_curve_value(low);
    let mapped_high = profile_curve_value(high);
    let scale = (mapped_high - mapped_low) / (high - low);
    let curved = vec3<f32>(mapped_low)
        + (prophoto - vec3<f32>(low)) * scale;
    return PROPHOTO_TO_REC2020 * curved * profile_headroom;
}

fn apply_profile_view_tone(rgb: vec3<f32>) -> vec3<f32> {
    return apply_profile_tone_curve(rgb);
}

fn output_lut_fetch(r: u32, g: u32, b: u32) -> vec3<f32> {
    let lut_info = Common::camera_uniforms.output_lut;
    let index = lut_info.w + (b * lut_info.y + g) * lut_info.x + r;
    return profile_data[index].xyz;
}

fn map_output_lut_input_rec2020(rgb: vec3<f32>) -> vec3<f32> {
    if Color::rgb_is_unit(rgb) {
        return clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    return Color::perceptual_gamut_compress_unit_rec2020(rgb);
}

fn apply_output_lut(rgb: vec3<f32>) -> vec3<f32> {
    // The output LUT is sRGB-encoded; never apply an sRGB transfer afterward.
    let lut_info = Common::camera_uniforms.output_lut;
    if lut_info.x < 2u || lut_info.y < 2u || lut_info.z < 2u {
        let output_linear = Common::REC2020_TO_SRGB * rgb;
        let mapped = Color::perceptual_gamut_compress_unit_srgb(output_linear);
        return Color::srgb_oetf(mapped);
    }
    let mapped = map_output_lut_input_rec2020(rgb);
    let shaped = vec3<f32>(
        profile_srgb_encode_value(mapped.r),
        profile_srgb_encode_value(mapped.g),
        profile_srgb_encode_value(mapped.b),
    );
    let lookup_coordinate = clamp(shaped, vec3<f32>(0.0), vec3<f32>(1.0));
    let coordinate = lookup_coordinate
        * vec3<f32>(f32(lut_info.x - 1u), f32(lut_info.y - 1u), f32(lut_info.z - 1u));
    let low = vec3<u32>(floor(coordinate));
    let high = min(low + vec3<u32>(1u), lut_info.xyz - vec3<u32>(1u));
    let f = coordinate - vec3<f32>(low);

    let c000 = output_lut_fetch(low.x, low.y, low.z);
    let c100 = output_lut_fetch(high.x, low.y, low.z);
    let c010 = output_lut_fetch(low.x, high.y, low.z);
    let c110 = output_lut_fetch(high.x, high.y, low.z);
    let c001 = output_lut_fetch(low.x, low.y, high.z);
    let c101 = output_lut_fetch(high.x, low.y, high.z);
    let c011 = output_lut_fetch(low.x, high.y, high.z);
    let c111 = output_lut_fetch(high.x, high.y, high.z);

    let low_z = mix(mix(c000, c100, f.x), mix(c010, c110, f.x), f.y);
    let high_z = mix(mix(c001, c101, f.x), mix(c011, c111, f.x), f.y);
    return mix(low_z, high_z, f.z);
}
