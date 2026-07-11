@group(0) @binding(3) var reconstructed_raw_write: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(8, 8, 1)
fn highlight_lch_reconstruct(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    textureStore(
        reconstructed_raw_write,
        pos,
        vec4<f32>(lch_reconstructed_cfa_at(pos), 0.0, 0.0, 1.0),
    );
}
