fn interpolate_green(pos: vec2<i32>) -> f32 {
    if color_at(pos) == 1u {
        return normalized_raw_at(pos);
    }

    let cross = average4(
        sample_if_color(pos + vec2<i32>(-1, 0), 1u),
        sample_if_color(pos + vec2<i32>(1, 0), 1u),
        sample_if_color(pos + vec2<i32>(0, -1), 1u),
        sample_if_color(pos + vec2<i32>(0, 1), 1u)
    );
    return resolve_average(cross, normalized_raw_at(pos));
}

fn interpolate_red_or_blue(pos: vec2<i32>, channel: u32) -> f32 {
    let center_color = color_at(pos);
    if center_color == channel {
        return normalized_raw_at(pos);
    }

    let fallback = normalized_raw_at(pos);
    if center_color == 1u {
        let horizontal = average2(
            sample_if_color(pos + vec2<i32>(-1, 0), channel),
            sample_if_color(pos + vec2<i32>(1, 0), channel)
        );
        let vertical = average2(
            sample_if_color(pos + vec2<i32>(0, -1), channel),
            sample_if_color(pos + vec2<i32>(0, 1), channel)
        );

        if horizontal.y >= vertical.y && horizontal.y > 0.0 {
            return horizontal.x / horizontal.y;
        }
        return resolve_average(vertical, fallback);
    }

    let diagonal = average4(
        sample_if_color(pos + vec2<i32>(-1, -1), channel),
        sample_if_color(pos + vec2<i32>(1, -1), channel),
        sample_if_color(pos + vec2<i32>(-1, 1), channel),
        sample_if_color(pos + vec2<i32>(1, 1), channel)
    );
    return resolve_average(diagonal, fallback);
}

fn demosaic(pos: vec2<i32>) -> vec3<f32> {
    return vec3<f32>(
        interpolate_red_or_blue(pos, 0u),
        interpolate_green(pos),
        interpolate_red_or_blue(pos, 2u)
    );
}

