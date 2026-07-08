fn interpolate_green(pos: vec2<i32>) -> f32 {
    if color_at(pos) == 1u {
        return normalized_raw_at(pos);
    }

    let left = sample_if_color(pos + vec2<i32>(-1, 0), 1u);
    let right = sample_if_color(pos + vec2<i32>(1, 0), 1u);
    let top = sample_if_color(pos + vec2<i32>(0, -1), 1u);
    let bottom = sample_if_color(pos + vec2<i32>(0, 1), 1u);

    let h_sum = left.x + right.x;
    let h_count = left.y + right.y;
    let v_sum = top.x + bottom.x;
    let v_count = top.y + bottom.y;

    // *** FIX: use gradient-based edge direction, not sample count. ***
    // The old code used average4() on all four neighbors (isotropic),
    // which causes zippering on edges. Now we pick the smoother
    // direction (lower gradient) like PPG/Malvar.
    if h_count > 0.0 && v_count > 0.0 {
        let h_grad = abs(left.x - right.x);
        let v_grad = abs(top.x - bottom.x);
        if h_grad <= v_grad {
            return h_sum / h_count;
        }
        return v_sum / v_count;
    }
    if h_count > 0.0 {
        return h_sum / h_count;
    }
    if v_count > 0.0 {
        return v_sum / v_count;
    }
    return normalized_raw_at(pos);
}

fn interpolate_red_or_blue(pos: vec2<i32>, channel: u32) -> f32 {
    let center_color = color_at(pos);
    if center_color == channel {
        return normalized_raw_at(pos);
    }

    let fallback = normalized_raw_at(pos);
    if center_color == 1u {
        let left = sample_if_color(pos + vec2<i32>(-1, 0), channel);
        let right = sample_if_color(pos + vec2<i32>(1, 0), channel);
        let top = sample_if_color(pos + vec2<i32>(0, -1), channel);
        let bottom = sample_if_color(pos + vec2<i32>(0, 1), channel);

        let h_sum = left.x + right.x;
        let h_count = left.y + right.y;
        let v_sum = top.x + bottom.x;
        let v_count = top.y + bottom.y;

        // *** FIX: gradient-based edge direction (was sample-count based). ***
        if h_count > 0.0 && v_count > 0.0 {
            let h_grad = abs(left.x - right.x);
            let v_grad = abs(top.x - bottom.x);
            if h_grad <= v_grad {
                return h_sum / h_count;
            }
            return v_sum / v_count;
        }
        if h_count > 0.0 {
            return h_sum / h_count;
        }
        if v_count > 0.0 {
            return v_sum / v_count;
        }
        return fallback;
    }

    // On opposite-color pixel: diagonal average
    let diagonal = average4(
        sample_if_color(pos + vec2<i32>(-1, -1), channel),
        sample_if_color(pos + vec2<i32>(1, -1), channel),
        sample_if_color(pos + vec2<i32>(-1, 1), channel),
        sample_if_color(pos + vec2<i32>(1, 1), channel),
    );
    return resolve_average(diagonal, fallback);
}

fn demosaic(pos: vec2<i32>) -> vec3<f32> {
    return vec3<f32>(
        interpolate_red_or_blue(pos, 0u),
        interpolate_green(pos),
        interpolate_red_or_blue(pos, 2u),
    );
}