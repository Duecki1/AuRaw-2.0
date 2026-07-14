// Markesteijn directional derivative stage. Eight RGB candidates are converted
// to perceptual YUV and differentiated along the four reference axes. The
// scalar derivatives fit in two RGBA scratch textures.
@group(0) @binding(20) var mark_drv_0_3_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;
@group(0) @binding(21) var mark_drv_4_7_write: texture_storage_2d<rgba16float /* AURAW_WORK_FORMAT */, write>;

fn mark_derivative(pos: vec2<i32>, index: u32) -> f32 {
    let axis = mark_axis(index);
    let center = mark_yuv(mark_candidate(pos, index));
    let forward = mark_yuv(mark_candidate(pos + axis, index));
    let backward = mark_yuv(mark_candidate(pos - axis, index));
    let second = 2.0 * center - forward - backward;
    return dot(second, second);
}

@compute @workgroup_size(8, 8, 1)
fn xtrans_markesteijn_derivatives(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    if !mark_has_margin(pos) {
        textureStore(mark_drv_0_3_write, pos, vec4<f32>(0.0));
        textureStore(mark_drv_4_7_write, pos, vec4<f32>(0.0));
        return;
    }
    textureStore(mark_drv_0_3_write, pos, vec4<f32>(
        mark_derivative(pos, 0u),
        mark_derivative(pos, 1u),
        mark_derivative(pos, 2u),
        mark_derivative(pos, 3u),
    ));
    textureStore(mark_drv_4_7_write, pos, vec4<f32>(
        mark_derivative(pos, 4u),
        mark_derivative(pos, 5u),
        mark_derivative(pos, 6u),
        mark_derivative(pos, 7u),
    ));
}
