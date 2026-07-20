// Regression-only scene export. The internal demosaic texture stores camera
// RGB so the interactive pipeline can reuse it when input/profile settings
// change. This pass converts it to the canonical scene-linear Rec.2020 image
// without applying a creative look, tone curve, display transform, sharpening,
// or resizing.

@group(0) @binding(11) var regression_camera_scene: texture_2d<f32>;
@group(0) @binding(12) var regression_working_scene: texture_storage_2d<rgba32float, write>;

@compute @workgroup_size(8, 8, 1)
fn write_regression_scene(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let pos = vec2<i32>(i32(gid.x), i32(gid.y));
    let camera_rgb = textureLoad(regression_camera_scene, pos, 0).xyz;
    let working = cam_to_working(camera_rgb);
    let profile_corrected = apply_profile_hue_sat(working);
    let baseline_exposure_ev = bitcast<f32>(params.profile_flags.z);
    let scene_linear = map_negative_gamut(
        profile_corrected * exp2(baseline_exposure_ev),
    );
    textureStore(regression_working_scene, pos, vec4<f32>(scene_linear, 1.0));
}
