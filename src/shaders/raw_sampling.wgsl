// The reconstructed texture is written by the pre-demosaic highlight pass.
// It already contains black-level-normalized, white-balanced camera samples.
@group(0) @binding(3) var reconstructed_raw_tex: texture_2d<f32>;

// The loader canonicalizes physical CFA planes to R, G1, B, G2. Keep G1
// and G2 distinct for black/white/WB calibration, but expose a collapsed RGB
// color to the Bayer demosaic code.
fn cfa_channel_at(pos: vec2<i32>) -> u32 {
    return min(textureLoad(color_tex, clamp_pos(pos), 0).r, 3u);
}

fn color_at(pos: vec2<i32>) -> u32 {
    let channel = cfa_channel_at(pos);
    return select(channel, 1u, channel == 3u);
}

fn wb_for_cfa_channel(channel: u32) -> f32 {
    return params.wb[min(channel, 3u)];
}

fn raw_sensor_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let channel = cfa_channel_at(p);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let black = params.black_levels[channel];
    let white = max(params.white_levels[channel], black + 1.0);
    return clamp((raw - black) / (white - black), 0.0, 4.0);
}

fn raw_camera_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    return raw_sensor_at(p) * wb_for_cfa_channel(cfa_channel_at(p));
}

fn shared_highlight_clip() -> f32 {
    // The common post-WB threshold must include both green photosite planes.
    let min_wb = min(
        min(params.wb.r, params.wb.g),
        min(params.wb.b, params.wb.a),
    );
    return 0.995 * max(params.highlight_clip, 0.01) * max(min_wb, 1e-6);
}

fn is_raw_clipped(pos: vec2<i32>) -> bool {
    let p = clamp_pos(pos);
    if params.highlight_options.x >= 1.5 {
        // Guided reconstruction keeps a per-sensor-plane clipping mask before
        // white balance.
        return raw_sensor_at(p) >= 0.995 * max(params.highlight_clip, 0.01);
    }
    return raw_camera_at(p) >= shared_highlight_clip();
}

fn raw_cfa_at(pos: vec2<i32>) -> f32 {
    return textureLoad(reconstructed_raw_tex, clamp_pos(pos), 0).x;
}
