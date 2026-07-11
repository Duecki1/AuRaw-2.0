// Fast pre-demosaic highlight reconstruction.
//
// This is the Bayer LCh method from Ansel's highlight-reconstruction module,
// expressed in WGSL.  It deliberately runs on the white-balanced CFA mosaic:
// reconstructing after demosaic cannot recover the lost sensor channel and is
// the source of the magenta/grey artifacts the old shader produced.

const ANSEL_SQRT3: f32 = 1.7320508075688772;
const ANSEL_SQRT12: f32 = 3.4641016151377544;

fn highlight_color_at(pos: vec2<i32>) -> u32 {
    return textureLoad(color_tex, clamp_pos(pos), 0).r;
}

fn highlight_wb_for_channel(channel: u32) -> f32 {
    if channel == 0u { return params.wb.r; }
    if channel == 1u { return params.wb.g; }
    return params.wb.b;
}

fn highlight_raw_camera_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let channel = highlight_color_at(p);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let black = params.black_levels[channel];
    let white = max(params.white_levels[channel], black + 1.0);
    let sensor = clamp((raw - black) / (white - black), 0.0, 4.0);
    return sensor * highlight_wb_for_channel(channel);
}

fn highlight_clip_for(channel: u32) -> f32 {
    return max(params.highlight_clip, 0.01) * highlight_wb_for_channel(channel);
}

fn lch_reconstructed_cfa_at(pos: vec2<i32>) -> f32 {
    let center = clamp_pos(pos);
    let center_color = highlight_color_at(center);
    let original = highlight_raw_camera_at(center);

    // A 2×2 support is the exact fast Bayer route used by Ansel.  It is
    // intentionally disabled at the edge and for non-Bayer CFA blocks; those
    // samples remain untouched rather than inventing colour.
    if center.x >= i32(params.width) - 1 || center.y >= i32(params.height) - 1 {
        return original;
    }

    var r = 0.0;
    var b = 0.0;
    var g_min = 1e20;
    var g_max = -1e20;
    var have_r = false;
    var have_b = false;
    var greens = 0u;

    for (var dy = 0; dy <= 1; dy = dy + 1) {
        for (var dx = 0; dx <= 1; dx = dx + 1) {
            let p = center + vec2<i32>(dx, dy);
            let channel = highlight_color_at(p);
            let value = highlight_raw_camera_at(p);
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

    if !have_r || !have_b || greens < 2u {
        return original;
    }

    let clipped = r > highlight_clip_for(0u)
        || g_max > highlight_clip_for(1u)
        || b > highlight_clip_for(2u);
    if !clipped || params.highlight_reconstruction <= 1e-6 {
        return original;
    }

    // Ansel's LCh-like transform works from the un-clipped lightness and
    // reduces only chroma using the clipped reference values.  Keeping L
    // preserves highlight texture instead of turning broad clipped areas grey.
    let ro = min(r, highlight_clip_for(0u));
    let go = min(g_min, highlight_clip_for(1u));
    let bo = min(b, highlight_clip_for(2u));

    let lightness = (r + g_max + b) / 3.0;
    var chroma = ANSEL_SQRT3 * (r - g_max);
    var hue_axis = 2.0 * b - g_max - r;
    let clipped_chroma = ANSEL_SQRT3 * (ro - go);
    let clipped_hue_axis = 2.0 * bo - go - ro;
    let denominator = chroma * chroma + hue_axis * hue_axis;

    if denominator > 1e-12 {
        let numerator = max(
            clipped_chroma * clipped_chroma + clipped_hue_axis * clipped_hue_axis,
            0.0,
        );
        let ratio = clamp(sqrt(numerator / denominator), 0.0, 1.0);
        chroma = chroma * ratio;
        hue_axis = hue_axis * ratio;
    }

    let recovered_r = lightness - hue_axis / 6.0 + chroma / ANSEL_SQRT12;
    let recovered_g = lightness - hue_axis / 6.0 - chroma / ANSEL_SQRT12;
    let recovered_b = lightness + hue_axis / 3.0;
    let recovered = select(
        select(recovered_r, recovered_g, center_color == 1u),
        recovered_b,
        center_color == 2u,
    );

    return mix(original, max(recovered, 0.0), clamp(params.highlight_reconstruction, 0.0, 1.0));
}
