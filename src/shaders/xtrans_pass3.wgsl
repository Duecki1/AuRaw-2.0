// Reconstruct red/blue from colour differences guided by the refined green
// plane. Sampling only measured photosites avoids recursively amplifying seed
// errors and is important for X-Trans' irregular red/blue spacing.
@group(0) @binding(7) var xtrans_green_read: texture_2d<f32>;
@group(0) @binding(8) var xtrans_rgb_write: texture_storage_2d<rgba16float, write>;

fn xtrans3_in_bounds(pos: vec2<i32>) -> bool {
    return pos.x >= 0 && pos.y >= 0
        && pos.x < i32(params.width) && pos.y < i32(params.height);
}

fn xtrans_interpolate_difference(pos: vec2<i32>, channel: u32) -> f32 {
    let center_green = textureLoad(xtrans_green_read, pos, 0).g;
    var sum = 0.0;
    var weight_sum = 0.0;
    var lower = 1e20;
    var upper = -1e20;

    // A 7x7 footprint spans the useful local geometry of the 6x6 X-Trans
    // pattern while remaining practical for an interactive GPU preview.
    for (var dy = -3; dy <= 3; dy = dy + 1) {
        for (var dx = -3; dx <= 3; dx = dx + 1) {
            if dx == 0 && dy == 0 { continue; }
            let sample_pos = pos + vec2<i32>(dx, dy);
            if !xtrans3_in_bounds(sample_pos) { continue; }
            if color_at(sample_pos) != channel { continue; }

            let sample_green = textureLoad(xtrans_green_read, sample_pos, 0).g;
            let difference = raw_cfa_at(sample_pos) - sample_green;
            let distance_squared = f32(dx * dx + dy * dy);
            let spatial_weight = 1.0 / max(distance_squared, 1.0);
            let edge_weight = 1.0 / (1.0 + 24.0 * abs(sample_green - center_green));
            let weight = spatial_weight * edge_weight * edge_weight;
            sum = sum + difference * weight;
            weight_sum = weight_sum + weight;
            lower = min(lower, difference);
            upper = max(upper, difference);
        }
    }

    if weight_sum <= 0.0 {
        let seed = textureLoad(xtrans_green_read, pos, 0);
        return select(seed.r - seed.g, seed.b - seed.g, channel == 2u);
    }

    let estimate = sum / weight_sum;
    let range = max(upper - lower, 1e-4);
    return clamp(estimate, lower - 0.15 * range, upper + 0.15 * range);
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_refine_chroma(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let channel = color_at(pos);
    let base = textureLoad(xtrans_green_read, pos, 0);
    let green = base.g;

    let red_difference = xtrans_interpolate_difference(pos, 0u);
    let blue_difference = xtrans_interpolate_difference(pos, 2u);
    var red = green + red_difference;
    var blue = green + blue_difference;

    if channel == 0u { red = raw_cfa_at(pos); }
    if channel == 2u { blue = raw_cfa_at(pos); }

    textureStore(
        xtrans_rgb_write,
        pos,
        vec4<f32>(max(red, 0.0), max(green, 0.0), max(blue, 0.0), base.a),
    );
}
