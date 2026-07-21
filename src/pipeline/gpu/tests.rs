use super::{
    color_grade_hue_turns, highlight_final_read_slot, highlight_stage_slots,
    pack_local_point_curve, processing_work_format, shader_highlight_method, work_shader_source,
    HighlightWorkSlot, ProcessingQuality, HIGHLIGHT_GUIDED_ENTRY_POINTS, SHADER_ADJUSTMENTS,
    SHADER_BAYER_RCD_P1, SHADER_BAYER_RCD_P2, SHADER_BAYER_RCD_P3, SHADER_BAYER_RCD_P4,
    SHADER_HIGHLIGHTS, SHADER_REGRESSION_SCENE, SHADER_TONE_ANALYSIS, SHADER_XTRANS_P1,
    SHADER_XTRANS_P2, SHADER_XTRANS_P3, SHADER_XTRANS_P4, SHADER_XTRANS_P5, SHADER_XTRANS_P6,
    SHADER_XTRANS_P7,
};
use crate::pipeline::{CfaKind, HighlightReconstructionMethod, PointCurve};

#[test]
fn compute_shaders_parse_and_validate() {
    for (name, source) in [
        ("highlight reconstruction", SHADER_HIGHLIGHTS),
        ("Bayer RCD pass 1", SHADER_BAYER_RCD_P1),
        ("Bayer RCD pass 2", SHADER_BAYER_RCD_P2),
        ("Bayer RCD pass 3", SHADER_BAYER_RCD_P3),
        ("Bayer RCD pass 4", SHADER_BAYER_RCD_P4),
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
            ),
        ),
        (
            "32-bit Bayer pass 1",
            work_shader_source(
                SHADER_BAYER_RCD_P1,
                processing_work_format(ProcessingQuality::High),
            ),
        ),
        (
            "32-bit perceptual color mixer",
            work_shader_source(
                SHADER_ADJUSTMENTS,
                processing_work_format(ProcessingQuality::High),
            ),
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
fn profile_highlight_shoulder_is_monotonic() {
    let map_peak = |peak: f32| {
        let knee = 0.82;
        if peak <= knee {
            peak
        } else {
            let distance = peak - knee;
            knee + distance / (1.0 + distance / (1.0 - knee))
        }
    };
    let mut previous = map_peak(0.0);
    for step in 1..=10_000 {
        let current = map_peak(step as f32 / 1_000.0);
        assert!(current >= previous, "shoulder reversed at step {step}");
        assert!(current <= 1.0);
        previous = current;
    }
    assert!(SHADER_ADJUSTMENTS.contains("mapped_peak = knee + distance /"));
    assert!(!SHADER_ADJUSTMENTS.contains("mix(positive, darktable_sigmoid"));
}

#[test]
fn lifted_black_curve_uses_continuous_luminance_remapping() {
    assert!(SHADER_ADJUSTMENTS.contains("fn remap_scene_luminance"));
    assert!(SHADER_ADJUSTMENTS.contains("smoothstep(1e-7, 1e-5, luminance)"));
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
    assert_eq!(std::mem::size_of::<super::GpuParams>(), 7008);
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
    assert_eq!(std::mem::offset_of!(super::GpuParams, tone_curve_0), 192);
    assert_eq!(std::mem::offset_of!(super::GpuParams, tone_curve_meta), 256);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, tone_curve_red_0),
        272
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, tone_curve_green_0),
        352
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, tone_curve_blue_0),
        432
    );
    assert_eq!(std::mem::offset_of!(super::GpuParams, wb), 608);
    assert_eq!(std::mem::offset_of!(super::GpuParams, inpaint_wb_0), 672);
    assert_eq!(std::mem::offset_of!(super::GpuParams, width), 752);
    assert_eq!(std::mem::offset_of!(super::GpuParams, tile_origin_x), 760);
    assert_eq!(std::mem::offset_of!(super::GpuParams, full_width), 768);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, tone_histogram_bounds),
        784
    );
    assert_eq!(std::mem::offset_of!(super::GpuParams, profile_hue_sat), 800);
    assert_eq!(std::mem::offset_of!(super::GpuParams, profile_flags), 864);
    assert_eq!(std::mem::offset_of!(super::GpuParams, process_info), 880);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_counts), 896);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_meta), 912);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_adjust_0), 1040);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_adjust_1), 1168);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_adjust_2), 1296);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_curve_0), 1424);
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_curve_7), 2320);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_curve_red_0),
        2448
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_curve_green_0),
        3472
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_curve_blue_0),
        4496
    );
    assert_eq!(std::mem::offset_of!(super::GpuParams, mask_hsl_hue_0), 5520);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_hsl_luminance_1),
        6160
    );
    assert_eq!(std::mem::offset_of!(super::GpuParams, grade_shadows), 6288);
    assert_eq!(std::mem::offset_of!(super::GpuParams, grade_options), 6352);
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_grade_shadows),
        6368
    );
    assert_eq!(
        std::mem::offset_of!(super::GpuParams, mask_grade_options),
        6880
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
fn adjustments_shader_contains_lightroom_style_controls() {
    assert!(!SHADER_ADJUSTMENTS.contains("apply_camera_temperature_tint"));
    assert!(SHADER_ADJUSTMENTS.contains("bitcast<f32>(params.profile_flags.w)"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_basic_contrast"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_point_tone_curve"));
    assert!(SHADER_ADJUSTMENTS.contains("linear_srgb_to_oklab"));
    assert!(SHADER_ADJUSTMENTS.contains("skin_protection"));
    assert!(SHADER_ADJUSTMENTS.contains("prepare_adjustment_base"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_lightroom_effects"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_creative_effects"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_glow"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_vignette"));
    assert!(SHADER_ADJUSTMENTS.contains("stabilized_mixer_sample"));
    assert!(SHADER_ADJUSTMENTS.contains("nonnegative_rec2020_from_oklab"));
    assert!(SHADER_ADJUSTMENTS.contains("mixer_luminance_ev"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_color_grading_wheels"));
    assert!(SHADER_ADJUSTMENTS.contains("color_grade_tonal_weights"));
    assert!(SHADER_ADJUSTMENTS.contains("apply_local_color_grading"));
    assert!(!SHADER_ADJUSTMENTS.contains("rgb_to_hsl"));
    assert!(!SHADER_ADJUSTMENTS.contains("hsl_to_rgb"));
}

#[test]
fn dcp_characterization_precedes_exposure_and_profile_rendering() {
    let prepare = &SHADER_ADJUSTMENTS[SHADER_ADJUSTMENTS
        .find("fn prepare_adjustment_base")
        .unwrap()..];
    let hue_sat = prepare
        .find("var rgb = apply_profile_hue_sat(scene_working_at(pos))")
        .unwrap();
    let profile_exposure = prepare.find("let profile_exposure_ev").unwrap();
    let exposure = prepare.find("rgb = apply_exposure(rgb)").unwrap();
    let look = prepare.find("rgb = apply_profile_look(rgb)").unwrap();
    let curve = prepare.find("rgb = apply_profile_tone_curve(rgb)").unwrap();
    let gamut = prepare.find("rgb = map_negative_gamut(rgb)").unwrap();
    assert!(
        hue_sat < profile_exposure
            && profile_exposure < exposure
            && exposure < look
            && look < curve
            && curve < gamut
    );
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
            cfa_kind,
            raw_pixels: vec![2048; (width * height) as usize],
            color_indices,
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            black_levels: [0.0; 4],
            black_levels_per_pixel: vec![0.0; (width * height) as usize],
            white_levels: [4095.0; 4],
            camera_profile: Default::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
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

        let camera_scene = pipeline
            .read_scene_texture_blocking(&device, &queue)
            .unwrap_or_else(|error| {
                panic!("{cfa_kind:?} scene texture readback failed: {error:#}")
            });
        assert_eq!(camera_scene.len(), (width * height * 3) as usize);
        assert!(camera_scene.iter().all(|value| value.is_finite()));
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
            cfa_kind: CfaKind::Bayer,
            raw_pixels,
            color_indices,
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            black_levels: [0.0; 4],
            black_levels_per_pixel: vec![0.0; (width * height) as usize],
            white_levels: [white; 4],
            camera_profile: Default::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
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
        cfa_kind: CfaKind::Bayer,
        raw_pixels,
        color_indices,
        wb_coeffs: wb,
        cam_to_srgb: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
        black_levels: [0.0; 4],
        black_levels_per_pixel: vec![0.0; (width * height) as usize],
        white_levels: [white; 4],
        camera_profile: Default::default(),
        camera_profile_source: None,
        available_camera_profiles: Vec::new(),
        white_balance_model: None,
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
            cfa_kind,
            raw_pixels,
            color_indices,
            wb_coeffs: WB,
            cam_to_srgb: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            black_levels: [0.0; 4],
            black_levels_per_pixel: vec![0.0; (WIDTH * HEIGHT) as usize],
            white_levels: [WHITE; 4],
            camera_profile: Default::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
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
