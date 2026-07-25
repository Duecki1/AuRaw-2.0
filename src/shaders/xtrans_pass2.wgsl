// Markesteijn X-Trans passes 1 and 3. Pass 1 performs bounded high-order green
// interpolation at red/blue sites. Pass 3 refines missing red/blue components
// as directional color differences. The ping-pong layout intentionally keeps
// these operations separate from the homogeneity-selection stage.
@group(0) @binding(5) var markesteijn_read_13: texture_2d<f32>;
@group(0) @binding(6) var markesteijn_write_13: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn mark13_load(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(markesteijn_read_13, clamp_pos(pos), 0).rgb;
}

fn mark13_component(rgb: vec3<f32>, channel: u32) -> f32 {
    if channel == 0u { return rgb.r; }
    if channel == 1u { return rgb.g; }
    return rgb.b;
}

fn mark13_set(rgb: vec3<f32>, channel: u32, value: f32) -> vec3<f32> {
    var out = rgb;
    if channel == 0u { out.r = value; }
    if channel == 1u { out.g = value; }
    if channel == 2u { out.b = value; }
    return out;
}

fn mark13_green_bounds(pos: vec2<i32>) -> vec2<f32> {
    var lo = 1e20;
    var hi = -1e20;
    for (var dy = -3; dy <= 3; dy = dy + 1) {
        for (var dx = -3; dx <= 3; dx = dx + 1) {
            let q = pos + vec2<i32>(dx, dy);
            if q.x < 0 || q.y < 0 || q.x >= i32(params.width) || q.y >= i32(params.height) {
                continue;
            }
            if color_at(q) == 1u {
                let value = raw_cfa_at(q);
                lo = min(lo, value);
                hi = max(hi, value);
            }
        }
    }
    if hi < lo { return vec2<f32>(0.0, 1e20); }
    return vec2<f32>(lo, hi);
}

fn mark13_green_axis(pos: vec2<i32>, axis: vec2<i32>, measured_channel: u32) -> vec2<f32> {
    let center = mark13_load(pos);
    let m1 = mark13_load(pos - axis);
    let p1 = mark13_load(pos + axis);
    let m2 = mark13_load(pos - 2 * axis);
    let p2 = mark13_load(pos + 2 * axis);

    // High-order coefficients are the constants used by Markesteijn's initial
    // green interpolation. The color-channel correction keeps the estimate
    // anchored to the measured red/blue sample.
    let base = 0.6796875 * (m1.g + p1.g) - 0.1796875 * (m2.g + p2.g);
    let c0 = mark13_component(center, measured_channel);
    let cm = mark13_component(m2, measured_channel);
    let cp = mark13_component(p2, measured_channel);
    let estimate = base + 0.12890625 * (2.0 * c0 - cm - cp);
    let gradient = 1e-5
        + abs(m1.g - p1.g)
        + abs(m2.g - p2.g)
        + abs((mark13_component(m1, measured_channel) - m1.g)
            - (mark13_component(p1, measured_channel) - p1.g));
    return vec2<f32>(estimate, gradient);
}

fn mark13_pass1(pos: vec2<i32>) -> vec3<f32> {
    let center = mark13_load(pos);
    let measured_channel = color_at(pos);
    if measured_channel == 1u { return center; }

    let h = mark13_green_axis(pos, vec2<i32>(1, 0), measured_channel);
    let v = mark13_green_axis(pos, vec2<i32>(0, 1), measured_channel);
    let p = mark13_green_axis(pos, vec2<i32>(1, 1), measured_channel);
    let q = mark13_green_axis(pos, vec2<i32>(1, -1), measured_channel);
    let wh = 1.0 / (h.y * h.y);
    let wv = 1.0 / (v.y * v.y);
    let wp = 1.0 / (p.y * p.y);
    let wq = 1.0 / (q.y * q.y);
    let bounds = mark13_green_bounds(pos);
    let green = clamp(
        (h.x * wh + v.x * wv + p.x * wp + q.x * wq) / max(wh + wv + wp + wq, 1e-8),
        bounds.x,
        bounds.y,
    );
    var out = center;
    out.g = green;
    out = mark13_set(out, measured_channel, raw_cfa_at(pos));
    return out;
}

fn mark13_color_axis(pos: vec2<i32>, axis: vec2<i32>, channel: u32) -> vec2<f32> {
    let center = mark13_load(pos);
    let m1 = mark13_load(pos - axis);
    let p1 = mark13_load(pos + axis);
    let m2 = mark13_load(pos - 2 * axis);
    let p2 = mark13_load(pos + 2 * axis);
    let dm = mark13_component(m1, channel) - m1.g;
    let dp = mark13_component(p1, channel) - p1.g;
    let gm = 1e-5 + abs(center.g - m2.g) + abs(m1.g - p1.g)
        + abs(dm - (mark13_component(m2, channel) - m2.g));
    let gp = 1e-5 + abs(center.g - p2.g) + abs(m1.g - p1.g)
        + abs(dp - (mark13_component(p2, channel) - p2.g));
    return vec2<f32>((gm * dp + gp * dm) / (gm + gp), gm + gp);
}

fn mark13_refine_channel(pos: vec2<i32>, channel: u32) -> f32 {
    let h = mark13_color_axis(pos, vec2<i32>(1, 0), channel);
    let v = mark13_color_axis(pos, vec2<i32>(0, 1), channel);
    let p = mark13_color_axis(pos, vec2<i32>(1, 1), channel);
    let q = mark13_color_axis(pos, vec2<i32>(1, -1), channel);
    let wh = 1.0 / (h.y * h.y);
    let wv = 1.0 / (v.y * v.y);
    let wp = 1.0 / (p.y * p.y);
    let wq = 1.0 / (q.y * q.y);
    return (h.x * wh + v.x * wv + p.x * wp + q.x * wq) / max(wh + wv + wp + wq, 1e-8);
}

fn mark13_pass3(pos: vec2<i32>) -> vec3<f32> {
    let center = mark13_load(pos);
    let measured_channel = color_at(pos);
    var out = center;
    if measured_channel != 0u { out.r = center.g + mark13_refine_channel(pos, 0u); }
    if measured_channel != 2u { out.b = center.g + mark13_refine_channel(pos, 2u); }
    out = mark13_set(out, measured_channel, raw_cfa_at(pos));
    return out;
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_pass1(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(markesteijn_write_13, pos, vec4<f32>(mark13_pass1(pos), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_pass3(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(markesteijn_write_13, pos, vec4<f32>(mark13_pass3(pos), 1.0));
}
