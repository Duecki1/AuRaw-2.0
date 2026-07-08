// demosaic.wgsl

fn interpolate_green_at(pos: vec2<i32>) -> f32 {
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
    let green_center = interpolate_green_at(pos);

    if center_color == channel {
        return normalized_raw_at(pos);
    }

    // Color-difference interpolation: estimate (channel - green) at neighbors,
    // average the differences, then add back to the interpolated green.
    if center_color == 1u {
        // Green site: channel samples are axial (N/S or E/W) neighbors
        let left = sample_if_color(pos + vec2<i32>(-1, 0), channel);
        let right = sample_if_color(pos + vec2<i32>(1, 0), channel);
        let top = sample_if_color(pos + vec2<i32>(0, -1), channel);
        let bottom = sample_if_color(pos + vec2<i32>(0, 1), channel);

        var sum_diff = 0.0;
        var count = 0.0;

        if left.y > 0.0 {
            sum_diff += left.x - interpolate_green_at(pos + vec2<i32>(-1, 0));
            count += 1.0;
        }
        if right.y > 0.0 {
            sum_diff += right.x - interpolate_green_at(pos + vec2<i32>(1, 0));
            count += 1.0;
        }
        if top.y > 0.0 {
            sum_diff += top.x - interpolate_green_at(pos + vec2<i32>(0, -1));
            count += 1.0;
        }
        if bottom.y > 0.0 {
            sum_diff += bottom.x - interpolate_green_at(pos + vec2<i32>(0, 1));
            count += 1.0;
        }

        if count > 0.0 {
            return green_center + sum_diff / count;
        }
        return green_center;
    }

    // Opposite-color site: channel samples are diagonal neighbors
    let d1 = sample_if_color(pos + vec2<i32>(-1, -1), channel);
    let d2 = sample_if_color(pos + vec2<i32>(1, -1), channel);
    let d3 = sample_if_color(pos + vec2<i32>(-1, 1), channel);
    let d4 = sample_if_color(pos + vec2<i32>(1, 1), channel);

    var sum_diff = 0.0;
    var count = 0.0;

    if d1.y > 0.0 {
        sum_diff += d1.x - interpolate_green_at(pos + vec2<i32>(-1, -1));
        count += 1.0;
    }
    if d2.y > 0.0 {
        sum_diff += d2.x - interpolate_green_at(pos + vec2<i32>(1, -1));
        count += 1.0;
    }
    if d3.y > 0.0 {
        sum_diff += d3.x - interpolate_green_at(pos + vec2<i32>(-1, 1));
        count += 1.0;
    }
    if d4.y > 0.0 {
        sum_diff += d4.x - interpolate_green_at(pos + vec2<i32>(1, 1));
        count += 1.0;
    }

    if count > 0.0 {
        return green_center + sum_diff / count;
    }
    return green_center;
}

fn demosaic(pos: vec2<i32>) -> vec3<f32> {
    return vec3<f32>(
        interpolate_red_or_blue(pos, 0u),
        interpolate_green_at(pos),
        interpolate_red_or_blue(pos, 2u)
    );
}