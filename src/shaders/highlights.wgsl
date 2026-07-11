// Pre-demosaic highlight reconstruction translated from Ansel's highlights
// module. All values here are black-level-normalized, white-balanced camera
// samples. Repairing the CFA before demosaic prevents a clipped sensor channel
// from becoming the familiar magenta/pink highlight after exposure is lowered.

const ANSEL_SQRT3: f32 = 1.7320508075688772;
const ANSEL_SQRT12: f32 = 3.4641016151377544;
const INV_SQRT3: f32 = 0.5773502691896258;

struct HighlightSample {
    rgb: vec3<f32>,
    clipped: vec3<f32>,
}

fn highlight_cfa_channel_at(pos: vec2<i32>) -> u32 {
    return min(textureLoad(color_tex, clamp_pos(pos), 0).r, 3u);
}

fn highlight_color_at(pos: vec2<i32>) -> u32 {
    let channel = highlight_cfa_channel_at(pos);
    return select(channel, 1u, channel == 3u);
}

fn highlight_wb_for_cfa_channel(channel: u32) -> f32 {
    return params.wb[min(channel, 3u)];
}

fn highlight_raw_sensor_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let channel = highlight_cfa_channel_at(p);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let black = params.black_levels[channel];
    let white = max(params.white_levels[channel], black + 1.0);
    return clamp((raw - black) / (white - black), 0.0, 4.0);
}

fn highlight_raw_camera_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    return highlight_raw_sensor_at(p)
        * highlight_wb_for_cfa_channel(highlight_cfa_channel_at(p));
}

fn ansel_lch_common_clip() -> f32 {
    // Ansel publishes the WB multipliers as processed channel maxima and uses
    // their minimum as ONE common post-WB threshold. Using clip*WB[channel]
    // here is incorrect: exact sensor saturation then never crosses the test.
    let min_wb = min(
        min(params.wb.r, params.wb.g),
        min(params.wb.b, params.wb.a),
    );
    return max(params.highlight_clip, 0.01) * max(min_wb, 1e-6);
}

fn guided_sensor_clip() -> f32 {
    // Guided Laplacians use per-channel maxima with a 0.5% guard. Since this
    // implementation tests pre-WB sensor values, the WB factor cancels.
    return 0.995 * max(params.highlight_clip, 0.01);
}

// Exact Bayer opponent-colour reconstruction from Ansel's
// highlights_1f_lch_bayer, with an app-specific final strength blend.
fn ansel_lch_reconstructed_cfa_at(pos: vec2<i32>) -> f32 {
    let center = clamp_pos(pos);
    let center_color = highlight_color_at(center);
    let original = highlight_raw_camera_at(center);
    let clip = ansel_lch_common_clip();

    if center.x >= i32(params.width) - 1 || center.y >= i32(params.height) - 1 {
        return mix(original, min(original, clip), clamp(params.highlight_reconstruction, 0.0, 1.0));
    }

    var r = 0.0;
    var b = 0.0;
    var g_min = 1e20;
    var g_max = -1e20;
    var have_r = false;
    var have_b = false;
    var greens = 0u;
    var clipped = false;

    for (var dy = 0; dy <= 1; dy = dy + 1) {
        for (var dx = 0; dx <= 1; dx = dx + 1) {
            let p = center + vec2<i32>(dx, dy);
            let channel = highlight_color_at(p);
            let value = highlight_raw_camera_at(p);
            clipped = clipped || value > clip;
            if channel == 0u {
                r = value;
                have_r = true;
            } else if channel == 1u {
                g_min = min(g_min, value);
                g_max = max(g_max, value);
                greens = greens + 1u;
            } else if channel == 2u {
                b = value;
                have_b = true;
            }
        }
    }

    // A non-Bayer 2x2 block is routed unchanged. The guided method below is
    // pattern-driven and remains available for X-Trans and unusual CFAs.
    if !have_r || !have_b || greens < 2u || !clipped {
        return original;
    }

    let ro = min(r, clip);
    let go = min(g_min, clip);
    let bo = min(b, clip);
    let lightness = (r + g_max + b) / 3.0;
    var chroma = ANSEL_SQRT3 * (r - g_max);
    var hue_axis = 2.0 * b - g_max - r;
    let clipped_chroma = ANSEL_SQRT3 * (ro - go);
    let clipped_hue_axis = 2.0 * bo - go - ro;

    // Match Ansel's guard rather than clamping the ratio: in rare cases the
    // clipped opponent chroma can legitimately be larger than the input one.
    if r != g_max && g_max != b {
        let denominator = chroma * chroma + hue_axis * hue_axis;
        if denominator > 1e-12 {
            let numerator = max(
                clipped_chroma * clipped_chroma + clipped_hue_axis * clipped_hue_axis,
                0.0,
            );
            let ratio = sqrt(numerator / denominator);
            chroma = chroma * ratio;
            hue_axis = hue_axis * ratio;
        }
    }

    let recovered_r = lightness - hue_axis / 6.0 + chroma / ANSEL_SQRT12;
    let recovered_g = lightness - hue_axis / 6.0 - chroma / ANSEL_SQRT12;
    let recovered_b = lightness + hue_axis / 3.0;
    let recovered = select(
        select(recovered_r, recovered_g, center_color == 1u),
        recovered_b,
        center_color == 2u,
    );
    return mix(
        original,
        max(recovered, 0.0),
        clamp(params.highlight_reconstruction, 0.0, 1.0),
    );
}

// Pattern-driven bilinear interpolation and clipping mask. For Bayer, choosing
// all nearest photosites is the same cross/row/diagonal support Ansel uses. The
// search radius also makes the guided route usable on X-Trans mosaics.
fn highlight_interpolate_and_mask(pos: vec2<i32>) -> HighlightSample {
    let center = clamp_pos(pos);
    let center_color = highlight_color_at(center);
    let sensor_clip = guided_sensor_clip();
    var rgb = vec3<f32>(0.0);
    var clipped = vec3<f32>(0.0);

    for (var channel = 0u; channel < 3u; channel = channel + 1u) {
        if center_color == channel {
            let sensor = highlight_raw_sensor_at(center);
            rgb[channel] = highlight_raw_camera_at(center);
            clipped[channel] = select(0.0, 1.0, sensor >= sensor_clip);
        } else {
            var best_distance = 1000;
            var sum_camera = 0.0;
            var count = 0.0;
            var used_clipped = false;
            for (var dy = -2; dy <= 2; dy = dy + 1) {
                for (var dx = -2; dx <= 2; dx = dx + 1) {
                    if dx == 0 && dy == 0 { continue; }
                    let sample_pos = clamp_pos(center + vec2<i32>(dx, dy));
                    if highlight_color_at(sample_pos) != channel { continue; }
                    let distance = dx * dx + dy * dy;
                    let sensor = highlight_raw_sensor_at(sample_pos);
                    let camera = highlight_raw_camera_at(sample_pos);
                    if distance < best_distance {
                        best_distance = distance;
                        sum_camera = camera;
                        count = 1.0;
                        used_clipped = sensor >= sensor_clip;
                    } else if distance == best_distance {
                        sum_camera = sum_camera + camera;
                        count = count + 1.0;
                        used_clipped = used_clipped || sensor >= sensor_clip;
                    }
                }
            }
            rgb[channel] = select(
                highlight_raw_camera_at(center),
                sum_camera / max(count, 1.0),
                count > 0.0,
            );
            clipped[channel] = select(0.0, 1.0, used_clipped);
        }
    }

    return HighlightSample(rgb, clipped);
}

fn guided_clipping_mask(pos: vec2<i32>) -> f32 {
    let sensor_clip = guided_sensor_clip();
    var mask = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let clipped = highlight_raw_sensor_at(pos + vec2<i32>(dx, dy)) >= sensor_clip;
            mask = mask + select(0.0, 1.0, clipped);
        }
    }
    // A feathered support avoids zippering at the clipped/unclipped boundary.
    return smoothstep(0.0, 0.45, mask / 9.0);
}
