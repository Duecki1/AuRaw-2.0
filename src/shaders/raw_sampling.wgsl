fn color_at(pos: vec2<i32>) -> u32 {
    return textureLoad(color_tex, clamp_pos(pos), 0).r;
}

fn raw_value_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let color = min(color_at(p), 3u);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let black = params.black_levels[color];
    let white = max(params.white_levels[color], black + 1.0);
    return clamp((raw - black) / (white - black), 0.0, 4.0);
}

fn normalized_raw_at(pos: vec2<i32>) -> f32 {
    let center_color = color_at(pos);
    let center = raw_value_at(pos);
    var sum = 0.0;
    var count = 0.0;

    for(var dy = -2; dy <= 2; dy = dy + 1) {
        for(var dx = -2; dx <= 2; dx = dx + 1) {
            if dx == 0 && dy == 0 {
                continue;
            }
            let p = pos + vec2<i32>(dx, dy);
            if color_at(p) == center_color {
                sum = sum + raw_value_at(p);
                count = count + 1.0;
            }
        }
    }

    if count < 2.0 {
        return center;
    }

    let local = sum / count;
    if center > max(local * 3.0, local + 0.08) {
        return local;
    }
    if local > 0.03 && center < local * 0.15 {
        return local;
    }
    return center;
}

fn sample_if_color(pos: vec2<i32>, channel: u32) -> vec2<f32> {
    if color_at(pos) == channel {
        return vec2<f32>(normalized_raw_at(pos), 1.0);
    }
    return vec2<f32>(0.0, 0.0);
}

fn average2(a: vec2<f32>, b: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x + b.x, a.y + b.y);
}

fn average4(a: vec2<f32>, b: vec2<f32>, c: vec2<f32>, d: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(a.x + b.x + c.x + d.x, a.y + b.y + c.y + d.y);
}

fn resolve_average(v: vec2<f32>, fallback: f32) -> f32 {
    if v.y > 0.0 {
        return v.x / v.y;
    }
    return fallback;
}

