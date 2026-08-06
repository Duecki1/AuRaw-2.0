// SPDX-License-Identifier: GPL-3.0-or-later
// Pre-demosaic highlight reconstruction adapted from darktable 5.6.0.
// The inpaint-opposed path follows src/iop/hlreconstruct/opposed.c and the
// highlights_opposed kernel in data/kernels/basic.cl.
// Copyright (C) 2010-2026 darktable developers.
// Copyright (C) 2026 AuRaw contributors (WGSL adaptation).

@group(0) @binding(3) var reconstructed_raw_write: texture_storage_2d<r32float, write>;

const DARKTABLE_SQRT3: f32 = 1.7320508075688772;
const DARKTABLE_SQRT12: f32 = 3.4641016151377544;
const DARKTABLE_OPPOSED_CLIP_MAGIC: f32 = 0.987;

fn highlight_cfa_channel_at(pos: vec2<i32>) -> u32 {
    return min(textureLoad(color_tex, clamp_pos(pos), 0).r, 3u);
}

fn highlight_color_at(pos: vec2<i32>) -> u32 {
    let channel = highlight_cfa_channel_at(pos);
    return select(channel, 1u, channel == 3u);
}

fn highlight_wb_for_cfa_channel(channel: u32) -> f32 {
    return camera_uniforms.wb[min(channel, 3u)];
}

fn highlight_raw_sensor_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let channel = highlight_cfa_channel_at(p);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let metadata_black = textureLoad(black_tex, p, 0).x;
    let white = max(camera_uniforms.white_levels[channel], metadata_black + 1.0);
    let sensor_range = max(white - metadata_black, 1.0);
    let black_offset = clamp(camera_uniforms.black_point, -0.25, 0.25) * sensor_range;
    let calibrated_black = clamp(metadata_black + black_offset, 0.0, white - 1.0);
    return clamp((raw - calibrated_black) / (white - calibrated_black), 0.0, 4.0);
}

fn highlight_raw_camera_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let channel = highlight_cfa_channel_at(p);
    return highlight_raw_sensor_at(p) * highlight_wb_for_cfa_channel(channel);
}

fn lch_common_clip() -> f32 {
    let min_wb = min(min(camera_uniforms.wb.r, camera_uniforms.wb.g), min(camera_uniforms.wb.b, camera_uniforms.wb.a));
    return max(camera_uniforms.highlight_clip, 0.01) * max(min_wb, 1e-6);
}

// Bayer opponent-colour reconstruction from darktable's LCh method. It is
// retained as an optional compatibility choice; X-Trans falls back to opposed.
fn lch_reconstructed_cfa_at(pos: vec2<i32>) -> f32 {
    let center = clamp_pos(pos);
    let center_color = highlight_color_at(center);
    let original = highlight_raw_camera_at(center);
    let clip = lch_common_clip();
    let strength = clamp(camera_uniforms.highlight_reconstruction, 0.0, 1.0);

    if center.x >= i32(camera_uniforms.width) - 1 || center.y >= i32(camera_uniforms.height) - 1 {
        return mix(original, min(original, clip), strength);
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
            clipped = clipped || value >= clip;
            if channel == 0u {
                r = value;
                have_r = true;
            } else if channel == 1u {
                g_min = min(g_min, value);
                g_max = max(g_max, value);
                greens = greens + 1u;
            } else {
                b = value;
                have_b = true;
            }
        }
    }

    if !have_r || !have_b || greens < 2u || !clipped {
        return original;
    }

    let ro = min(r, clip);
    let go = min(g_min, clip);
    let bo = min(b, clip);
    let lightness = (r + g_max + b) / 3.0;
    var chroma = DARKTABLE_SQRT3 * (r - g_max);
    var hue_axis = 2.0 * b - g_max - r;
    let clipped_chroma = DARKTABLE_SQRT3 * (ro - go);
    let clipped_hue_axis = 2.0 * bo - go - ro;

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

    let recovered_r = lightness - hue_axis / 6.0 + chroma / DARKTABLE_SQRT12;
    let recovered_g = lightness - hue_axis / 6.0 - chroma / DARKTABLE_SQRT12;
    let recovered_b = lightness + hue_axis / 3.0;
    let recovered = select(
        select(recovered_r, recovered_g, center_color == 1u),
        recovered_b,
        center_color == 2u,
    );
    return mix(original, max(recovered, 0.0), strength);
}

// darktable calculates every channel mean in the local 3x3 raw-RGB cube,
// moves those means into cube-root space, and defines a channel's reference as
// the arithmetic mean of both opposing channels before cubing it again.
fn inpaint_opposed_refavg(pos: vec2<i32>) -> f32 {
    let center = clamp_pos(pos);
    let color = highlight_color_at(center);
    var mean = vec3<f32>(0.0);
    var count = vec3<f32>(0.0);
    let max_row = max(i32(camera_uniforms.height) - 1, 0);
    let max_col = max(i32(camera_uniforms.width) - 1, 0);
    let row_end = min(max_row, center.y + 2);
    let col_end = min(max_col, center.x + 2);

    for (var row = max(0, center.y - 1); row < row_end; row = row + 1) {
        for (var col = max(0, center.x - 1); col < col_end; col = col + 1) {
            let sample_pos = vec2<i32>(col, row);
            let sample_color = highlight_color_at(sample_pos);
            mean[sample_color] = mean[sample_color] + max(highlight_raw_camera_at(sample_pos), 0.0);
            count[sample_color] = count[sample_color] + 1.0;
        }
    }
    for (var channel = 0u; channel < 3u; channel = channel + 1u) {
        mean[channel] = select(0.0, pow(mean[channel] / count[channel], 1.0 / 3.0), count[channel] > 0.0);
    }
    let root_reference = select(
        select(0.5 * (mean.r + mean.b), 0.5 * (mean.g + mean.b), color == 0u),
        0.5 * (mean.r + mean.g),
        color == 2u,
    );
    return root_reference * root_reference * root_reference;
}

fn inpaint_opposed_cfa_at(pos: vec2<i32>) -> f32 {
    let center = clamp_pos(pos);
    let physical_channel = highlight_cfa_channel_at(center);
    let color = highlight_color_at(center);
    let original = highlight_raw_camera_at(center);
    let clip = DARKTABLE_OPPOSED_CLIP_MAGIC
        * max(camera_uniforms.highlight_clip, 0.01)
        * highlight_wb_for_cfa_channel(physical_channel);
    if original < clip {
        return original;
    }
    let reference = inpaint_opposed_refavg(center);
    let chrominance = camera_uniforms.highlight_options[color + 1u];
    return max(original, reference + chrominance);
}

@compute @workgroup_size(8, 8, 1)
fn highlight_reconstruct(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= camera_uniforms.width || gid.y >= camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let method = camera_uniforms.highlight_options.x;
    var output = highlight_raw_camera_at(pos);
    if method >= 0.5 && method < 1.5 {
        output = lch_reconstructed_cfa_at(pos);
    } else if method >= 1.5 {
        output = inpaint_opposed_cfa_at(pos);
    }
    textureStore(
        reconstructed_raw_write,
        pos,
        vec4<f32>(max(output, 0.0), 0.0, 0.0, 1.0),
    );
}
