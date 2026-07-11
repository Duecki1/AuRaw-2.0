// DNG Camera Profile creative stages and the ICC-managed display/output LUT.
// Data is packed as vec4 entries so one read-only storage buffer can carry
// dual-illuminant HueSat maps, LookTable data, a tone-curve LUT, and output LUT.
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

fn profile_srgb_encode_value(value: f32) -> f32 {
    let x = max(value, 0.0);
    return select(
        12.92 * x,
        1.055 * pow(x, 1.0 / 2.4) - 0.055,
        x > 0.0031308,
    );
}

fn profile_srgb_decode_value(value: f32) -> f32 {
    let x = max(value, 0.0);
    return select(
        x / 12.92,
        pow((x + 0.055) / 1.055, 2.4),
        x > 0.04045,
    );
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
    // DNG storage order: value outermost, hue next, saturation innermost.
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
    let profile_rgb = max(REC2020_TO_PROPHOTO * rgb_rec2020, vec3<f32>(0.0));
    var hsv = profile_rgb_to_hsv(profile_rgb);

    // For standard-dynamic-range DCPs, the encoding tags apply only to the
    // HSV value coordinate. Encoding RGB before the HSV conversion changes
    // hue and saturation and is not the DNG-defined operation.
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
    return PROPHOTO_TO_REC2020 * profile_hsv_to_rgb(hsv);
}

fn apply_profile_hue_sat(rgb: vec3<f32>) -> vec3<f32> {
    return apply_profile_hsv_map(rgb, params.profile_hue_sat, params.profile_flags.x);
}

fn apply_profile_look(rgb: vec3<f32>) -> vec3<f32> {
    return apply_profile_hsv_map(rgb, params.profile_look, params.profile_flags.y);
}

fn profile_curve_value(x: f32) -> f32 {
    let size = params.profile_tone.x;
    if size < 2u {
        return x;
    }
    let offset = params.profile_tone.y;
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
    if params.profile_tone.x < 2u {
        return rgb_rec2020;
    }
    let prophoto = REC2020_TO_PROPHOTO * rgb_rec2020;
    let curved = vec3<f32>(
        profile_curve_value(prophoto.r),
        profile_curve_value(prophoto.g),
        profile_curve_value(prophoto.b),
    );
    return PROPHOTO_TO_REC2020 * curved;
}

fn output_lut_fetch(r: u32, g: u32, b: u32) -> vec3<f32> {
    let lut_info = params.output_lut;
    let index = lut_info.w + (b * lut_info.y + g) * lut_info.x + r;
    return profile_data[index].xyz;
}

fn apply_output_lut(rgb: vec3<f32>) -> vec3<f32> {
    let lut_info = params.output_lut;
    if lut_info.x < 2u || lut_info.y < 2u || lut_info.z < 2u {
        return clamp(srgb_oetf(REC2020_TO_SRGB * rgb), vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let coordinate = clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0))
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
    return clamp(mix(low_z, high_z, f.z), vec3<f32>(0.0), vec3<f32>(1.0));
}
