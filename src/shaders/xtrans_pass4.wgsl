// Final X-Trans false-colour suppression, optional chroma denoise, lateral CA,
// and scene-linear output.
@group(0) @binding(7) var xtrans_green_read_final: texture_2d<f32>;
@group(0) @binding(9) var xtrans_rgb_read: texture_2d<f32>;
@group(0) @binding(10) var xtrans_scene_write: texture_storage_2d<rgba16float, write>;

fn xtrans_rgb_at(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(xtrans_rgb_read, clamp_pos(pos), 0).rgb;
}

fn xtrans_smoothed_chroma(pos: vec2<i32>, radius: i32) -> vec2<f32> {
    let center = xtrans_rgb_at(pos);
    let center_green = center.g;
    var sum = vec2<f32>(0.0);
    var weight_sum = 0.0;

    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            if abs(dx) > radius || abs(dy) > radius { continue; }
            let sample = xtrans_rgb_at(pos + vec2<i32>(dx, dy));
            let distance_squared = f32(dx * dx + dy * dy);
            let spatial_weight = 1.0 / (1.0 + distance_squared);
            let edge_weight = 1.0 / (1.0 + 32.0 * abs(sample.g - center_green));
            let weight = spatial_weight * edge_weight * edge_weight;
            sum = sum + vec2<f32>(sample.r - sample.g, sample.b - sample.g) * weight;
            weight_sum = weight_sum + weight;
        }
    }

    if weight_sum > 0.0 {
        return sum / weight_sum;
    }
    return vec2<f32>(center.r - center.g, center.b - center.g);
}

fn xtrans_green_frequency(pos: vec2<i32>) -> f32 {
    let center = xtrans_rgb_at(pos).g;
    let north = xtrans_rgb_at(pos + vec2<i32>(0, -1)).g;
    let south = xtrans_rgb_at(pos + vec2<i32>(0, 1)).g;
    let west = xtrans_rgb_at(pos + vec2<i32>(-1, 0)).g;
    let east = xtrans_rgb_at(pos + vec2<i32>(1, 0)).g;
    return abs(4.0 * center - north - south - west - east);
}

fn xtrans_warped_pos(pos: vec2<i32>, amount: f32) -> vec2<f32> {
    let extent = vec2<f32>(f32(params.width - 1u), f32(params.height - 1u));
    let center = 0.5 * extent;
    let p = vec2<f32>(pos);
    let relative = p - center;
    let normalized = relative / max(center, vec2<f32>(1.0));
    let radius_squared = dot(normalized, normalized);
    let scale = 1.0 + amount * 0.001 * radius_squared;
    return clamp(center + relative * scale, vec2<f32>(0.0), extent);
}

fn xtrans_rgb_bilinear(pos: vec2<f32>) -> vec3<f32> {
    let floored = floor(pos);
    let p0 = vec2<i32>(i32(floored.x), i32(floored.y));
    let p1 = p0 + vec2<i32>(1, 1);
    let fraction = fract(pos);
    let v00 = xtrans_rgb_at(p0);
    let v10 = xtrans_rgb_at(vec2<i32>(p1.x, p0.y));
    let v01 = xtrans_rgb_at(vec2<i32>(p0.x, p1.y));
    let v11 = xtrans_rgb_at(p1);
    return mix(mix(v00, v10, fraction.x), mix(v01, v11, fraction.x), fraction.y);
}

fn xtrans_apply_lateral_ca(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    var output = rgb;
    if abs(params.ca_red) >= 1e-6 {
        output.r = xtrans_rgb_bilinear(xtrans_warped_pos(pos, params.ca_red)).r;
    }
    if abs(params.ca_blue) >= 1e-6 {
        output.b = xtrans_rgb_bilinear(xtrans_warped_pos(pos, params.ca_blue)).b;
    }
    return output;
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_output(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let rgb = xtrans_rgb_at(pos);
    let center_chroma = vec2<f32>(rgb.r - rgb.g, rgb.b - rgb.g);
    let fine_chroma = xtrans_smoothed_chroma(pos, 1);
    let broad_chroma = xtrans_smoothed_chroma(pos, 2);

    // X-Trans false colour is most visible where the green plane has strong
    // high-frequency energy. Apply a small automatic suppression there, then
    // let the existing chroma-denoise control add a broader user adjustment.
    let high_frequency = smoothstep(0.008, 0.10, xtrans_green_frequency(pos));
    let automatic = 0.30 * high_frequency;
    let user_strength = clamp(params.chroma_denoise, 0.0, 1.0);
    let chroma = mix(
        mix(center_chroma, fine_chroma, automatic),
        broad_chroma,
        user_strength * mix(0.35, 1.0, high_frequency),
    );

    var camera_rgb = vec3<f32>(rgb.g + chroma.x, rgb.g, rgb.g + chroma.y);
    camera_rgb = xtrans_apply_lateral_ca(pos, camera_rgb);
    textureStore(
        xtrans_scene_write,
        pos,
        vec4<f32>(max(camera_rgb, vec3<f32>(0.0)), 1.0),
    );
}
