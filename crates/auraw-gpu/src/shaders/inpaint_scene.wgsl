#import auraw::common as Common
#import auraw::color as Color

@group(0) @binding(11) var camera_scene: texture_2d<f32>;
@group(0) @binding(12) var working_scene: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(8, 8, 1)
fn write_inpaint_working_scene(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let camera_rgb = textureLoad(camera_scene, pos, 0).xyz;
    let working = Color::cam_to_working(camera_rgb);
    textureStore(working_scene, pos, vec4<f32>(working, 1.0));
}
