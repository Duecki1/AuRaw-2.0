// The reconstructed texture is written by the pre-demosaic highlight pass.
// It already contains black-level-normalized, white-balanced camera samples.
@group(0) @binding(3) var reconstructed_raw_tex: texture_2d<f32>;

fn color_at(pos: vec2<i32>) -> u32 {
    return textureLoad(color_tex, clamp_pos(pos), 0).r;
}

fn wb_for_channel(channel: u32) -> f32 {
    if channel == 0u { return params.wb.r; }
    if channel == 1u { return params.wb.g; }
    return params.wb.b;
}

fn raw_sensor_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    let color = color_at(p);
    let raw = f32(textureLoad(raw_tex, p, 0).r);
    let black = params.black_levels[color];
    let white = max(params.white_levels[color], black + 1.0);
    return clamp((raw - black) / (white - black), 0.0, 4.0);
}

fn raw_camera_at(pos: vec2<i32>) -> f32 {
    let p = clamp_pos(pos);
    return raw_sensor_at(p) * wb_for_channel(color_at(p));
}

fn shared_highlight_clip() -> f32 {
    // Ansel's LCh path uses one post-white-balance threshold based on the
    // smallest processed channel maximum. A per-channel threshold leaves the
    // saturated red/blue CFA values untouched and produces magenta highlights.
    let min_wb = min(params.wb.r, min(params.wb.g, params.wb.b));
    return 0.995 * max(params.highlight_clip, 0.01) * max(min_wb, 1e-6);
}

fn is_raw_clipped(pos: vec2<i32>) -> bool {
    let p = clamp_pos(pos);
    let color = color_at(p);
    if params.highlight_options.x >= 1.5 {
        // Guided reconstruction keeps Ansel's per-sensor-channel clipping
        // mask, expressed before white balance.
        return raw_sensor_at(p) >= 0.995 * max(params.highlight_clip, 0.01);
    }
    return raw_camera_at(p) >= shared_highlight_clip();
}

fn raw_cfa_at(pos: vec2<i32>) -> f32 {
    return textureLoad(reconstructed_raw_tex, clamp_pos(pos), 0).x;
}
