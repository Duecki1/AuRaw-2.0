@group(0) @binding(3) var reconstructed_raw_write: texture_storage_2d<r32float, write>;
@group(0) @binding(13) var highlight_work_read: texture_2d<f32>;
@group(0) @binding(14) var highlight_work_write: texture_storage_2d<rgba16float, write>;

fn store_highlight_work(pos: vec2<i32>, value: vec4<f32>) {
    textureStore(highlight_work_write, pos, value);
}

fn highlight_intensity(rgb: vec3<f32>) -> f32 {
    // The data is still in white-balanced camera RGB, not Rec.2020, so use an
    // equal-energy intensity rather than the display-space LUMA coefficients.
    return max((rgb.r + rgb.g + rgb.b) / 3.0, 1e-6);
}

fn highlight_chroma_ratio(rgb: vec3<f32>) -> vec3<f32> {
    return max(rgb, vec3<f32>(0.0)) / highlight_intensity(rgb);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_prepare(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let sample = highlight_interpolate_and_mask(pos);

    // Alpha is reliability, not a clipping flag. A pixel with one surviving
    // channel remains partially useful, while a fully clipped RGB estimate
    // starts at zero and must be filled from its boundary.
    let clipped_fraction = clamp(
        (sample.clipped.r + sample.clipped.g + sample.clipped.b) / 3.0,
        0.0,
        1.0,
    );
    let reliability = 1.0 - clipped_fraction;
    store_highlight_work(pos, vec4<f32>(sample.rgb, reliability));
}

fn reconstruct_highlight_at(
    pos: vec2<i32>,
    radius: i32,
    minimum_quality: f32,
    gradient_gain: f32,
) -> vec4<f32> {
    let center = textureLoad(highlight_work_read, clamp_pos(pos), 0);
    let guided = params.highlight_options.x >= 1.5;
    let quality = clamp(params.highlight_options.y, 1.0, 4.0);
    let strength = clamp(params.highlight_reconstruction, 0.0, 1.0);

    if !guided || quality < minimum_quality || strength <= 1e-5 {
        return center;
    }

    let center_reliability = clamp(center.w, 0.0, 1.0);
    if center_reliability >= 0.9995 {
        // Reliable source pixels are Dirichlet boundary conditions. Never
        // diffuse into them, otherwise edges outside the clipped region blur.
        return center;
    }

    let center_rgb = max(center.rgb, vec3<f32>(0.0));
    let center_log_intensity = log(highlight_intensity(center_rgb));
    var rgb_sum = vec3<f32>(0.0);
    var reliability_sum = 0.0;
    var weight_sum = 0.0;

    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            if dx == 0 && dy == 0 { continue; }

            let offset = vec2<i32>(dx * radius, dy * radius);
            let neighbour_pos = clamp_pos(pos + offset);
            let outward_pos = clamp_pos(pos + offset * 2);
            let neighbour = textureLoad(highlight_work_read, neighbour_pos, 0);
            let outward = textureLoad(highlight_work_read, outward_pos, 0);
            let neighbour_reliability = clamp(neighbour.w, 0.0, 1.0);

            if neighbour_reliability <= 1e-5 { continue; }

            let neighbour_rgb = max(neighbour.rgb, vec3<f32>(0.0));
            let outward_rgb = max(outward.rgb, vec3<f32>(0.0));
            let neighbour_log_intensity = log(highlight_intensity(neighbour_rgb));
            let outward_reliability = clamp(outward.w, 0.0, 1.0);
            let measured_outward_log_intensity = log(highlight_intensity(outward_rgb));
            // Do not derive a gradient from an unknown second sample. Falling
            // back to the neighbour itself yields a zero-gradient continuation.
            let outward_log_intensity = mix(
                neighbour_log_intensity,
                measured_outward_log_intensity,
                outward_reliability,
            );

            // Extend the reliable outside-to-boundary log-luminance gradient
            // one step into the clipped component. This transports structure,
            // rather than preserving the clipped plateau's original magnitude.
            let log_gradient = clamp(
                neighbour_log_intensity - outward_log_intensity,
                -0.35,
                0.35,
            );
            let candidate_log_intensity = clamp(
                neighbour_log_intensity + gradient_gain * log_gradient,
                center_log_intensity - 1.5,
                center_log_intensity + 1.5,
            );
            let candidate_intensity = exp(candidate_log_intensity);

            let propagated_chroma = highlight_chroma_ratio(neighbour_rgb);
            let colour_reliability = clamp(
                neighbour_reliability
                    * (0.35 + 0.65 * outward_reliability)
                    * params.highlight_options.z,
                0.0,
                1.0,
            );
            let chroma = mix(vec3<f32>(1.0), propagated_chroma, colour_reliability);
            let candidate_rgb = max(chroma * candidate_intensity, vec3<f32>(0.0));

            let distance_squared = f32(dx * dx + dy * dy);
            let spatial_weight = 1.0 / (1.0 + distance_squared);
            let range_weight = 1.0
                / (1.0 + 0.35 * abs(neighbour_log_intensity - center_log_intensity));
            let gradient_weight = 1.0 / (1.0 + 1.5 * abs(log_gradient));
            let reliability_weight = neighbour_reliability * neighbour_reliability;
            let weight = spatial_weight * range_weight * gradient_weight * reliability_weight;

            rgb_sum = rgb_sum + candidate_rgb * weight;
            reliability_sum = reliability_sum + neighbour_reliability * weight;
            weight_sum = weight_sum + weight;
        }
    }

    if weight_sum <= 1e-8 {
        return center;
    }

    let candidate = rgb_sum / weight_sum;
    let propagated_reliability = clamp(
        (reliability_sum / weight_sum) * 0.985,
        0.0,
        1.0,
    );
    let missing = 1.0 - center_reliability;
    let update_amount = clamp(missing * (0.35 + 0.65 * strength), 0.0, 1.0);
    let reconstructed = mix(center_rgb, candidate, update_amount);
    let next_reliability = max(center_reliability, propagated_reliability);

    return vec4<f32>(max(reconstructed, vec3<f32>(0.0)), next_reliability);
}

fn run_highlight_guided_pass(
    gid: vec3<u32>,
    radius: i32,
    minimum_quality: f32,
    gradient_gain: f32,
) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    store_highlight_work(
        pos,
        reconstruct_highlight_at(pos, radius, minimum_quality, gradient_gain),
    );
}

// Quality 1: radius 2 -> 1 (2 passes)
// Quality 2: adds radius 4 and another radius-1 refinement (4 passes)
// Quality 3: adds radius 8 and radius 2/1 refinements (7 passes)
// Quality 4: adds radius 16 and a second multiscale refinement cycle (11 passes)
@compute @workgroup_size(8, 8, 1)
fn highlight_guided_16_a(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 16, 4.0, 0.45);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_8_a(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 8, 3.0, 0.50);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_4_a(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 4, 2.0, 0.55);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_2_a(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 2, 1.0, 0.60);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_1_a(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 1, 1.0, 0.65);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_4_b(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 4, 4.0, 0.45);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_2_b(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 2, 3.0, 0.50);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_1_b(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 1, 2.0, 0.55);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_2_c(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 2, 4.0, 0.45);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_1_c(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 1, 3.0, 0.50);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_guided_1_d(@builtin(global_invocation_id) gid: vec3<u32>) {
    run_highlight_guided_pass(gid, 1, 4.0, 0.45);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_finalize(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let channel = highlight_color_at(pos);
    let original = highlight_raw_camera_at(pos);
    let method = params.highlight_options.x;
    let strength = clamp(params.highlight_reconstruction, 0.0, 1.0);
    var output = original;

    if method >= 0.5 && method < 1.5 {
        output = ansel_lch_reconstructed_cfa_at(pos);
    } else if method >= 1.5 && strength > 1e-5 {
        let guided_rgb = max(textureLoad(highlight_work_read, pos, 0).rgb, vec3<f32>(0.0));
        let guided = guided_rgb[channel];
        let clip_amount = guided_cfa_clip_amount(pos);

        if guided_cfa_is_clipped(pos) {
            // Once a photosite is known to be clipped, its measured value is
            // invalid. Replace it completely instead of blending the plateau
            // back into the reconstruction. The strength slider already
            // controls how strongly the multiscale solver changes its seed.
            output = guided;
        } else if clip_amount > 0.0 {
            // A narrow pre-clip feather avoids a hard seam without modifying
            // ordinary valid highlights.
            output = mix(original, guided, clip_amount * strength);
        }
    }

    textureStore(
        reconstructed_raw_write,
        pos,
        vec4<f32>(max(output, 0.0), 0.0, 0.0, 1.0),
    );
}
