// Quarter/eighth-resolution image analysis. One pass creates a log-luminance
// guide and histogram, two separable bilateral passes make the guide
// edge-aware, and one tiny reduction pass extracts robust percentiles.

struct ToneHistogram {
    bins: array<atomic<u32>, 256>,
}

@group(0) @binding(11) var tone_scene_tex: texture_2d<f32>;
@group(0) @binding(15) var<storage, read_write> tone_histogram: ToneHistogram;
@group(0) @binding(16) var<storage, read_write> tone_stats_out: ToneStats;
@group(0) @binding(17) var tone_guide_read: texture_2d<f32>;
@group(0) @binding(18) var tone_guide_write: texture_storage_2d<r32float, write>;

fn tone_unexposed_working_at(pos: vec2<i32>) -> vec3<f32> {
    let camera_rgb = textureLoad(tone_scene_tex, clamp_pos(pos), 0).xyz;

    // Sensor black calibration has already happened per CFA plane. Include
    // every fixed camera-profile rendering stage and DNG default exposure so
    // the adaptive bounds describe the same signal that reaches the display
    // transform. Deliberately omit only the user's creative Exposure control,
    // keeping the histogram stable while that slider moves.
    let working = map_negative_gamut(cam_to_working(camera_rgb));
    // Match the rendered DCP order: user white balance precedes the profile's
    // HueSat map. Omitting WB here made adaptive profile statistics describe a
    // different colour/luminance signal than the final render.
    let white_balanced = map_negative_gamut(apply_temperature_tint(working));
    let hue_sat = map_negative_gamut(apply_profile_hue_sat(white_balanced));
    let profile_exposure_ev = bitcast<f32>(params.profile_flags.z);
    let exposed = hue_sat * exp2(profile_exposure_ev);
    let looked = apply_profile_look(exposed);
    let curved = apply_profile_tone_curve(looked);
    return max(curved, vec3<f32>(0.0));
}

@compute @workgroup_size(8, 8, 1)
fn tone_guide_prepare(@builtin(global_invocation_id) gid: vec3<u32>) {
    let guide_size = textureDimensions(tone_guide_write);
    if gid.x >= guide_size.x || gid.y >= guide_size.y { return; }

    let source_size = vec2<u32>(params.width, params.height);
    let cell_min = vec2<u32>(
        gid.x * source_size.x / guide_size.x,
        gid.y * source_size.y / guide_size.y,
    );
    let cell_max = vec2<u32>(
        max((gid.x + 1u) * source_size.x / guide_size.x, cell_min.x + 1u),
        max((gid.y + 1u) * source_size.y / guide_size.y, cell_min.y + 1u),
    );

    var log_sum = 0.0;
    var count = 0.0;
    var brightest_ev = TONE_EV_MIN;
    var y = cell_min.y;
    loop {
        if y >= min(cell_max.y, source_size.y) { break; }
        var x = cell_min.x;
        loop {
            if x >= min(cell_max.x, source_size.x) { break; }
            let rgb = tone_unexposed_working_at(vec2<i32>(i32(x), i32(y)));
            let ev = clamp(
                log2(safe_luma(rgb) / SCENE_MIDDLE_GREY),
                TONE_EV_MIN,
                TONE_EV_MAX,
            );
            log_sum = log_sum + ev;
            brightest_ev = max(brightest_ev, ev);
            count = count + 1.0;

            // Histogram every source pixel rather than the cell average. This
            // preserves small specular highlights and makes the 99.5th
            // percentile independent of the reduced guide resolution.
            let histogram_min = params.tone_histogram_bounds.xy;
            let histogram_max = params.tone_histogram_bounds.zw;
            if x >= histogram_min.x && y >= histogram_min.y
                && x < histogram_max.x && y < histogram_max.y {
                atomicAdd(&tone_histogram.bins[tone_ev_to_bin(ev)], 1u);
            }
            x = x + 1u;
        }
        y = y + 1u;
    }

    let average_ev = log_sum / max(count, 1.0);

    // The local guide should remain stable for broad tonal masks, but retain a
    // bounded trace of a sub-cell highlight instead of averaging it away.
    let guide_ev = max(average_ev, brightest_ev - 1.5);
    textureStore(tone_guide_write, vec2<i32>(gid.xy), vec4<f32>(guide_ev, 0.0, 0.0, 1.0));
}

fn tone_bilateral_guide(pos: vec2<i32>, axis: vec2<i32>) -> f32 {
    let guide_max = vec2<i32>(textureDimensions(tone_guide_read)) - vec2<i32>(1);
    let center_pos = clamp(pos, vec2<i32>(0), guide_max);
    let center = textureLoad(tone_guide_read, center_pos, 0).x;
    let radius = clamp(i32(round(params.tone_guide_radius)), 1, 6);
    let sigma = max(f32(radius) * 0.65, 1.0);

    var weighted_sum = 0.0;
    var weight_sum = 0.0;
    for (var offset = -6; offset <= 6; offset = offset + 1) {
        if abs(offset) > radius { continue; }
        let sample_pos = clamp(center_pos + axis * offset, vec2<i32>(0), guide_max);
        let sample_ev = textureLoad(tone_guide_read, sample_pos, 0).x;
        let distance = f32(offset);
        let spatial_weight = exp(-0.5 * distance * distance / (sigma * sigma));
        let range_weight = exp(-1.35 * abs(sample_ev - center));
        let weight = spatial_weight * range_weight;
        weighted_sum = weighted_sum + sample_ev * weight;
        weight_sum = weight_sum + weight;
    }

    return weighted_sum / max(weight_sum, 1e-6);
}

@compute @workgroup_size(8, 8, 1)
fn tone_guide_horizontal(@builtin(global_invocation_id) gid: vec3<u32>) {
    let guide_size = textureDimensions(tone_guide_write);
    if gid.x >= guide_size.x || gid.y >= guide_size.y { return; }
    let pos = vec2<i32>(gid.xy);
    let value = tone_bilateral_guide(pos, vec2<i32>(1, 0));
    textureStore(tone_guide_write, pos, vec4<f32>(value, 0.0, 0.0, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn tone_guide_vertical(@builtin(global_invocation_id) gid: vec3<u32>) {
    let guide_size = textureDimensions(tone_guide_write);
    if gid.x >= guide_size.x || gid.y >= guide_size.y { return; }
    let pos = vec2<i32>(gid.xy);
    let value = tone_bilateral_guide(pos, vec2<i32>(0, 1));
    textureStore(tone_guide_write, pos, vec4<f32>(value, 0.0, 0.0, 1.0));
}

@compute @workgroup_size(1, 1, 1)
fn tone_reduce_histogram(@builtin(global_invocation_id) gid: vec3<u32>) {
    if any(gid != vec3<u32>(0u)) { return; }

    var total = 0u;
    for (var index = 0u; index < TONE_HISTOGRAM_BIN_COUNT; index = index + 1u) {
        total = total + atomicLoad(&tone_histogram.bins[index]);
    }

    if total == 0u {
        tone_stats_out.percentiles_0 = vec4<f32>(-8.0, -5.0, 0.0, 2.5);
        tone_stats_out.percentiles_1 = vec4<f32>(4.0, 12.0, 0.0, 0.0);
        return;
    }

    let target_005 = max(1u, u32(ceil(f32(total) * 0.005)));
    let target_05 = max(1u, u32(ceil(f32(total) * 0.05)));
    let target_50 = max(1u, u32(ceil(f32(total) * 0.50)));
    let target_95 = max(1u, u32(ceil(f32(total) * 0.95)));
    let target_995 = max(1u, u32(ceil(f32(total) * 0.995)));

    var cumulative = 0u;
    var p005 = TONE_EV_MIN;
    var p05 = TONE_EV_MIN;
    var p50 = 0.0;
    var p95 = TONE_EV_MAX;
    var p995 = TONE_EV_MAX;
    var found_005 = false;
    var found_05 = false;
    var found_50 = false;
    var found_95 = false;
    var found_995 = false;

    for (var index = 0u; index < TONE_HISTOGRAM_BIN_COUNT; index = index + 1u) {
        cumulative = cumulative + atomicLoad(&tone_histogram.bins[index]);
        let ev = tone_bin_to_ev(index);
        if !found_005 && cumulative >= target_005 { p005 = ev; found_005 = true; }
        if !found_05 && cumulative >= target_05 { p05 = ev; found_05 = true; }
        if !found_50 && cumulative >= target_50 { p50 = ev; found_50 = true; }
        if !found_95 && cumulative >= target_95 { p95 = ev; found_95 = true; }
        if !found_995 && cumulative >= target_995 { p995 = ev; found_995 = true; }
    }

    tone_stats_out.percentiles_0 = vec4<f32>(p005, p05, p50, p95);
    tone_stats_out.percentiles_1 = vec4<f32>(p995, max(p995 - p005, 1.0), f32(total), 0.0);
}
