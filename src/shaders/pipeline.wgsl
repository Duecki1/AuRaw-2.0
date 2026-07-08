// pipeline.wgsl

@compute @workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height {
        return;
    }

    let pos = vec2<i32>(i32(gid.x), i32(gid.y));

    // 1. Demosaic with color-difference interpolation
    var camera_rgb = demosaic(pos);

    // 2. White balance (in camera raw space, before color transform)
    camera_rgb = apply_wb(camera_rgb);

    // 3. Highlight reconstruction (ratio-based, preserves hue)
    camera_rgb = reconstruct_sensor_highlights(camera_rgb);

    // 4. Camera → linear Rec2020 working space
    var rgb = cam_to_working(camera_rgb);

    // 5. Negative gamut mapping (handle out-of-gamut from camera transform)
    rgb = map_negative_gamut(rgb);

    // 6. Exposure (scene-referred)
    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));

    // 7. Contrast (luma-based, preserves hue)
    rgb = apply_contrast(rgb);

    // 8. Saturation / vibrance (perceptual, skin-tone protected)
    rgb = apply_saturation_vibrance(rgb);

    // 9. Filmic tonemap + Rec2020→sRGB + sRGB OETF
    textureStore(out_tex, pos, vec4<f32>(display_render(rgb), 1.0));
}