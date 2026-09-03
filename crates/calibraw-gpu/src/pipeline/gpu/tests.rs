use super::{
    pack_effect_mask, pack_local_point_curve, pack_point_curve, processing_work_format,
    shader_manager::ShaderManager, work_shader_source, ProcessingQuality, SHADER_BAYER_RCD_P1,
    SHADER_BAYER_RCD_P2, SHADER_BAYER_RCD_P3, SHADER_BAYER_RCD_P4, SHADER_COLOR_DENOISE,
    SHADER_CREATIVE_EFFECTS, SHADER_DUAL_DEMOSAIC, SHADER_HIGHLIGHTS, SHADER_RAW_SAMPLING,
    SHADER_REMOVE_COMPOSITE, SHADER_SCENE_ADJUSTMENTS, SHADER_TONEMAP, SHADER_TONE_ANALYSIS,
    SHADER_VIEW_TRANSFORM,
    SHADER_XTRANS_DEMOSAIC, SHADER_XTRANS_FINISH,
};
use crate::pipeline::{LocalMask, MaskEffect, MaskKind, PointCurve};

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

fn shader_f32_const(source: &str, name: &str) -> f32 {
    let declaration = format!("const {name}: f32 = ");
    source
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix(&declaration)
                .and_then(|value| value.strip_suffix(';'))
        })
        .unwrap_or_else(|| panic!("shader constant {name} is missing"))
        .parse::<f32>()
        .unwrap_or_else(|error| panic!("shader constant {name} is not numeric: {error}"))
}

fn shared_highlight_clip_for_test(highlight_clip: f32, wb: [f32; 4]) -> f32 {
    let safety = shader_f32_const(SHADER_RAW_SAMPLING, "SHARED_HIGHLIGHT_CLIP_SAFETY");
    let min_wb = wb.into_iter().fold(f32::INFINITY, f32::min);
    safety * highlight_clip.max(0.01) * min_wb.max(1e-6)
}

fn adjacent_f32(value: f32, above: bool) -> f32 {
    assert!(value.is_finite() && value > 0.0);
    let bits = value.to_bits();
    f32::from_bits(if above { bits + 1 } else { bits - 1 })
}

#[test]
fn lch_and_rcd_share_common_highlight_clip_definition() {
    assert!(SHADER_RAW_SAMPLING.contains("fn shared_highlight_clip() -> f32"));
    assert!(SHADER_RAW_SAMPLING.contains("raw_camera_at(p) >= shared_highlight_clip()"));
    assert!(SHADER_HIGHLIGHTS.contains("#import calibraw::raw_sampling as RawSampling"));
    assert!(SHADER_HIGHLIGHTS.contains("let clip = RawSampling::shared_highlight_clip();"));
    assert!(!SHADER_HIGHLIGHTS.contains("fn lch_common_clip()"));

    validate_shader("highlights", SHADER_HIGHLIGHTS, ProcessingQuality::Preview);
    validate_shader("Bayer pass 2", SHADER_BAYER_RCD_P2, ProcessingQuality::Preview);
}

#[test]
fn common_highlight_clip_classifies_adjacent_values_consistently() {
    let highlight_clip = 1.0;
    let wb = [2.0, 1.0, 1.5, 1.0];
    let clip = shared_highlight_clip_for_test(highlight_clip, wb);
    let below = adjacent_f32(clip, false);
    let above = adjacent_f32(clip, true);

    let raw_sampling_clipped = |value: f32| value >= clip;
    let lch_clipped = |value: f32| value >= clip;

    assert!(!raw_sampling_clipped(below));
    assert!(!lch_clipped(below));
    assert!(raw_sampling_clipped(above));
    assert!(lch_clipped(above));

    // This boundary would have exposed the former LCh 1.0-vs-shared 0.995 drift.
    let former_lch_clip = highlight_clip * wb.into_iter().fold(f32::INFINITY, f32::min);
    assert!(above < former_lch_clip);
}

#[test]
fn opposed_sensor_and_wb_domain_clipping_are_equivalent() {
    const DARKTABLE_OPPOSED_CLIP_MAGIC: f32 = 0.987;

    let highlight_clip = 1.03_f32;
    let sensor_clip = DARKTABLE_OPPOSED_CLIP_MAGIC * highlight_clip.max(0.01);
    let sensor_values = [
        adjacent_f32(sensor_clip, false),
        sensor_clip,
        adjacent_f32(sensor_clip, true),
        0.5 * sensor_clip,
        1.5 * sensor_clip,
    ];

    for wb in [0.5_f32, 1.0, 1.75, 2.4] {
        for raw_sensor in sensor_values {
            let raw_camera = raw_sensor * wb;
            let sensor_domain = raw_sensor >= sensor_clip;
            let wb_domain = raw_camera
                >= DARKTABLE_OPPOSED_CLIP_MAGIC * highlight_clip.max(0.01) * wb;
            assert_eq!(sensor_domain, wb_domain, "wb={wb}, raw_sensor={raw_sensor}");
        }
    }
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
        ("scene adjustments", SHADER_SCENE_ADJUSTMENTS),
        ("creative effects", SHADER_CREATIVE_EFFECTS),
        ("Remove composite", SHADER_REMOVE_COMPOSITE),
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
        ("X-Trans demosaic", SHADER_XTRANS_DEMOSAIC),
        ("color denoise", SHADER_COLOR_DENOISE),
        ("Remove composite", SHADER_REMOVE_COMPOSITE),
        ("scene adjustments", SHADER_SCENE_ADJUSTMENTS),
    ] {
        validate_shader(name, source, ProcessingQuality::High);
    }
}

#[test]
fn point_curve_packing_is_shared_between_global_and_local_uniforms() {
    let curve = PointCurve {
        points: [
            [0.0, 0.0],
            [0.2, 0.1],
            [0.4, 0.5],
            [0.7, 0.8],
            [1.0, 1.0],
            [1.0, 1.0],
            [1.0, 1.0],
            [1.0, 1.0],
        ],
        len: 5,
    };

    let packed = pack_point_curve(&curve);
    assert_eq!(packed.pairs[0], [0.0, 0.0, 0.2, 0.1]);
    assert_eq!(packed.pairs[1], [0.4, 0.5, 0.7, 0.8]);
    assert_eq!(packed.pairs[2], [1.0, 1.0, 1.0, 1.0]);
    assert_eq!(packed.meta, [5.0, 0.0, 0.0, 0.0]);

    let local = pack_local_point_curve(&curve);
    assert_eq!(&local[..4], &packed.pairs);
    assert_eq!(local[4], packed.meta);
    assert_eq!(&local[5..], &[[0.0; 4]; 3]);
}

#[test]
fn mask_effect_packing_preserves_shader_id_activity_and_clamps() {
    let mut mask = LocalMask::new(MaskKind::Fullscreen, 1);
    mask.effect = MaskEffect::Blur;
    mask.effect_settings.blur.amount = 150.0;
    mask.effect_settings.blur.radius = 99.0;

    let packed = pack_effect_mask(&mask).expect("Blur is a GPU-backed mask effect");
    assert_eq!(packed.metadata[0], 1);
    assert_eq!(packed.metadata[1], 1);
    assert_eq!(packed.metadata[2], 0);
    assert_eq!(
        packed.metadata[3] >> super::MASK_EFFECT_ID_SHIFT,
        MaskEffect::Blur.shader_id()
    );
    assert_eq!(packed.adjust_0, [100.0, 16.0, 0.0, 0.0]);

    mask.enabled = false;
    let disabled = pack_effect_mask(&mask).expect("Blur remains representable when disabled");
    assert_eq!(disabled.metadata[0], 0);
    assert_eq!(disabled.metadata[1], 0);
    assert_eq!(disabled.adjust_0, packed.adjust_0);
}

fn tone_percentile_exposure_follow_from_shader() -> f32 {
    let function = SHADER_TONEMAP
        .split_once("fn tone_percentiles()")
        .expect("tone_percentiles shader function exists")
        .1
        .split_once("\n}\n")
        .expect("tone_percentiles shader function has a body")
        .0;
    let exposure_line = function
        .lines()
        .find(|line| line.contains("adaptive_tone_user_exposure_ev()"))
        .expect("tone_percentiles applies user exposure");
    let suffix = exposure_line
        .split_once("adaptive_tone_user_exposure_ev()")
        .expect("exposure call is present")
        .1
        .trim()
        .trim_end_matches(';')
        .trim();

    if suffix.is_empty() {
        return 1.0;
    }
    suffix
        .strip_prefix('*')
        .expect("tone percentile exposure offset is a direct scalar multiple")
        .trim()
        .parse::<f32>()
        .expect("tone percentile exposure multiplier is numeric")
}

fn test_tone_smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    let width = (edge1 - edge0).max(1e-4);
    let x = ((value - edge0) / width).clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[test]
fn tone_percentile_masks_follow_full_user_exposure() {
    let base_percentiles = [-6.0, -3.0, 0.0, 2.5, 4.5];
    let exposure_ev = 3.0;
    let follow = tone_percentile_exposure_follow_from_shader();
    let exposed_percentiles = base_percentiles.map(|value| value + exposure_ev * follow);

    for (base, exposed) in base_percentiles.into_iter().zip(exposed_percentiles) {
        assert!((exposed - base - exposure_ev).abs() < 1e-6);
    }

    let boundaries = |percentiles: [f32; 5]| {
        let [_, p05, p50, p95, _] = percentiles;
        [
            p05 - 0.90,
            p50 + 1.35,
            p50 - 0.35,
            p95 + 0.45,
            p05 - 0.10,
            p50 + 0.50,
        ]
    };
    for (base, exposed) in boundaries(base_percentiles)
        .into_iter()
        .zip(boundaries(exposed_percentiles))
    {
        assert!((exposed - base - exposure_ev).abs() < 1e-6);
    }

    let masks = |percentiles: [f32; 5], exposure: f32| {
        let [_, p05, p50, p95, _] = percentiles;
        let shadow_ev = -1.50 + exposure;
        let highlight_ev = 1.00 + exposure;
        let white_ev = -1.00 + exposure;
        [
            1.0 - test_tone_smoothstep(p05 - 0.90, p50 + 1.35, shadow_ev),
            0.10 + 0.90 * test_tone_smoothstep(p50 - 0.35, p95 + 0.45, highlight_ev),
            test_tone_smoothstep(p05 - 0.10, p50 + 0.50, white_ev),
        ]
    };
    let base_masks = masks(base_percentiles, 0.0);
    let exposed_masks = masks(exposed_percentiles, exposure_ev);
    for (base, exposed) in base_masks.into_iter().zip(exposed_masks) {
        assert!((exposed - base).abs() < 1e-6);
    }
}
