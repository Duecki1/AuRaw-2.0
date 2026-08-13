use super::{
    composite_inpaint_rgba16f, processing_work_format, shader_manager::ShaderManager,
    work_shader_source, ProcessingQuality, SHADER_BAYER_RCD_P1, SHADER_BAYER_RCD_P2,
    SHADER_BAYER_RCD_P3, SHADER_BAYER_RCD_P4, SHADER_COLOR_DENOISE, SHADER_CREATIVE_EFFECTS,
    SHADER_DUAL_DEMOSAIC, SHADER_HIGHLIGHTS, SHADER_INPAINT_SCENE, SHADER_SCENE_ADJUSTMENTS,
    SHADER_TONE_ANALYSIS, SHADER_VIEW_TRANSFORM, SHADER_XTRANS_DEMOSAIC, SHADER_XTRANS_FINISH,
};

fn validate_shader(name: &str, source: &str, quality: ProcessingQuality) {
    let format = processing_work_format(quality);
    let mut manager = ShaderManager::new(format).unwrap();
    let source = match quality {
        ProcessingQuality::Preview => std::borrow::Cow::Borrowed(source),
        ProcessingQuality::High => work_shader_source(source, format).unwrap(),
    };
    let module = manager
        .compose_naga_module(source.as_ref(), "shader_test.wgsl")
        .unwrap_or_else(|error| panic!("{name} did not compose: {error:#}"));
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|error| panic!("{name} did not validate: {error}"));
}

#[test]
fn compute_shaders_validate() {
    for (name, source) in [
        ("highlights", SHADER_HIGHLIGHTS),
        ("Bayer pass 1", SHADER_BAYER_RCD_P1),
        ("Bayer pass 2", SHADER_BAYER_RCD_P2),
        ("Bayer pass 3", SHADER_BAYER_RCD_P3),
        ("Bayer pass 4", SHADER_BAYER_RCD_P4),
        ("dual demosaic", SHADER_DUAL_DEMOSAIC),
        ("X-Trans demosaic", SHADER_XTRANS_DEMOSAIC),
        ("X-Trans finish", SHADER_XTRANS_FINISH),
        ("color denoise", SHADER_COLOR_DENOISE),
        ("tone analysis", SHADER_TONE_ANALYSIS),
        ("inpaint scene", SHADER_INPAINT_SCENE),
        ("scene adjustments", SHADER_SCENE_ADJUSTMENTS),
        ("creative effects", SHADER_CREATIVE_EFFECTS),
        ("view transform", SHADER_VIEW_TRANSFORM),
    ] {
        validate_shader(name, source, ProcessingQuality::Preview);
    }
}

#[test]
fn high_quality_shaders_validate() {
    for (name, source) in [
        ("Bayer pass 1", SHADER_BAYER_RCD_P1),
        ("dual demosaic", SHADER_DUAL_DEMOSAIC),
        ("color denoise", SHADER_COLOR_DENOISE),
        ("scene adjustments", SHADER_SCENE_ADJUSTMENTS),
    ] {
        validate_shader(name, source, ProcessingQuality::High);
    }
}

#[test]
fn soft_inpaint_composites_color_and_alpha() {
    use half::f16;

    let mut destination = [
        f16::from_f32(0.2).to_bits(),
        f16::from_f32(0.4).to_bits(),
        f16::from_f32(0.6).to_bits(),
        f16::from_f32(0.5).to_bits(),
    ];
    composite_inpaint_rgba16f(&mut destination, [0.8, 0.2, 0.1], 0.25);
    let output = destination.map(|value| f16::from_bits(value).to_f32());

    for (actual, expected) in output.into_iter().zip([0.44, 0.32, 0.4, 0.625]) {
        assert!((actual - expected).abs() < 1e-3);
    }
}
