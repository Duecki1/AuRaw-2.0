use super::{
    canonicalize_green_noise, color_grade_hue_turns, composite_inpaint_rgba16f,
    explicit_render_graph_contracts_are_contiguous, highlight_final_read_slot,
    highlight_stage_slots, pack_local_point_curve, processing_work_format, render_graph_flags,
    shader_highlight_method, work_shader_source, HighlightWorkSlot, ProcessingQuality,
    HIGHLIGHT_GUIDED_ENTRY_POINTS, RENDER_GRAPH_EXPLICIT_SCENE_DISPLAY, SHADER_ADJUSTMENTS,
    SHADER_BAYER_RCD_P1, SHADER_BAYER_RCD_P2, SHADER_BAYER_RCD_P3, SHADER_BAYER_RCD_P4,
    SHADER_DUAL_DEMOSAIC, SHADER_HIGHLIGHTS, SHADER_REGRESSION_SCENE, SHADER_TONE_ANALYSIS,
    SHADER_XTRANS_P1, SHADER_XTRANS_P2, SHADER_XTRANS_P3, SHADER_XTRANS_P4, SHADER_XTRANS_P5,
    SHADER_XTRANS_P6, SHADER_XTRANS_P7,
};
use crate::pipeline::{CfaKind, HighlightReconstructionMethod, PointCurve};
use eframe::wgpu;

fn shader_module(name: &str, source: &str) -> naga::Module {
    naga::front::wgsl::parse_str(source)
        .unwrap_or_else(|error| panic!("{name} did not parse: {error}"))
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
                        .clone()
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
    let (_, function) = module
        .functions
        .iter()
        .find(|(_, function)| function.name.as_deref() == Some(function_name))
        .unwrap_or_else(|| panic!("missing WGSL function {function_name}"));
    let mut calls = Vec::new();
    append_direct_call_names(module, &function.body, &mut calls);
    calls
}

fn function_name_count(module: &naga::Module, function_name: &str) -> usize {
    module
        .functions
        .iter()
        .filter(|(_, function)| function.name.as_deref() == Some(function_name))
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
    let module = shader_module("Lightroom adjustments", SHADER_ADJUSTMENTS);

    let prepare_calls = entry_point_call_names(&module, "prepare_scene_node");
    assert!(
        call_position(&prepare_calls, "apply_camera_characterization")
            < call_position(&prepare_calls, "apply_exposure")
    );
    assert!(
        call_position(&prepare_calls, "apply_exposure")
            < call_position(&prepare_calls, "apply_local_exposure_nodes")
    );
    assert!(
        call_position(&prepare_calls, "uses_explicit_scene_display_domains")
            < call_position(&prepare_calls, "apply_optional_profile_look")
    );

    let tone_calls = entry_point_call_names(&module, "apply_scene_tone_node");
    assert!(
        call_position(&tone_calls, "apply_capture_sharpening")
            < call_position(&tone_calls, "apply_profile_view_tone")
    );
    assert!(
        call_position(&tone_calls, "apply_profile_view_tone")
            < call_position(&tone_calls, "apply_lightroom_tone")
    );

    let local_calls = entry_point_call_names(&module, "apply_local_scene_tone_node");
    assert!(local_calls
        .iter()
        .any(|call| call == "apply_local_scene_tone_nodes"));

    let effects_calls = entry_point_call_names(&module, "apply_scene_effects_node");
    assert!(!effects_calls
        .iter()
        .any(|call| call == "apply_capture_sharpening"));

    let view_calls = function_call_names(&module, "apply_explicit_view_node");
    let look = call_position(&view_calls, "apply_optional_profile_look");
    assert!(look < call_position(&view_calls, "apply_dcp_view_transform"));
    assert!(look < call_position(&view_calls, "apply_sigmoid_view_transform"));
}

#[test]
fn generated_finish_shaders_define_each_shared_routine_once() {
    for (name, source) in [
        ("Bayer finish", SHADER_BAYER_RCD_P4),
        ("X-Trans finish", SHADER_XTRANS_P7),
    ] {
        let module = shader_module(name, source);
        for routine in [
            "finish_warped_pos",
            "finish_reference_bilinear",
            "finish_apply_ca",
            "finish_apply_legacy_chroma_denoise",
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
    let bayer = shader_module("Bayer finish", SHADER_BAYER_RCD_P4);
    let bayer_calls = function_call_names(&bayer, "finish_reference_at");
    assert!(bayer_calls.iter().any(|call| call == "clamp_pos"));
    assert!(bayer_calls.iter().any(|call| call == "rcd_reference_at"));
    assert!(!bayer_calls.iter().any(|call| call == "xt_high"));

    let xtrans = shader_module("X-Trans finish", SHADER_XTRANS_P7);
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
        ("X-Trans pass 1", SHADER_XTRANS_P1),
        ("X-Trans pass 2", SHADER_XTRANS_P2),
        ("X-Trans pass 3", SHADER_XTRANS_P3),
        ("X-Trans derivatives", SHADER_XTRANS_P4),
        ("X-Trans homogeneity", SHADER_XTRANS_P5),
        ("X-Trans accumulation", SHADER_XTRANS_P6),
        ("X-Trans finish", SHADER_XTRANS_P7),
        ("adaptive tone analysis", SHADER_TONE_ANALYSIS),
        ("regression scene export", SHADER_REGRESSION_SCENE),
        ("Lightroom adjustments", SHADER_ADJUSTMENTS),
    ] {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|error| panic!("{name} did not parse: {error}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{name} did not validate: {error}"));
    }
}

#[test]
fn high_quality_shader_variants_parse_and_use_full_float_storage() {
    for (name, source) in [
        (
            "32-bit highlight reconstruction",
            work_shader_source(
                SHADER_HIGHLIGHTS,
                processing_work_format(ProcessingQuality::High),
            )
            .expect("specialize high-quality shader"),
        ),
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
            "32-bit perceptual color mixer",
            work_shader_source(
                SHADER_ADJUSTMENTS,
                processing_work_format(ProcessingQuality::High),
            )
            .expect("specialize high-quality shader"),
        ),
    ] {
        assert!(!source.contains("rgba16float"));
        assert!(source.contains("rgba32float"));
        let module = naga::front::wgsl::parse_str(source.as_ref())
            .unwrap_or_else(|error| panic!("{name} did not parse: {error}"));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|error| panic!("{name} did not validate: {error}"));
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
fn green_noise_is_averaged_once_and_stored_symmetrically() {
    let canonical = canonicalize_green_noise([1.0, 2.0, 3.0, 6.0], true);
    assert_eq!(canonical, [1.0, 4.0, 3.0, 4.0]);
    let unchanged = canonicalize_green_noise([1.0, 2.0, 3.0, 6.0], false);
    assert_eq!(unchanged, [1.0, 2.0, 3.0, 6.0]);
}

#[test]
fn every_edit_uses_the_current_scene_display_contract_graph() {
    assert_eq!(render_graph_flags(), RENDER_GRAPH_EXPLICIT_SCENE_DISPLAY);
    assert!(explicit_render_graph_contracts_are_contiguous());
}

#[test]
fn highlight_ping_pong_plan_is_contiguous_and_finishes_on_expected_slot() {
    let mut current = HighlightWorkSlot::A;
    for index in 0..HIGHLIGHT_GUIDED_ENTRY_POINTS.len() {
        let (read, write) = highlight_stage_slots(index);
        assert_eq!(read, current, "stage {index} reads the wrong work texture");
        assert_ne!(read, write, "stage {index} aliases its input and output");
        current = write;
    }
    assert_eq!(
        current,
        highlight_final_read_slot(HIGHLIGHT_GUIDED_ENTRY_POINTS.len())
    );
    assert_eq!(current, HighlightWorkSlot::B);
}

#[test]
fn highlight_shader_exposes_every_dispatched_entry_point() {
    let module =
        naga::front::wgsl::parse_str(SHADER_HIGHLIGHTS).expect("highlight shader did not parse");

    let expected_entry_points = std::iter::once("highlight_prepare")
        .chain(HIGHLIGHT_GUIDED_ENTRY_POINTS.iter().copied())
        .chain(std::iter::once("highlight_finalize"));

    for expected in expected_entry_points {
        assert!(
            module
                .entry_points
                .iter()
                .any(|entry| entry.name == expected),
            "highlight shader is missing entry point {expected}"
        );
    }
}

#[test]
fn guided_highlight_strength_is_one_continuous_final_blend() {
    assert!(SHADER_HIGHLIGHTS.contains("output = mix(original, guided, clip_amount * strength)"));
    assert!(!SHADER_HIGHLIGHTS.contains("0.35 + 0.65 * strength"));
    assert!(!SHADER_HIGHLIGHTS.contains("output = guided;"));
}

#[test]
fn xtrans_never_dispatches_the_bayer_phase_lch_reconstruction() {
    assert_eq!(
        shader_highlight_method(CfaKind::Bayer, HighlightReconstructionMethod::Lch),
        HighlightReconstructionMethod::Lch.shader_value()
    );
    assert_eq!(
        shader_highlight_method(CfaKind::XTrans, HighlightReconstructionMethod::Lch),
        HighlightReconstructionMethod::Guided.shader_value()
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

    assert!(SHADER_ADJUSTMENTS.contains("fn profile_tone_scene_shoulder_knee"));
    assert!(SHADER_ADJUSTMENTS.contains("let broad_highlight_pressure"));
    assert!(SHADER_ADJUSTMENTS.contains("let isolated_specular"));
    assert!(SHADER_ADJUSTMENTS.contains("let shoulder_knee = profile_tone_scene_shoulder_knee()"));
    assert!(!SHADER_ADJUSTMENTS.contains("let shoulder_knee = 0.70"));
    assert!(!SHADER_ADJUSTMENTS.contains("mix(positive, darktable_sigmoid"));
}

#[test]
fn basic_contrast_has_protected_toe_midtones_and_shoulder() {
    fn contrast_ev(scene_ev: f32, amount: f32) -> f32 {
        let toe_distance = (-scene_ev).max(0.0);
        let shoulder_distance = scene_ev.max(0.0);
        let toe_response = 1.0 - 2.0f32.powf(-toe_distance / 1.65);
        let shoulder_response = 1.0 - 2.0f32.powf(-shoulder_distance / 1.85);
        let shape = shoulder_response - toe_response * 0.85;
        let strength = if amount >= 0.0 { 1.0 } else { 0.72 };
        scene_ev + amount.clamp(-1.0, 1.0) * strength * shape
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
    assert!(contrast_ev(1.0, 1.0) > 1.25);
    assert!(contrast_ev(-8.0, 1.0) > -9.0);
    assert!(contrast_ev(8.0, 1.0) < 9.1);
    assert_eq!(contrast_ev(0.0, 1.0), 0.0);

    assert!(SHADER_ADJUSTMENTS.contains("apply_basic_contrast_value"));
}

#[test]
fn lifted_black_curve_uses_continuous_luminance_remapping() {
    assert!(SHADER_ADJUSTMENTS.contains("fn remap_scene_luminance"));
    assert!(SHADER_ADJUSTMENTS.contains("if luminance <= 0.0"));
    assert!(SHADER_ADJUSTMENTS.contains("vec3<f32>(black) + rgb * zero_slope"));
    assert!(!SHADER_ADJUSTMENTS.contains("adjusted * clamp(curved / luminance, 0.0, 256.0)"));
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
    assert!(SHADER_ADJUSTMENTS.contains("fn local_curve_tangent"));
    assert!(SHADER_ADJUSTMENTS.contains("let hermite ="));
    assert!(!SHADER_ADJUSTMENTS.contains("clamp(input, 0.0, 1.0) * 31.0"));
}

#[test]
fn demosaic_reference_invariants_are_present() {
    assert!(SHADER_BAYER_RCD_P4.contains("const RCD_MARGIN: i32 = 9"));
    assert!(SHADER_BAYER_RCD_P4.contains("ppg_rgb_at"));
    assert!(SHADER_BAYER_RCD_P2.contains("green = mix(vertical.x, horizontal.x, vh)"));
    assert!(SHADER_BAYER_RCD_P3.contains("return mix(p_est, q_est, pq)"));

    assert!(SHADER_XTRANS_P6.contains("index < 8u"));
    assert!(SHADER_XTRANS_P5.contains("minimum * 8.0"));
    assert!(SHADER_XTRANS_P6.contains("mark_homo_sum5"));
    assert!(SHADER_XTRANS_P6.contains("index + 4u"));
    assert!(SHADER_XTRANS_P6.contains("MARKESTEIJN3_MARGIN"));

    assert!(SHADER_BAYER_RCD_P4.contains("for (var dy = -6; dy <= 6"));
    assert!(SHADER_XTRANS_P7.contains("for (var dy = -6; dy <= 6"));
    assert!(SHADER_BAYER_RCD_P4.contains("detail /= 256.0"));
    assert!(SHADER_XTRANS_P7.contains("detail /= 256.0"));
}

#[test]
fn demosaic_shaders_expose_every_dispatched_entry_point() {
    for (source, expected) in [
        (SHADER_BAYER_RCD_P1, "bayer_rcd_directional"),
        (SHADER_BAYER_RCD_P2, "bayer_rcd_green"),
        (SHADER_BAYER_RCD_P3, "bayer_rcd_chroma"),
        (SHADER_BAYER_RCD_P4, "bayer_rcd_output"),
        (SHADER_XTRANS_P1, "xtrans_seed"),
        (SHADER_XTRANS_P2, "xtrans_markesteijn_pass1"),
        (SHADER_XTRANS_P2, "xtrans_markesteijn_pass3"),
        (SHADER_XTRANS_P3, "xtrans_markesteijn_pass2"),
        (SHADER_XTRANS_P4, "xtrans_markesteijn_derivatives"),
        (SHADER_XTRANS_P5, "xtrans_markesteijn_homogeneity"),
        (SHADER_XTRANS_P6, "xtrans_markesteijn_accumulate"),
        (SHADER_XTRANS_P7, "xtrans_demosaic_finish"),
    ] {
        let module = naga::front::wgsl::parse_str(source).expect("demosaic shader did not parse");
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
fn tone_analysis_shader_exposes_every_dispatched_entry_point() {
    let module = naga::front::wgsl::parse_str(SHADER_TONE_ANALYSIS)
        .expect("adaptive tone-analysis shader did not parse");

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
fn gpu_params_follow_the_wgsl_uniform_layout() {
    // Sixteen scalar values keep the stable 64-byte prefix. The two
    // darktable sigmoid vec4s follow the local-tone controls, then the
    // remaining adjustment, camera/raw, dimension and profile blocks.
    assert_eq!(std::mem::size_of::<super::GpuParams>(), 25136);
    assert_eq!(std::mem::offset_of!(super::GpuParams, basic_tone), 64);
    assert_eq!(std::mem::offset_of!(super::GpuParams, sigmoid_curve), 80);
    assert_eq!(std::mem::offset_of!(super::GpuParams, sigmoid_power), 96);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, creative_effects),
        128
    );
    assert_eq!(std::mem::offset_of!(super::GpuParams, vignette), 144);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, vignette_options),
        160
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, highlight_options),
        176
    );
    assert_eq!(std::mem::offset_of!(super::GpuParams, tone_curve_0), 240);
    assert_eq!(std::mem::offset_of!(super::GpuParams, tone_curve_meta), 304);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, tone_curve_red_0),
        320
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, tone_curve_green_0),
        400
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, tone_curve_blue_0),
        480
    );
    assert_eq!(std::mem::offset_of!(super::GpuParams, wb), 656);
    assert_eq!(std::mem::offset_of!(super::GpuParams, inpaint_wb_0), 720);
    assert_eq!(std::mem::offset_of!(super::GpuParams, width), 800);
    assert_eq!(std::mem::offset_of!(super::GpuParams, tile_origin_x), 808);
    assert_eq!(std::mem::offset_of!(super::GpuParams, full_width), 816);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, tone_histogram_bounds),
        832
    );
    assert_eq!(std::mem::offset_of!(super::GpuParams, profile_hue_sat), 848);
    assert_eq!(std::mem::offset_of!(super::GpuParams, profile_flags), 912);
    assert_eq!(std::mem::offset_of!(super::GpuParams, process_info), 928);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_counts), 944);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_meta), 960);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_adjust_0), 1472);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_adjust_1), 1984);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_adjust_2), 2496);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_curve_0), 3008);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_curve_7), 6592);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_curve_red_0),
        7104
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_curve_green_0),
        11200
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_curve_blue_0),
        15296
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_hsl_hue_0),
        19392
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_hsl_luminance_1),
        21952
    );
    assert_eq!(std::mem::offset_of!(super::GpuParams, grade_shadows), 22464);
    assert_eq!(std::mem::offset_of!(super::GpuParams, grade_options), 22528);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_grade_shadows),
        22544
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_grade_options),
        24592
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, vignette_frame),
        25104
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, vignette_transform),
        25120
    );
}

#[test]
fn adjustments_shader_contains_darktable_sigmoid_paths() {
    assert!(SHADER_ADJUSTMENTS.contains("generalized_loglogistic_sigmoid"));
    assert!(SHADER_ADJUSTMENTS.contains("preserve_hue_and_energy"));
    assert!(SHADER_ADJUSTMENTS.contains("sigmoid_rgb_ratio"));
    assert!(SHADER_ADJUSTMENTS.contains("hyperbolic_chroma"));
}

#[test]
fn signed_scene_rgb_is_preserved_until_explicit_positive_domain_boundaries() {
    // Shared projection is used only where a positive/unit RGB domain is part
    // of the algorithm contract; scene intermediates must not floor channels.
    assert!(SHADER_ADJUSTMENTS.contains("fn gamut_project_nonnegative("));
    assert!(SHADER_ADJUSTMENTS.contains("fn gamut_project_unit("));
    assert!(
        SHADER_ADJUSTMENTS.contains("let view_input = gamut_project_nonnegative_rec2020(looked)")
    );
    assert!(SHADER_ADJUSTMENTS.contains("perceptual_gamut_compress_unit_rec2020"));

    for forbidden in [
        "max(REC2020_TO_PROPHOTO *",
        "linear_srgb_to_oklab(REC2020_TO_SRGB * max(rgb",
        "return max(adjusted, vec3<f32>(0.0))",
        "vec4<f32>(max(rgb, vec3<f32>(0.0)), 1.0)",
        "let view_input = max(map_negative_gamut",
    ] {
        assert!(
            !SHADER_ADJUSTMENTS.contains(forbidden),
            "premature RGB floor reintroduced: {forbidden}"
        );
    }

    for (name, source) in [
        ("Bayer pass 2", SHADER_BAYER_RCD_P2),
        ("Bayer pass 3", SHADER_BAYER_RCD_P3),
        ("Bayer finish", SHADER_BAYER_RCD_P4),
        ("X-Trans pass 2", SHADER_XTRANS_P2),
        ("X-Trans pass 3", SHADER_XTRANS_P3),
        ("X-Trans accumulation", SHADER_XTRANS_P6),
        ("X-Trans finish", SHADER_XTRANS_P7),
    ] {
        for forbidden in [
            "max(camera_rgb, vec3<f32>(0.0))",
            "max(rgb, vec3<f32>(0.0))",
            "max(out, vec3<f32>(0.0))",
            "max(green, 0.0)",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} reintroduced destructive demosaic flooring: {forbidden}"
            );
        }
    }
}

#[test]
fn adjustments_shader_contains_lightroom_style_controls() {
    assert!(!SHADER_ADJUSTMENTS.contains("apply_camera_temperature_tint"));
    assert!(SHADER_ADJUSTMENTS.contains("bitcast<f32>(params.profile_flags.w)"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_basic_contrast"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_point_tone_curve"));
    assert!(SHADER_ADJUSTMENTS.contains("linear_srgb_to_oklab"));
    assert!(SHADER_ADJUSTMENTS.contains("skin_protection"));
    assert!(SHADER_ADJUSTMENTS.contains("prepare_scene_node"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_scene_tone_node"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_scene_effects_node"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_creative_effects"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_glow"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_vignette"));
    assert!(SHADER_ADJUSTMENTS.contains("stabilized_mixer_sample"));
    assert!(SHADER_ADJUSTMENTS.contains("perceptual_rec2020_from_oklab_nonnegative"));
    assert!(SHADER_ADJUSTMENTS.contains("mixer_luminance_ev"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_color_grading_wheels"));
    assert!(SHADER_ADJUSTMENTS.contains("color_grade_tonal_weights"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_local_color_grading"));
    assert!(!SHADER_ADJUSTMENTS.contains("rgb_to_hsl"));
    assert!(!SHADER_ADJUSTMENTS.contains("hsl_to_rgb"));
}

#[test]
fn profile_tables_preserve_value_channel_and_maximum_lookup() {
    assert!(SHADER_ADJUSTMENTS.contains("hsv.z = clamp(hsv.z * adjustment.z, 0.0, 1.0)"));
    assert!(SHADER_ADJUSTMENTS.contains("return profile_data[offset + maximum].x"));
}

#[test]
fn global_wb_changes_camera_transform_for_dng_metadata() {
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
    assert_ne!(neutral.cam_to_srgb_0, changed.cam_to_srgb_0);
    assert_ne!(neutral.cam_to_srgb_1, changed.cam_to_srgb_1);
    assert_ne!(neutral.cam_to_srgb_2, changed.cam_to_srgb_2);

    let tint_rendition = |tint| {
        let params = super::GpuParams::new(
            &crate::pipeline::ExposureParams {
                tint,
                ..Default::default()
            },
            &crate::pipeline::MaskStack::default(),
            &raw,
        );
        [
            params.cam_to_srgb_0[..3].iter().sum::<f32>(),
            params.cam_to_srgb_1[..3].iter().sum::<f32>(),
            params.cam_to_srgb_2[..3].iter().sum::<f32>(),
        ]
    };
    let green = tint_rendition(-20.0);
    let magenta = tint_rendition(20.0);
    let magenta_axis = |rgb: [f32; 3]| (rgb[0] + rgb[2]) * 0.5 - rgb[1];
    assert!(magenta_axis(magenta) > magenta_axis(green));
}

#[derive(Clone, Copy)]
enum LocalToneSchedulingCase {
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Temperature,
    Tint,
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
            Self::Curves => {
                let mut curve = PointCurve::linear();
                curve.points[1] = [0.42, 0.68];
                curve.points[2] = [1.0, 1.0];
                curve.len = 3;
                adjustments.tone_curve = curve;
            }
        }
    }
}

struct LocalMaskSchedulingHarness {
    device: eframe::wgpu::Device,
    queue: eframe::wgpu::Queue,
    pipeline: super::RawGpuPipeline,
    raw: super::LoadedRaw,
    exposure: super::ExposureParams,
}

impl LocalMaskSchedulingHarness {
    const WIDTH: u32 = 96;
    const HEIGHT: u32 = 64;
    const MASK_EDGE: u32 = 64;

    fn try_new() -> Option<Self> {
        use eframe::wgpu;
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
        assert!(
            adjusted_params.needs_intermediate_adjustment_passes(),
            "{} did not schedule the local tone pass",
            case.label()
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
    assert!(
        adjusted_params.needs_intermediate_adjustment_passes(),
        "{} did not schedule the local tone pass",
        case.label()
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
    harness
        .lock()
        .expect("local-mask scheduling harness mutex poisoned")
        .assert_case(case);
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
fn masked_curves_are_independently_scheduled_in_preview_and_export() {
    assert_local_tone_scheduling_case(LocalToneSchedulingCase::Curves);
}

#[test]
fn gpu_pipeline_renders_and_reads_scene_textures_when_an_adapter_exists() {
    use super::{CfaKind, ExposureParams, LoadedRaw, ProcessingQuality, RawGpuPipeline};
    use eframe::wgpu;

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
    use super::{ExposureParams, ProcessingQuality, RawGpuPipeline};
    use crate::pipeline::{
        build_proxy, crop_raw, load_raw_file, HighlightReconstructionMethod, MaskStack, ProxySpec,
    };
    use eframe::wgpu;

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
    use super::{CfaKind, ExposureParams, LoadedRaw, ProcessingQuality, RawGpuPipeline};
    use crate::pipeline::{HighlightReconstructionMethod, MaskStack};
    use eframe::wgpu;

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
fn guided_reconstruction_keeps_large_clipped_neutral_highlights_neutral() {
    use super::{CfaKind, ExposureParams, LoadedRaw, ProcessingQuality, RawGpuPipeline};
    use eframe::wgpu;

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
    let minimum = mean.into_iter().fold(f32::INFINITY, f32::min);
    let maximum = mean.into_iter().fold(f32::NEG_INFINITY, f32::max);
    let average = mean.into_iter().sum::<f32>() / 3.0;
    let relative_chroma = (maximum - minimum) / average.max(1e-6);
    assert!(
        relative_chroma < 0.02,
        "clipped neutral core became coloured: rgb={mean:?}, relative chroma={relative_chroma}"
    );
}

#[test]
fn guided_reconstruction_recovers_selectively_clipped_neutral_highlights() {
    use super::{CfaKind, ExposureParams, LoadedRaw, ProcessingQuality, RawGpuPipeline};
    use eframe::wgpu;

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
        assert!(
            spread_p95 <= 0.10,
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
