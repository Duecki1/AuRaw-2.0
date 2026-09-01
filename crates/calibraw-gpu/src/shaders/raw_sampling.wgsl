#import calibraw::common as Common

@group(0) @binding(3) var reconstructed_raw_tex: texture_2d<f32>;

// The loader supplies physical CFA planes in R, G1, B, G2 order.
fn cfa_channel_at(pos: vec2<i32>) -> u32 {
    return min(textureLoad(Common::color_tex, Common::clamp_pos(pos), 0).r, 3u);
}

fn color_at(pos: vec2<i32>) -> u32 {
    let channel = cfa_channel_at(pos);
    return select(channel, 1u, channel == 3u);
}

fn wb_for_cfa_channel(channel: u32) -> f32 {
    return Common::camera_uniforms.wb[min(channel, 3u)];
}

fn raw_sensor_at(pos: vec2<i32>) -> f32 {
    let p = Common::clamp_pos(pos);
    let channel = cfa_channel_at(p);
    let raw = f32(textureLoad(Common::raw_tex, p, 0).r);
    let metadata_black = textureLoad(Common::black_tex, p, 0).x;
    let white = max(Common::camera_uniforms.white_levels[channel], metadata_black + 1.0);
    let sensor_range = max(white - metadata_black, 1.0);

    let black_offset = clamp(Common::camera_uniforms.black_point, -0.25, 0.25) * sensor_range;
    let calibrated_black = clamp(
        metadata_black + black_offset,
        0.0,
        white - 1.0,
    );
    return clamp((raw - calibrated_black) / (white - calibrated_black), 0.0, 4.0);
}

fn raw_camera_at(pos: vec2<i32>) -> f32 {
    let p = Common::clamp_pos(pos);
    return raw_sensor_at(p) * wb_for_cfa_channel(cfa_channel_at(p));
}

fn shared_highlight_clip() -> f32 {
    let min_wb = min(
        min(Common::camera_uniforms.wb.r, Common::camera_uniforms.wb.g),
        min(Common::camera_uniforms.wb.b, Common::camera_uniforms.wb.a),
    );
    return 0.995 * max(Common::camera_uniforms.highlight_clip, 0.01) * max(min_wb, 1e-6);
}

fn is_raw_clipped(pos: vec2<i32>) -> bool {
    let p = Common::clamp_pos(pos);
    if Common::camera_uniforms.highlight_options.x >= 1.5 {
        return raw_sensor_at(p) >= 0.987 * max(Common::camera_uniforms.highlight_clip, 0.01);
    }
    return raw_camera_at(p) >= shared_highlight_clip();
}

fn raw_cfa_at(pos: vec2<i32>) -> f32 {
    return textureLoad(reconstructed_raw_tex, Common::clamp_pos(pos), 0).x;
}
