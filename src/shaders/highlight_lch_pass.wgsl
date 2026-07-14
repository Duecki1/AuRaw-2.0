@group(0) @binding(3) var reconstructed_raw_write: texture_storage_2d<r32float, write>;
@group(0) @binding(13) var highlight_work_read: texture_2d<f32>;
@group(0) @binding(14) var highlight_work_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

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

// Keep the original per-channel clipping mask alongside the propagated
// support confidence. The integer part stores RGB clip bits and the fractional
// half-range stores confidence, which remains representable in both RGBA16F
// preview and RGBA32F high-quality work textures.
fn highlight_encode_state(mask: u32, confidence: f32) -> f32 {
    return f32(mask) + 0.5 * clamp(confidence, 0.0, 1.0);
}

fn highlight_state_mask(encoded: f32) -> u32 {
    return u32(floor(max(encoded, 0.0)));
}

fn highlight_state_confidence(encoded: f32) -> f32 {
    return clamp(2.0 * fract(max(encoded, 0.0)), 0.0, 1.0);
}

fn highlight_clip_mask(clipped: vec3<f32>) -> u32 {
    return select(0u, 1u, clipped.r > 0.5)
        | select(0u, 2u, clipped.g > 0.5)
        | select(0u, 4u, clipped.b > 0.5);
}

fn highlight_mask_channels(mask: u32) -> vec3<f32> {
    return vec3<f32>(
        select(0.0, 1.0, (mask & 1u) != 0u),
        select(0.0, 1.0, (mask & 2u) != 0u),
        select(0.0, 1.0, (mask & 4u) != 0u),
    );
}

fn highlight_opposed_power_mean(a: f32, b: f32) -> f32 {
    let root = 0.5 * (
        pow(max(a, 0.0), 1.0 / 3.0)
        + pow(max(b, 0.0), 1.0 / 3.0)
    );
    return root * root * root;
}

fn highlight_safe_seed(sample: HighlightSample, mask: u32) -> vec3<f32> {
    var seed = max(sample.rgb, vec3<f32>(0.0));
    let clipped_count = sample.clipped.r + sample.clipped.g + sample.clipped.b;

    if clipped_count > 1.5 {
        // With two unknown components there is only one trustworthy colour
        // coordinate; with three there is no hue evidence at all. A common
        // peak keeps the strongest measured saturation lower bound while the
        // valid CFA sites are selectively restored during remosaicing.
        return vec3<f32>(max(seed.r, max(seed.g, seed.b)));
    }

    // One missing component can be estimated from its two opposed, surviving
    // channels. The cube-root power mean is robust to a strongly coloured
    // survivor, and max() never lowers the sensor's saturation lower bound.
    if (mask & 1u) != 0u {
        seed.r = max(seed.r, highlight_opposed_power_mean(seed.g, seed.b));
    } else if (mask & 2u) != 0u {
        seed.g = max(seed.g, highlight_opposed_power_mean(seed.r, seed.b));
    } else if (mask & 4u) != 0u {
        seed.b = max(seed.b, highlight_opposed_power_mean(seed.r, seed.g));
    }
    return seed;
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
    let confidence = 1.0 - clipped_fraction;
    let mask = highlight_clip_mask(sample.clipped);
    let seed = highlight_safe_seed(sample, mask);
    store_highlight_work(pos, vec4<f32>(seed, highlight_encode_state(mask, confidence)));
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

    if !guided || quality < minimum_quality || strength <= 0.0 {
        return center;
    }

    let center_mask = highlight_state_mask(center.w);
    let center_reliability = highlight_state_confidence(center.w);
    if center_mask == 0u {
        // Unclipped source pixels are Dirichlet boundary conditions. The clip
        // mask stays persistent even after confidence has propagated inward,
        // so reconstructed channels can receive every refinement pass.
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
            let neighbour_reliability = highlight_state_confidence(neighbour.w);

            if neighbour_reliability <= 1e-5 { continue; }

            let neighbour_rgb = max(neighbour.rgb, vec3<f32>(0.0));
            let outward_rgb = max(outward.rgb, vec3<f32>(0.0));
            let neighbour_log_intensity = log(highlight_intensity(neighbour_rgb));
            let outward_reliability = highlight_state_confidence(outward.w);
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

    var candidate = rgb_sum / weight_sum;
    let propagated_reliability = clamp(
        (reliability_sum / weight_sum) * 0.985,
        0.0,
        1.0,
    );
    let unknown_channels = highlight_mask_channels(center_mask);
    let known_channels = vec3<f32>(1.0) - unknown_channels;
    let known_energy = dot(center_rgb * candidate, known_channels);
    let candidate_known_energy = dot(candidate * candidate, known_channels);
    if candidate_known_energy > 1e-10 {
        // A surviving sensor component is an exact exposure anchor. Transport
        // the boundary's chroma/gradient, but scale it to agree with those
        // measured components before filling only the unknown channels.
        let anchor_scale = clamp(known_energy / candidate_known_energy, 0.25, 4.0);
        candidate = candidate * anchor_scale;
    }
    // Saturated measurements and the opposed-channel seed are lower bounds,
    // not values that diffusion may darken. Guided structure can add missing
    // energy, while the final display transform remains responsible for
    // compressing it into the output range.
    let proposed = max(candidate, center_rgb);
    // Only replace components known to be invalid. Surviving sensor channels
    // retain their measured values and anchor colour/luminance propagation.
    let reconstructed = mix(
        center_rgb,
        proposed,
        unknown_channels,
    );
    let next_reliability = max(center_reliability, propagated_reliability);

    return vec4<f32>(
        max(reconstructed, vec3<f32>(0.0)),
        highlight_encode_state(center_mask, next_reliability),
    );
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
    } else if method >= 1.5 && strength > 0.0 {
        let final_sample = textureLoad(highlight_work_read, pos, 0);
        var guided_rgb = max(final_sample.rgb, vec3<f32>(0.0));
        let clip_mask = highlight_state_mask(final_sample.w);
        if clip_mask == 7u {
            // Every sensor plane was saturated, so hue is unknowable. Keep a
            // common post-WB lower bound even if no guided boundary reached
            // this pixel; this is the hard invariant that prevents magenta
            // cores from reappearing when exposure is reduced.
            let maximum_wb = max(
                max(params.wb.r, params.wb.g),
                max(params.wb.b, params.wb.a),
            );
            let neutral_floor = guided_sensor_clip() * maximum_wb;
            guided_rgb = max(guided_rgb, vec3<f32>(neutral_floor));
        }
        let guided = guided_rgb[channel];
        let clip_amount = guided_cfa_clip_amount(pos);

        // Reconstruct a full-strength candidate internally, then apply the UI
        // strength exactly once. This keeps the slider continuous from the
        // untouched RAW plateau at zero to complete replacement at one.
        output = mix(original, guided, clip_amount * strength);
    }

    textureStore(
        reconstructed_raw_write,
        pos,
        vec4<f32>(max(output, 0.0), 0.0, 0.0, 1.0),
    );
}
