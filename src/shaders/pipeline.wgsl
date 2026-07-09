@compute @workgroup_size(8, 8, 1)
fn pass3_reconstruct(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height {
        return;
    }

    let pos = vec2<i32>(i32(gid.x), i32(gid.y));

    let g_val = textureLoad(green_tex_read, pos, 0);
    let g = g_val.x;
    let clipG = g_val.w;
    
    let chroma = textureLoad(chroma_tex_read, pos, 0);
    let diffR = chroma.x;
    let diffB = chroma.y;
    let clipR = chroma.z;
    let clipB = chroma.w;
    
    let r = g + diffR;
    let b = g + diffB;
    
    let clip_mask = clipG * 10.0 + clipR * 1.0 + clipB * 100.0;
    
    var camera_rgb = vec3<f32>(r, g, b);

    camera_rgb = apply_wb(camera_rgb);
    camera_rgb = reconstruct_sensor_highlights(camera_rgb, clip_mask);

    var rgb = cam_to_working(camera_rgb);
    rgb = map_negative_gamut(rgb);

    rgb = apply_exposure(rgb);
    rgb = max(rgb, vec3<f32>(0.0));

    rgb = apply_contrast(rgb);
    rgb = apply_saturation_vibrance(rgb);

    textureStore(out_tex, pos, vec4<f32>(display_render(rgb), 1.0));
}