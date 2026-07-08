@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height {
        return;
    }

    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    var camera_rgb = demosaic(pos);
    camera_rgb = apply_wb(camera_rgb);
    camera_rgb = reconstruct_sensor_highlights(camera_rgb);

    var rgb = cam_to_srgb(camera_rgb);
    rgb = map_negative_gamut(rgb);
    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));
    rgb = reconstruct_display_highlights(rgb);
    rgb = compress_highlights(rgb);
    rgb = apply_brightness(rgb);
    rgb = apply_contrast(rgb);
    rgb = apply_saturation_vibrance(rgb);

    textureStore(out_tex, pos, vec4<f32>(display_render(rgb), 1.0));
}
