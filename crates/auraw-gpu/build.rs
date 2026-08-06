use std::path::PathBuf;

#[path = "build_support/shader_preprocessor.rs"]
mod shader_preprocessor;

fn main() {
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .unwrap_or_else(|error| panic!("Cargo did not set CARGO_MANIFEST_DIR: {error}")),
    );
    let shader_dir = manifest_dir.join("src/shaders");
    let output_dir = PathBuf::from(
        std::env::var("OUT_DIR")
            .unwrap_or_else(|error| panic!("Cargo did not set OUT_DIR: {error}")),
    );

    shader_preprocessor::generate_shader_sources(&shader_dir, &output_dir)
        .unwrap_or_else(|error| panic!("could not generate WGSL shader sources: {error}"));

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
