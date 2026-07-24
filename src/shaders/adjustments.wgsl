// Post-demosaic render graph with explicit domain boundaries. New-process
// pixels flow through camera characterization -> scene edits -> optional look ->
// exactly one view transform -> output encoding. The physical passes below may
// fuse adjacent logical nodes, but they never move scene-style controls across
// the scene/display boundary. Legacy process versions branch inside the same
// entry points to preserve the historical DCP ordering.

@group(0) @binding(11) var scene_tex: texture_2d<f32>;
@group(0) @binding(12) var out_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(21) var adjustment_base_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(22) var adjustment_base_tex: texture_2d<f32>;
@group(0) @binding(23) var local_effects_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(24) var local_effects_tex: texture_2d<f32>;
@group(0) @binding(25) var creative_effects_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(26) var final_adjustment_tex: texture_2d<f32>;
@group(0) @binding(27) var local_mask_tex: texture_2d_array<f32>;
@group(0) @binding(28) var local_mask_sampler: sampler;
@group(0) @binding(29) var display_linear_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(30) var glow_work_tex: texture_2d<f32>;
@group(0) @binding(31) var glow_work_out: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
// Baseline inpainting is stored as scene-linear Rec.2020 RGBA16F plus alpha and
// inserted before all Develop adjustments, so both global and masked adjustments affect it.
@group(0) @binding(32) var inpaint_tex: texture_2d<f32>;

struct LocalAdjustmentMix {
    tone0: vec4<f32>,
    tone1: vec4<f32>,
    effects: vec4<f32>,
}

fn local_adjustment_mix(pos: vec2<i32>) -> LocalAdjustmentMix {
    var tone0 = vec4<f32>(0.0);
    var tone1 = vec4<f32>(0.0);
    var effects = vec4<f32>(0.0);
    let full_size = vec2<f32>(
        f32(max(params.full_width, 1u)),
        f32(max(params.full_height, 1u)),
    );
    let global_pos = vec2<f32>(pos + tile_origin()) + vec2<f32>(0.5);
    let uv = clamp(global_pos / full_size, vec2<f32>(0.0), vec2<f32>(1.0));
    let count = min(params.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let mask_state = params.mask_meta[index];
        if mask_state.x == 0u || mask_state.y == 0u {
            continue;
        }
        let weight = textureSampleLevel(
            local_mask_tex,
            local_mask_sampler,
            uv,
            i32(index),
            0.0,
        ).x;
        if weight <= 1e-5 {
            continue;
        }
        tone0 = tone0 + params.mask_adjust_0[index] * weight;
        tone1 = tone1 + params.mask_adjust_1[index] * weight;
        effects = effects + params.mask_adjust_2[index] * weight;
    }
    return LocalAdjustmentMix(tone0, tone1, effects);
}

fn scene_working_at(pos: vec2<i32>) -> vec3<f32> {
    let camera_rgb = textureLoad(scene_tex, clamp_pos(pos), 0).xyz;
    // The global white balance and its DCP interpolation are folded into the
    // camera-specific matrix assembled on the CPU. Preserve the matrix result
    // until all DCP stages are complete: gamut remapping between HueSatMap,
    // exposure, LookTable and ProfileToneCurve changes the profile itself.
    let working = cam_to_working(camera_rgb);

    // LaMa is run on a neutral pre-adjustment rendition. Its generated output
    // is converted to scene-linear Rec.2020 on the CPU immediately after
    // inference and retained as RGBA16F, so no 8-bit sRGB round trip occurs here.
    // From this point onward the replacement follows the exact same profile,
    // global, mask, Effects, grading, vignette, sigmoid and output-transform path.
    let replacement = textureLoad(inpaint_tex, clamp_pos(pos), 0);
    if replacement.a <= 1e-6 {
        return working;
    }
    let replacement_neutral = replacement.rgb;
    // Inpaint pixels are generated in the RAW's neutral camera-WB working
    // basis. Remap them through the live camera transform so global
    // temperature/tint changes remain non-destructive after the erase.
    let replacement_working = vec3<f32>(
        dot(params.inpaint_wb_0.xyz, replacement_neutral),
        dot(params.inpaint_wb_1.xyz, replacement_neutral),
        dot(params.inpaint_wb_2.xyz, replacement_neutral),
    );
    return mix(working, replacement_working, clamp(replacement.a, 0.0, 1.0));
}

fn adjustment_base_at(pos: vec2<i32>) -> vec3<f32> {
    // Binding 22 is deliberately stage-relative. The capture-sharpen/tone pass
    // binds the pre-tone base here; the later presence pass binds its post-tone
    // output here. This keeps both spatial operators sampling the correct domain
    // without allocating another full-frame working texture.
    return max(textureLoad(adjustment_base_tex, clamp_pos(pos), 0).xyz, vec3<f32>(0.0));
}

fn local_effects_at(pos: vec2<i32>) -> vec3<f32> {
    return max(textureLoad(local_effects_tex, clamp_pos(pos), 0).xyz, vec3<f32>(0.0));
}

fn log_luminance(rgb: vec3<f32>) -> f32 {
    return log2(safe_luma(max(rgb, vec3<f32>(0.0))));
}

fn presence_reference_scale() -> f32 {
    // Spatial presence controls are authored relative to a 1080-pixel short
    // edge. Scaling their sample steps makes the preview proxy, zoom detail,
    // and full-resolution export operate on comparable subject detail.
    return clamp(
        f32(min(params.full_width, params.full_height)) / 1080.0,
        0.55,
        3.0,
    );
}

fn presence_step(reference_pixels: f32, maximum: i32) -> i32 {
    return clamp(
        i32(round(reference_pixels * presence_reference_scale())),
        1,
        maximum,
    );
}

fn bilateral_log_luminance(
    pos: vec2<i32>,
    radius: i32,
    step: i32,
    range_strength: f32,
) -> f32 {
    let center = log_luminance(adjustment_base_at(pos));
    let sigma = max(f32(radius) * 0.72, 0.85);
    var sum = 0.0;
    var sum_w = 0.0;

    // The shader has a fixed maximum footprint so mobile and desktop compile
    // the same code. `radius` selects 3x3, 5x5, or 7x7 behavior at runtime.
    for (var dy = -3; dy <= 3; dy = dy + 1) {
        for (var dx = -3; dx <= 3; dx = dx + 1) {
            if abs(dx) > radius || abs(dy) > radius { continue; }
            let sample_ev = log_luminance(
                adjustment_base_at(pos + vec2<i32>(dx * step, dy * step)),
            );
            let distance_squared = f32(dx * dx + dy * dy);
            let spatial = exp(-0.5 * distance_squared / (sigma * sigma));
            let delta = sample_ev - center;
            let range = exp(-range_strength * delta * delta);
            let weight = spatial * range;
            sum = sum + sample_ev * weight;
            sum_w = sum_w + weight;
        }
    }
    return sum / max(sum_w, 1e-6);
}

fn atrous_kernel_weight(offset: i32) -> f32 {
    switch abs(offset) {
        case 0: { return 6.0; }
        case 1: { return 4.0; }
        default: { return 1.0; }
    }
}

fn atrous_log_luminance(pos: vec2<i32>, step: i32, range_strength: f32) -> f32 {
    let center = log_luminance(adjustment_base_at(pos));
    var sum = 0.0;
    var sum_w = 0.0;
    // A 5x5 B3-spline à-trous kernel samples a much wider spatial scale than
    // a dense 7x7 blur at lower cost. This gives Clarity a genuinely mid-scale
    // response (roughly 25-35 preview pixels) while remaining edge-aware.
    for (var ky = -2; ky <= 2; ky = ky + 1) {
        for (var kx = -2; kx <= 2; kx = kx + 1) {
            let sample_pos = pos + vec2<i32>(kx * step, ky * step);
            let sample_ev = log_luminance(adjustment_base_at(sample_pos));
            let delta = sample_ev - center;
            let spatial = atrous_kernel_weight(kx) * atrous_kernel_weight(ky);
            let range = exp(-range_strength * delta * delta);
            let weight = spatial * range;
            sum = sum + sample_ev * weight;
            sum_w = sum_w + weight;
        }
    }
    return sum / max(sum_w, 1e-6);
}

fn soft_detail_threshold(detail: f32, threshold: f32) -> f32 {
    return sign(detail) * max(abs(detail) - threshold, 0.0);
}

fn capture_sharpen_blur_ev(pos: vec2<i32>, radius_pixels: f32, step: i32) -> f32 {
    let center_ev = log_luminance(adjustment_base_at(pos));
    let sigma_samples = clamp(radius_pixels / max(f32(step), 1.0), 0.65, 2.25);
    var weighted_sum = 0.0;
    var weight_sum = 0.0;

    // A compact bilateral Gaussian gives Radius a genuine spatial footprint
    // while suppressing cross-edge halos. The fixed 5x5 support keeps capture
    // sharpening practical on preview proxies and full-resolution exports.
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            let sample_ev = log_luminance(
                adjustment_base_at(pos + vec2<i32>(dx * step, dy * step)),
            );
            let distance_squared = f32(dx * dx + dy * dy);
            let spatial = exp(-0.5 * distance_squared / (sigma_samples * sigma_samples));
            let delta = sample_ev - center_ev;
            let range = exp(-2.4 * delta * delta);
            let weight = spatial * range;
            weighted_sum = weighted_sum + sample_ev * weight;
            weight_sum = weight_sum + weight;
        }
    }
    return weighted_sum / max(weight_sum, 1e-6);
}

fn capture_sharpen_edge_strength(pos: vec2<i32>, step: i32) -> f32 {
    let left = log_luminance(adjustment_base_at(pos + vec2<i32>(-step, 0)));
    let right = log_luminance(adjustment_base_at(pos + vec2<i32>(step, 0)));
    let up = log_luminance(adjustment_base_at(pos + vec2<i32>(0, -step)));
    let down = log_luminance(adjustment_base_at(pos + vec2<i32>(0, step)));
    return length(vec2<f32>(right - left, down - up));
}

fn apply_capture_sharpening(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = clamp(params.creative_effects.w / 150.0, 0.0, 1.0);
    if amount < 1e-6 {
        return rgb;
    }

    let radius = clamp(params.vignette_options.y, 0.5, 3.0);
    let detail = clamp(params.vignette_options.z / 100.0, 0.0, 1.0);
    let masking = clamp(params.vignette_options.w / 100.0, 0.0, 1.0);
    let radius_pixels = radius * presence_reference_scale();
    let step = clamp(i32(round(max(radius_pixels * 0.55, 1.0))), 1, 5);

    let center_ev = log_luminance(rgb);
    let base_ev = capture_sharpen_blur_ev(pos, radius_pixels, step);
    let broad_detail_ev = center_ev - base_ev;

    // Detail progressively restores the highest spatial frequencies rather
    // than simply multiplying Amount. This gives hair, foliage, masonry and
    // fine texture a Lightroom-like crispness without forcing broad halos.
    let micro_left = log_luminance(adjustment_base_at(pos + vec2<i32>(-1, 0)));
    let micro_right = log_luminance(adjustment_base_at(pos + vec2<i32>(1, 0)));
    let micro_up = log_luminance(adjustment_base_at(pos + vec2<i32>(0, -1)));
    let micro_down = log_luminance(adjustment_base_at(pos + vec2<i32>(0, 1)));
    let micro_base_ev = 0.25 * (micro_left + micro_right + micro_up + micro_down);
    let micro_detail_ev = center_ev - micro_base_ev;
    let detail_ev = mix(broad_detail_ev, mix(broad_detail_ev, micro_detail_ev, 0.72), detail);

    // Low Detail suppresses tiny noise-like residuals. Higher Detail lowers
    // the threshold deliberately, matching the expectation that the slider
    // reveals progressively finer real structure as well as more grain.
    let detail_threshold = mix(0.018, 0.0035, detail);
    let selected_detail = soft_detail_threshold(detail_ev, detail_threshold);

    // Masking 0 sharpens the full image. Increasing it smoothly restricts the
    // effect to stronger luminance edges, protecting skies, skin and flat noise.
    var edge_mask = 1.0;
    if masking > 1e-6 {
        let edge_strength = capture_sharpen_edge_strength(pos, 1);
        let edge_threshold = mix(0.035, 0.62, pow(masking, 1.35));
        edge_mask = smoothstep(edge_threshold * 0.72, edge_threshold + 0.16, edge_strength);
    }

    // Protect very deep noise and extreme specular values from ringing. Apply
    // only a luminance gain so RGB ratios and therefore hue remain stable.
    let shadow_gate = smoothstep(-9.0, -4.8, center_ev);
    let highlight_gate = 1.0 - 0.55 * smoothstep(3.0, 6.5, center_ev);
    let strength = amount * mix(1.45, 2.20, detail);
    let sharpen_ev = clamp(
        selected_detail * strength * edge_mask * shadow_gate * highlight_gate,
        -0.42,
        0.48,
    );
    return max(rgb * exp2(sharpen_ev), vec3<f32>(0.0));
}

fn apply_texture_and_clarity_values(
    pos: vec2<i32>,
    rgb: vec3<f32>,
    texture_value: f32,
    clarity_value: f32,
) -> vec3<f32> {
    let texture = perceptual_control(texture_value);
    let clarity = perceptual_control(clarity_value);
    if abs(texture) < 1e-6 && abs(clarity) < 1e-6 {
        return rgb;
    }

    let center_ev = log_luminance(rgb);
    // Texture uses a compact 5x5 edge-aware base. The previous 3x3 residual
    // combined with a large threshold rejected most real surface detail.
    let fine_step = presence_step(1.0, 3);
    let fine_base_ev = bilateral_log_luminance(pos, 2, fine_step, 9.5);
    var broad_base_ev = fine_base_ev;
    if abs(clarity) >= 1e-6 {
        let clarity_reference = select(4.0, 5.0, params.tone_guide_radius > 3.5);
        let clarity_step = presence_step(clarity_reference, 12);
        broad_base_ev = atrous_log_luminance(pos, clarity_step, 0.82);
    }

    let fine_detail_ev = center_ev - fine_base_ev;
    // Keep most of the medium-scale center-to-base residual while subtracting a
    // little fine detail. This is visibly stronger than fine_base-broad_base but
    // still avoids turning Clarity into ordinary sharpening.
    let mid_detail_ev = center_ev - broad_base_ev - fine_detail_ev * 0.24;

    let signal_gate = smoothstep(-7.6, -2.5, center_ev);
    let fine_threshold = mix(0.040, 0.0035, signal_gate);
    let positive_fine = soft_detail_threshold(fine_detail_ev, fine_threshold);
    let negative_fine = clamp(fine_detail_ev, -0.28, 0.28);
    let selected_fine = select(negative_fine, positive_fine, texture >= 0.0);

    let midtone_gate = smoothstep(-7.0, -2.35, center_ev)
        * (1.0 - 0.72 * smoothstep(0.85, 3.5, center_ev));
    let selected_mid = soft_detail_threshold(mid_detail_ev, 0.0035);
    let edge_guard = 1.0 - 0.48 * smoothstep(0.22, 0.92, abs(fine_detail_ev));
    let clarity_band = selected_mid * edge_guard;

    let texture_strength = select(1.75, 2.65, texture >= 0.0)
        * mix(0.86, 1.18, abs(texture));
    let clarity_strength = select(2.20, 3.25, clarity >= 0.0)
        * mix(0.88, 1.14, abs(clarity));
    let texture_ev = texture * selected_fine * texture_strength;
    let clarity_ev = clarity * clarity_band * clarity_strength * midtone_gate;
    let delta_ev = clamp(texture_ev + clarity_ev, -1.35, 1.50);
    return max(rgb * exp2(delta_ev), vec3<f32>(0.0));
}

struct HazeNeighborhood {
    dark_ratio: f32,
    airlight: vec3<f32>,
    airlight_luma: f32,
}

fn normalized_dark_ratio(rgb: vec3<f32>, airlight_luma: f32) -> f32 {
    let normalized = max(rgb, vec3<f32>(0.0)) / max(airlight_luma, 1e-6);
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
    let ambient_ev = clamp(tone_stats.percentiles_1.x + params.exposure, -16.0, 16.0);
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
        // Dark-channel transmission recovery. Even relatively clear regions get
        // a modest contrast response, while detected veil receives the stronger
        // atmospheric-light subtraction expected from a real Dehaze control.
        let transmission = clamp(
            1.0 - amount * (0.20 + 0.70 * haze_likelihood),
            0.22,
            1.0,
        );
        let physical = max(
            (rgb - airlight * (1.0 - transmission)) / transmission,
            vec3<f32>(0.0),
        );
        let restored_lum = safe_luma(physical);
        let luminance_gain = clamp(restored_lum / max(center_lum, 1e-6), 0.28, 4.5);
        let hue_safe = rgb * luminance_gain;
        var restored = mix(hue_safe, physical, 0.34 + 0.22 * haze_likelihood);

        let local_detail = clamp(center_ev - broad_ev, -1.2, 1.2);
        restored = restored * exp2(amount * local_detail * 0.22);
        let lab = linear_srgb_to_oklab(REC2020_TO_SRGB * restored);
        let chroma = length(lab.yz);
        let content_saturation = clamp(chroma / max(0.045 + 0.38 * lab.x, 0.06), 0.0, 1.0);
        let chroma_boost = 1.0
            + amount * (0.10 + 0.30 * haze_likelihood)
                * (1.0 - 0.38 * content_saturation);
        restored = SRGB_TO_REC2020 * oklab_to_linear_srgb(
            vec3<f32>(lab.x, lab.yz * chroma_boost),
        );
        return repair_negative_rec2020(restored);
    }

    let haze = -amount;
    let haze_mix = clamp(haze * (0.14 + 0.38 * (1.0 - haze_likelihood)), 0.0, 0.58);
    let hazed = mix(rgb, airlight, haze_mix);
    let lab = linear_srgb_to_oklab(REC2020_TO_SRGB * hazed);
    let desaturation = 1.0 - haze * (0.12 + 0.20 * (1.0 - haze_likelihood));
    return repair_negative_rec2020(
        SRGB_TO_REC2020 * oklab_to_linear_srgb(vec3<f32>(lab.x, lab.yz * desaturation)),
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
    let linear_luma = safe_luma(rgb);
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
    let colour_ratio = clamp(rgb / max(linear_luma, 1e-6), vec3<f32>(0.0), vec3<f32>(3.5));
    let warm_tint = vec3<f32>(1.025, 1.0, 0.975);
    return colour_ratio * warm_tint
        * intensity * pow(linear_luma, 0.62) * cutoff_fade * black_gate;
}

fn glow_cutoff() -> f32 {
    let threshold = clamp(params.creative_effects.z / 100.0, 0.0, 1.0);
    return mix(0.06, 0.92, pow(threshold, 1.12));
}

fn glow_work_at(pos: vec2<i32>) -> vec3<f32> {
    return max(textureLoad(glow_work_tex, clamp_pos(pos), 0).xyz, vec3<f32>(0.0));
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
        f32(min(params.full_width, params.full_height)) / 1080.0,
        0.45,
        3.0,
    );
    return max(i32(round(reference_step * scale)), 1);
}

fn glow_stage_mix(stage: u32) -> f32 {
    let radius = clamp(params.creative_effects.y / 100.0, 0.0, 1.0);
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
    let amount = clamp(params.creative_effects.x / 100.0, 0.0, 1.0);
    if amount < 1e-6 {
        return rgb;
    }

    let bloom = glow_work_at(pos);

    // Very bright cores already carry their own energy. Protecting them keeps
    // Glow from clipping the light source while the blurred halo expands into
    // the surrounding darker pixels.
    let current_luma = safe_luma(rgb);
    let core_protection = 1.0 - 0.72 * smoothstep(1.0, 3.2, current_luma);
    return max(rgb + bloom * amount * 2.8 * core_protection, vec3<f32>(0.0));
}

fn full_image_uv(pos: vec2<i32>) -> vec2<f32> {
    let dimensions = max(
        vec2<f32>(f32(params.full_width), f32(params.full_height)),
        vec2<f32>(1.0),
    );
    let global_pos = clamp(pos + tile_origin(), vec2<i32>(0), full_image_max());
    return (vec2<f32>(global_pos) + vec2<f32>(0.5)) / dimensions;
}

fn vignette_distance(pos: vec2<i32>, roundness: f32) -> f32 {
    let dimensions = max(
        vec2<f32>(f32(params.full_width), f32(params.full_height)),
        vec2<f32>(1.0),
    );
    let p = abs(full_image_uv(pos) * 2.0 - vec2<f32>(1.0));
    let frame_ellipse = length(p);
    let frame_rectangle = pow(pow(p.x, 8.0) + pow(p.y, 8.0), 1.0 / 8.0);
    let short_dimension = max(min(dimensions.x, dimensions.y), 1.0);
    let image_circle = length(vec2<f32>(
        p.x * dimensions.x / short_dimension,
        p.y * dimensions.y / short_dimension,
    ));

    if roundness < 0.0 {
        return mix(frame_ellipse, frame_rectangle, -roundness);
    }
    return mix(frame_ellipse, image_circle, roundness);
}

fn apply_vignette(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let amount = clamp(params.vignette.x / 100.0, -1.0, 1.0);
    if abs(amount) < 1e-6 {
        return rgb;
    }

    let midpoint = clamp(params.vignette.y / 100.0, 0.0, 1.0);
    let roundness = clamp(params.vignette.z / 100.0, -1.0, 1.0);
    let feather = clamp(params.vignette.w / 100.0, 0.0, 1.0);
    let distance = vignette_distance(pos, roundness);

    // Midpoint places the optical falloff; Feather controls both the inward
    // reach and the outer shoulder. A second smooth polynomial removes the
    // visibly cubic ring that a single smoothstep can leave on flat skies.
    let midpoint_shaped = pow(midpoint, 0.82);
    let transition_center = mix(0.20, 0.982, midpoint_shaped);
    let inward_softness = mix(0.012, 0.58, feather)
        * mix(1.0, 0.50, midpoint_shaped * midpoint_shaped);
    let outward_softness = mix(0.018, 0.34, feather)
        * mix(1.0, 0.72, midpoint_shaped * midpoint_shaped);
    let transition_start = max(transition_center - inward_softness, 0.0);
    let transition_end = min(
        max(transition_center + outward_softness, transition_start + 0.018),
        1.0,
    );
    let transition = smoothstep(transition_start, transition_end, distance);
    let smoother = transition * transition * (3.0 - 2.0 * transition);
    let mask = pow(smoother, mix(1.28, 0.88, feather));

    // Exposure-domain gain preserves hue. Dark vignettes protect both deep
    // shadows and user-selected highlights, avoiding crushed corners and the
    // gray highlight rings produced by RGB subtraction. Positive vignettes get
    // a gentler shoulder to keep bright edges photographic rather than glowing.
    let edge_ev = select(amount * 2.20, amount * 1.28, amount > 0.0);
    let luminance = safe_luma(rgb);
    var highlight_protection = 1.0;
    var tonal_protection = 1.0;
    if amount < 0.0 {
        let highlights = clamp(params.vignette_options.x / 100.0, 0.0, 1.0);
        highlight_protection = 1.0
            - highlights * smoothstep(0.48, 2.3, luminance);
        tonal_protection = mix(0.58, 1.0, smoothstep(0.012, 0.20, luminance));
    } else {
        tonal_protection = 1.0 - 0.68 * smoothstep(0.75, 3.0, luminance);
    }
    let delta_ev = clamp(
        edge_ev * mask * highlight_protection * tonal_protection,
        -2.7,
        1.35,
    );
    return max(rgb * exp2(delta_ev), vec3<f32>(0.0));
}

// Lightroom's named HSL channels are a UI model, not a reason to process in
// mathematical HSL. HSL lightness is based on per-pixel RGB extrema and turns
// demosaic/chroma noise into luminance noise. The mixer below instead selects
// hues in perceptual OKLab, stabilizes only the selector with an edge-aware
// neighbourhood, and applies luminance as a scene-linear exposure gain.

struct MixerSample {
    lab: vec3<f32>,
    chroma: f32,
    hue_vector: vec2<f32>,
    confidence: f32,
}

struct MixerBandWeights {
    first: vec4<f32>,
    second: vec4<f32>,
    total: f32,
}

fn max_abs_vec4(value: vec4<f32>) -> f32 {
    return max(max(abs(value.x), abs(value.y)), max(abs(value.z), abs(value.w)));
}

fn color_mixer_strength() -> vec3<f32> {
    return vec3<f32>(
        max(max_abs_vec4(params.hsl_hue_0), max_abs_vec4(params.hsl_hue_1)),
        max(max_abs_vec4(params.hsl_saturation_0), max_abs_vec4(params.hsl_saturation_1)),
        max(max_abs_vec4(params.hsl_luminance_0), max_abs_vec4(params.hsl_luminance_1)),
    );
}

fn circular_distance(a: f32, b: f32) -> f32 {
    let d = abs(a - b);
    return min(d, 1.0 - d);
}

fn smooth_hue_bell(hue: f32, anchor: f32, width: f32) -> f32 {
    let t = clamp(1.0 - circular_distance(hue, anchor) / width, 0.0, 1.0);
    let feather = t * t * (3.0 - 2.0 * t);
    // Squaring the smoothstep keeps channel centers precise while retaining
    // soft overlaps between Lightroom's eight named colour ranges.
    return feather * feather;
}

fn mixer_band_weights(hue: f32) -> MixerBandWeights {
    // Red, orange, yellow, green, aqua, blue, purple, magenta. The
    // anchors are the OKLab hue angles of fully saturated sRGB swatches at
    // HSL hue 0, 30, 60, 120, 180, 240, 270 and 300 degrees. Using ordinary
    // HSL angles directly in OKLab would mislabel red as orange and yellow as
    // green. Widths follow the unequal perceptual spacing between anchors.
    let first = vec4<f32>(
        smooth_hue_bell(hue, 0.0812052, 0.160),
        smooth_hue_bell(hue, 0.1465993, 0.150),
        smooth_hue_bell(hue, 0.3049145, 0.150),
        smooth_hue_bell(hue, 0.3958204, 0.140),
    );
    let second = vec4<f32>(
        smooth_hue_bell(hue, 0.5410248, 0.180),
        smooth_hue_bell(hue, 0.7334778, 0.180),
        smooth_hue_bell(hue, 0.8160390, 0.100),
        smooth_hue_bell(hue, 0.9121206, 0.160),
    );
    return MixerBandWeights(first, second, dot(first, vec4<f32>(1.0)) + dot(second, vec4<f32>(1.0)));
}

fn mixer_band_value(weights: MixerBandWeights, first: vec4<f32>, second: vec4<f32>) -> f32 {
    return (dot(weights.first, first) + dot(weights.second, second)) / max(weights.total, 1e-6);
}

fn directed_hue_shift(value: f32, backward_span: f32, forward_span: f32) -> f32 {
    // Preserve the historical +/-100 response exactly, but permit the extended
    // UI range to continue linearly up to twice that shift. This lets a named
    // channel travel beyond its immediate neighbour without changing existing
    // sidecars or reducing fine control around zero.
    let amount = clamp(value / 100.0, -2.0, 2.0);
    let span = select(backward_span, forward_span, amount >= 0.0);
    return amount * span * 0.90;
}

fn mixer_hue_shift(weights: MixerBandWeights) -> f32 {
    // Backward/forward spans are distances to the adjacent calibrated
    // OKLab anchors. This makes each slider endpoint move toward the color
    // shown next to it in the Lightroom-style channel order.
    let first = vec4<f32>(
        directed_hue_shift(params.hsl_hue_0.x, 0.1690846, 0.0653940),
        directed_hue_shift(params.hsl_hue_0.y, 0.0653940, 0.1583152),
        directed_hue_shift(params.hsl_hue_0.z, 0.1583152, 0.0909059),
        directed_hue_shift(params.hsl_hue_0.w, 0.0909059, 0.1452044),
    );
    let second = vec4<f32>(
        directed_hue_shift(params.hsl_hue_1.x, 0.1452044, 0.1924530),
        directed_hue_shift(params.hsl_hue_1.y, 0.1924530, 0.0825612),
        directed_hue_shift(params.hsl_hue_1.z, 0.0825612, 0.0960816),
        directed_hue_shift(params.hsl_hue_1.w, 0.0960816, 0.1690846),
    );
    return (dot(weights.first, first) + dot(weights.second, second)) / max(weights.total, 1e-6);
}

fn mixer_sample_from_rgb(rgb: vec3<f32>) -> MixerSample {
    let lab = linear_srgb_to_oklab(REC2020_TO_SRGB * max(rgb, vec3<f32>(0.0)));
    let chroma = length(lab.yz);
    var hue_vector = vec2<f32>(1.0, 0.0);
    if chroma > 1e-9 {
        hue_vector = lab.yz / chroma;
    }

    // Chroma is judged relative to perceptual lightness. This makes nearly
    // neutral pixels in shadows and highlights ineligible for arbitrary hue
    // classification, while retaining low-saturation real colours.
    let relative_chroma = chroma / max(0.028 + 0.095 * max(lab.x, 0.0), 0.028);
    let chroma_confidence = smoothstep(0.10, 0.62, relative_chroma);
    let signal_confidence = smoothstep(0.035, 0.115, max(lab.x, 0.0));
    let confidence = chroma_confidence * signal_confidence;
    return MixerSample(lab, chroma, hue_vector, confidence);
}

fn stabilized_mixer_sample(pos: vec2<i32>, center_rgb: vec3<f32>) -> MixerSample {
    let center = mixer_sample_from_rgb(center_rgb);
    if center.confidence < 1e-5 {
        return center;
    }

    // A compact bilateral selector filter is enough to suppress Bayer/X-Trans
    // chroma speckle without softening image detail. Only hue selection is
    // filtered; the actual RGB detail always comes from the center pixel.
    var vector_sum = center.hue_vector * center.confidence * 4.0;
    var weight_sum = center.confidence * 4.0;
    // Desktop/high-quality rendering already uses a wider tone guide, so use
    // a 5x5 selector there. Android preview keeps a 3x3 selector for speed.
    let selector_radius = select(1, 2, params.tone_guide_radius > 3.5);
    for (var dy = -2; dy <= 2; dy = dy + 1) {
        for (var dx = -2; dx <= 2; dx = dx + 1) {
            if (dx == 0 && dy == 0) || abs(dx) > selector_radius || abs(dy) > selector_radius {
                continue;
            }
            let neighbour_rgb = textureLoad(final_adjustment_tex, clamp_pos(pos + vec2<i32>(dx, dy)), 0).xyz;
            let neighbour = mixer_sample_from_rgb(neighbour_rgb);
            if neighbour.confidence < 1e-5 {
                continue;
            }

            let distance_squared = f32(dx * dx + dy * dy);
            let spatial = 1.0 / (1.0 + 0.65 * distance_squared);
            let lightness_delta = neighbour.lab.x - center.lab.x;
            let chroma_delta = neighbour.chroma - center.chroma;
            let hue_agreement = clamp(dot(center.hue_vector, neighbour.hue_vector), -1.0, 1.0);
            let range_weight = 1.0 / (
                1.0
                + 72.0 * lightness_delta * lightness_delta
                + 34.0 * chroma_delta * chroma_delta
                + 8.0 * (1.0 - hue_agreement)
            );
            let weight = spatial * range_weight * neighbour.confidence;
            vector_sum = vector_sum + neighbour.hue_vector * weight;
            weight_sum = weight_sum + weight;
        }
    }

    var stable_hue = center.hue_vector;
    let vector_length = length(vector_sum);
    if weight_sum > 1e-5 && vector_length > 1e-5 {
        stable_hue = vector_sum / vector_length;
    }
    return MixerSample(center.lab, center.chroma, stable_hue, center.confidence);
}

fn nonnegative_rec2020_from_oklab(lightness: f32, hue_vector: vec2<f32>, requested_chroma: f32) -> vec3<f32> {
    // Hue/saturation moves can leave the positive Rec.2020 working gamut.
    // Compress chroma at constant OKLab lightness and hue instead of clipping
    // RGB channels, which would visibly change hue and create hard boundaries.
    var low = 0.0;
    var high = max(requested_chroma, 0.0);
    let requested = SRGB_TO_REC2020 * oklab_to_linear_srgb(
        vec3<f32>(lightness, hue_vector * high),
    );
    if min(requested.r, min(requested.g, requested.b)) >= 0.0 {
        return requested;
    }

    // Chroma zero is a valid neutral fallback for every non-negative
    // lightness, so the binary search always starts from a valid candidate.
    var candidate = SRGB_TO_REC2020 * oklab_to_linear_srgb(
        vec3<f32>(lightness, vec2<f32>(0.0)),
    );
    for (var iteration = 0; iteration < 10; iteration = iteration + 1) {
        let middle = 0.5 * (low + high);
        let probe = SRGB_TO_REC2020 * oklab_to_linear_srgb(
            vec3<f32>(lightness, hue_vector * middle),
        );
        if min(probe.r, min(probe.g, probe.b)) >= 0.0 {
            low = middle;
            candidate = probe;
        } else {
            high = middle;
        }
    }
    return max(candidate, vec3<f32>(0.0));
}

fn mixer_saturation_factor(amount: f32) -> f32 {
    let value = clamp(amount, -1.0, 1.0);
    if value >= 0.0 {
        // Positive saturation has a soft shoulder; +100 is strong but does not
        // produce the brittle 2x channel excursion of a simple RGB multiplier.
        return exp2(value * 0.85);
    }
    return max(1.0 + value, 0.0);
}

fn mixer_luminance_ev(amount: f32, lightness: f32) -> f32 {
    let value = clamp(amount, -1.0, 1.0);
    let endpoint_ev = select(1.45, 1.20, value >= 0.0);
    // Avoid turning barely-exposed chroma noise into bright coloured pixels.
    // This is a smooth signal confidence, not a hard shadow threshold.
    let signal = smoothstep(0.040, 0.135, max(lightness, 0.0));
    let hdr_guard = 1.0 / (1.0 + 0.20 * max(lightness - 1.0, 0.0));
    return value * endpoint_ev * signal * hdr_guard;
}

fn color_grade_strength(
    shadows: vec4<f32>,
    midtones: vec4<f32>,
    highlights: vec4<f32>,
    global: vec4<f32>,
) -> f32 {
    return max(
        max(max(abs(shadows.y), abs(shadows.z)), max(abs(midtones.y), abs(midtones.z))),
        max(max(abs(highlights.y), abs(highlights.z)), max(abs(global.y), abs(global.z))),
    );
}

fn color_grade_vector(wheel: vec4<f32>) -> vec2<f32> {
    let angle = wheel.x * 2.0 * 3.14159265359;
    return vec2<f32>(cos(angle), sin(angle)) * clamp(wheel.y, 0.0, 1.0);
}

fn color_grade_tonal_weights(luminance: f32, options: vec4<f32>) -> vec3<f32> {
    // Evaluate ranges in exposure space around photographic middle gray.
    // Wider blending produces Lightroom-like overlap without hard range
    // boundaries. Balance shifts the shared pivot by up to 1.5 stops.
    let ev = log2(max(luminance, 1e-7) / SCENE_MIDDLE_GREY);
    let width = mix(0.60, 2.80, clamp(options.x, 0.0, 1.0));
    let pivot = -clamp(options.y, -1.0, 1.0) * 1.5;
    let shadows = 1.0 - smoothstep(
        -1.25 + pivot - 0.5 * width,
        -1.25 + pivot + 0.5 * width,
        ev,
    );
    let highlights = smoothstep(
        1.25 + pivot - 0.5 * width,
        1.25 + pivot + 0.5 * width,
        ev,
    );
    let midtones = max(1.0 - shadows - highlights, 0.0);
    let total = max(shadows + midtones + highlights, 1e-6);
    return vec3<f32>(shadows, midtones, highlights) / total;
}

fn apply_color_grading_wheels(
    input_rgb: vec3<f32>,
    shadows: vec4<f32>,
    midtones: vec4<f32>,
    highlights: vec4<f32>,
    global: vec4<f32>,
    options: vec4<f32>,
) -> vec3<f32> {
    if color_grade_strength(shadows, midtones, highlights, global) < 1e-7 {
        return input_rgb;
    }

    let rgb = max(input_rgb, vec3<f32>(0.0));
    let luminance = max(dot(rgb, LUMA), 0.0);
    let weights = color_grade_tonal_weights(luminance, options);
    let lab = linear_srgb_to_oklab(REC2020_TO_SRGB * rgb);

    let grade_vector = color_grade_vector(shadows) * weights.x
        + color_grade_vector(midtones) * weights.y
        + color_grade_vector(highlights) * weights.z
        + color_grade_vector(global);

    var adjusted = rgb;
    if dot(grade_vector, grade_vector) > 1e-12 {
        // Deep-shadow confidence and a soft saturation guard prevent grading
        // from amplifying demosaic chroma noise or forcing already vivid
        // colors against the gamut boundary. Gamut compression then holds
        // OKLab lightness and hue instead of clipping individual RGB channels.
        let signal = smoothstep(0.025, 0.115, max(lab.x, 0.0));
        let hdr_guard = 1.0 / (1.0 + 0.25 * max(lab.x - 1.0, 0.0));
        let existing_chroma = length(lab.yz);
        let saturation_guard = 1.0 / (1.0 + 1.8 * existing_chroma);
        let target_ab = lab.yz + grade_vector * (0.135 * signal * hdr_guard * saturation_guard);
        let target_chroma = length(target_ab);
        if target_chroma > 1e-8 {
            adjusted = nonnegative_rec2020_from_oklab(
                lab.x,
                target_ab / target_chroma,
                target_chroma,
            );
        }
    }

    let luminance_grade = shadows.z * weights.x
        + midtones.z * weights.y
        + highlights.z * weights.z
        + global.z;
    if abs(luminance_grade) > 1e-7 {
        // Scene-linear scalar gain preserves the graded hue and RGB ratios.
        adjusted = adjusted * exp2(mixer_luminance_ev(luminance_grade, lab.x));
    }
    return max(adjusted, vec3<f32>(0.0));
}

fn local_curve_block(mask_index: u32, curve: u32, block: u32) -> vec4<f32> {
    if curve == 1u {
        switch block {
            case 0u: { return params.mask_curve_red_0[mask_index]; }
            case 1u: { return params.mask_curve_red_1[mask_index]; }
            case 2u: { return params.mask_curve_red_2[mask_index]; }
            case 3u: { return params.mask_curve_red_3[mask_index]; }
            case 4u: { return params.mask_curve_red_4[mask_index]; }
            case 5u: { return params.mask_curve_red_5[mask_index]; }
            case 6u: { return params.mask_curve_red_6[mask_index]; }
            default: { return params.mask_curve_red_7[mask_index]; }
        }
    }
    if curve == 2u {
        switch block {
            case 0u: { return params.mask_curve_green_0[mask_index]; }
            case 1u: { return params.mask_curve_green_1[mask_index]; }
            case 2u: { return params.mask_curve_green_2[mask_index]; }
            case 3u: { return params.mask_curve_green_3[mask_index]; }
            case 4u: { return params.mask_curve_green_4[mask_index]; }
            case 5u: { return params.mask_curve_green_5[mask_index]; }
            case 6u: { return params.mask_curve_green_6[mask_index]; }
            default: { return params.mask_curve_green_7[mask_index]; }
        }
    }
    if curve == 3u {
        switch block {
            case 0u: { return params.mask_curve_blue_0[mask_index]; }
            case 1u: { return params.mask_curve_blue_1[mask_index]; }
            case 2u: { return params.mask_curve_blue_2[mask_index]; }
            case 3u: { return params.mask_curve_blue_3[mask_index]; }
            case 4u: { return params.mask_curve_blue_4[mask_index]; }
            case 5u: { return params.mask_curve_blue_5[mask_index]; }
            case 6u: { return params.mask_curve_blue_6[mask_index]; }
            default: { return params.mask_curve_blue_7[mask_index]; }
        }
    }
    switch block {
        case 0u: { return params.mask_curve_0[mask_index]; }
        case 1u: { return params.mask_curve_1[mask_index]; }
        case 2u: { return params.mask_curve_2[mask_index]; }
        case 3u: { return params.mask_curve_3[mask_index]; }
        case 4u: { return params.mask_curve_4[mask_index]; }
        case 5u: { return params.mask_curve_5[mask_index]; }
        case 6u: { return params.mask_curve_6[mask_index]; }
        default: { return params.mask_curve_7[mask_index]; }
    }
}

fn local_curve_point(mask_index: u32, curve: u32, index: u32) -> vec2<f32> {
    let packed = local_curve_block(mask_index, curve, index / 2u);
    return select(packed.xy, packed.zw, (index & 1u) != 0u);
}

fn local_curve_secant(a: vec2<f32>, b: vec2<f32>) -> f32 {
    return (b.y - a.y) / max(b.x - a.x, 1e-5);
}

fn local_curve_tangent(mask_index: u32, curve: u32, index: u32, count: u32) -> f32 {
    if index == 0u {
        return local_curve_secant(
            local_curve_point(mask_index, curve, 0u),
            local_curve_point(mask_index, curve, 1u),
        );
    }
    if index + 1u >= count {
        return local_curve_secant(
            local_curve_point(mask_index, curve, count - 2u),
            local_curve_point(mask_index, curve, count - 1u),
        );
    }
    let previous = local_curve_secant(
        local_curve_point(mask_index, curve, index - 1u),
        local_curve_point(mask_index, curve, index),
    );
    let next = local_curve_secant(
        local_curve_point(mask_index, curve, index),
        local_curve_point(mask_index, curve, index + 1u),
    );
    if previous * next <= 0.0 {
        return 0.0;
    }
    return 2.0 * previous * next / max(abs(previous + next), 1e-6) * sign(previous + next);
}

fn local_curve_value(mask_index: u32, curve: u32, input: f32) -> f32 {
    let count = u32(clamp(local_curve_block(mask_index, curve, 4u).x, 2.0, 8.0));
    let x = clamp(input, 0.0, 1.0);
    var segment = count - 2u;
    for (var index = 0u; index + 1u < count; index = index + 1u) {
        if x <= local_curve_point(mask_index, curve, index + 1u).x {
            segment = index;
            break;
        }
    }

    let p0 = local_curve_point(mask_index, curve, segment);
    let p1 = local_curve_point(mask_index, curve, segment + 1u);
    let width = max(p1.x - p0.x, 1e-5);
    let t = clamp((x - p0.x) / width, 0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    let m0 = local_curve_tangent(mask_index, curve, segment, count) * width;
    let m1 = local_curve_tangent(mask_index, curve, segment + 1u, count) * width;
    let hermite = (2.0 * t3 - 3.0 * t2 + 1.0) * p0.y
        + (t3 - 2.0 * t2 + t) * m0
        + (-2.0 * t3 + 3.0 * t2) * p1.y
        + (t3 - t2) * m1;
    return clamp(hermite, min(p0.y, p1.y), max(p0.y, p1.y));
}

fn local_hue_shift(weights: MixerBandWeights, first_values: vec4<f32>, second_values: vec4<f32>) -> f32 {
    let first = vec4<f32>(
        directed_hue_shift(first_values.x, 0.1690846, 0.0653940),
        directed_hue_shift(first_values.y, 0.0653940, 0.1583152),
        directed_hue_shift(first_values.z, 0.1583152, 0.0909059),
        directed_hue_shift(first_values.w, 0.0909059, 0.1452044),
    );
    let second = vec4<f32>(
        directed_hue_shift(second_values.x, 0.1452044, 0.1924530),
        directed_hue_shift(second_values.y, 0.1924530, 0.0825612),
        directed_hue_shift(second_values.z, 0.0825612, 0.0960816),
        directed_hue_shift(second_values.w, 0.0960816, 0.1690846),
    );
    return (dot(weights.first, first) + dot(weights.second, second)) / max(weights.total, 1e-6);
}

fn apply_local_curve_and_hsl(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let full_size = vec2<f32>(f32(max(params.full_width, 1u)), f32(max(params.full_height, 1u)));
    let global_pos = vec2<f32>(pos + tile_origin()) + vec2<f32>(0.5);
    let uv = clamp(global_pos / full_size, vec2<f32>(0.0), vec2<f32>(1.0));
    let count = min(params.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = params.mask_meta[index];
        if state.x == 0u || (state.z == 0u && state.w == 0u) { continue; }
        let weight = textureSampleLevel(local_mask_tex, local_mask_sampler, uv, i32(index), 0.0).x;
        if weight <= 1e-5 { continue; }
        var adjusted = rgb;
        if (state.z & 1u) != 0u {
            let luminance = max(dot(adjusted, LUMA), 0.0);
            let curved = scene_curve_decode(local_curve_value(index, 0u, scene_curve_encode(luminance)));
            adjusted = remap_scene_luminance(adjusted, curved);
        }
        if (state.z & 2u) != 0u && adjusted.r >= 0.0 {
            adjusted.r = scene_curve_decode(local_curve_value(index, 1u, scene_curve_encode(adjusted.r)));
        }
        if (state.z & 4u) != 0u && adjusted.g >= 0.0 {
            adjusted.g = scene_curve_decode(local_curve_value(index, 2u, scene_curve_encode(adjusted.g)));
        }
        if (state.z & 8u) != 0u && adjusted.b >= 0.0 {
            adjusted.b = scene_curve_decode(local_curve_value(index, 3u, scene_curve_encode(adjusted.b)));
        }
        if (state.w & 1u) != 0u {
            let sample = mixer_sample_from_rgb(adjusted);
            if sample.confidence > 1e-5 {
                let hue = fract(atan2(sample.hue_vector.y, sample.hue_vector.x) / (2.0 * 3.14159265359) + 1.0);
                let bands = mixer_band_weights(hue);
                let hue_shift = local_hue_shift(bands, params.mask_hsl_hue_0[index], params.mask_hsl_hue_1[index]) * sample.confidence;
                let saturation = mixer_band_value(bands, params.mask_hsl_saturation_0[index], params.mask_hsl_saturation_1[index]) / 100.0 * sample.confidence;
                let luminance = mixer_band_value(bands, params.mask_hsl_luminance_0[index], params.mask_hsl_luminance_1[index]) / 100.0 * sample.confidence;
                if abs(hue_shift) > 1e-7 || abs(saturation) > 1e-7 {
                    let angle = atan2(sample.lab.z, sample.lab.y) + hue_shift * 2.0 * 3.14159265359;
                    adjusted = nonnegative_rec2020_from_oklab(
                        sample.lab.x,
                        vec2<f32>(cos(angle), sin(angle)),
                        sample.chroma * mixer_saturation_factor(saturation),
                    );
                }
                if abs(luminance) > 1e-7 {
                    adjusted = adjusted * exp2(mixer_luminance_ev(luminance, sample.lab.x));
                }
            }
        }
        rgb = mix(rgb, adjusted, weight);
    }
    return max(rgb, vec3<f32>(0.0));
}

fn apply_local_color_grading(pos: vec2<i32>, input_rgb: vec3<f32>) -> vec3<f32> {
    var rgb = input_rgb;
    let full_size = vec2<f32>(f32(max(params.full_width, 1u)), f32(max(params.full_height, 1u)));
    let global_pos = vec2<f32>(pos + tile_origin()) + vec2<f32>(0.5);
    let uv = clamp(global_pos / full_size, vec2<f32>(0.0), vec2<f32>(1.0));
    let count = min(params.mask_counts.x, 32u);
    for (var index = 0u; index < count; index = index + 1u) {
        let state = params.mask_meta[index];
        if state.x == 0u || (state.w & 2u) == 0u { continue; }
        let weight = textureSampleLevel(local_mask_tex, local_mask_sampler, uv, i32(index), 0.0).x;
        if weight <= 1e-5 { continue; }
        let adjusted = apply_color_grading_wheels(
            rgb,
            params.mask_grade_shadows[index],
            params.mask_grade_midtones[index],
            params.mask_grade_highlights[index],
            params.mask_grade_global[index],
            params.mask_grade_options[index],
        );
        rgb = mix(rgb, adjusted, weight);
    }
    return max(rgb, vec3<f32>(0.0));
}

fn apply_color_mixer(pos: vec2<i32>, rgb: vec3<f32>) -> vec3<f32> {
    let strengths = color_mixer_strength();
    if max(strengths.x, max(strengths.y, strengths.z)) < 1e-6 {
        // Exact no-op: enabling the mixer with neutral sliders must not change
        // pixels or run the selector filter.
        return rgb;
    }

    let sample = stabilized_mixer_sample(pos, rgb);
    if sample.confidence < 1e-5 {
        return rgb;
    }

    let selector_hue = fract(atan2(sample.hue_vector.y, sample.hue_vector.x) / (2.0 * 3.14159265359) + 1.0);
    let weights = mixer_band_weights(selector_hue);
    let hue_shift = mixer_hue_shift(weights) * sample.confidence;
    let saturation_amount = mixer_band_value(
        weights,
        params.hsl_saturation_0,
        params.hsl_saturation_1,
    ) / 100.0 * sample.confidence;
    let luminance_amount = mixer_band_value(
        weights,
        params.hsl_luminance_0,
        params.hsl_luminance_1,
    ) / 100.0 * sample.confidence;

    if max(abs(hue_shift), max(abs(saturation_amount), abs(luminance_amount))) < 1e-7 {
        return rgb;
    }

    var adjusted = rgb;
    if abs(hue_shift) > 1e-7 || abs(saturation_amount) > 1e-7 {
        let center_hue = sample.lab.yz / max(sample.chroma, 1e-9);
        let center_angle = atan2(center_hue.y, center_hue.x);
        let target_angle = center_angle + hue_shift * 2.0 * 3.14159265359;
        let target_hue = vec2<f32>(cos(target_angle), sin(target_angle));
        let target_chroma = sample.chroma * mixer_saturation_factor(saturation_amount);
        adjusted = nonnegative_rec2020_from_oklab(sample.lab.x, target_hue, target_chroma);
    }

    if abs(luminance_amount) > 1e-7 {
        // A scalar scene-linear gain preserves RGB ratios, hue and saturation.
        // Unlike changing HSL lightness, it cannot expose per-channel extrema
        // as a checkerboard of different brightness values.
        adjusted = adjusted * exp2(mixer_luminance_ev(luminance_amount, sample.lab.x));
    }
    return max(adjusted, vec3<f32>(0.0));
}

@compute @workgroup_size(8, 8, 1)
fn prepare_scene_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));

    // Camera characterization is the only DCP color component allowed before
    // scene edits in the new graph. Fixed profile exposure and editable global/
    // local Exposure are also scene-linear. The LookTable is deferred until all
    // scene controls have finished. Legacy edits retain the old pre-edit look.
    var rgb = apply_camera_characterization(scene_working_at(pos));
    let profile_exposure_ev = bitcast<f32>(params.profile_flags.z);
    let local = local_adjustment_mix(pos);
    let local_exposure_ev = clamp(local.tone0.x, -10.0, 10.0);
    rgb = rgb * exp2(profile_exposure_ev + local_exposure_ev);
    rgb = apply_exposure(rgb);
    if !uses_explicit_scene_display_domains() {
        rgb = apply_optional_profile_look(rgb);
    }
    textureStore(adjustment_base_out, pos, vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn apply_scene_tone_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = adjustment_base_at(pos);
    let local = local_adjustment_mix(pos);

    // Capture sharpening and all H/S/W/B, Contrast, curve, and local tone
    // controls operate in the scene domain. ProfileToneCurve is excluded from
    // this node for process 13+, which makes slider semantics profile-independent.
    // Process 12 and earlier preserve the historical curve-before-edits order.
    rgb = apply_capture_sharpening(pos, rgb);
    if !uses_explicit_scene_display_domains() {
        rgb = apply_profile_view_tone(rgb);
    }
    rgb = map_negative_gamut(rgb);
    rgb = max(rgb, vec3<f32>(0.0));
    rgb = apply_lightroom_tone(rgb, pos);
    rgb = apply_local_basic_tone_values(
        rgb,
        pos,
        local.tone0.z,
        local.tone0.w,
        local.tone1.x,
        local.tone1.y,
    );
    rgb = apply_basic_contrast_value(rgb, local.tone0.y);
    rgb = apply_temperature_tint_values(rgb, local.tone1.z, local.tone1.w);
    rgb = apply_local_curve_and_hsl(pos, rgb);
    textureStore(local_effects_out, pos, vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn apply_scene_effects_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = adjustment_base_at(pos);
    let local = local_adjustment_mix(pos);
    rgb = apply_texture_and_clarity_values(
        pos,
        rgb,
        params.presence.x + local.effects.y,
        params.presence.y + local.effects.z,
    );
    rgb = apply_dehaze_value(pos, rgb, params.presence.z + local.effects.w);
    rgb = apply_saturation_vibrance(rgb);
    rgb = apply_saturation_value(rgb, local.effects.x);
    textureStore(local_effects_out, pos, vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0));
}

@compute @workgroup_size(8, 8, 1)
fn prepare_glow_source(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let emission = glow_emission(local_effects_at(pos), glow_cutoff());
    textureStore(glow_work_out, pos, vec4<f32>(emission, 1.0));
}

fn store_glow_stage(gid: vec3<u32>, stage: u32) {
    if gid.x >= params.width || gid.y >= params.height { return; }
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
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var rgb = local_effects_at(pos);
    rgb = apply_glow(pos, rgb);
    rgb = apply_vignette(pos, rgb);
    textureStore(creative_effects_out, pos, vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0));
}

fn default_view_chroma_limit(mapped_luma: f32, candidate: vec3<f32>) -> f32 {
    let delta = candidate - vec3<f32>(mapped_luma);
    var limit = 1.0;
    if delta.r > 1e-7 {
        limit = min(limit, (1.0 - mapped_luma) / delta.r);
    } else if delta.r < -1e-7 {
        limit = min(limit, mapped_luma / -delta.r);
    }
    if delta.g > 1e-7 {
        limit = min(limit, (1.0 - mapped_luma) / delta.g);
    } else if delta.g < -1e-7 {
        limit = min(limit, mapped_luma / -delta.g);
    }
    if delta.b > 1e-7 {
        limit = min(limit, (1.0 - mapped_luma) / delta.b);
    } else if delta.b < -1e-7 {
        limit = min(limit, mapped_luma / -delta.b);
    }
    return clamp(limit, 0.0, 1.0);
}

fn profile_tone_scene_shoulder_knee() -> f32 {
    // In the explicit-domain graph, tone statistics are measured after camera
    // characterization plus fixed rendering exposure, before LookTable and the
    // view transform. Legacy processes retain their historical profiled stats.
    // Reapply only live user Exposure so the shoulder follows scene headroom
    // without making Exposure trigger a new analysis pass.
    let user_exposure_ev = adaptive_tone_user_exposure_ev();
    let p95_over_white_ev = tone_stats.percentiles_0.w
        + user_exposure_ev
        + log2(SCENE_MIDDLE_GREY);
    let p995_over_white_ev = tone_stats.percentiles_1.x
        + user_exposure_ev
        + log2(SCENE_MIDDLE_GREY);

    // Broad bright content (p95) should pull the knee down sooner than a tiny
    // isolated specular (p99.5). The percentile gap is therefore treated as a
    // specular-isolation signal: a large gap delays the knee, protecting bright
    // skin, flowers and sunsets from being flattened just because a few pixels
    // sit far above display white.
    let broad_highlight_pressure = smoothstep(-0.55, 1.25, p95_over_white_ev);
    let peak_headroom_pressure = smoothstep(0.0, 3.5, p995_over_white_ev);
    let specular_gap_ev = max(
        tone_stats.percentiles_1.x - tone_stats.percentiles_0.w,
        0.0,
    );
    let isolated_specular = smoothstep(0.65, 3.0, specular_gap_ev);
    let peak_weight = peak_headroom_pressure * mix(1.0, 0.38, isolated_specular);
    let scene_pressure = clamp(
        broad_highlight_pressure * 0.74 + peak_weight * 0.26,
        0.0,
        1.0,
    );

    // Low-headroom scenes keep a late, nearly invisible shoulder. Scenes with
    // genuinely broad highlight headroom progressively move the knee earlier,
    // reserving enough display range for clouds and high-key texture. This is
    // scene-adaptive rather than a universal fixed knee.
    return mix(0.91, 0.62, scene_pressure);
}

fn profile_tone_display_shoulder(rgb: vec3<f32>) -> vec3<f32> {
    let positive = desaturate_negative_values(rgb);
    let luma = safe_luma(positive);
    if luma <= 1e-8 {
        return vec3<f32>(0.0);
    }

    // The DCP ProfileToneCurve already supplies the camera/profile's intended
    // midtone character. Add only a restrained display finish: a gentle toe
    // deepens blacks without pinning shadow detail to zero, while a luminance-
    // driven, scene-adaptive shoulder reserves display headroom according to
    // the actual bright-end distribution. Using luminance instead of the
    // brightest RGB channel avoids darkening saturated colors unnecessarily.
    let toe_weight = 1.0 - smoothstep(0.018, 0.22, luma);
    var mapped_luma = luma * mix(1.0, 0.91, toe_weight);
    let shoulder_knee = profile_tone_scene_shoulder_knee();
    if mapped_luma > shoulder_knee {
        let distance = mapped_luma - shoulder_knee;
        mapped_luma = shoulder_knee
            + distance / (1.0 + distance / (1.0 - shoulder_knee));
    }

    // Preserve the scene/profile hue and saturation by mapping luminance with
    // one scalar gain. Only compress chroma when a channel would leave display
    // gamut; this keeps bright flowers, signs, fabrics, and sunsets colorful
    // instead of washing them out through independent channel clipping.
    let ratio_preserved = positive * (mapped_luma / luma);
    let chroma_limit = default_view_chroma_limit(mapped_luma, ratio_preserved);
    let chroma_scale = select(
        chroma_limit,
        1.0,
        chroma_limit >= 0.9999,
    );
    return clamp(
        vec3<f32>(mapped_luma)
            + (ratio_preserved - vec3<f32>(mapped_luma)) * chroma_scale,
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}

fn apply_dcp_view_transform(scene_rgb: vec3<f32>) -> vec3<f32> {
    // ProfileToneCurve is a component of this one selected view operator, not
    // an upstream scene edit. The shoulder completes its HDR-to-display range
    // mapping without stacking the configurable sigmoid on top.
    let profile_view = apply_profile_view_tone(scene_rgb);
    return profile_tone_display_shoulder(profile_view);
}

fn apply_explicit_view_node(scene_rgb: vec3<f32>) -> vec3<f32> {
    // Optional creative profile look is the final scene-domain operation. It is
    // deliberately downstream of H/S/W/B, Contrast, curves, presence, mixer,
    // and grading so those controls mean the same thing across camera profiles.
    let looked = apply_optional_profile_look(scene_rgb);
    let view_input = max(map_negative_gamut(looked), vec3<f32>(0.0));

    // Select exactly one view-transform path. A default DCP rendition uses its
    // ProfileToneCurve inside the DCP-aware view node; a custom/user sigmoid is
    // the complete view transform and therefore does not stack the profile tone
    // curve ahead of it. This removes the previous double-tone behavior.
    if (params.process_info.y & 1u) != 0u {
        return apply_dcp_view_transform(view_input);
    }
    return apply_sigmoid_view_transform(view_input);
}

fn apply_legacy_view_node(scene_rgb: vec3<f32>) -> vec3<f32> {
    // Process <=12 compatibility: LookTable/ProfileToneCurve have already run
    // upstream. Preserve the historical final view selection byte-for-byte.
    if (params.process_info.y & 1u) != 0u {
        return profile_tone_display_shoulder(scene_rgb);
    }
    return darktable_sigmoid(scene_rgb);
}

@compute @workgroup_size(8, 8, 1)
fn apply_view_node(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let rgb = textureLoad(final_adjustment_tex, pos, 0).xyz;
    let mixed = apply_color_mixer(pos, rgb);
    let globally_graded = apply_color_grading_wheels(
        mixed,
        params.grade_shadows,
        params.grade_midtones,
        params.grade_highlights,
        params.grade_global,
        params.grade_options,
    );
    let graded = apply_local_color_grading(pos, globally_graded);
    var display_linear = vec3<f32>(0.0);
    if uses_explicit_scene_display_domains() {
        display_linear = apply_explicit_view_node(graded);
    } else {
        display_linear = apply_legacy_view_node(graded);
    }
    textureStore(display_linear_out, pos, vec4<f32>(display_linear, 1.0));
    // Output ICC/device encoding is a separate display-domain operation, not a
    // second view transform. It receives already display-referred linear RGB.
    textureStore(out_tex, pos, vec4<f32>(apply_output_lut(display_linear), 1.0));
}
