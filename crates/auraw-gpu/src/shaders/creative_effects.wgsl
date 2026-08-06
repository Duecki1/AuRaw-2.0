// Creative effects layered after scene-domain adjustments: dehaze, Glow diffusion,
// and the post-crop vignette used by the final display-linear pass.

// Lightroom-like post-crop vignette calibration anchors. Each vec4 stores
// (smoothstep start radius, smoothstep end radius, falloff exponent, corner
// opacity). These are empirical curve fits measured from linear-light
// differences against Lightroom Amount -50, -100, +50, and +100 reference
// renders at the default Midpoint/Feather settings; they are not analytic lens
// vignetting coefficients. Darkening uses a broader transition and lower power,
// while brightening uses a narrower, steeper shoulder.
const VIGNETTE_DARK_HALF_FIT: vec4<f32> = vec4<f32>(0.10, 1.235, 2.88, 0.86);
const VIGNETTE_DARK_FULL_FIT: vec4<f32> = vec4<f32>(0.02, 1.135, 3.46, 1.0);
const VIGNETTE_LIGHT_HALF_FIT: vec4<f32> = vec4<f32>(0.305, 1.24, 4.36, 0.90);
const VIGNETTE_LIGHT_FULL_FIT: vec4<f32> = vec4<f32>(0.13, 1.075, 5.66, 1.0);

struct HazeNeighborhood {
    dark_ratio: f32,
    airlight: vec3<f32>,
    airlight_luma: f32,
}

fn normalized_dark_ratio(rgb: vec3<f32>, airlight_luma: f32) -> f32 {
    let positive = gamut_project_nonnegative_rec2020(rgb);
    let normalized = positive / max(airlight_luma, 1e-6);
    return clamp(min(normalized.r, min(normalized.g, normalized.b)), 0.0, 1.0);
}

fn haze_neighborhood(pos: vec2<i32>, step: i32, airlight_luma: f32) -> HazeNeighborhood {
    var dark_ratio = 1.0;
    var brightest = adjustment_base_at(pos);
    var brightest_luma = safe_luma(brightest);
    var haziest_ratio = normalized_dark_ratio(brightest, airlight_luma);
    // A scale-aware 7x7 dark-channel stencil tracks a similar subject-space
    // footprint in previews and exports. The denser stencil avoids the phase
    // holes produced by the previous sparse 5x5 gather.
    for (var ky = -3; ky <= 3; ky = ky + 1) {
        for (var kx = -3; kx <= 3; kx = kx + 1) {
            let sample = adjustment_base_at(pos + vec2<i32>(kx * step, ky * step));
            let sample_dark_ratio = normalized_dark_ratio(sample, airlight_luma);
            dark_ratio = min(dark_ratio, sample_dark_ratio);
            let luminance = safe_luma(sample);
            // The colour hint comes from the locally haziest candidate, not
            // simply the brightest edge/specular. Its energy is replaced by
            // the image-global ambient estimate below.
            if sample_dark_ratio > haziest_ratio
                || (abs(sample_dark_ratio - haziest_ratio) < 1e-5
                    && luminance > brightest_luma) {
                brightest = sample;
                brightest_luma = luminance;
                haziest_ratio = sample_dark_ratio;
            }
        }
    }
    return HazeNeighborhood(dark_ratio, brightest, brightest_luma);
}

fn apply_dehaze_value(pos: vec2<i32>, rgb: vec3<f32>, value: f32) -> vec3<f32> {
    let amount = perceptual_control(value);
    if abs(amount) < 1e-6 {
        return rgb;
    }

    let center_lum = safe_luma(rgb);
    let center_ev = log2(center_lum);
    let broad_ev = bilateral_log_luminance(
        pos,
        2,
        presence_step(1.0, 3),
        0.95,
    );
    // Darktable estimates one ambient A0 for the complete image before it
    // builds the transmission map. ToneStats already contains the full-image
    // 99.5th-percentile luminance in pre-user-exposure EV, which is a stable,
    // tile-safe approximation of that global ambient energy. The old shader
    // promoted each pixel's local brightest neighbour to A, causing colour and
    // contrast to pump across edges and export tiles.
    let ambient_ev = clamp(tone_stats.percentiles_1.x + scene_tone_uniforms.exposure, -16.0, 16.0);
    let airlight_luma = max(SCENE_MIDDLE_GREY * exp2(ambient_ev), 1e-5);
    let haze_step = presence_step(2.0, 6);
    let neighborhood = haze_neighborhood(pos, haze_step, airlight_luma);

    let airlight_colour = neighborhood.airlight
        / max(neighborhood.airlight_luma, 1e-6) * airlight_luma;
    // Keep ambient energy global and retain only a restrained local colour
    // hint. This stays seam-safe while avoiding a forced neutral cast in blue
    // sky, sunset, and warm indoor haze.
    let airlight = max(
        mix(vec3<f32>(airlight_luma), airlight_colour, 0.14),
        vec3<f32>(airlight_luma * 0.28),
    );
    let dark_ratio = neighborhood.dark_ratio;
    let veil = smoothstep(0.025, 0.78, dark_ratio);
    let low_contrast = 1.0 - smoothstep(0.10, 0.72, abs(center_ev - broad_ev));
    // The edge-aware broad guide refines the raw dark-channel estimate in the
    // same spirit as Darktable's guided transmission filter: real edges are
    // protected while low-contrast veil receives the stronger correction.
    let haze_likelihood = clamp(veil * (0.52 + 0.48 * low_contrast), 0.0, 1.0);

    if amount > 0.0 {
        // Lightroom's +100 endpoint is aggressive in the toe but remains
        // monotone through the midtones. The former transmission range
        // (down to 0.22) subtracted 20-78% of A and drove broad, ordinary
        // midtones to black. Keep physical veil subtraction near one percent
        // of global ambient, then use a restrained ambient-relative mask that
        // supplies about 1 EV in lower midtones and fades near airlight.
        let ambient_position = clamp(center_lum / max(airlight_luma, 1e-6), 0.0, 1.0);
        let shaped_position = pow(ambient_position, 0.33);
        let mid_position_hump = 0.30 * shaped_position * (1.0 - shaped_position);
        let tone_mask = min(
            1.0,
            1.0 - tone_smoothstep(0.0, 1.0, shaped_position) + mid_position_hump,
        );
        let transmission = 1.0 - amount * mix(0.008, 0.012, haze_likelihood);
        let physical = (rgb - airlight * (1.0 - transmission)) / transmission;
        let physical_lum = safe_luma(physical);
        let luminance_gain = clamp(physical_lum / max(center_lum, 1e-6), 0.0, 2.0);
        let hue_safe = rgb * luminance_gain;
        var restored = mix(hue_safe, physical, 0.30 + 0.16 * haze_likelihood);
        restored = restored * exp2(-amount * 0.90 * tone_mask);

        let local_detail = clamp(center_ev - broad_ev, -1.2, 1.2);
        restored = restored * exp2(amount * local_detail * 0.12);
        let lab = linear_srgb_to_oklab(REC2020_TO_SRGB * restored);
        let chroma = length(lab.yz);
        let content_saturation = clamp(chroma / max(0.045 + 0.38 * lab.x, 0.06), 0.0, 1.0);
        let chroma_boost = 1.0
            + amount * (0.30 + 0.22 * tone_mask)
                * (1.0 - 0.10 * content_saturation);
        return perceptual_gamut_compress_nonnegative_rec2020(
            SRGB_TO_REC2020 * oklab_to_linear_srgb(
                vec3<f32>(lab.x, lab.yz * chroma_boost),
            ),
        );
    }

    // Negative Dehaze is a controlled move toward ambient. Lightroom protects
    // absolute black, lifts the lower midtones more than AuRaw's old
    // haze-likelihood inversion, and strongly reduces chroma in the veil.
    // Increase the mix with normalized scene brightness: this avoids turning
    // the darkest one percent into a grey pedestal while filling the broad
    // lower-mid range.
    let haze = -amount;
    let ambient_position = clamp(center_lum / max(airlight_luma, 1e-6), 0.0, 1.0);
    let position_weight = pow(ambient_position, 0.35);
    let haze_mix = clamp(haze * mix(0.045, 0.23, position_weight), 0.0, 0.30);
    let hazed = mix(rgb, airlight, haze_mix);
    let lab = linear_srgb_to_oklab(REC2020_TO_SRGB * hazed);
    let desaturation = 1.0 - haze * mix(0.32, 0.27, haze_likelihood);
    return perceptual_gamut_compress_nonnegative_rec2020(
        SRGB_TO_REC2020 * oklab_to_linear_srgb(
            vec3<f32>(lab.x, lab.yz * desaturation),
        ),
    );
}

fn extended_perceptual_luminance(linear_luma: f32) -> f32 {
    if linear_luma <= 1.0 {
        return pow(max(linear_luma, 0.0), 1.0 / 2.2);
    }
    // C1-continuous extension: value and derivative match the 1/2.2 power
    // curve at one, while logarithmic growth avoids an HDR threshold cusp.
    return 1.0 + (1.0 / 2.2) * log(linear_luma);
}

fn glow_emission(rgb: vec3<f32>, cutoff: f32) -> vec3<f32> {
    // Glow is an emissive positive-domain effect. Project a local proxy for the
    // extraction math without overwriting the signed scene RGB carried by the
    // main pipeline.
    let glow_rgb = gamut_project_nonnegative_rec2020(rgb);
    let linear_luma = safe_luma(glow_rgb);
    let perceptual_luma = extended_perceptual_luminance(linear_luma);
    let cutoff_fade = smoothstep(cutoff, cutoff + 0.16, perceptual_luma);
    let excess = max(perceptual_luma - cutoff, 0.0);
    let range = max(2.25 - cutoff, 0.25);
    let intensity = pow(smoothstep(0.0, range, excess), 0.48);
    let black_gate = pow(smoothstep(0.0, 0.42, linear_luma), 0.5);

    // Preserve the source hue while giving the bloom the subtle warm bias of
    // optical diffusion. The source is normalized by luminance so a coloured
    // light blooms in its own colour instead of becoming neutral grey. The
    // ratio is softly clamped so very narrow-band highlights cannot explode
    // into dotted colour speckles when the blur radius becomes large.
    let colour_ratio = clamp(glow_rgb / max(linear_luma, 1e-6), vec3<f32>(0.0), vec3<f32>(3.5));
    let warm_tint = vec3<f32>(1.025, 1.0, 0.975);
    return colour_ratio * warm_tint
        * intensity * pow(linear_luma, 0.62) * cutoff_fade * black_gate;
}

fn glow_cutoff() -> f32 {
    let threshold = clamp(effects_uniforms.creative_effects.z / 100.0, 0.0, 1.0);
    return mix(0.06, 0.92, pow(threshold, 1.12));
}

fn glow_work_at(pos: vec2<i32>) -> vec3<f32> {
    return textureLoad(glow_work_tex, clamp_pos(pos), 0).xyz;
}

fn glow_stage_step(stage: u32) -> i32 {
    // The cascade has a maximum cumulative support of 96 pixels:
    // 2 * (3 + 3 + 6 + 12 + 24) at the capped 3x reference scale.
    // That bound is mirrored by GLOW_SUPPORT in processing.rs.
    var reference_step = 1.0;
    switch stage {
        case 2u: { reference_step = 2.0; }
        case 3u: { reference_step = 4.0; }
        case 4u: { reference_step = 8.0; }
        default: {}
    }
    let scale = clamp(
        f32(min(camera_uniforms.full_width, camera_uniforms.full_height)) / 1080.0,
        0.45,
        3.0,
    );
    return max(i32(round(reference_step * scale)), 1);
}

fn glow_stage_mix(stage: u32) -> f32 {
    let radius = clamp(effects_uniforms.creative_effects.y / 100.0, 0.0, 1.0);
    switch stage {
        case 0u: { return 1.0; }
        case 1u: { return smoothstep(0.0, 0.20, radius); }
        case 2u: { return smoothstep(0.15, 0.45, radius); }
        case 3u: { return smoothstep(0.40, 0.75, radius); }
        default: { return smoothstep(0.70, 1.0, radius); }
    }
}

fn glow_diffuse_at(pos: vec2<i32>, stage: u32) -> vec3<f32> {
    let center = glow_work_at(pos);
    let stage_mix = glow_stage_mix(stage);
    if stage_mix < 1e-6 {
        return center;
    }

    let step = glow_stage_step(stage);
    var sum = vec3<f32>(0.0);
    var sum_weight = 0.0;
    // Each pass is one normalized B3-spline diffusion step. Cascading adjacent
    // scales gives every source a continuous path to every halo pixel. The old
    // direct +/-step lattice could only see highlights whose phase happened to
    // align with one of its sparse taps, producing dotted/ringed Glow.
    for (var ky = -2; ky <= 2; ky = ky + 1) {
        for (var kx = -2; kx <= 2; kx = kx + 1) {
            let weight = atrous_kernel_weight(kx) * atrous_kernel_weight(ky);
            let sample_pos = pos + vec2<i32>(kx * step, ky * step);
            sum = sum + glow_work_at(sample_pos) * weight;
            sum_weight = sum_weight + weight;
        }
    }
    return mix(center, sum / max(sum_weight, 1e-6), stage_mix);
}

fn apply_glow(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = clamp(effects_uniforms.creative_effects.x / 100.0, 0.0, 1.0);
    if amount < 1e-6 {
        return rgb;
    }

    let bloom = glow_work_at(pos);

    // Very bright cores already carry their own energy. Protecting them keeps
    // Glow from clipping the light source while the blurred halo expands into
    // the surrounding darker pixels.
    let current_luma = safe_luma(rgb);
    let core_protection = 1.0 - 0.72 * smoothstep(1.0, 3.2, current_luma);
    return rgb + bloom * amount * 2.8 * core_protection;
}

fn full_image_uv(pos: vec2<i32>) -> vec2<f32> {
    let dimensions = max(
        vec2<f32>(f32(camera_uniforms.full_width), f32(camera_uniforms.full_height)),
        vec2<f32>(1.0),
    );
    let global_pos = clamp(pos + tile_origin(), vec2<i32>(0), full_image_max());
    return (vec2<f32>(global_pos) + vec2<f32>(0.5)) / dimensions;
}

fn vignette_distance(pos: vec2<i32>, roundness: f32) -> f32 {
    let dimensions = max(effects_uniforms.vignette_frame.zw, vec2<f32>(1.0));
    let source_delta = full_image_uv(pos) - effects_uniforms.vignette_frame.xy;
    let transform = effects_uniforms.vignette_transform;
    // Evaluate directly in final-frame coordinates even though the view pass
    // runs before geometry resampling. X and Y are normalized by their
    // own final-frame half extent, matching darktable's auto-ratio geometry.
    // The same normalized point therefore receives the same falloff on 3:2,
    // 4:3, square, portrait, and panoramic frames.
    let frame_uv = vec2<f32>(
        0.5 + transform.x * source_delta.x + transform.y * source_delta.y,
        0.5 + transform.z * source_delta.x + transform.w * source_delta.y,
    );
    let p = abs(frame_uv * 2.0 - vec2<f32>(1.0));
    let frame_ellipse = length(p);
    let frame_rectangle = pow(pow(p.x, 8.0) + pow(p.y, 8.0), 1.0 / 8.0);
    let short_dimension = max(min(dimensions.x, dimensions.y), 1.0);
    let image_circle = length(vec2<f32>(
        p.x * dimensions.x / short_dimension,
        p.y * dimensions.y / short_dimension,
    ));

    if abs(roundness) < 1e-6 {
        return frame_ellipse;
    }
    if roundness < 0.0 {
        return mix(frame_ellipse, frame_rectangle, -roundness);
    }
    return mix(frame_ellipse, image_circle, roundness);
}

fn calibrated_vignette_anchor(
    distance: f32,
    start: f32,
    end: f32,
    power: f32,
    edge_opacity: f32,
) -> f32 {
    return edge_opacity * pow(smoothstep(start, end, distance), power);
}

fn lightroom_vignette_opacity(
    distance: f32,
    amount: f32,
    midpoint: f32,
    feather: f32,
) -> f32 {
    let magnitude = clamp(abs(amount), 0.0, 1.0);
    // Preserve the corner endpoint while shifting the transition inward or
    // outward. At the default Midpoint 50 this is an exact identity.
    var shaped_distance = distance;
    if abs(midpoint - 0.5) >= 1e-6 {
        let midpoint_power = exp2((midpoint - 0.5) * 1.4);
        let corner_distance = sqrt(2.0);
        shaped_distance = corner_distance
            * pow(max(distance / corner_distance, 0.0), midpoint_power);
    }
    var half_amount = 0.0;
    var full_amount = 0.0;

    // Interpolate between the calibrated half/full Amount anchor curves. The
    // fit coefficients are declared above so their measured origin and tuple
    // ordering remain visible instead of appearing as call-site magic numbers.
    if amount < 0.0 {
        half_amount = calibrated_vignette_anchor(
            shaped_distance,
            VIGNETTE_DARK_HALF_FIT.x,
            VIGNETTE_DARK_HALF_FIT.y,
            VIGNETTE_DARK_HALF_FIT.z,
            VIGNETTE_DARK_HALF_FIT.w
        );
        full_amount = calibrated_vignette_anchor(
            shaped_distance,
            VIGNETTE_DARK_FULL_FIT.x,
            VIGNETTE_DARK_FULL_FIT.y,
            VIGNETTE_DARK_FULL_FIT.z,
            VIGNETTE_DARK_FULL_FIT.w
        );
    } else {
        half_amount = calibrated_vignette_anchor(
            shaped_distance,
            VIGNETTE_LIGHT_HALF_FIT.x,
            VIGNETTE_LIGHT_HALF_FIT.y,
            VIGNETTE_LIGHT_HALF_FIT.z,
            VIGNETTE_LIGHT_HALF_FIT.w
        );
        full_amount = calibrated_vignette_anchor(
            shaped_distance,
            VIGNETTE_LIGHT_FULL_FIT.x,
            VIGNETTE_LIGHT_FULL_FIT.y,
            VIGNETTE_LIGHT_FULL_FIT.z,
            VIGNETTE_LIGHT_FULL_FIT.w
        );
    }

    var opacity = 0.0;
    if magnitude <= 0.5 {
        opacity = half_amount * (magnitude * 2.0);
    } else {
        opacity = mix(half_amount, full_amount, (magnitude - 0.5) * 2.0);
    }
    // Feather 50 is also an exact identity. Lower values tighten the shoulder;
    // higher values spread a softer trace farther into the frame.
    if abs(feather - 0.5) < 1e-6 {
        return opacity;
    }
    let feather_power = exp2((0.5 - feather) * 1.3);
    return pow(clamp(opacity, 0.0, 1.0), feather_power);
}

fn apply_vignette(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = clamp(effects_uniforms.vignette.x / 100.0, -1.0, 1.0);
    if abs(amount) < 1e-6 {
        return rgb;
    }

    let midpoint = clamp(effects_uniforms.vignette.y / 100.0, 0.0, 1.0);
    let roundness = clamp(effects_uniforms.vignette.z / 100.0, -1.0, 1.0);
    let feather = clamp(effects_uniforms.vignette.w / 100.0, 0.0, 1.0);
    var opacity = lightroom_vignette_opacity(
        vignette_distance(pos, roundness),
        amount,
        midpoint,
        feather,
    );
    if amount < 0.0 {
        let highlights = clamp(effects_uniforms.vignette_options.x / 100.0, 0.0, 1.0);
        let highlight_protection = 1.0
            - highlights * smoothstep(0.35, 1.0, safe_luma(rgb));
        opacity = opacity * highlight_protection;
        // Darktable and Lightroom both implement the dark branch as an edge
        // multiplication. It preserves hue and reaches a true black corner at
        // -100 without the gray rings caused by RGB subtraction.
        return rgb * (1.0 - opacity);
    }
    // A positive vignette is not exposure gain: it is an additive/white edge
    // treatment. Blending in display-linear RGB reproduces Lightroom's neutral
    // white corners without amplifying hue or clipping channels independently.
    return mix(rgb, vec3<f32>(1.0), opacity);
}


fn apply_local_scene_effect_nodes(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let count = min(scene_tone_uniforms.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = mask_data[index].metadata;
        if state.x == 0u || state.y == 0u { continue; }
        let local = mask_data[index].adjust_2;
        if max(max(abs(local.x), abs(local.y)), max(abs(local.z), abs(local.w))) <= 1e-7 {
            continue;
        }
        let weight = local_mask_weight(pos, index);
        if weight <= 1e-5 { continue; }
        var adjusted = rgb;
        adjusted = apply_texture_and_clarity_values(pos, adjusted, local.y, local.z);
        adjusted = apply_dehaze_value(pos, adjusted, local.w);
        adjusted = apply_saturation_value(adjusted, local.x);
        rgb = mix(rgb, adjusted, weight);
    }
    return rgb;
}


@compute @workgroup_size(8, 8, 1)
fn apply_scene_effects_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= camera_uniforms.width || gid.y >= camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = adjustment_base_at(pos);
    rgb = apply_texture_and_clarity_values(pos, rgb, effects_uniforms.presence.x, effects_uniforms.presence.y);
    rgb = apply_dehaze_value(pos, rgb, effects_uniforms.presence.z);
    rgb = apply_saturation_vibrance(rgb);
    rgb = apply_local_scene_effect_nodes(pos, rgb);
    textureStore(local_effects_out, pos, vec4<f32>(rgb, 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn copy_scene_effects_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= camera_uniforms.width || gid.y >= camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(local_effects_out, pos, vec4<f32>(adjustment_base_at(pos), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn prepare_glow_source(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= camera_uniforms.width || gid.y >= camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let emission = glow_emission(local_effects_at(pos), glow_cutoff());
    textureStore(glow_work_out, pos, vec4<f32>(emission, 1.0));
}

fn store_glow_stage(gid: vec3<u32>, stage: u32) {
    if gid.x >= camera_uniforms.width || gid.y >= camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(glow_work_out, pos, vec4<f32>(glow_diffuse_at(pos, stage), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_0(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 0u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_1(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 1u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_2(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 2u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_3(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 3u);
}

@compute @workgroup_size(8, 8, 1)
fn diffuse_glow_4(@builtin(global_invocation_id) gid: vec3<u32>) {
    store_glow_stage(gid, 4u);
}

@compute @workgroup_size(8, 8, 1)
fn apply_creative_effects(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= camera_uniforms.width || gid.y >= camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = local_effects_at(pos);
    rgb = apply_glow(pos, rgb);
    textureStore(creative_effects_out, pos, vec4<f32>(rgb, 1.0));
}

