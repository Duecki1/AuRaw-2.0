// Markesteijn final directional selection. Build the 5x5 sum of each 3x3
// homogeneity map, quench the weaker member of opposite-direction pairs, and
// average only candidates within one eighth of the maximum map response.
@group(0) @binding(24) var mark_homo_0_3_read: texture_2d<f32>;
@group(0) @binding(25) var mark_homo_4_7_read: texture_2d<f32>;
@group(0) @binding(26) var mark_high_write: texture_storage_2d<rgba16float, write>;

fn mark_homo(pos: vec2<i32>, index: u32) -> f32 {
    let p = clamp_pos(pos);
    if index < 4u {
        return textureLoad(mark_homo_0_3_read, p, 0)[index];
    }
    return textureLoad(mark_homo_4_7_read, p, 0)[index - 4u];
}

fn mark_homo_sum5(pos: vec2<i32>, index: u32) -> f32 {
    var sum = 0.0;
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            sum += mark_homo(pos + vec2<i32>(dx, dy), index);
        }
    }
    return sum;
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_accumulate(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    if !mark_has_margin(pos) {
        textureStore(mark_high_write, pos, vec4<f32>(mark_border_rgb(pos), 1.0));
        return;
    }

    var hm: array<f32, 8>;
    var maximum = 0.0;
    for (var index = 0u; index < 8u; index = index + 1u) {
        hm[index] = mark_homo_sum5(pos, index);
        maximum = max(maximum, hm[index]);
    }
    let cutoff = maximum - floor(maximum / 8.0);

    // Markesteijn-3 keeps one of each opposing pair when their homogeneity
    // differs, avoiding a blurred average of incompatible directions.
    for (var index = 0u; index < 4u; index = index + 1u) {
        if hm[index] < hm[index + 4u] {
            hm[index] = 0.0;
        } else if hm[index] > hm[index + 4u] {
            hm[index + 4u] = 0.0;
        }
    }

    var sum = vec3<f32>(0.0);
    var count = 0.0;
    for (var index = 0u; index < 8u; index = index + 1u) {
        if hm[index] >= cutoff {
            sum += mark_candidate(pos, index);
            count += 1.0;
        }
    }
    var rgb = select(mark_load(pos), sum / max(count, 1.0), count > 0.0);
    let measured_channel = color_at(pos);
    rgb = mark_set_component(
        rgb,
        measured_channel,
        mark_component(mark_load(pos), measured_channel),
    );
    textureStore(mark_high_write, pos, vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0));
}
