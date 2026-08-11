fn main() {
    for shader in [
        "common.wgsl",
        "profile.wgsl",
        "raw_sampling.wgsl",
        "noise.wgsl",
        "noise_ca_finish.wgsl",
        "color.wgsl",
        "color_denoise.wgsl",
        "highlights.wgsl",
        "basic_adjustments.wgsl",
        "tone_common.wgsl",
        "tone_analysis.wgsl",
        "tonemap.wgsl",
        "scene_adjustments.wgsl",
        "mask_effects/shared.wgsl",
        "mask_effects/glow.wgsl",
        "mask_effects/neon.wgsl",
        "creative_effects.wgsl",
        "view_transform.wgsl",
        "detail_capture.wgsl",
        "detail_scale_space.wgsl",
        "regression_scene.wgsl",
        "pass1.wgsl",
        "pass2.wgsl",
        "pass3.wgsl",
        "pass4.wgsl",
        "dual_demosaic.wgsl",
        "xtrans_demosaic.wgsl",
        "xtrans_finish.wgsl",
    ] {
        println!("cargo:rerun-if-changed=src/shaders/{shader}");
    }
}
