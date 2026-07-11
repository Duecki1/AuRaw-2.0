@group(0) @binding(3) var reconstructed_raw_write: texture_storage_2d<r32float, write>;
@group(0) @binding(13) var highlight_work_read: texture_2d<f32>;
@group(0) @binding(14) var highlight_work_write: texture_storage_2d<rgba16float, write>;

fn store_highlight_work(pos: vec2<i32>, value: vec4<f32>) {
    textureStore(highlight_work_write, pos, value);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_prepare(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let sample = highlight_interpolate_and_mask(pos);
    let confidence = max(sample.clipped.r, max(sample.clipped.g, sample.clipped.b));
    store_highlight_work(pos, vec4<f32>(sample.rgb, confidence));
}

fn diffuse_highlight_at(pos: vec2<i32>, radius: i32, pass_index: f32) -> vec4<f32> {
    let center = textureLoad(highlight_work_read, clamp_pos(pos), 0);
    let guided = params.highlight_options.x >= 1.5;
    let pass_enabled = pass_index <= clamp(params.highlight_options.y, 1.0, 4.0);
    if !guided || !pass_enabled || center.w <= 1e-5 {
        return center;
    }

    let center_rgb = max(center.rgb, vec3<f32>(0.0));
    let center_norm = max(length(center_rgb), 1e-6);
    var chroma_sum = vec3<f32>(0.0);
    var sum_weight = 0.0;
    var confidence_sum = 0.0;

    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            if dx == 0 && dy == 0 { continue; }
            let p = clamp_pos(pos + vec2<i32>(dx * radius, dy * radius));
            let sample = textureLoad(highlight_work_read, p, 0);
            let sample_rgb = max(sample.rgb, vec3<f32>(0.0));
            let sample_norm = max(length(sample_rgb), 1e-6);
            let spatial = 1.0 / (1.0 + f32(dx * dx + dy * dy));
            let reliability = 0.02 + 0.98 * (1.0 - clamp(sample.w, 0.0, 1.0));
            let range = 1.0 / (1.0 + 2.0 * abs(sample_norm - center_norm));
            let weight = spatial * reliability * range;
            chroma_sum = chroma_sum + (sample_rgb / sample_norm) * weight;
            confidence_sum = confidence_sum + clamp(sample.w, 0.0, 1.0) * weight;
            sum_weight = sum_weight + weight;
        }
    }

    if sum_weight <= 1e-8 || dot(chroma_sum, chroma_sum) <= 1e-12 {
        return center;
    }

    let neighbour_chroma = normalize(chroma_sum / sum_weight);
    let neighbour_confidence = clamp(confidence_sum / sum_weight, 0.0, 1.0);
    let next_confidence = clamp(center.w * neighbour_confidence, 0.0, 1.0);
    let neutral_chroma = vec3<f32>(INV_SQRT3);
    // Deep inside a fully clipped patch, cautiously converge toward neutral;
    // near a reliable boundary, retain the colour propagated from that edge.
    let colour_reliability = 1.0 - neighbour_confidence;
    let colour_amount = clamp(params.highlight_options.z, 0.0, 1.0) * colour_reliability;
    let recovered_chroma = normalize(mix(neutral_chroma, neighbour_chroma, colour_amount));
    let recovered = recovered_chroma * center_norm;
    let progress = clamp(0.20 + (1.0 - next_confidence), 0.0, 1.0);
    let rgb = mix(center_rgb, recovered, progress);
    return vec4<f32>(rgb, next_confidence);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_diffuse_1(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    store_highlight_work(pos, diffuse_highlight_at(pos, 1, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn highlight_diffuse_2(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    store_highlight_work(pos, diffuse_highlight_at(pos, 2, 2.0));
}

@compute @workgroup_size(8, 8, 1)
fn highlight_diffuse_4(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    store_highlight_work(pos, diffuse_highlight_at(pos, 4, 3.0));
}

@compute @workgroup_size(8, 8, 1)
fn highlight_diffuse_8(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    store_highlight_work(pos, diffuse_highlight_at(pos, 8, 4.0));
}

@compute @workgroup_size(8, 8, 1)
fn highlight_finalize(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let channel = highlight_color_at(pos);
    let original = highlight_raw_camera_at(pos);
    let method = params.highlight_options.x;
    var output = original;

    if method >= 0.5 && method < 1.5 {
        output = ansel_lch_reconstructed_cfa_at(pos);
    } else if method >= 1.5 {
        let guided_rgb = max(textureLoad(highlight_work_read, pos, 0).rgb, vec3<f32>(0.0));
        let guided = guided_rgb[channel];
        let blend = guided_clipping_mask(pos) * clamp(params.highlight_reconstruction, 0.0, 1.0);
        output = mix(original, guided, blend);
    }

    textureStore(reconstructed_raw_write, pos, vec4<f32>(max(output, 0.0), 0.0, 0.0, 1.0));
}
