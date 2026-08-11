// Deterministic procedural atmosphere for the non-destructive Fog and Smoke
// mask effects. Every field is evaluated in full-image coordinates, making the
// pattern invariant under viewport crops and independently rendered tiles.

fn atmosphere_hash(position: vec2<f32>) -> f32 {
    let signed_cell = vec2<i32>(position);
    let cell = bitcast<vec2<u32>>(signed_cell);
    var state = cell.x * 1597334677u ^ cell.y * 3812015801u;
    state = (state ^ (state >> 16u)) * 2246822519u;
    state = (state ^ (state >> 13u)) * 3266489917u;
    state = state ^ (state >> 16u);
    return f32(state & 0x00ffffffu) / 16777215.0;
}

fn atmosphere_noise(position: vec2<f32>) -> f32 {
    let cell = floor(position);
    let fraction = fract(position);
    let smooth_fraction = fraction * fraction * (vec2<f32>(3.0) - 2.0 * fraction);
    let bottom = mix(
        atmosphere_hash(cell),
        atmosphere_hash(cell + vec2<f32>(1.0, 0.0)),
        smooth_fraction.x,
    );
    let top = mix(
        atmosphere_hash(cell + vec2<f32>(0.0, 1.0)),
        atmosphere_hash(cell + vec2<f32>(1.0, 1.0)),
        smooth_fraction.x,
    );
    return mix(bottom, top, smooth_fraction.y);
}

fn atmosphere_fbm(position: vec2<f32>) -> f32 {
    var point = position;
    var amplitude = 0.5;
    var value = 0.0;
    var normalization = 0.0;
    for (var octave = 0u; octave < 5u; octave = octave + 1u) {
        value = value + atmosphere_noise(point) * amplitude;
        normalization = normalization + amplitude;
        point = vec2<f32>(
            point.x * 1.62 + point.y * 1.17,
            point.x * -1.17 + point.y * 1.62,
        ) + vec2<f32>(13.7, 9.2);
        amplitude = amplitude * 0.52;
    }
    return value / max(normalization, 1e-6);
}

fn atmosphere_image_point(pos: vec2<i32>) -> vec2<f32> {
    let full_size = max(
        vec2<f32>(
            f32(Common::camera_uniforms.full_width),
            f32(Common::camera_uniforms.full_height),
        ),
        vec2<f32>(1.0),
    );
    var point = full_image_uv(pos) - vec2<f32>(0.5);
    point.x = point.x * full_size.x / full_size.y;
    return point;
}

fn apply_fog(
    pos: vec2<i32>,
    input_rgb: vec3<f32>,
    primary: vec4<f32>,
    secondary: vec4<f32>,
    tertiary: vec4<f32>,
) -> vec3<f32> {
    let amount = clamp(primary.x / 100.0, 0.0, 1.0);
    let density = clamp(primary.y / 100.0, 0.0, 1.0);
    if amount <= 1e-6 || density <= 1e-6 {
        return input_rgb;
    }

    let scale = clamp(primary.z / 100.0, 0.01, 1.0);
    let frequency = mix(10.0, 2.0, scale);
    let softness = clamp(primary.w / 100.0, 0.0, 1.0);
    let variation = clamp(secondary.w / 100.0, 0.0, 1.0);
    let seed = clamp(tertiary.x, 0.0, 1000.0);
    let offset = vec2<f32>(seed * 0.071 + 19.3, seed * -0.113 + 47.1);
    let point = atmosphere_image_point(pos) * frequency + offset;
    let broad = atmosphere_fbm(point);
    let fine = atmosphere_noise(point * 2.1 + vec2<f32>(7.4, -3.8));
    let field = broad * 0.84 + fine * 0.16;
    let transition = mix(0.035, 0.24, softness);
    let banks = smoothstep(0.52 - transition, 0.52 + transition, field);
    let density_field = mix(0.72, 0.28 + 1.05 * banks, variation);
    let opacity = clamp(amount * density * density_field * 1.22, 0.0, 0.92);
    let color = mask_effect_picker_color_to_working(secondary.xyz);
    return mix(input_rgb, color, opacity);
}

fn apply_smoke(
    pos: vec2<i32>,
    input_rgb: vec3<f32>,
    primary: vec4<f32>,
    secondary: vec4<f32>,
    tertiary: vec4<f32>,
) -> vec3<f32> {
    let amount = clamp(primary.x / 100.0, 0.0, 1.0);
    let density = clamp(primary.y / 100.0, 0.0, 1.0);
    if amount <= 1e-6 || density <= 1e-6 {
        return input_rgb;
    }

    let angle = radians(clamp(secondary.w, -180.0, 180.0));
    let cosine = cos(angle);
    let sine = sin(angle);
    let image_point = atmosphere_image_point(pos);
    var point = vec2<f32>(
        cosine * image_point.x - sine * image_point.y,
        sine * image_point.x + cosine * image_point.y,
    );
    point = point * vec2<f32>(0.78, 1.18);

    let scale = clamp(primary.z / 100.0, 0.01, 1.0);
    let frequency = mix(11.0, 2.4, scale);
    let turbulence = clamp(primary.w / 100.0, 0.0, 1.0);
    let softness = clamp(tertiary.x / 100.0, 0.0, 1.0);
    let seed = clamp(tertiary.y, 0.0, 1000.0);
    let offset = vec2<f32>(seed * 0.097 + 31.6, seed * -0.067 + 8.9);

    let warp_point = point * frequency * 0.58 + offset;
    let warp = vec2<f32>(
        atmosphere_fbm(warp_point + vec2<f32>(0.0, 17.2)),
        atmosphere_fbm(warp_point + vec2<f32>(23.4, 0.0)),
    ) - vec2<f32>(0.5);
    let warped = point * frequency + offset
        + warp * mix(0.10, 2.35, turbulence);
    let body = atmosphere_fbm(warped);
    let ridge_noise = atmosphere_noise(warped * 2.15 + vec2<f32>(5.7, 12.9));
    let ridges = 1.0 - abs(ridge_noise * 2.0 - 1.0);
    let field = mix(body, body * 0.78 + ridges * 0.22, turbulence);
    let threshold = mix(0.68, 0.39, density);
    let transition = mix(0.025, 0.20, softness);
    let plume = smoothstep(threshold - transition, threshold + transition, field);
    let opacity = clamp(
        amount * mix(0.35, 1.0, density) * plume * 1.08,
        0.0,
        0.94,
    );
    let color = mask_effect_picker_color_to_working(secondary.xyz);
    return mix(input_rgb, color, opacity);
}
