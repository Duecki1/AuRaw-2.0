use super::{
    canonicalize_green_noise, color_grade_hue_turns, composite_inpaint_rgba16f,
    explicit_render_graph_contracts_are_contiguous, pack_local_point_curve,
    pack_view_color_options, processing_work_format, shader_highlight_method,
    shader_manager::ShaderManager, specialize_compute_workgroup_size, work_shader_source,
    ComputeWorkgroupSize, ProcessingQuality, COLOR_DENOISE_ENTRY_POINTS, SHADER_BAYER_RCD_P1,
    SHADER_BAYER_RCD_P2, SHADER_BAYER_RCD_P3, SHADER_BAYER_RCD_P4, SHADER_COLOR_DENOISE,
    SHADER_CREATIVE_EFFECTS, SHADER_DUAL_DEMOSAIC, SHADER_HIGHLIGHTS, SHADER_NOISE_CA_FINISH,
    SHADER_REGRESSION_SCENE, SHADER_SCENE_ADJUSTMENTS, SHADER_TONE_ANALYSIS, SHADER_VIEW_TRANSFORM,
    SHADER_XTRANS_DEMOSAIC, SHADER_XTRANS_FINISH,
};
use crate::pipeline::{CfaKind, HighlightReconstructionMethod, PointCurve};

fn gpu_resource_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .expect("GPU resource test lock poisoned")
}

fn shader_module_with_format(
    name: &str,
    source: &str,
    work_format: wgpu::TextureFormat,
) -> naga::Module {
    let mut manager = ShaderManager::new(work_format)
        .unwrap_or_else(|error| panic!("{name} module registry failed:\n{error:#}"));
    manager
        .compose_naga_module(source, "shader_composition_test.wgsl")
        .unwrap_or_else(|error| panic!("{name} did not compose:\n{error:#}"))
}

fn shader_module(name: &str, source: &str) -> naga::Module {
    shader_module_with_format(
        name,
        source,
        processing_work_format(ProcessingQuality::Preview),
    )
}

fn validated_shader_module_with_format(
    name: &str,
    source: &str,
    work_format: wgpu::TextureFormat,
) -> naga::Module {
    let module = shader_module_with_format(name, source, work_format);
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|error| panic!("{name} did not validate: {error}"));
    module
}

fn validated_shader_module(name: &str, source: &str) -> naga::Module {
    validated_shader_module_with_format(
        name,
        source,
        processing_work_format(ProcessingQuality::Preview),
    )
}

fn naga_name_matches(actual: &str, expected: &str) -> bool {
    actual == expected || actual.rsplit("::").next() == Some(expected)
}

fn unqualified_naga_name(name: &str) -> &str {
    name.rsplit("::").next().unwrap_or(name)
}

fn has_function(module: &naga::Module, function_name: &str) -> bool {
    module.functions.iter().any(|(_, function)| {
        function
            .name
            .as_deref()
            .is_some_and(|name| naga_name_matches(name, function_name))
    }) || module
        .entry_points
        .iter()
        .any(|entry| entry.name == function_name)
}

fn named_i32_constant(module: &naga::Module, constant_name: &str) -> i32 {
    let (_, constant) = module
        .constants
        .iter()
        .find(|(_, constant)| constant.name.as_deref() == Some(constant_name))
        .unwrap_or_else(|| panic!("missing WGSL constant {constant_name}"));
    match &module.global_expressions[constant.init] {
        naga::Expression::Literal(naga::Literal::I32(value)) => *value,
        expression => {
            panic!("WGSL constant {constant_name} is not an i32 literal: {expression:?}")
        }
    }
}

fn named_f32_constant(module: &naga::Module, constant_name: &str) -> f32 {
    let (_, constant) = module
        .constants
        .iter()
        .find(|(_, constant)| constant.name.as_deref() == Some(constant_name))
        .unwrap_or_else(|| panic!("missing WGSL constant {constant_name}"));
    match &module.global_expressions[constant.init] {
        naga::Expression::Literal(naga::Literal::F32(value)) => *value,
        expression => {
            panic!("WGSL constant {constant_name} is not an f32 literal: {expression:?}")
        }
    }
}

fn wgsl_struct_layout(
    module: &naga::Module,
    struct_name: &str,
) -> (u32, Vec<(String, u32)>) {
    let (_, ty) = module
        .types
        .iter()
        .find(|(_, ty)| ty.name.as_deref() == Some(struct_name))
        .unwrap_or_else(|| panic!("missing WGSL struct {struct_name}"));
    match &ty.inner {
        naga::TypeInner::Struct { members, span } => (
            *span,
            members
                .iter()
                .map(|member| {
                    (
                        member
                            .name
                            .clone()
                            .unwrap_or_else(|| "<anonymous>".to_owned()),
                        member.offset,
                    )
                })
                .collect(),
        ),
        other => panic!("WGSL type {struct_name} is not a struct: {other:?}"),
    }
}

fn wgsl_field_offset(layout: &[(String, u32)], field_name: &str) -> usize {
    layout
        .iter()
        .find(|(name, _)| name == field_name || name.strip_suffix("_field") == Some(field_name))
        .map(|(_, offset)| *offset as usize)
        .unwrap_or_else(|| panic!("missing WGSL field {field_name}"))
}

fn append_direct_call_names(module: &naga::Module, block: &naga::Block, calls: &mut Vec<String>) {
    for statement in block {
        match statement {
            naga::Statement::Block(block) => append_direct_call_names(module, block, calls),
            naga::Statement::If { accept, reject, .. } => {
                append_direct_call_names(module, accept, calls);
                append_direct_call_names(module, reject, calls);
            }
            naga::Statement::Switch { cases, .. } => {
                for case in cases {
                    append_direct_call_names(module, &case.body, calls);
                }
            }
            naga::Statement::Loop {
                body, continuing, ..
            } => {
                append_direct_call_names(module, body, calls);
                append_direct_call_names(module, continuing, calls);
            }
            naga::Statement::Call { function, .. } => {
                calls.push(
                    module.functions[*function]
                        .name
                        .as_deref()
                        .map(unqualified_naga_name)
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("<anonymous {:?}>", function)),
                );
            }
            _ => {}
        }
    }
}

fn entry_point_call_names(module: &naga::Module, entry_point: &str) -> Vec<String> {
    let entry = module
        .entry_points
        .iter()
        .find(|entry| entry.name == entry_point)
        .unwrap_or_else(|| panic!("missing WGSL entry point {entry_point}"));
    let mut calls = Vec::new();
    append_direct_call_names(module, &entry.function.body, &mut calls);
    calls
}

fn function_call_names(module: &naga::Module, function_name: &str) -> Vec<String> {
    let matching_functions: Vec<_> = module
        .functions
        .iter()
        .filter(|(_, function)| {
            function
                .name
                .as_deref()
                .is_some_and(|name| naga_name_matches(name, function_name))
        })
        .collect();
    assert!(
        !matching_functions.is_empty(),
        "missing WGSL function {function_name}"
    );

    // A virtual function and its override may both retain diagnostic names in
    // the composed IR. The concrete override has the meaningful call body.
    matching_functions
        .into_iter()
        .map(|(_, function)| {
            let mut calls = Vec::new();
            append_direct_call_names(module, &function.body, &mut calls);
            calls
        })
        .max_by_key(Vec::len)
        .unwrap_or_default()
}

fn function_name_count(module: &naga::Module, function_name: &str) -> usize {
    module
        .functions
        .iter()
        .filter(|(_, function)| {
            function
                .name
                .as_deref()
                .is_some_and(|name| naga_name_matches(name, function_name))
        })
        .count()
}

fn call_position(calls: &[String], name: &str) -> usize {
    calls
        .iter()
        .position(|call| call == name)
        .unwrap_or_else(|| panic!("missing WGSL call to {name}; calls were {calls:?}"))
}

#[test]
fn overlapping_soft_inpaint_patches_compose_in_stroke_order() {
    use half::f16;

    let mut destination = [
        f16::from_f32(0.2).to_bits(),
        f16::from_f32(0.4).to_bits(),
        f16::from_f32(0.6).to_bits(),
        f16::from_f32(0.5).to_bits(),
    ];
    composite_inpaint_rgba16f(&mut destination, [0.8, 0.2, 0.1], 0.25);
    let decoded = destination.map(|value| f16::from_bits(value).to_f32());
    assert!((decoded[3] - 0.625).abs() < 1e-3);
    let expected = [0.44, 0.32, 0.4];
    for channel in 0..3 {
        assert!((decoded[channel] - expected[channel]).abs() < 1e-3);
    }

    let same_rgb = [0.35, 0.45, 0.55];
    let mut opaque = [
        f16::from_f32(same_rgb[0]).to_bits(),
        f16::from_f32(same_rgb[1]).to_bits(),
        f16::from_f32(same_rgb[2]).to_bits(),
        f16::from_f32(1.0).to_bits(),
    ];
    composite_inpaint_rgba16f(&mut opaque, same_rgb, 0.2);
    let unchanged = opaque.map(|value| f16::from_bits(value).to_f32());
    assert!((unchanged[3] - 1.0).abs() < 1e-6);
    for channel in 0..3 {
        assert!((unchanged[channel] - same_rgb[channel]).abs() < 1e-3);
    }
}

#[test]
fn scene_graph_preserves_native_call_order_and_stage_ownership() {
    let scene_module = shader_module("scene adjustments", SHADER_SCENE_ADJUSTMENTS);
    let creative_module = shader_module("creative effects", SHADER_CREATIVE_EFFECTS);
    let view_module = shader_module("view transform", SHADER_VIEW_TRANSFORM);

    let prepare_calls = entry_point_call_names(&scene_module, "prepare_scene_node");
    assert!(
        call_position(&prepare_calls, "apply_camera_characterization")
            < call_position(&prepare_calls, "apply_exposure")
    );
    assert!(
        call_position(&prepare_calls, "apply_exposure")
            < call_position(&prepare_calls, "apply_local_exposure_nodes")
    );
    assert!(!prepare_calls
        .iter()
        .any(|call| call == "apply_optional_profile_look"));

    let tone_calls = entry_point_call_names(&scene_module, "apply_scene_tone_node");
    assert!(
        call_position(&tone_calls, "apply_capture_sharpening")
            < call_position(&tone_calls, "apply_lightroom_tone")
    );
    assert!(!tone_calls
        .iter()
        .any(|call| call == "apply_profile_view_tone"));

    let local_calls = entry_point_call_names(&scene_module, "apply_local_scene_tone_node");
    assert!(local_calls
        .iter()
        .any(|call| call == "apply_local_scene_tone_nodes"));

    let effects_calls = entry_point_call_names(&creative_module, "apply_scene_effects_node");
    assert!(!effects_calls
        .iter()
        .any(|call| call == "apply_capture_sharpening"));

    let view_calls = function_call_names(&view_module, "apply_view_transform");
    let look = call_position(&view_calls, "apply_optional_profile_look");
    assert!(look < call_position(&view_calls, "apply_sigmoid_view_transform"));
}

#[test]
fn noise_ca_virtual_declaration_stays_at_source_start() {
    assert!(
        SHADER_NOISE_CA_FINISH.starts_with("virtual fn finish_reference_at"),
        "naga_oil 0.22 requires this virtual declaration before any line comment"
    );
}

#[test]
fn generated_finish_shaders_define_each_shared_routine_once() {
    for (name, source) in [
        ("Bayer finish", SHADER_BAYER_RCD_P4),
        ("X-Trans finish", SHADER_XTRANS_FINISH),
    ] {
        let module = shader_module(name, source);
        for routine in [
            "finish_warped_pos",
            "finish_reference_bilinear",
            "finish_apply_ca",
            "finish_apply_sensor_denoise",
        ] {
            assert_eq!(
                function_name_count(&module, routine),
                1,
                "{name} must contain exactly one {routine} definition"
            );
        }
    }
}

#[test]
fn generated_finish_shaders_keep_cfa_specific_reference_adapters() {
    let bayer = validated_shader_module("Bayer finish", SHADER_BAYER_RCD_P4);
    let bayer_calls = function_call_names(&bayer, "finish_reference_at");
    assert!(bayer_calls.iter().any(|call| call == "clamp_pos"));
    assert!(bayer_calls.iter().any(|call| call == "rcd_reference_at"));
    assert!(!bayer_calls.iter().any(|call| call == "xt_high"));

    let xtrans = validated_shader_module("X-Trans finish", SHADER_XTRANS_FINISH);
    let xtrans_calls = function_call_names(&xtrans, "finish_reference_at");
    assert_eq!(xtrans_calls, vec!["xt_high".to_owned()]);
}

#[test]
fn compute_shaders_parse_and_validate() {
    for (name, source) in [
        ("highlight reconstruction", SHADER_HIGHLIGHTS),
        ("Bayer RCD pass 1", SHADER_BAYER_RCD_P1),
        ("Bayer RCD pass 2", SHADER_BAYER_RCD_P2),
        ("Bayer RCD pass 3", SHADER_BAYER_RCD_P3),
        ("Bayer RCD pass 4", SHADER_BAYER_RCD_P4),
        ("robust dual demosaic", SHADER_DUAL_DEMOSAIC),
        ("grouped X-Trans demosaic", SHADER_XTRANS_DEMOSAIC),
        ("X-Trans finish", SHADER_XTRANS_FINISH),
        ("multiscale color denoise", SHADER_COLOR_DENOISE),
        ("adaptive tone analysis", SHADER_TONE_ANALYSIS),
        ("regression scene export", SHADER_REGRESSION_SCENE),
        ("scene adjustments", SHADER_SCENE_ADJUSTMENTS),
        ("creative effects", SHADER_CREATIVE_EFFECTS),
        ("view transform", SHADER_VIEW_TRANSFORM),
    ] {
        validated_shader_module(name, source);
    }
}

#[test]
fn high_quality_shader_variants_parse_and_use_full_float_storage() {
    for (name, source) in [
        (
            "32-bit Bayer pass 1",
            work_shader_source(
                SHADER_BAYER_RCD_P1,
                processing_work_format(ProcessingQuality::High),
            )
            .expect("specialize high-quality shader"),
        ),
        (
            "32-bit robust dual demosaic",
            work_shader_source(
                SHADER_DUAL_DEMOSAIC,
                processing_work_format(ProcessingQuality::High),
            )
            .expect("specialize high-quality shader"),
        ),
        (
            "32-bit multiscale color denoise",
            work_shader_source(
                SHADER_COLOR_DENOISE,
                processing_work_format(ProcessingQuality::High),
            )
            .expect("specialize high-quality shader"),
        ),
        (
            "32-bit scene adjustments",
            work_shader_source(
                SHADER_SCENE_ADJUSTMENTS,
                processing_work_format(ProcessingQuality::High),
            )
            .expect("specialize high-quality shader"),
        ),
        (
            "32-bit creative effects",
            std::borrow::Cow::Borrowed(SHADER_CREATIVE_EFFECTS),
        ),
        (
            "32-bit view transform",
            std::borrow::Cow::Borrowed(SHADER_VIEW_TRANSFORM),
        ),
    ] {
        assert_eq!(processing_work_format(ProcessingQuality::High), wgpu::TextureFormat::Rgba32Float);
        validated_shader_module_with_format(
            name,
            source.as_ref(),
            processing_work_format(ProcessingQuality::High),
        );
    }
}

#[test]
fn shader_specialization_fails_closed_without_marker() {
    let error = work_shader_source(
        "@compute @workgroup_size(1) fn main() {}",
        wgpu::TextureFormat::Rgba32Float,
    )
    .expect_err("missing marker must be an error in release builds");
    assert!(error.to_string().contains("work-format marker"));
}

#[test]
fn compute_workgroup_specialization_and_dispatch_cover_partial_tiles() {
    let workgroup = ComputeWorkgroupSize::new(16, 8).expect("valid workgroup");
    assert_eq!(workgroup.dispatch_for_extent(257, 259), [17, 33, 1]);

    let specialized = specialize_compute_workgroup_size(SHADER_VIEW_TRANSFORM, workgroup);
    assert!(specialized.contains("@workgroup_size(16, 8, 1)"));
    assert!(specialized.contains(
        "if gid.x >= Common::camera_uniforms.width || gid.y >= Common::camera_uniforms.height { return; }"
    ));

    let mut manager = ShaderManager::new_with_workgroup_size(
        processing_work_format(ProcessingQuality::Preview),
        workgroup,
    )
    .expect("workgroup-aware shader manager");
    let module = manager
        .compose_naga_module(SHADER_VIEW_TRANSFORM, "view_transform.wgsl")
        .expect("specialized view transform composes");
    let entry = module
        .entry_points
        .iter()
        .find(|entry| entry.name == "apply_view_node")
        .expect("view entry point");
    assert_eq!(entry.workgroup_size, [16, 8, 1]);
}

#[test]
fn green_noise_is_averaged_once_and_stored_symmetrically() {
    let canonical = canonicalize_green_noise([1.0, 2.0, 3.0, 6.0], true);
    assert_eq!(canonical, [1.0, 4.0, 3.0, 4.0]);
    let unchanged = canonicalize_green_noise([1.0, 2.0, 3.0, 6.0], false);
    assert_eq!(unchanged, [1.0, 2.0, 3.0, 6.0]);
}

#[test]
fn unified_scene_display_contract_graph_is_contiguous() {
    assert!(explicit_render_graph_contracts_are_contiguous());
}

#[test]
fn highlight_shader_exposes_the_single_reconstruction_entry_point() {
    let module = validated_shader_module("highlight reconstruction", SHADER_HIGHLIGHTS);
    assert_eq!(module.entry_points.len(), 1);
    assert_eq!(module.entry_points[0].name, "highlight_reconstruct");
}

#[test]
fn inpaint_opposed_uses_darktable_clip_and_never_lowers_clipped_signal() {
    let module = validated_shader_module("highlight reconstruction", SHADER_HIGHLIGHTS);
    assert!(
        (named_f32_constant(&module, "DARKTABLE_OPPOSED_CLIP_MAGIC") - 0.987).abs() < 1e-6
    );
    let calls = function_call_names(&module, "inpaint_opposed_cfa_at");
    assert!(calls.iter().any(|call| call == "inpaint_opposed_refavg"));
}

#[test]
fn xtrans_never_dispatches_the_bayer_phase_lch_reconstruction() {
    assert_eq!(
        shader_highlight_method(CfaKind::Bayer, HighlightReconstructionMethod::Lch),
        HighlightReconstructionMethod::Lch.shader_value()
    );
    assert_eq!(
        shader_highlight_method(CfaKind::XTrans, HighlightReconstructionMethod::Lch),
        HighlightReconstructionMethod::InpaintOpposed.shader_value()
    );
    assert_eq!(
        shader_highlight_method(CfaKind::XTrans, HighlightReconstructionMethod::Off),
        HighlightReconstructionMethod::Off.shader_value()
    );
}

#[test]
fn grading_hues_match_the_visible_srgb_wheel_in_oklab() {
    let red = color_grade_hue_turns(0.0);
    let green = color_grade_hue_turns(120.0);
    let blue = color_grade_hue_turns(240.0);
    assert!((red - 0.081).abs() < 0.002);
    assert!((green - 0.396).abs() < 0.002);
    assert!((blue - 0.733).abs() < 0.002);
    assert!((color_grade_hue_turns(360.0) - red).abs() < f32::EPSILON);
}

#[test]
fn global_and_local_hue_share_the_reserved_color_options_lane() {
    let grading = crate::pipeline::ColorGrading::default();
    assert_eq!(pack_view_color_options(grading, 42.5)[2], 42.5);
    assert_eq!(pack_view_color_options(grading, 999.0)[2], 180.0);
    assert_eq!(pack_view_color_options(grading, -999.0)[2], -180.0);
}

#[test]
fn profile_highlight_shoulder_is_scene_adaptive_and_monotonic() {
    fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
        let x = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
        x * x * (3.0 - 2.0 * x)
    }

    fn knee_for_scene(p95_over_white_ev: f32, p995_over_white_ev: f32) -> f32 {
        let broad = smoothstep(-0.55, 1.25, p95_over_white_ev);
        let peak = smoothstep(0.0, 3.5, p995_over_white_ev);
        let gap = (p995_over_white_ev - p95_over_white_ev).max(0.0);
        let isolated = smoothstep(0.65, 3.0, gap);
        let peak_weight = peak * (1.0 + (0.38 - 1.0) * isolated);
        let pressure = (broad * 0.74 + peak_weight * 0.26).clamp(0.0, 1.0);
        0.91 + (0.62 - 0.91) * pressure
    }

    let low_headroom = knee_for_scene(-1.0, -0.2);
    let broad_highlights = knee_for_scene(0.8, 1.6);
    let isolated_specular = knee_for_scene(-0.2, 2.5);
    assert!(broad_highlights < low_headroom);
    assert!(isolated_specular > broad_highlights);
    assert!((0.62..=0.91).contains(&low_headroom));
    assert!((0.62..=0.91).contains(&broad_highlights));
    assert!((0.62..=0.91).contains(&isolated_specular));

    for knee in [low_headroom, broad_highlights, isolated_specular] {
        let map_luma = |luma: f32| {
            if luma <= knee {
                luma
            } else {
                let distance = luma - knee;
                knee + distance / (1.0 + distance / (1.0 - knee))
            }
        };
        let mut previous = map_luma(0.0);
        for step in 1..=10_000 {
            let current = map_luma(step as f32 / 1_000.0);
            assert!(current >= previous, "shoulder reversed at step {step}");
            assert!(current <= 1.0);
            previous = current;
        }
    }

}

#[test]
fn masked_contrast_has_protected_toe_midtones_and_shoulder() {
    fn contrast_ev(scene_ev: f32, amount: f32) -> f32 {
        let pivot_ev = 0.12;
        let relative_ev = scene_ev - pivot_ev;
        let toe_distance = (-relative_ev).max(0.0);
        let shoulder_distance = relative_ev.max(0.0);
        let toe_response = 1.0 - 2.0f32.powf(-toe_distance / 1.65);
        let shoulder_response = 1.0 - 2.0f32.powf(-shoulder_distance / 1.85);
        let toe_endpoint = if amount >= 0.0 { 5.80 } else { 1.70 };
        let shoulder_endpoint = if amount >= 0.0 { 0.85 } else { 0.95 };
        let shape = shoulder_response * shoulder_endpoint - toe_response * toe_endpoint;
        scene_ev + amount.clamp(-1.0, 1.0) * shape
    }

    for amount in [-1.0, 1.0] {
        let mut previous = contrast_ev(-16.0, amount);
        for step in 1..=28_000 {
            let scene_ev = -16.0 + step as f32 / 1_000.0;
            let current = contrast_ev(scene_ev, amount);
            assert!(current > previous, "contrast reversed at {scene_ev} EV");
            previous = current;
        }
    }

    // +100 is assertive around the midtones, but the finite tail displacement
    // protects deep shadows and highlights from runaway EV multiplication.
    assert!(contrast_ev(-1.0, 1.0) < -1.25);
    assert!(contrast_ev(1.0, 1.0) > 1.20);
    assert!(contrast_ev(-8.0, 1.0) > -14.0);
    assert!(contrast_ev(8.0, 1.0) < 9.0);
    assert_eq!(contrast_ev(0.12, 1.0), 0.12);

}

#[test]
fn basic_contrast_drives_sigmoid_without_switching_view_operators() {
    let mut raw = local_mask_scheduling_fixture(12, 12);
    raw.camera_profile.tone_curve = Some(
        crate::pipeline::ToneCurve::new(vec![[0.0, 0.0], [0.5, 0.62], [1.0, 1.0]])
            .expect("test DCP tone curve"),
    );
    let masks = crate::pipeline::MaskStack::default();
    let mut low = crate::pipeline::ExposureParams::default();
    low.contrast = -50.0;
    let mut high = crate::pipeline::ExposureParams::default();
    high.contrast = 50.0;

    let low_params = super::GpuParams::new(&low, &masks, &raw);
    let high_params = super::GpuParams::new(&high, &masks, &raw);
    assert_eq!(low_params.camera.ai_denoise_enabled, 0);
    assert_eq!(high_params.camera.ai_denoise_enabled, 0);
    assert_eq!(low_params.effects.presence[3], 0.0);
    assert_eq!(high_params.effects.presence[3], 0.0);
    assert!(low_params.scene_tone.sigmoid_power[0] < high_params.scene_tone.sigmoid_power[0]);
}

#[test]
fn lifted_black_curve_uses_continuous_luminance_remapping() {
    let module = validated_shader_module("view transform", SHADER_VIEW_TRANSFORM);
    assert!(has_function(&module, "remap_scene_luminance"));
    let calls = function_call_names(&module, "apply_point_tone_curve");
    assert!(calls.iter().any(|call| call == "remap_scene_luminance"));
}

#[test]
fn local_curves_preserve_exact_control_points() {
    let mut curve = PointCurve::linear();
    curve.len = 4;
    curve.points[1] = [0.500, 0.20];
    curve.points[2] = [0.505, 0.80];
    curve.points[3] = [1.0, 1.0];
    let packed = pack_local_point_curve(&curve);

    assert_eq!(packed[0], [0.0, 0.0, 0.500, 0.20]);
    assert_eq!(packed[1], [0.505, 0.80, 1.0, 1.0]);
    assert_eq!(packed[4][0], 4.0);
}

#[test]
fn demosaic_contracts_are_compiler_validated() {
    let bayer_finish = validated_shader_module("Bayer finish", SHADER_BAYER_RCD_P4);
    let xtrans = validated_shader_module("X-Trans demosaic", SHADER_XTRANS_DEMOSAIC);
    let xtrans_finish = validated_shader_module("X-Trans finish", SHADER_XTRANS_FINISH);
    let dual = validated_shader_module("dual demosaic", SHADER_DUAL_DEMOSAIC);

    assert_eq!(named_i32_constant(&bayer_finish, "RCD_MARGIN"), 9);
    assert_eq!(named_i32_constant(&xtrans, "MARKESTEIJN3_MARGIN"), 17);
    assert!(has_function(&bayer_finish, "ppg_rgb_at"));
    assert!(has_function(&bayer_finish, "frequency_chroma_at"));
    assert!(has_function(&bayer_finish, "gaussian5_weight"));
    assert!(has_function(&xtrans, "mark_candidate"));
    assert!(has_function(&xtrans, "mark_homo_sum5"));
    assert!(has_function(&xtrans_finish, "xt_frequency_uv"));
    assert!(has_function(&xtrans_finish, "xt_median5"));
    assert!(has_function(&xtrans_finish, "xt_gaussian5_weight"));

    for entry in ["dual_green_reconstruct", "dual_rgb_reconstruct"] {
        assert!(dual.entry_points.iter().any(|candidate| candidate.name == entry));
    }
    let bayer_output_calls = entry_point_call_names(&bayer_finish, "bayer_rcd_output");
    assert!(bayer_output_calls.iter().any(|call| call == "finish_reference_at"));
    let xtrans_output_calls = entry_point_call_names(&xtrans_finish, "xtrans_demosaic_finish");
    assert!(xtrans_output_calls.iter().any(|call| call == "finish_reference_at"));
}

#[test]
fn demosaic_shaders_expose_every_dispatched_entry_point() {
    for (source, expected) in [
        (SHADER_BAYER_RCD_P1, "bayer_rcd_directional"),
        (SHADER_BAYER_RCD_P2, "bayer_rcd_green"),
        (SHADER_BAYER_RCD_P3, "bayer_rcd_chroma"),
        (SHADER_BAYER_RCD_P4, "bayer_rcd_output"),
        (SHADER_XTRANS_DEMOSAIC, "xtrans_seed"),
        (SHADER_XTRANS_DEMOSAIC, "xtrans_markesteijn_pass1"),
        (SHADER_XTRANS_DEMOSAIC, "xtrans_markesteijn_pass3"),
        (SHADER_XTRANS_DEMOSAIC, "xtrans_markesteijn_pass2"),
        (SHADER_XTRANS_DEMOSAIC, "xtrans_markesteijn_derivatives"),
        (SHADER_XTRANS_DEMOSAIC, "xtrans_markesteijn_homogeneity"),
        (SHADER_XTRANS_DEMOSAIC, "xtrans_markesteijn_accumulate"),
        (SHADER_XTRANS_FINISH, "xtrans_demosaic_finish"),
    ] {
        let module = validated_shader_module(expected, source);
        assert!(
            module
                .entry_points
                .iter()
                .any(|entry| entry.name == expected),
            "demosaic shader is missing entry point {expected}"
        );
    }
}

#[test]
fn color_denoise_shader_exposes_every_dispatched_scale() {
    let module = validated_shader_module("multiscale color denoise", SHADER_COLOR_DENOISE);
    for expected in COLOR_DENOISE_ENTRY_POINTS {
        assert!(
            module
                .entry_points
                .iter()
                .any(|entry| entry.name == expected),
            "color-denoise shader is missing entry point {expected}"
        );
    }
}

#[test]
fn tone_analysis_shader_exposes_every_dispatched_entry_point() {
    let module = validated_shader_module("adaptive tone analysis", SHADER_TONE_ANALYSIS);

    for expected in [
        "tone_guide_prepare",
        "tone_guide_horizontal",
        "tone_guide_vertical",
        "tone_reduce_histogram",
    ] {
        assert!(
            module
                .entry_points
                .iter()
                .any(|entry| entry.name == expected),
            "tone-analysis shader is missing entry point {expected}"
        );
    }
}

#[test]
fn stage_uniforms_follow_the_wgsl_uniform_layout() {
    macro_rules! assert_offsets {
        ($layout:expr, $ty:ty, [$($field:ident),+ $(,)?]) => {
            $(
                assert_eq!(
                    wgsl_field_offset(&$layout, stringify!($field)),
                    std::mem::offset_of!($ty, $field),
                    "{}.{}",
                    stringify!($ty),
                    stringify!($field),
                );
            )+
        };
    }

    assert_eq!(std::mem::size_of::<super::CameraUniforms>(), 416);
    assert_eq!(std::mem::size_of::<super::SceneToneUniforms>(), 768);
    assert_eq!(std::mem::size_of::<super::EffectsUniforms>(), 208);
    assert_eq!(super::GPU_STAGE_UNIFORM_SIZE_BYTES, 1_392);
    // Persisted process metadata intentionally retains the previous monolithic
    // ABI marker even though live GPU bindings are now stage-specific.
    assert_eq!(super::GPU_PARAMS_ABI_SIZE_BYTES, 1_072);

    let common = validated_shader_module("stage uniform ABI", super::SHADER_COMMON_FOR_TESTS);

    let (camera_span, camera_layout) = wgsl_struct_layout(&common, "CameraUniforms");
    assert_eq!(
        camera_span as usize,
        std::mem::size_of::<super::CameraUniforms>()
    );
    assert_offsets!(
        camera_layout,
        super::CameraUniforms,
        [
            black_point,
            temperature,
            highlight_clip,
            chroma_denoise,
            ca_red,
            ca_blue,
            highlight_reconstruction,
            tone_analysis_scale,
            tone_guide_radius,
            demosaic_mode,
            dual_threshold,
            frequency_chroma,
            tint,
            _pad_0,
            _pad_1,
            _pad_2,
            highlight_options,
            noise_shot,
            noise_read,
            noise_options,
            wb,
            cam_to_srgb_0,
            cam_to_srgb_1,
            cam_to_srgb_2,
            inpaint_wb_0,
            inpaint_wb_1,
            inpaint_wb_2,
            black_levels,
            white_levels,
            width,
            height,
            tile_origin_x,
            tile_origin_y,
            full_width,
            full_height,
            abi_version,
            abi_size_bytes,
            tone_histogram_bounds,
            profile_hue_sat,
            profile_look,
            profile_tone,
            output_lut,
            profile_flags,
            ai_denoise_enabled,
            user_exposure_bits,
            _pad_camera_0,
            _pad_camera_1,
        ]
    );

    let (scene_span, scene_layout) = wgsl_struct_layout(&common, "SceneToneUniforms");
    assert_eq!(
        scene_span as usize,
        std::mem::size_of::<super::SceneToneUniforms>()
    );
    assert_offsets!(
        scene_layout,
        super::SceneToneUniforms,
        [
            exposure,
            saturation,
            vibrance,
            _pad_0,
            basic_tone,
            sigmoid_curve,
            sigmoid_power,
            tone_curve_0,
            tone_curve_1,
            tone_curve_2,
            tone_curve_3,
            tone_curve_meta,
            tone_curve_red_0,
            tone_curve_red_1,
            tone_curve_red_2,
            tone_curve_red_3,
            tone_curve_red_meta,
            tone_curve_green_0,
            tone_curve_green_1,
            tone_curve_green_2,
            tone_curve_green_3,
            tone_curve_green_meta,
            tone_curve_blue_0,
            tone_curve_blue_1,
            tone_curve_blue_2,
            tone_curve_blue_3,
            tone_curve_blue_meta,
            hsl_hue_0,
            hsl_hue_1,
            hsl_saturation_0,
            hsl_saturation_1,
            hsl_luminance_0,
            hsl_luminance_1,
            mask_counts,
            grade_shadows,
            grade_midtones,
            grade_highlights,
            grade_global,
            grade_options,
            rec2020_to_xyz,
            xyz_to_rec2020,
            xyz_to_bradford,
            bradford_to_xyz,
        ]
    );

    let (effects_span, effects_layout) = wgsl_struct_layout(&common, "EffectsUniforms");
    assert_eq!(
        effects_span as usize,
        std::mem::size_of::<super::EffectsUniforms>()
    );
    assert_offsets!(
        effects_layout,
        super::EffectsUniforms,
        [
            presence,
            creative_effects,
            vignette,
            vignette_options,
            vignette_frame,
            vignette_transform,
            vignette_dark_half_fit,
            vignette_dark_full_fit,
            vignette_light_half_fit,
            vignette_light_full_fit,
            capture_scale_sigma,
            capture_thresholds,
            capture_mask_coherence,
        ]
    );

    assert_eq!(std::mem::size_of::<super::MaskData>(), 752);
    let (mask_span, mask_layout) = wgsl_struct_layout(&common, "MaskData");
    assert_eq!(mask_span as usize, std::mem::size_of::<super::MaskData>());
    assert_offsets!(
        mask_layout,
        super::MaskData,
        [
            metadata,
            adjust_0,
            adjust_1,
            adjust_2,
            curves,
            grade_shadows,
            grade_midtones,
            grade_highlights,
            grade_global,
            grade_options,
            curves_red,
            curves_green,
            curves_blue,
            hsl_hue_0,
            hsl_hue_1,
            hsl_saturation_0,
            hsl_saturation_1,
            hsl_luminance_0,
            hsl_luminance_1,
        ]
    );
}

#[test]
fn shader_tuning_defaults_match_the_previous_wgsl_constants() {
    let tuning = super::GpuShaderTuning::default();

    assert_eq!(
        tuning.rec2020_to_xyz,
        [
            [0.6369580, 0.2627002, 0.0000000, 0.0],
            [0.1446169, 0.6779981, 0.0280727, 0.0],
            [0.1688809, 0.0593017, 1.0609851, 0.0],
        ]
    );
    assert_eq!(
        tuning.xyz_to_rec2020,
        [
            [1.7166512, -0.6666844, 0.0176399, 0.0],
            [-0.3556708, 1.6164812, -0.0427706, 0.0],
            [-0.2533663, 0.0157685, 0.9421031, 0.0],
        ]
    );
    assert_eq!(
        tuning.xyz_to_bradford,
        [
            [0.8951000, -0.7502000, 0.0389000, 0.0],
            [0.2664000, 1.7135000, -0.0685000, 0.0],
            [-0.1614000, 0.0367000, 1.0296000, 0.0],
        ]
    );
    assert_eq!(
        tuning.bradford_to_xyz,
        [
            [0.9869929, 0.4323053, -0.0085287, 0.0],
            [-0.1470543, 0.5183603, 0.0400428, 0.0],
            [0.1599627, 0.0492912, 0.9684867, 0.0],
        ]
    );
    assert_eq!(tuning.vignette_dark_half_fit, [0.10, 1.235, 2.88, 0.86]);
    assert_eq!(tuning.vignette_dark_full_fit, [0.02, 1.135, 3.46, 1.0]);
    assert_eq!(
        tuning.vignette_light_half_fit,
        [0.305, 1.24, 4.36, 0.90]
    );
    assert_eq!(
        tuning.vignette_light_full_fit,
        [0.13, 1.075, 5.66, 1.0]
    );
    assert_eq!(tuning.capture_scale_sigma, [0.74, 1.75, 0.58, 1.65]);
    assert_eq!(
        tuning.capture_thresholds,
        [0.015, 0.0045, 0.055, 0.28]
    );
    assert_eq!(
        tuning.capture_mask_coherence,
        [0.035, 0.62, 0.055, 0.22]
    );
}

#[test]
fn adjustments_shader_exposes_darktable_sigmoid_paths() {
    let module = validated_shader_module("view transform", SHADER_VIEW_TRANSFORM);
    for function in [
        "generalized_loglogistic_sigmoid",
        "preserve_hue_and_energy",
        "sigmoid_rgb_ratio",
    ] {
        assert!(has_function(&module, function), "missing WGSL function {function}");
    }
    let calls = function_call_names(&module, "apply_sigmoid_view_transform");
    assert!(calls.iter().any(|call| call == "sigmoid_rgb_ratio"));
}

#[test]
fn signed_scene_rgb_is_preserved_until_explicit_positive_domain_boundaries() {
    let module = validated_shader_module("view transform", SHADER_VIEW_TRANSFORM);
    for function in [
        "gamut_project_nonnegative",
        "gamut_project_unit",
        "perceptual_gamut_compress_unit_rec2020",
    ] {
        assert!(has_function(&module, function), "missing explicit gamut boundary {function}");
    }
    let calls = function_call_names(&module, "apply_explicit_view_node");
    assert!(calls.iter().any(|call| call == "gamut_project_nonnegative_rec2020"));

    for (name, source) in [
        ("Bayer pass 2", SHADER_BAYER_RCD_P2),
        ("Bayer pass 3", SHADER_BAYER_RCD_P3),
        ("Bayer finish", SHADER_BAYER_RCD_P4),
        ("X-Trans", SHADER_XTRANS_DEMOSAIC),
        ("X-Trans finish", SHADER_XTRANS_FINISH),
    ] {
        validated_shader_module(name, source);
    }
}

#[test]
fn adjustment_modules_expose_the_render_graph_controls() {
    let scene = validated_shader_module("scene adjustments", SHADER_SCENE_ADJUSTMENTS);
    let creative = validated_shader_module("creative effects", SHADER_CREATIVE_EFFECTS);
    let view = validated_shader_module("view transform", SHADER_VIEW_TRANSFORM);

    for function in ["apply_mask_contrast_value", "apply_point_tone_curve", "local_curve_tangent"] {
        assert!(has_function(&scene, function), "missing scene-control function {function}");
    }
    for function in ["apply_creative_effects", "apply_glow", "apply_vignette"] {
        assert!(
            has_function(&creative, function) || has_function(&view, function),
            "missing creative-control function {function}"
        );
    }
    for function in [
        "apply_hue_rotation_value",
        "apply_local_hue_rotations",
        "stabilized_mixer_sample",
        "mixer_luminance_ev",
        "apply_color_grading_wheels",
        "color_grade_tonal_weights",
        "apply_local_color_grading",
    ] {
        assert!(has_function(&view, function), "missing view-control function {function}");
    }
}

#[test]
fn profile_shader_parses_with_the_profile_storage_contract() {
    let module = validated_shader_module("view transform", SHADER_VIEW_TRANSFORM);
    for function in [
        "profile_map_sample",
        "apply_profile_hsv_map",
        "apply_profile_tone_curve",
        "apply_profile_view_tone",
    ] {
        assert!(has_function(&module, function), "missing profile function {function}");
    }
}

#[test]
fn global_wb_changes_raw_multipliers_without_changing_the_camera_transform() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("regression/raw/synthetic-bayer.dng");
    let raw = crate::pipeline::load_raw_file(&path).unwrap();
    let neutral = super::GpuParams::new(
        &crate::pipeline::ExposureParams::default(),
        &crate::pipeline::MaskStack::default(),
        &raw,
    );
    let adjusted = crate::pipeline::ExposureParams {
        temperature: 40.0,
        tint: 20.0,
        ..crate::pipeline::ExposureParams::default()
    };
    let changed = super::GpuParams::new(&adjusted, &crate::pipeline::MaskStack::default(), &raw);
    assert_ne!(neutral.camera.wb, changed.camera.wb);
    assert_eq!(neutral.camera.cam_to_srgb_0, changed.camera.cam_to_srgb_0);
    assert_eq!(neutral.camera.cam_to_srgb_1, changed.camera.cam_to_srgb_1);
    assert_eq!(neutral.camera.cam_to_srgb_2, changed.camera.cam_to_srgb_2);

    let tint_rendition = |tint| {
        let params = super::GpuParams::new(
            &crate::pipeline::ExposureParams {
                tint,
                ..Default::default()
            },
            &crate::pipeline::MaskStack::default(),
            &raw,
        );
        let wb = [
            params.camera.wb[0],
            0.5 * (params.camera.wb[1] + params.camera.wb[3]),
            params.camera.wb[2],
        ];
        [
            params.camera.cam_to_srgb_0,
            params.camera.cam_to_srgb_1,
            params.camera.cam_to_srgb_2,
        ]
        .map(|row| (0..3).map(|column| row[column] * wb[column]).sum::<f32>())
    };
    let lower_tint = tint_rendition(-20.0);
    let higher_tint = tint_rendition(20.0);
    let magenta_axis = |rgb: [f32; 3]| (rgb[0] + rgb[2]) * 0.5 - rgb[1];
    // darktable's control is an absolute Y divisor: larger displayed values
    // move toward green, while smaller displayed values move toward magenta.
    assert!(magenta_axis(higher_tint) < magenta_axis(lower_tint));
}

#[derive(Clone, Copy)]
enum LocalToneSchedulingCase {
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Temperature,
    Tint,
    Hue,
    Curves,
}

impl LocalToneSchedulingCase {
    fn label(self) -> &'static str {
        match self {
            Self::Contrast => "masked Contrast",
            Self::Highlights => "masked Highlights",
            Self::Shadows => "masked Shadows",
            Self::Whites => "masked Whites",
            Self::Temperature => "masked Temperature",
            Self::Tint => "masked Tint",
            Self::Hue => "masked Hue",
            Self::Curves => "masked Curves",
        }
    }

    fn apply(self, adjustments: &mut crate::pipeline::LocalAdjustments) {
        match self {
            Self::Contrast => adjustments.contrast = 70.0,
            Self::Highlights => adjustments.highlights = -75.0,
            Self::Shadows => adjustments.shadows = 75.0,
            Self::Whites => adjustments.whites = 70.0,
            Self::Temperature => adjustments.temperature = 70.0,
            Self::Tint => adjustments.tint = 70.0,
            Self::Hue => adjustments.hue = 120.0,
            Self::Curves => {
                let mut curve = PointCurve::linear();
                curve.points[1] = [0.42, 0.68];
                curve.points[2] = [1.0, 1.0];
                curve.len = 3;
                adjustments.tone_curve = curve;
            }
        }
    }

    fn needs_intermediate_pass(self) -> bool {
        !matches!(self, Self::Hue)
    }
}

struct LocalMaskSchedulingHarness {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: super::RawGpuPipeline,
    raw: super::LoadedRaw,
    exposure: super::ExposureParams,
}

impl LocalMaskSchedulingHarness {
    const WIDTH: u32 = 96;
    const HEIGHT: u32 = 64;
    const MASK_EDGE: u32 = 64;

    fn try_new() -> Option<Self> {
                use half::f16;

        let instance = wgpu::Instance::default();
        let adapter =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
                .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("auraw local-mask scheduling test device"),
            ..Default::default()
        }))
        .ok()?;

        let raw = local_mask_scheduling_fixture(Self::WIDTH, Self::HEIGHT);
        let exposure = super::ExposureParams {
            highlight_method: crate::pipeline::HighlightReconstructionMethod::Off,
            sharpen_amount: 0.0,
            ..super::ExposureParams::default()
        };
        let masks = local_mask_scheduling_stack(None);
        let params = super::GpuParams::new(&exposure, &masks, &raw);
        let pipeline = super::RawGpuPipeline::new_headless_with_quality_and_mask_edge(
            &device,
            &queue,
            &raw,
            &params,
            super::ProcessingQuality::High,
            Self::MASK_EDGE,
        )
        .unwrap_or_else(|error| panic!("local-mask GPU pipeline creation failed: {error:#}"));

        let mask = (0..Self::MASK_EDGE)
            .flat_map(|y| {
                let value =
                    f16::from_f32(if y < Self::MASK_EDGE / 2 { 1.0 } else { 0.0 }).to_bits();
                std::iter::repeat_n(value, Self::MASK_EDGE as usize)
            })
            .collect::<Vec<_>>();
        pipeline
            .update_mask_layer(&queue, 0, &mask)
            .expect("upload local-mask scheduling test layer");

        Some(Self {
            device,
            queue,
            pipeline,
            raw,
            exposure,
        })
    }

    fn render_preview(&self, params: &super::GpuParams) -> Vec<f32> {
        self.pipeline.dispatch_stage(
            &self.queue,
            &self.device,
            params,
            super::ProcessingStage::Raw,
        );
        self.pipeline.dispatch_stage(
            &self.queue,
            &self.device,
            params,
            super::ProcessingStage::Tone,
        );
        self.pipeline.dispatch_stage(
            &self.queue,
            &self.device,
            params,
            super::ProcessingStage::Output,
        );
        self.pipeline
            .read_display_linear_region_blocking(
                &self.device,
                &self.queue,
                0,
                0,
                Self::WIDTH,
                Self::HEIGHT,
            )
            .expect("read local-mask preview pixels")
    }

    fn render_export(&self, params: &super::GpuParams) -> Vec<f32> {
        self.pipeline
            .begin_export_tone_analysis(&self.queue, &self.device);
        self.pipeline
            .accumulate_export_tone_tile(&self.queue, &self.device, params);
        self.pipeline
            .finish_export_tone_analysis(&self.queue, &self.device);
        self.pipeline
            .dispatch_export_tile(&self.queue, &self.device, params);
        self.pipeline
            .read_display_linear_region_blocking(
                &self.device,
                &self.queue,
                0,
                0,
                Self::WIDTH,
                Self::HEIGHT,
            )
            .expect("read local-mask export pixels")
    }

    fn assert_case(&self, case: LocalToneSchedulingCase) {
        let empty_masks = crate::pipeline::MaskStack::default();
        let neutral_masks = local_mask_scheduling_stack(None);
        let adjusted_masks = local_mask_scheduling_stack(Some(case));
        let empty_params = super::GpuParams::new(&self.exposure, &empty_masks, &self.raw);
        let neutral_params = super::GpuParams::new(&self.exposure, &neutral_masks, &self.raw);
        let adjusted_params = super::GpuParams::new(&self.exposure, &adjusted_masks, &self.raw);

        assert!(
            !neutral_params.needs_intermediate_adjustment_passes(),
            "{} scheduled an intermediate pass at neutral",
            case.label()
        );
        assert_eq!(
            adjusted_params.needs_intermediate_adjustment_passes(),
            case.needs_intermediate_pass(),
            "{} used the wrong intermediate-pass plan",
            case.label(),
        );

        let preview_empty = self.render_preview(&empty_params);
        let preview_neutral = self.render_preview(&neutral_params);
        let preview_adjusted = self.render_preview(&adjusted_params);
        let export_empty = self.render_export(&empty_params);
        let export_neutral = self.render_export(&neutral_params);
        let export_adjusted = self.render_export(&adjusted_params);

        assert_max_delta(
            case.label(),
            "preview neutral mask",
            &preview_empty,
            &preview_neutral,
            3e-6,
        );
        assert_max_delta(
            case.label(),
            "export neutral mask",
            &export_empty,
            &export_neutral,
            3e-6,
        );
        assert_masked_pixels_change(
            case.label(),
            &preview_neutral,
            &preview_adjusted,
            Self::WIDTH,
            Self::HEIGHT,
        );
        assert_masked_pixels_change(
            case.label(),
            &export_neutral,
            &export_adjusted,
            Self::WIDTH,
            Self::HEIGHT,
        );
        assert_max_delta(
            case.label(),
            "preview/export neutral",
            &preview_neutral,
            &export_neutral,
            3e-5,
        );
        assert_max_delta(
            case.label(),
            "preview/export adjusted",
            &preview_adjusted,
            &export_adjusted,
            3e-5,
        );
    }

    fn assert_global_hue(&self) {
        let masks = crate::pipeline::MaskStack::default();
        let neutral_params = super::GpuParams::new(&self.exposure, &masks, &self.raw);
        let adjusted_exposure = super::ExposureParams {
            hue: 120.0,
            ..self.exposure
        };
        let adjusted_params = super::GpuParams::new(&adjusted_exposure, &masks, &self.raw);
        assert!(!adjusted_params.needs_intermediate_adjustment_passes());

        let preview_neutral = self.render_preview(&neutral_params);
        let preview_adjusted = self.render_preview(&adjusted_params);
        let export_neutral = self.render_export(&neutral_params);
        let export_adjusted = self.render_export(&adjusted_params);
        let maximum_delta = preview_neutral
            .iter()
            .zip(&preview_adjusted)
            .map(|(before, after)| (after - before).abs())
            .fold(0.0f32, f32::max);
        assert!(
            maximum_delta > 2e-4,
            "global Hue changed no pixels: max delta {maximum_delta}"
        );
        assert_max_delta(
            "global Hue",
            "preview/export adjusted",
            &preview_adjusted,
            &export_adjusted,
            3e-5,
        );
        assert_max_delta(
            "global Hue",
            "preview/export neutral",
            &preview_neutral,
            &export_neutral,
            3e-5,
        );
    }
}

fn local_mask_scheduling_fixture(width: u32, height: u32) -> super::LoadedRaw {
    let white = 4095.0f32;
    let mut raw_pixels = Vec::with_capacity((width * height) as usize);
    let mut color_indices = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let channel = match (x % 2, y % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
            color_indices.push(channel);
            let gradient = 0.035 + 0.90 * x as f32 / (width - 1) as f32;
            let vertical = 0.035 * (y as f32 / (height - 1) as f32 - 0.5);
            let texture = if (x / 3 + y / 3) % 2 == 0 {
                0.018
            } else {
                -0.018
            };
            let channel_scale = [1.04, 0.94, 0.82, 0.94][channel as usize];
            let value = (gradient + vertical + texture).clamp(0.0, 0.98) * channel_scale;
            raw_pixels.push((value.clamp(0.0, 1.0) * white).round() as u16);
        }
    }

    super::LoadedRaw {
        width,
        height,
        camera_make: "test".to_owned(),
        camera_model: "local-mask-scheduling".to_owned(),
        lens_make: String::new(),
        lens_model: String::new(),
        focal_length: 0.0,
        aperture: 0.0,
        focus_distance: 0.0,
        capture_metadata: Default::default(),
        cfa_kind: crate::pipeline::CfaKind::Bayer,
        raw_pixels,
        color_indices: crate::pipeline::CompactPixelMap::dense(width, height, color_indices),
        wb_coeffs: [1.0; 4],
        cam_to_srgb: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
        black_levels: [0.0; 4],
        black_levels_per_pixel: crate::pipeline::CompactPixelMap::dense(
            width,
            height,
            vec![0.0; (width * height) as usize],
        ),
        white_levels: [white; 4],
        noise_profile: crate::pipeline::NoiseProfile::default(),
        camera_profile: Default::default(),
        camera_profile_source: None,
        available_camera_profiles: Vec::new(),
        white_balance_model: None,
        lens_geometry: None,
        ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
        opposed_chroma_cache: Default::default(),
    }
}

fn local_mask_scheduling_stack(
    case: Option<LocalToneSchedulingCase>,
) -> crate::pipeline::MaskStack {
    let mut mask = crate::pipeline::LocalMask::new(crate::pipeline::MaskKind::Brush, 1);
    if let Some(case) = case {
        case.apply(&mut mask.adjustments);
    }
    crate::pipeline::MaskStack {
        masks: vec![mask],
        selected_mask: None,
        selected_component: None,
        subject_refinement: Default::default(),
    }
}

fn assert_max_delta(
    adjustment: &str,
    comparison: &str,
    left: &[f32],
    right: &[f32],
    tolerance: f32,
) {
    let max_delta = left
        .iter()
        .zip(right)
        .map(|(before, after)| (after - before).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_delta <= tolerance,
        "{adjustment} {comparison} diverged by {max_delta}, tolerance {tolerance}"
    );
}

fn assert_masked_pixels_change(
    adjustment: &str,
    neutral: &[f32],
    adjusted: &[f32],
    width: u32,
    height: u32,
) {
    let mut inside_max = 0.0f32;
    let mut outside_max = 0.0f32;
    for y in 4..height - 4 {
        for x in 4..width - 4 {
            let pixel = ((y * width + x) * 3) as usize;
            let delta = (0..3)
                .map(|channel| (adjusted[pixel + channel] - neutral[pixel + channel]).abs())
                .fold(0.0f32, f32::max);
            if y < height / 4 {
                inside_max = inside_max.max(delta);
            } else if y >= height * 3 / 4 {
                outside_max = outside_max.max(delta);
            }
        }
    }
    assert!(
        inside_max > 2e-4,
        "{adjustment} changed no masked pixels: max delta {inside_max}"
    );
    assert!(
        outside_max <= 3e-5,
        "{adjustment} leaked outside the mask: max delta {outside_max}"
    );
}

fn assert_local_tone_scheduling_case(case: LocalToneSchedulingCase) {
    use std::sync::{Mutex, OnceLock};

    let _gpu_guard = gpu_resource_test_guard();

    // This assertion remains active even on GPU-less CI and directly guards
    // the scheduler regression that originally made these controls silent.
    let raw = local_mask_scheduling_fixture(8, 8);
    let exposure = super::ExposureParams {
        highlight_method: crate::pipeline::HighlightReconstructionMethod::Off,
        sharpen_amount: 0.0,
        ..super::ExposureParams::default()
    };
    let neutral_params = super::GpuParams::new(&exposure, &local_mask_scheduling_stack(None), &raw);
    let adjusted_params =
        super::GpuParams::new(&exposure, &local_mask_scheduling_stack(Some(case)), &raw);
    assert!(
        !neutral_params.needs_intermediate_adjustment_passes(),
        "{} scheduled an intermediate pass at neutral",
        case.label()
    );
    assert_eq!(
        adjusted_params.needs_intermediate_adjustment_passes(),
        case.needs_intermediate_pass(),
        "{} used the wrong intermediate-pass plan",
        case.label(),
    );

    static HARNESS: OnceLock<Option<Mutex<LocalMaskSchedulingHarness>>> = OnceLock::new();
    let Some(harness) =
        HARNESS.get_or_init(|| LocalMaskSchedulingHarness::try_new().map(Mutex::new))
    else {
        // Headless CI is allowed to have no usable GPU. The scheduling
        // assertions still run when an adapter is available, matching the
        // repository's existing GPU behavior-test convention.
        return;
    };
    let harness = harness
        .lock()
        .expect("local-mask scheduling harness mutex poisoned");
    harness.assert_case(case);
    if matches!(case, LocalToneSchedulingCase::Hue) {
        harness.assert_global_hue();
    }
}

#[test]
fn masked_contrast_is_independently_scheduled_in_preview_and_export() {
    assert_local_tone_scheduling_case(LocalToneSchedulingCase::Contrast);
}

#[test]
fn masked_highlights_are_independently_scheduled_in_preview_and_export() {
    assert_local_tone_scheduling_case(LocalToneSchedulingCase::Highlights);
}

#[test]
fn masked_shadows_are_independently_scheduled_in_preview_and_export() {
    assert_local_tone_scheduling_case(LocalToneSchedulingCase::Shadows);
}

#[test]
fn masked_whites_are_independently_scheduled_in_preview_and_export() {
    assert_local_tone_scheduling_case(LocalToneSchedulingCase::Whites);
}

#[test]
fn masked_temperature_is_independently_scheduled_in_preview_and_export() {
    assert_local_tone_scheduling_case(LocalToneSchedulingCase::Temperature);
}

#[test]
fn masked_tint_is_independently_scheduled_in_preview_and_export() {
    assert_local_tone_scheduling_case(LocalToneSchedulingCase::Tint);
}

#[test]
fn global_and_masked_hue_run_in_the_view_pass_for_preview_and_export() {
    assert_local_tone_scheduling_case(LocalToneSchedulingCase::Hue);
}

#[test]
fn masked_curves_are_independently_scheduled_in_preview_and_export() {
    assert_local_tone_scheduling_case(LocalToneSchedulingCase::Curves);
}

#[test]
fn gpu_pipeline_renders_and_reads_scene_textures_when_an_adapter_exists() {
    let _gpu_guard = gpu_resource_test_guard();
    use super::{CfaKind, ExposureParams, LoadedRaw, ProcessingQuality, RawGpuPipeline};

    let instance = wgpu::Instance::default();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        // Headless CI runners are allowed to lack a usable GPU. The
        // parser/validator test above still covers all WGSL in that case.
        return;
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("auraw shader-layout test device"),
        ..Default::default()
    })) else {
        return;
    };

    let width = 12;
    let height = 12;
    let xtrans_pattern: [[u8; 6]; 6] = [
        [1, 2, 1, 1, 0, 1],
        [0, 1, 0, 2, 1, 2],
        [1, 2, 1, 1, 0, 1],
        [1, 0, 1, 1, 2, 1],
        [2, 1, 2, 0, 1, 0],
        [1, 0, 1, 1, 2, 1],
    ];

    for cfa_kind in [CfaKind::Bayer, CfaKind::XTrans] {
        let color_indices = (0..height)
            .flat_map(|y| {
                (0..width).map(move |x| match cfa_kind {
                    CfaKind::Bayer => match (x % 2, y % 2) {
                        (0, 0) => 0,
                        (1, 1) => 2,
                        _ => 1,
                    },
                    CfaKind::XTrans => xtrans_pattern[(y % 6) as usize][(x % 6) as usize],
                })
            })
            .collect();

        let raw = LoadedRaw {
            width,
            height,
            camera_make: "test".to_owned(),
            camera_model: "test".to_owned(),
            lens_make: String::new(),
            lens_model: String::new(),
            focal_length: 0.0,
            aperture: 0.0,
            focus_distance: 0.0,
            capture_metadata: Default::default(),
            cfa_kind,
            raw_pixels: vec![2048; (width * height) as usize],
            color_indices: crate::pipeline::CompactPixelMap::dense(width, height, color_indices),
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [
                [1.15, 0.08, 0.02, 0.0],
                [0.03, 0.87, 0.04, 0.0],
                [0.01, 0.06, 0.72, 0.0],
            ],
            black_levels: [0.0; 4],
            black_levels_per_pixel: crate::pipeline::CompactPixelMap::dense(
                width,
                height,
                vec![0.0; (width * height) as usize],
            ),
            white_levels: [4095.0; 4],
            noise_profile: crate::pipeline::NoiseProfile::default(),
            camera_profile: Default::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
            lens_geometry: None,
            ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
            opposed_chroma_cache: Default::default(),
        };
        let params = super::GpuParams::new(
            &ExposureParams::default(),
            &crate::pipeline::MaskStack::default(),
            &raw,
        );

        let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = RawGpuPipeline::new_headless_with_quality(
            &device,
            &queue,
            &raw,
            &params,
            ProcessingQuality::High,
        );
        let validation_error = pollster::block_on(validation_scope.pop());
        let pipeline = pipeline
            .unwrap_or_else(|error| panic!("{cfa_kind:?} GPU pipeline creation failed: {error:#}"));
        assert!(
            validation_error.is_none(),
            "{cfa_kind:?} wgpu layout/shader validation failed: {validation_error:?}"
        );

        let regression_validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let regression = pipeline.render_regression_scene_blocking(&device, &queue, &params);
        let regression_validation_error = pollster::block_on(regression_validation_scope.pop());
        let regression = regression.unwrap_or_else(|error| {
            panic!("{cfa_kind:?} regression scene render failed: {error:#}")
        });
        assert!(
            regression_validation_error.is_none(),
            "{cfa_kind:?} regression shader validation failed: {regression_validation_error:?}"
        );
        assert_eq!(regression.len(), (width * height * 3) as usize);
        assert!(regression.iter().all(|value| value.is_finite()));

        let inpaint_working = pipeline
            .render_inpaint_working_scene_blocking(&device, &queue, &params)
            .unwrap_or_else(|error| {
                panic!("{cfa_kind:?} inpaint working-scene render failed: {error:#}")
            });
        let resized_width = 6;
        let resized_height = 5;
        let resized_origin_x = 2;
        let resized_origin_y = 3;
        let resized_working = pipeline
            .render_inpaint_working_scene_region_resized_blocking(
                &device,
                &queue,
                &params,
                resized_origin_x,
                resized_origin_y,
                resized_width,
                resized_height,
                resized_width,
                resized_height,
            )
            .unwrap_or_else(|error| {
                panic!("{cfa_kind:?} resized inpaint working-scene render failed: {error:#}")
            });
        for y in 0..resized_height {
            for x in 0..resized_width {
                let full = (((y + resized_origin_y) * width + x + resized_origin_x) * 3) as usize;
                let resized = ((y * resized_width + x) * 3) as usize;
                for channel in 0..3 {
                    assert!(
                        (resized_working[resized + channel]
                            - inpaint_working[full + channel])
                            .abs()
                            < 1e-5,
                        "{cfa_kind:?} resized inpaint channel {channel} bypassed the camera-to-working transform"
                    );
                }
            }
        }

        let camera_scene = pipeline
            .read_scene_texture_blocking(&device, &queue)
            .unwrap_or_else(|error| {
                panic!("{cfa_kind:?} scene texture readback failed: {error:#}")
            });
        assert_eq!(camera_scene.len(), (width * height * 3) as usize);
        assert!(camera_scene.iter().all(|value| value.is_finite()));
        assert!(camera_scene
            .iter()
            .zip(&inpaint_working)
            .any(|(camera, working)| (camera - working).abs() > 1e-3));
    }
}

#[test]
fn reused_gpu_program_layouts_match_fresh_glow_for_bayer_and_xtrans() {
    let _gpu_guard = gpu_resource_test_guard();
    use super::{ExposureParams, ProcessingQuality, RawGpuPipeline};
    use crate::pipeline::{
        build_proxy, crop_raw, load_raw_file, HighlightReconstructionMethod, MaskStack, ProxySpec,
    };

    let instance = wgpu::Instance::default();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        // Headless CI runners are allowed to lack a usable GPU. Shader parsing
        // and static pass-plan tests still run on those hosts.
        return;
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("auraw reused-program layout test device"),
        ..Default::default()
    })) else {
        return;
    };

    let exposure = ExposureParams {
        highlight_method: HighlightReconstructionMethod::Off,
        texture: 35.0,
        clarity: 30.0,
        dehaze: 40.0,
        glow_amount: 80.0,
        glow_radius: 75.0,
        glow_threshold: 35.0,
        ..ExposureParams::default()
    };
    let masks = MaskStack::default();

    for fixture_name in ["synthetic-bayer.dng", "synthetic-xtrans.dng"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("regression/raw")
            .join(fixture_name);
        let full_raw = load_raw_file(&path)
            .unwrap_or_else(|error| panic!("load {fixture_name} for program reuse: {error:#}"));
        let template_raw = build_proxy(&full_raw, ProxySpec { max_edge: 128 });
        // Differing texture dimensions are the zoom/navigation reuse case that
        // can expose a stale pass-layout index after inserting a GPU pass.
        let target_raw = crop_raw(&full_raw, 12, 12, 192, 180);
        assert_ne!(
            (template_raw.width, template_raw.height),
            (target_raw.width, target_raw.height)
        );

        let template_params = super::GpuParams::new(&exposure, &masks, &template_raw);
        let target_params = super::GpuParams::new(&exposure, &masks, &target_raw);
        let validation_scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let template = RawGpuPipeline::new_headless_with_quality(
            &device,
            &queue,
            &template_raw,
            &template_params,
            ProcessingQuality::Preview,
        )
        .unwrap_or_else(|error| {
            panic!("{fixture_name} template pipeline creation failed: {error:#}")
        });

        let render = |pipeline: &RawGpuPipeline| {
            pipeline.recompute(&queue, &device, &target_params);
            pipeline
                .read_output_region_blocking(
                    &device,
                    &queue,
                    0,
                    0,
                    target_raw.width,
                    target_raw.height,
                )
                .unwrap_or_else(|error| {
                    panic!("{fixture_name} reused-program readback failed: {error:#}")
                })
        };

        let reused_output = {
            let pipeline = RawGpuPipeline::new_headless_reusing_programs(
                &device,
                &queue,
                &target_raw,
                &target_params,
                ProcessingQuality::Preview,
                &template,
            )
            .unwrap_or_else(|error| {
                panic!("{fixture_name} reused pipeline creation failed: {error:#}")
            });
            render(&pipeline)
        };
        let reduced_atlas_output = {
            let pipeline = RawGpuPipeline::new_headless_reusing_programs_with_mask_edge(
                &device,
                &queue,
                &target_raw,
                &target_params,
                ProcessingQuality::Preview,
                &template,
                128,
            )
            .unwrap_or_else(|error| {
                panic!("{fixture_name} reduced-atlas pipeline creation failed: {error:#}")
            });
            render(&pipeline)
        };
        let fresh_output = {
            let pipeline = RawGpuPipeline::new_headless_with_quality(
                &device,
                &queue,
                &target_raw,
                &target_params,
                ProcessingQuality::Preview,
            )
            .unwrap_or_else(|error| {
                panic!("{fixture_name} fresh pipeline creation failed: {error:#}")
            });
            render(&pipeline)
        };

        let validation_error = pollster::block_on(validation_scope.pop());
        assert!(
            validation_error.is_none(),
            "{fixture_name} reused-program validation failed: {validation_error:?}"
        );
        assert_eq!(
            reused_output, fresh_output,
            "{fixture_name} reused program layouts changed Glow output"
        );
        assert_eq!(
            reduced_atlas_output, fresh_output,
            "{fixture_name} reduced-atlas program layouts changed Glow output"
        );
    }
}

#[test]
fn presence_and_glow_have_real_gpu_behavior_when_an_adapter_exists() {
    let _gpu_guard = gpu_resource_test_guard();
    use super::{CfaKind, ExposureParams, LoadedRaw, ProcessingQuality, RawGpuPipeline};
    use crate::pipeline::{HighlightReconstructionMethod, MaskStack};

    fn fixture(width: u32, height: u32, signal: impl Fn(u32, u32) -> f32) -> LoadedRaw {
        let white = 4095.0f32;
        let mut raw_pixels = Vec::with_capacity((width * height) as usize);
        let mut color_indices = Vec::with_capacity((width * height) as usize);
        for y in 0..height {
            for x in 0..width {
                color_indices.push(match (x % 2, y % 2) {
                    (0, 0) => 0,
                    (1, 1) => 2,
                    _ => 1,
                });
                raw_pixels.push((signal(x, y).clamp(0.0, 1.0) * white).round() as u16);
            }
        }
        LoadedRaw {
            width,
            height,
            camera_make: "test".to_owned(),
            camera_model: "adjustment-behavior".to_owned(),
            lens_make: String::new(),
            lens_model: String::new(),
            focal_length: 0.0,
            aperture: 0.0,
            focus_distance: 0.0,
            capture_metadata: Default::default(),
            cfa_kind: CfaKind::Bayer,
            raw_pixels,
            color_indices: crate::pipeline::CompactPixelMap::dense(width, height, color_indices),
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            black_levels: [0.0; 4],
            black_levels_per_pixel: crate::pipeline::CompactPixelMap::dense(
                width,
                height,
                vec![0.0; (width * height) as usize],
            ),
            white_levels: [white; 4],
            noise_profile: crate::pipeline::NoiseProfile::default(),
            camera_profile: Default::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
            lens_geometry: None,
            ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
            opposed_chroma_cache: Default::default(),
        }
    }

    fn luma(pixel: &[f32]) -> f32 {
        0.262_700_2 * pixel[0] + 0.677_998_1 * pixel[1] + 0.059_301_7 * pixel[2]
    }

    fn mean_luma_in(
        pixels: &[f32],
        width: u32,
        height: u32,
        predicate: impl Fn(f32, f32) -> bool,
    ) -> f32 {
        let mut sum = 0.0;
        let mut count = 0u32;
        for y in 0..height {
            for x in 0..width {
                if predicate(x as f32, y as f32) {
                    let index = ((y * width + x) * 3) as usize;
                    sum += luma(&pixels[index..index + 3]);
                    count += 1;
                }
            }
        }
        sum / count.max(1) as f32
    }

    let instance = wgpu::Instance::default();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return;
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("auraw adjustment behavior test device"),
        ..Default::default()
    })) else {
        return;
    };

    const WIDTH: u32 = 128;
    const HEIGHT: u32 = 128;
    let masks = MaskStack::default();
    let neutral = ExposureParams {
        highlight_method: HighlightReconstructionMethod::Off,
        ..ExposureParams::default()
    };
    let flat = fixture(WIDTH, HEIGHT, |_, _| 0.30);
    let initial_params = super::GpuParams::new(&neutral, &masks, &flat);
    let pipeline = RawGpuPipeline::new_headless_with_quality(
        &device,
        &queue,
        &flat,
        &initial_params,
        ProcessingQuality::High,
    )
    .unwrap();

    let render = |raw: &LoadedRaw, exposure: &ExposureParams| {
        pipeline.upload_raw_tile(&queue, raw).unwrap();
        let params = super::GpuParams::new(exposure, &masks, raw);
        pipeline.recompute(&queue, &device, &params);
        pipeline
            .read_display_linear_region_blocking(&device, &queue, 0, 0, WIDTH, HEIGHT)
            .unwrap()
    };

    let render_camera = |raw: &LoadedRaw, exposure: &ExposureParams| {
        pipeline.upload_raw_tile(&queue, raw).unwrap();
        let params = super::GpuParams::new(exposure, &masks, raw);
        pipeline.recompute(&queue, &device, &params);
        pipeline
            .read_scene_texture_blocking(&device, &queue)
            .unwrap()
    };

    // Band-pass presence controls must be exact no-ops on a flat field. This
    // catches accidental global exposure offsets in Texture/Clarity.
    let flat_neutral = render(&flat, &neutral);
    let flat_presence = render(
        &flat,
        &ExposureParams {
            texture: 100.0,
            clarity: 100.0,
            ..neutral
        },
    );
    let flat_max_delta = flat_neutral
        .iter()
        .zip(&flat_presence)
        .map(|(before, after)| (after - before).abs())
        .fold(0.0f32, f32::max);
    assert!(
        flat_max_delta <= 2e-5,
        "flat Texture/Clarity changed pixels by {flat_max_delta}"
    );

    // Profiled color denoise must reduce camera-space opponent noise
    // monotonically without changing the green-weighted camera signal. A
    // bright coherent color patch must survive even at Color 100.
    let mut chroma_noise = fixture(WIDTH, HEIGHT, |_, _| 0.12);
    chroma_noise.noise_profile = crate::pipeline::NoiseProfile {
        shot: [0.0; 4],
        read: [0.0001; 4],
        confidence: 1.0,
        green2_present: true,
    };
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = (y * WIDTH + x) as usize;
            let hash = x
                .wrapping_mul(0x9e37_79b9)
                .wrapping_add(y.wrapping_mul(0x85eb_ca6b))
                .wrapping_add((index as u32).rotate_left(13));
            let unit = (hash & 0xffff) as f32 / 65_535.0;
            let signal = 0.12 + 0.026 * (2.0 * unit - 1.0);
            chroma_noise.raw_pixels[index] = (signal * 4095.0).round() as u16;
        }
    }
    let color_exposure = |amount| ExposureParams {
        chroma_denoise: amount,
        luminance_denoise: 0.0,
        denoise_detail: 50.0,
        denoise_quality: crate::pipeline::DenoiseQuality::High,
        sharpen_amount: 0.0,
        ..neutral
    };
    let color_off = render_camera(&chroma_noise, &color_exposure(0.0));
    let color_25 = render_camera(&chroma_noise, &color_exposure(0.25));
    let color_100 = render_camera(&chroma_noise, &color_exposure(1.0));
    let opponent_noise = |pixels: &[f32]| {
        let mut sum = 0.0f64;
        let mut count = 0u64;
        for y in 32..HEIGHT - 32 {
            for x in 32..WIDTH - 33 {
                let left = ((y * WIDTH + x) * 3) as usize;
                let right = left + 3;
                let opponents = |index: usize| {
                    let r = pixels[index];
                    let g = pixels[index + 1];
                    let b = pixels[index + 2];
                    [0.5 * (r - b), 0.25 * r - 0.5 * g + 0.25 * b]
                };
                let a = opponents(left);
                let b = opponents(right);
                sum += f64::from((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2));
                count += 1;
            }
        }
        (sum / count as f64).sqrt() as f32
    };
    let off_noise = opponent_noise(&color_off);
    let color_25_noise = opponent_noise(&color_25);
    let color_100_noise = opponent_noise(&color_100);
    assert!(
        color_25_noise < 0.45 * off_noise,
        "Color 25 did not remove enough opponent noise: {off_noise} -> {color_25_noise}"
    );
    assert!(
        color_100_noise <= 1.02 * color_25_noise,
        "Color 100 increased opponent noise: {color_25_noise} -> {color_100_noise}"
    );
    for (label, denoised) in [("Color 25", &color_25), ("Color 100", &color_100)] {
        let maximum_signal_delta = color_off
            .chunks_exact(3)
            .zip(denoised.chunks_exact(3))
            .map(|(before, after)| {
                let signal = |pixel: &[f32]| 0.25 * pixel[0] + 0.5 * pixel[1] + 0.25 * pixel[2];
                (signal(before) - signal(after)).abs()
            })
            .fold(0.0f32, f32::max);
        assert!(
            maximum_signal_delta < 2e-5,
            "{label} changed camera signal by {maximum_signal_delta}"
        );
    }

    let mut colored_feature = chroma_noise.clone();
    for y in 48..80 {
        for x in 48..80 {
            let index = (y * WIDTH + x) as usize;
            let channel = match (x % 2, y % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
            let signal = [0.65f32, 0.12, 0.04][channel];
            colored_feature.raw_pixels[index] = (signal * 4095.0).round() as u16;
        }
    }
    let feature_off = render_camera(&colored_feature, &color_exposure(0.0));
    let feature_100 = render_camera(&colored_feature, &color_exposure(1.0));
    let feature_chroma = |pixels: &[f32]| {
        let mut sum = 0.0;
        let mut count = 0u32;
        for y in 56..72 {
            for x in 56..72 {
                let index = ((y * WIDTH + x) * 3) as usize;
                let r = pixels[index];
                let g = pixels[index + 1];
                let b = pixels[index + 2];
                sum += (0.5 * (r - b)).hypot(0.25 * r - 0.5 * g + 0.25 * b);
                count += 1;
            }
        }
        sum / count as f32
    };
    let feature_chroma_off = feature_chroma(&feature_off);
    let feature_chroma_100 = feature_chroma(&feature_100);
    assert!(
        feature_chroma_100 >= 0.90 * feature_chroma_off,
        "Color 100 desaturated a coherent color feature: {feature_chroma_off} -> {feature_chroma_100}"
    );
    let mean_opponents_in = |pixels: &[f32], x_range: std::ops::Range<u32>| {
        let mut sum = [0.0f32; 2];
        let mut count = 0u32;
        for y in 56..72 {
            for x in x_range.clone() {
                let index = ((y * WIDTH + x) * 3) as usize;
                let r = pixels[index];
                let g = pixels[index + 1];
                let b = pixels[index + 2];
                sum[0] += 0.5 * (r - b);
                sum[1] += 0.25 * r - 0.5 * g + 0.25 * b;
                count += 1;
            }
        }
        [sum[0] / count as f32, sum[1] / count as f32]
    };
    // The colored patch is x=48..80. Its chroma must not diffuse through an
    // equal-signal boundary into the neutral strip immediately to its left.
    // Compare against Color Off so demosaic's own one-pixel reconstruction
    // footprint is not attributed to the multiscale denoiser.
    let neutral_off = mean_opponents_in(&feature_off, 36..48);
    let neutral_100 = mean_opponents_in(&feature_100, 36..48);
    let neutral_chroma_shift =
        (neutral_100[0] - neutral_off[0]).hypot(neutral_100[1] - neutral_off[1]);
    assert!(
        neutral_chroma_shift <= 0.003,
        "Color 100 bled patch chroma into a neutral equal-signal neighbor: {neutral_chroma_shift}"
    );

    // Dark coherent colors need the same protection even when they are not
    // saturated or bright enough for the highlight-oriented feature guard.
    // A five-sigma opponent boundary is still real image structure; broad
    // denoise scales must neither desaturate it nor tint its neutral neighbor.
    let mut subtle_feature = fixture(WIDTH, HEIGHT, |_, _| 0.06);
    subtle_feature.noise_profile = crate::pipeline::NoiseProfile {
        shot: [0.0; 4],
        read: [0.000025; 4],
        confidence: 1.0,
        green2_present: true,
    };
    for y in 48..80 {
        for x in 48..80 {
            let index = (y * WIDTH + x) as usize;
            let channel = match (x % 2, y % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
            let signal = [0.085f32, 0.06, 0.035][channel];
            subtle_feature.raw_pixels[index] = (signal * 4095.0).round() as u16;
        }
    }
    let subtle_off = render_camera(&subtle_feature, &color_exposure(0.0));
    let subtle_100 = render_camera(&subtle_feature, &color_exposure(1.0));
    let subtle_inside_off = mean_opponents_in(&subtle_off, 56..72);
    let subtle_inside_100 = mean_opponents_in(&subtle_100, 56..72);
    let subtle_chroma_off = subtle_inside_off[0].hypot(subtle_inside_off[1]);
    let subtle_chroma_100 = subtle_inside_100[0].hypot(subtle_inside_100[1]);
    assert!(
        subtle_chroma_100 >= 0.95 * subtle_chroma_off,
        "Color 100 desaturated a coherent dark color: {subtle_chroma_off} -> {subtle_chroma_100}"
    );
    let subtle_neutral_off = mean_opponents_in(&subtle_off, 36..48);
    let subtle_neutral_100 = mean_opponents_in(&subtle_100, 36..48);
    let subtle_neutral_shift = (subtle_neutral_100[0] - subtle_neutral_off[0])
        .hypot(subtle_neutral_100[1] - subtle_neutral_off[1]);
    assert!(
        subtle_neutral_shift <= 0.002,
        "Color 100 bled dark coherent chroma into a neutral neighbor: {subtle_neutral_shift}"
    );

    // Real fine and medium detail must move visibly; source-string wiring
    // checks cannot detect a skipped intermediate effects pass.
    let detailed = fixture(WIDTH, HEIGHT, |x, y| {
        let gradient = 0.16 + 0.34 * x as f32 / (WIDTH - 1) as f32;
        let checker = if (x / 2 + y / 2) % 2 == 0 {
            0.026
        } else {
            -0.026
        };
        let dx = x as f32 - 64.0;
        let dy = y as f32 - 64.0;
        gradient + checker - 0.07 * (1.0 - (dx * dx + dy * dy).sqrt() / 42.0).clamp(0.0, 1.0)
    });
    let detail_neutral = render(&detailed, &neutral);
    for (label, exposure) in [
        (
            "Texture",
            ExposureParams {
                texture: 70.0,
                ..neutral
            },
        ),
        (
            "Clarity",
            ExposureParams {
                clarity: 70.0,
                ..neutral
            },
        ),
    ] {
        let adjusted = render(&detailed, &exposure);
        let mean_delta = detail_neutral
            .iter()
            .zip(adjusted)
            .map(|(before, after)| (after - before).abs())
            .sum::<f32>()
            / detail_neutral.len() as f32;
        assert!(
            mean_delta > 1e-4,
            "{label} is effectively a no-op: {mean_delta}"
        );
    }

    // Positive Dehaze must expand a low-contrast veil/object separation while
    // keeping the scene finite and non-negative.
    let hazy = fixture(WIDTH, HEIGHT, |x, y| {
        let base = 0.48 + 0.05 * x as f32 / (WIDTH - 1) as f32;
        let object = (38..90).contains(&x) && (38..90).contains(&y);
        base - if object { 0.10 } else { 0.0 }
    });
    let haze_neutral = render(&hazy, &neutral);
    let dehazed = render(
        &hazy,
        &ExposureParams {
            dehaze: 70.0,
            ..neutral
        },
    );
    assert!(dehazed
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0));
    let object = |pixels: &[f32]| {
        mean_luma_in(pixels, WIDTH, HEIGHT, |x, y| {
            (50.0..78.0).contains(&x) && (50.0..78.0).contains(&y)
        })
    };
    let background = |pixels: &[f32]| {
        mean_luma_in(pixels, WIDTH, HEIGHT, |x, y| {
            (8.0..28.0).contains(&x) && (48.0..80.0).contains(&y)
        })
    };
    let neutral_separation = (background(&haze_neutral) - object(&haze_neutral)).abs();
    let dehazed_separation = (background(&dehazed) - object(&dehazed)).abs();
    assert!(
        dehazed_separation > neutral_separation * 1.05,
        "Dehaze did not expand contrast: {neutral_separation} -> {dehazed_separation}"
    );

    // Glow should create a smooth halo outside a compact bright source while
    // leaving remote shadows essentially unchanged.
    let glow_source = fixture(WIDTH, HEIGHT, |x, y| {
        let dx = x as f32 - 64.0;
        let dy = y as f32 - 64.0;
        if dx * dx + dy * dy <= 25.0 {
            1.0
        } else {
            0.02
        }
    });
    let glow_neutral = render(&glow_source, &neutral);
    let glowed = render(
        &glow_source,
        &ExposureParams {
            glow_amount: 85.0,
            glow_radius: 80.0,
            glow_threshold: 40.0,
            ..neutral
        },
    );
    assert!(glowed
        .iter()
        .all(|value| value.is_finite() && *value >= 0.0));
    let ring = |pixels: &[f32]| {
        mean_luma_in(pixels, WIDTH, HEIGHT, |x, y| {
            let dx = x - 64.0;
            let dy = y - 64.0;
            let radius_squared = dx * dx + dy * dy;
            (100.0..484.0).contains(&radius_squared)
        })
    };
    let far = |pixels: &[f32]| {
        mean_luma_in(pixels, WIDTH, HEIGHT, |x, y| {
            let dx = x - 64.0;
            let dy = y - 64.0;
            dx * dx + dy * dy > 2_500.0
        })
    };
    let ring_lift = ring(&glowed) - ring(&glow_neutral);
    let far_lift = (far(&glowed) - far(&glow_neutral)).abs();
    assert!(ring_lift > 1e-4, "Glow produced no halo: {ring_lift}");
    assert!(
        far_lift < ring_lift * 0.12 + 2e-5,
        "Glow lifted remote shadows: ring={ring_lift}, far={far_lift}"
    );
}

#[test]
fn inpaint_opposed_keeps_large_clipped_highlights_finite() {
    let _gpu_guard = gpu_resource_test_guard();
    use super::{CfaKind, ExposureParams, LoadedRaw, ProcessingQuality, RawGpuPipeline};

    let instance = wgpu::Instance::default();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return;
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("auraw clipped-highlight test device"),
        ..Default::default()
    })) else {
        return;
    };

    let width = 128u32;
    let height = 128u32;
    let white = 4095.0f32;
    let wb = [2.0f32, 1.0, 1.5, 1.0];
    let mut color_indices = Vec::with_capacity((width * height) as usize);
    let mut raw_pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let channel = match (x % 2, y % 2) {
                (0, 0) => 0,
                (1, 1) => 2,
                _ => 1,
            };
            color_indices.push(channel);

            // A broad neutral light has a fully saturated core and a
            // smooth, valid neutral shoulder. A real camera records this
            // as unequal channel plateaus after white balance.
            let dx = x as f32 - 63.5;
            let dy = y as f32 - 63.5;
            let radius = (dx * dx + dy * dy).sqrt();
            let scene = if radius <= 34.0 {
                2.6
            } else if radius < 50.0 {
                0.2 + (2.6 - 0.2) * (50.0 - radius) / 16.0
            } else {
                0.2
            };
            let sensor = (scene / wb[channel as usize]).clamp(0.0, 1.0);
            raw_pixels.push((sensor * white).round() as u16);
        }
    }

    let raw = LoadedRaw {
        width,
        height,
        camera_make: "test".to_owned(),
        camera_model: "clipped-neutral".to_owned(),
        lens_make: String::new(),
        lens_model: String::new(),
        focal_length: 0.0,
        aperture: 0.0,
        focus_distance: 0.0,
        capture_metadata: Default::default(),
        cfa_kind: CfaKind::Bayer,
        raw_pixels,
        color_indices: crate::pipeline::CompactPixelMap::dense(width, height, color_indices),
        wb_coeffs: wb,
        cam_to_srgb: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
        black_levels: [0.0; 4],
        black_levels_per_pixel: crate::pipeline::CompactPixelMap::dense(
            width,
            height,
            vec![0.0; (width * height) as usize],
        ),
        white_levels: [white; 4],
        noise_profile: crate::pipeline::NoiseProfile::default(),
        camera_profile: Default::default(),
        camera_profile_source: None,
        available_camera_profiles: Vec::new(),
        white_balance_model: None,
        lens_geometry: None,
        ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
        opposed_chroma_cache: Default::default(),
    };
    let exposure = ExposureParams::default();
    let params = super::GpuParams::new(&exposure, &crate::pipeline::MaskStack::default(), &raw);
    let pipeline = RawGpuPipeline::new_headless_with_quality(
        &device,
        &queue,
        &raw,
        &params,
        ProcessingQuality::High,
    )
    .unwrap();
    pipeline.dispatch_stage(
        &queue,
        &device,
        &params,
        crate::pipeline::ProcessingStage::Raw,
    );
    let scene = pipeline
        .read_scene_texture_blocking(&device, &queue)
        .unwrap();

    let mut mean = [0.0f32; 3];
    let mut count = 0.0f32;
    for y in 60..68 {
        for x in 60..68 {
            let index = ((y * width + x) * 3) as usize;
            for channel in 0..3 {
                mean[channel] += scene[index + channel];
            }
            count += 1.0;
        }
    }
    for value in &mut mean {
        *value /= count;
    }
    let opposed_green = 0.5 * (2.0f32.cbrt() + 1.5f32.cbrt());
    let expected = [2.0, opposed_green * opposed_green * opposed_green, 1.5];
    assert!(mean.iter().all(|value| value.is_finite() && *value >= 0.0));
    for channel in 0..3 {
        assert!(
            (mean[channel] - expected[channel]).abs() < 0.02,
            "channel {channel} does not match opposed reconstruction: rgb={mean:?}, expected={expected:?}"
        );
    }
}

#[test]
fn inpaint_opposed_recovers_selectively_clipped_highlights() {
    let _gpu_guard = gpu_resource_test_guard();
    use super::{CfaKind, ExposureParams, LoadedRaw, ProcessingQuality, RawGpuPipeline};

    fn fixture(cfa_kind: CfaKind, scene_scale: f32, coloured_peak: Option<[f32; 3]>) -> LoadedRaw {
        const WIDTH: u32 = 96;
        const HEIGHT: u32 = 96;
        const WHITE: f32 = 4095.0;
        const WB: [f32; 4] = [2.0, 1.0, 1.5, 1.0];
        const XTRANS: [[u8; 6]; 6] = [
            [1, 2, 1, 1, 0, 1],
            [0, 1, 0, 2, 1, 2],
            [1, 2, 1, 1, 0, 1],
            [1, 0, 1, 1, 2, 1],
            [2, 1, 2, 0, 1, 0],
            [1, 0, 1, 1, 2, 1],
        ];

        let mut color_indices = Vec::with_capacity((WIDTH * HEIGHT) as usize);
        let mut raw_pixels = Vec::with_capacity((WIDTH * HEIGHT) as usize);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let channel = match cfa_kind {
                    CfaKind::Bayer => match (x % 2, y % 2) {
                        (0, 0) => 0,
                        (1, 1) => 2,
                        _ => 1,
                    },
                    CfaKind::XTrans => XTRANS[(y % 6) as usize][(x % 6) as usize],
                };
                color_indices.push(channel);

                let dx = x as f32 - 48.0;
                let dy = y as f32 - 48.0;
                let radius = (dx * dx + dy * dy).sqrt();
                let shoulder = (1.0 - radius / 24.0).clamp(0.0, 1.0).powf(1.7);
                let neutral_scene = scene_scale * (0.55 + 1.30 * shoulder);
                let scene = coloured_peak.map_or(neutral_scene, |peak| {
                    scene_scale * (0.12 + (peak[channel as usize] - 0.12) * shoulder)
                });
                let sensor = (scene / WB[channel as usize]).clamp(0.0, 1.0);
                raw_pixels.push((sensor * WHITE).round() as u16);
            }
        }

        LoadedRaw {
            width: WIDTH,
            height: HEIGHT,
            camera_make: "test".to_owned(),
            camera_model: "selective-neutral-clipping".to_owned(),
            lens_make: String::new(),
            lens_model: String::new(),
            focal_length: 0.0,
            aperture: 0.0,
            focus_distance: 0.0,
            capture_metadata: Default::default(),
            cfa_kind,
            raw_pixels,
            color_indices: crate::pipeline::CompactPixelMap::dense(WIDTH, HEIGHT, color_indices),
            wb_coeffs: WB,
            cam_to_srgb: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            black_levels: [0.0; 4],
            black_levels_per_pixel: crate::pipeline::CompactPixelMap::dense(
                WIDTH,
                HEIGHT,
                vec![0.0; (WIDTH * HEIGHT) as usize],
            ),
            white_levels: [WHITE; 4],
            noise_profile: crate::pipeline::NoiseProfile::default(),
            camera_profile: Default::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
            lens_geometry: None,
            ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
            opposed_chroma_cache: Default::default(),
        }
    }

    fn percentile(mut values: Vec<f32>, percent: usize) -> f32 {
        values.sort_by(f32::total_cmp);
        values[(values.len() - 1) * percent / 100]
    }

    let instance = wgpu::Instance::default();
    let Ok(adapter) =
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
    else {
        return;
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("auraw selective-highlight test device"),
        ..Default::default()
    })) else {
        return;
    };

    for cfa_kind in [CfaKind::Bayer, CfaKind::XTrans] {
        let clipped_raw = fixture(cfa_kind, 1.0, None);
        let unclipped_raw = fixture(cfa_kind, 0.5, None);
        let clipped_exposure = ExposureParams {
            exposure: -4.0,
            ..Default::default()
        };
        let clipped_params = super::GpuParams::new(
            &clipped_exposure,
            &crate::pipeline::MaskStack::default(),
            &clipped_raw,
        );
        let pipeline = RawGpuPipeline::new_headless_with_quality(
            &device,
            &queue,
            &clipped_raw,
            &clipped_params,
            ProcessingQuality::High,
        )
        .unwrap();
        pipeline.recompute(&queue, &device, &clipped_params);
        let clipped = pipeline
            .read_display_linear_region_blocking(&device, &queue, 43, 43, 11, 11)
            .unwrap();

        // Half the source signal at one stop more exposure has identical
        // intended display energy, while remaining below every sensor
        // plane's clipping point. It is the recovery-quality oracle.
        pipeline.upload_raw_tile(&queue, &unclipped_raw).unwrap();
        let oracle_exposure = ExposureParams {
            exposure: -3.0,
            ..Default::default()
        };
        let oracle_params = super::GpuParams::new(
            &oracle_exposure,
            &crate::pipeline::MaskStack::default(),
            &unclipped_raw,
        );
        pipeline.recompute(&queue, &device, &oracle_params);
        let oracle = pipeline
            .read_display_linear_region_blocking(&device, &queue, 43, 43, 11, 11)
            .unwrap();

        let mut pink = Vec::with_capacity(121);
        let mut spread = Vec::with_capacity(121);
        let mut luma_error = Vec::with_capacity(121);
        let mut luma_ratio = Vec::with_capacity(121);
        for (candidate, reference) in clipped.chunks_exact(3).zip(oracle.chunks_exact(3)) {
            assert!(candidate
                .iter()
                .all(|value| value.is_finite() && *value >= 0.0));
            let mean = ((candidate[0] + candidate[1] + candidate[2]) / 3.0).max(1e-6);
            pink.push((((candidate[0] + candidate[2]) * 0.5 - candidate[1]) / mean).max(0.0));
            let minimum = candidate.iter().copied().fold(f32::INFINITY, f32::min);
            let maximum = candidate.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            spread.push((maximum - minimum) / mean);

            let candidate_y = 0.262_700_2 * candidate[0]
                + 0.677_998_1 * candidate[1]
                + 0.059_301_7 * candidate[2];
            let reference_y = 0.262_700_2 * reference[0]
                + 0.677_998_1 * reference[1]
                + 0.059_301_7 * reference[2];
            luma_error.push((candidate_y - reference_y).abs() / reference_y.max(1e-6));
            luma_ratio.push(candidate_y / reference_y.max(1e-6));
        }

        let pink_p95 = percentile(pink, 95);
        let spread_p95 = percentile(spread, 95);
        let luma_error_p95 = percentile(luma_error, 95);
        let luma_ratio_median = percentile(luma_ratio, 50);
        assert!(
            pink_p95 <= 0.05,
            "{cfa_kind:?} reconstructed neutral is pink: p95={pink_p95}"
        );
        // darktable's default per-channel sigmoid retains slightly more of the
        // camera-space channel separation here than RGB-ratio processing.
        assert!(
            spread_p95 <= 0.19,
            "{cfa_kind:?} reconstructed neutral has channel spread p95={spread_p95}"
        );
        assert!(
            luma_error_p95 <= 0.15,
            "{cfa_kind:?} recovered highlight luma error p95={luma_error_p95}"
        );
        assert!(
            (0.85..=1.15).contains(&luma_ratio_median),
            "{cfa_kind:?} recovered highlight median luma ratio={luma_ratio_median}"
        );

        // Neutral safety must not turn genuinely coloured clipped lights
        // white. Each primary clips only its dominant sensor plane; the
        // two surviving components should keep the reconstructed hue.
        for (dominant, peak) in [
            (0usize, [2.50, 0.35, 0.20]),
            (1usize, [0.30, 2.00, 0.25]),
            (2usize, [0.20, 0.30, 2.30]),
        ] {
            let coloured_raw = fixture(cfa_kind, 1.0, Some(peak));
            pipeline.upload_raw_tile(&queue, &coloured_raw).unwrap();
            let coloured_params = super::GpuParams::new(
                &ExposureParams::default(),
                &crate::pipeline::MaskStack::default(),
                &coloured_raw,
            );
            pipeline.dispatch_stage(
                &queue,
                &device,
                &coloured_params,
                crate::pipeline::ProcessingStage::Raw,
            );
            let scene = pipeline
                .read_scene_texture_blocking(&device, &queue)
                .unwrap();
            let mut mean = [0.0f32; 3];
            let mut count = 0.0f32;
            for y in 46..51 {
                for x in 46..51 {
                    let index = ((y * 96 + x) * 3) as usize;
                    for channel in 0..3 {
                        mean[channel] += scene[index + channel];
                    }
                    count += 1.0;
                }
            }
            for value in &mut mean {
                *value /= count;
            }
            let strongest_other = mean
                .iter()
                .enumerate()
                .filter(|(channel, _)| *channel != dominant)
                .map(|(_, value)| *value)
                .fold(0.0f32, f32::max);
            assert!(
                mean[dominant] > 2.0 * strongest_other,
                "{cfa_kind:?} clipped primary {dominant} lost its hue: rgb={mean:?}"
            );
        }
    }
}
