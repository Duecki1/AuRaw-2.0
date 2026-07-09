// demosaic.wgsl

// Returns vec2(value, is_clipped)
fn interpolate_green_at(pos: vec2<i32>) -> vec2<f32> {
    if color_at(pos) == 1u {
        let v = normalized_raw_at(pos);
        let c = select(0.0, 1.0, is_raw_clipped(pos));
        return vec2<f32>(v, c);
    }

    let left = sample_if_color(pos + vec2<i32>(-1, 0), 1u);
    let right = sample_if_color(pos + vec2<i32>(1, 0), 1u);
    let top = sample_if_color(pos + vec2<i32>(0, -1), 1u);
    let bottom = sample_if_color(pos + vec2<i32>(0, 1), 1u);

    let h_sum = left.x + right.x;
    let h_count = left.y + right.y;
    let v_sum = top.x + bottom.x;
    let v_count = top.y + bottom.y;

    var value = 0.0;
    var clipped = 0.0;

    if h_count > 0.0 && v_count > 0.0 {
        let h_grad = abs(left.x - right.x);
        let v_grad = abs(top.x - bottom.x);
        if h_grad <= v_grad {
            value = h_sum / h_count;
            clipped = max(left.z, right.z);
        } else {
            value = v_sum / v_count;
            clipped = max(top.z, bottom.z);
        }
    } else if h_count > 0.0 {
        value = h_sum / h_count;
        clipped = max(left.z, right.z);
    } else if v_count > 0.0 {
        value = v_sum / v_count;
        clipped = max(top.z, bottom.z);
    } else {
        value = normalized_raw_at(pos);
        clipped = select(0.0, 1.0, is_raw_clipped(pos));
    }

    return vec2<f32>(value, clipped);
}

// Returns vec2(value, is_clipped)
fn interpolate_red_or_blue(pos: vec2<i32>, channel: u32) -> vec2<f32> {
    let center_color = color_at(pos);
    let green_center = interpolate_green_at(pos);
    
    if center_color == channel {
        let v = normalized_raw_at(pos);
        let c = select(0.0, 1.0, is_raw_clipped(pos));
        return vec2<f32>(v, c);
    }

    var sum_diff = 0.0;
    var count = 0.0;
    var clipped = 0.0;

    if center_color == 1u {
        let left = sample_if_color(pos + vec2<i32>(-1, 0), channel);
        let right = sample_if_color(pos + vec2<i32>(1, 0), channel);
        let top = sample_if_color(pos + vec2<i32>(0, -1), channel);
        let bottom = sample_if_color(pos + vec2<i32>(0, 1), channel);

        if left.y > 0.0 {
            sum_diff += left.x - interpolate_green_at(pos + vec2<i32>(-1, 0)).x;
            count += 1.0;
            clipped = max(clipped, left.z);
        }
        if right.y > 0.0 {
            sum_diff += right.x - interpolate_green_at(pos + vec2<i32>(1, 0)).x;
            count += 1.0;
            clipped = max(clipped, right.z);
        }
        if top.y > 0.0 {
            sum_diff += top.x - interpolate_green_at(pos + vec2<i32>(0, -1)).x;
            count += 1.0;
            clipped = max(clipped, top.z);
        }
        if bottom.y > 0.0 {
            sum_diff += bottom.x - interpolate_green_at(pos + vec2<i32>(0, 1)).x;
            count += 1.0;
            clipped = max(clipped, bottom.z);
        }
    } else {
        let d1 = sample_if_color(pos + vec2<i32>(-1, -1), channel);
        let d2 = sample_if_color(pos + vec2<i32>(1, -1), channel);
        let d3 = sample_if_color(pos + vec2<i32>(-1, 1), channel);
        let d4 = sample_if_color(pos + vec2<i32>(1, 1), channel);

        if d1.y > 0.0 {
            sum_diff += d1.x - interpolate_green_at(pos + vec2<i32>(-1, -1)).x;
            count += 1.0;
            clipped = max(clipped, d1.z);
        }
        if d2.y > 0.0 {
            sum_diff += d2.x - interpolate_green_at(pos + vec2<i32>(1, -1)).x;
            count += 1.0;
            clipped = max(clipped, d2.z);
        }
        if d3.y > 0.0 {
            sum_diff += d3.x - interpolate_green_at(pos + vec2<i32>(-1, 1)).x;
            count += 1.0;
            clipped = max(clipped, d3.z);
        }
        if d4.y > 0.0 {
            sum_diff += d4.x - interpolate_green_at(pos + vec2<i32>(1, 1)).x;
            count += 1.0;
            clipped = max(clipped, d4.z);
        }
    }

    if count > 0.0 {
        return vec2<f32>(green_center.x + sum_diff / count, max(green_center.y, clipped));
    }
    return vec2<f32>(green_center.x, green_center.y);
}

// Returns vec4(rgb, clip_mask)
fn demosaic(pos: vec2<i32>) -> vec4<f32> {
    let r = interpolate_red_or_blue(pos, 0u);
    let g = interpolate_green_at(pos);
    let b = interpolate_red_or_blue(pos, 2u);
    
    // Pack clip mask: 1=R, 10=G, 100=B
    let mask = r.y * 1.0 + g.y * 10.0 + b.y * 100.0;
    return vec4<f32>(r.x, g.x, b.x, mask);
}