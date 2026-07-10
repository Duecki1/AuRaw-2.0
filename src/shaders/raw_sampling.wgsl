fn color_at(pos: vec2<i32>) -> u32 {
    return textureLoad(color_tex, clamp_pos(pos), 0).r;
}

fn is_raw_clipped(pos: vec2<i32>) -> bool {
    let p = clamp_pos(pos);
    let color = min(color_at(p), 3u);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let white = params.white_levels[color];
    return raw >= white - 1.0;
}

fn raw_value_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let color = min(color_at(p), 3u);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let black = params.black_levels[color];
    let white = max(params.white_levels[color], black + 1.0);
    let wb = params.wb[color];
    return clamp((raw - black) / (white - black), 0.0, 4.0) * wb;
}

fn raw_cfa_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let color = min(color_at(p), 3u);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let black = params.black_levels[color];
    let white = max(params.white_levels[color], black + 1.0);
    let wb = params.wb[color];
    return clamp((raw - black) / (white - black), 0.0, 4.0) * wb;
}

fn normalized_raw_at(pos: vec2<i32>) -> f32 {
    let center_color = color_at(pos);
    let center = raw_value_at(pos);
    var sum = 0.0;
    var count = 0.0;

    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
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
    if center > local * 6.0 + 0.25 {
        return local;
    }
    if local > 0.08 && center < local * 0.05 {
        return local;
    }
    return center;
}

fn sample_if_color(pos: vec2<i32>, channel: u32) -> vec3<f32> {
    if color_at(pos) == channel {
        let v = normalized_raw_at(pos);
        let c = select(0.0, 1.0, is_raw_clipped(pos));
        return vec3<f32>(v, 1.0, c);
    }
    return vec3<f32>(0.0, 0.0, 0.0);
}


