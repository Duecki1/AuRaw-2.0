use super::*;

#[test]
fn common_mask_properties_mutate_through_shared_model_api() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Brush);
    let mask = &mut stack.masks[0];
    assert!(mask.common.rename("Primary"));
    assert!(mask.common.set_enabled(false));
    mask.common.toggle_invert();
    assert!(mask.set_opacity(0.42));
    assert_eq!(mask.name, "Primary");
    assert!(!mask.enabled);
    assert!(mask.invert);
    assert_eq!(mask.opacity, 0.42);

    for kind in [
        MaskKind::Brush,
        MaskKind::Radial,
        MaskKind::Linear,
        MaskKind::Subject,
        MaskKind::Landscape,
        MaskKind::Object,
        MaskKind::LuminanceRange,
        MaskKind::ColorRange,
    ] {
        stack.clear();
        stack.add_mask(kind);
        let component = &mut stack.masks[0].components[0];
        assert!(component.common.rename("Shared"));
        assert!(component.common.set_enabled(false));
        component.common.toggle_invert();
        assert!(component.set_feather(0.37));
        assert!(component.set_combine(MaskCombineMode::Intersect));
        assert_eq!(component.name, "Shared");
        assert!(!component.enabled);
        assert!(component.invert);
        assert_eq!(component.combine, MaskCombineMode::Intersect);
        let feather = match &component.geometry {
            MaskGeometry::Brush { feather, .. }
            | MaskGeometry::Radial { feather, .. }
            | MaskGeometry::Linear { feather, .. }
            | MaskGeometry::Ai { feather, .. }
            | MaskGeometry::Landscape { feather, .. }
            | MaskGeometry::Object { feather, .. }
            | MaskGeometry::LuminanceRange { feather, .. }
            | MaskGeometry::ColorRange { feather, .. } => *feather,
            _ => unreachable!("tested mask kind must expose feather"),
        };
        assert_eq!(feather, 0.37);
    }
}

#[test]
fn duplicate_delete_and_selection_use_shared_stack_rules() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Radial);
    stack.masks[0].common.rename("Source");
    stack.masks[0].adjustments.exposure = 1.25;
    assert!(stack.duplicate_mask(0, true));
    assert_eq!(stack.selected_mask, Some(1));
    assert_eq!(stack.selected_component, Some(0));
    assert_eq!(stack.masks[1].name, "Source Copy");
    assert!(stack.masks[1].invert);
    assert_eq!(stack.masks[1].adjustments, LocalAdjustments::default());
    assert_eq!(stack.masks[1].components, stack.masks[0].components);

    stack.select_mask(0);
    stack.add_component(MaskKind::Linear, MaskCombineMode::Subtract);
    assert!(stack.duplicate_component(0, 1, true));
    assert_eq!(stack.selected_component, Some(2));
    assert_eq!(stack.masks[0].components[2].name, "Linear Gradient Copy");
    assert!(stack.masks[0].components[2].invert);
    assert_eq!(
        stack.masks[0].components[2].geometry,
        stack.masks[0].components[1].geometry
    );

    assert!(stack.delete_component(0, 2));
    assert_eq!(stack.selected_mask, Some(0));
    assert_eq!(stack.selected_component, Some(1));
    assert!(stack.delete_mask(0));
    assert_eq!(stack.selected_mask, Some(0));
    assert_eq!(stack.selected_component, Some(0));
    assert_eq!(stack.masks.len(), 1);
}

#[test]
fn shared_common_state_keeps_legacy_flat_serialization() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Radial);
    stack.masks[0].common.rename("Flat group");
    let component = &mut stack.masks[0].components[0];
    component.common.rename("Flat component");
    component.set_feather(0.23);

    let value = serde_json::to_value(&stack).unwrap();
    let mask = value["masks"][0].as_object().unwrap();
    assert_eq!(mask["name"].as_str(), Some("Flat group"));
    assert!(mask.get("common").is_none());
    let component = mask["components"][0].as_object().unwrap();
    assert_eq!(component["name"].as_str(), Some("Flat component"));
    assert!(component.get("common").is_none());
    assert!(component["geometry"]["Radial"].get("feather").is_some());

    let restored: MaskStack = serde_json::from_value(value).unwrap();
    assert_eq!(restored, stack);
}

#[test]
fn shared_enabled_and_opacity_actions_preserve_raster_semantics() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Fullscreen);
    assert_eq!(stack.rasterize_layer(0, 2, 2, 2, 2), vec![255; 4]);

    assert!(stack.masks[0].set_opacity(0.5));
    assert_eq!(stack.rasterize_layer(0, 2, 2, 2, 2), vec![128; 4]);

    assert!(stack.masks[0].components[0].common.set_enabled(false));
    assert_eq!(stack.rasterize_layer(0, 2, 2, 2, 2), vec![0; 4]);
}

#[test]
fn new_brush_is_selected_and_paint_ready() {
    let mut stack = MaskStack::default();
    assert_eq!(stack.add_mask(MaskKind::Brush), Some((0, 0)));
    assert_eq!(stack.selected_mask, Some(0));
    assert_eq!(stack.selected_component, Some(0));
    assert_eq!(
        stack.selected_mask().unwrap().effect,
        MaskEffect::Adjustment
    );
    assert!(matches!(
        stack.selected_component().unwrap().geometry,
        MaskGeometry::Brush { .. }
    ));
}

#[test]
fn fullscreen_mask_is_immediately_initialized_and_covers_every_pixel() {
    let mut stack = MaskStack::default();
    assert_eq!(stack.add_mask(MaskKind::Fullscreen), Some((0, 0)));
    assert!(MaskKind::Fullscreen.is_available());
    assert!(matches!(
        stack.selected_component().unwrap().geometry,
        MaskGeometry::Fullscreen
    ));
    assert!(stack
        .selected_component()
        .unwrap()
        .geometry
        .is_initialized());
    assert_eq!(
        stack.rasterize_layer(0, 17, 11, 6000, 4000),
        vec![255; 17 * 11]
    );

    let cropped = stack.cropped_for_region(900, 700, 1200, 800, 6000, 4000);
    assert_eq!(
        cropped.rasterize_layer(0, 9, 7, 1200, 800),
        vec![255; 9 * 7]
    );
}

#[test]
fn mask_effect_picker_catalog_is_grouped_and_alphabetized() {
    assert_eq!(MaskEffect::ALL[0], MaskEffect::Adjustment);
    assert_eq!(MaskEffect::Adjustment.category(), None);

    for category in MaskEffectCategory::ALL {
        let labels: Vec<_> = MaskEffect::ALL
            .iter()
            .copied()
            .filter(|effect| effect.category() == Some(category))
            .map(MaskEffect::label)
            .collect();
        assert!(!labels.is_empty(), "{} category is empty", category.label());
        assert!(
            labels.windows(2).all(|pair| pair[0] <= pair[1]),
            "{} effects are not alphabetized: {labels:?}",
            category.label()
        );
        assert!(labels.iter().all(|label| !label.is_empty()));
    }
    assert_eq!(MaskEffect::ALL.len(), 13);
}

#[test]
fn legacy_local_mask_defaults_to_adjustment_effect() {
    let mask = LocalMask::new(MaskKind::Brush, 1);
    let mut serialized = serde_json::to_value(mask).expect("serialize local mask");
    let object = serialized
        .as_object_mut()
        .expect("local mask is a JSON object");
    object.remove("effect");
    object.remove("invert");
    let decoded: LocalMask =
        serde_json::from_value(serialized).expect("deserialize legacy local mask");
    assert_eq!(decoded.effect, MaskEffect::Adjustment);
    assert!(!decoded.invert);
    assert_eq!(decoded.effect_settings, MaskEffectSettings::default());
}

#[test]
fn neon_settings_round_trip_without_touching_local_adjustments() {
    let mut mask = LocalMask::new(MaskKind::Fullscreen, 1);
    mask.effect = MaskEffect::Neon;
    mask.effect_settings.neon.amount = 42.0;
    mask.effect_settings.neon.color = [1.0, 0.2, 0.7];
    mask.adjustments.exposure = 1.25;

    let encoded = serde_json::to_string(&mask).expect("serialize Neon mask");
    let decoded: LocalMask = serde_json::from_str(&encoded).expect("deserialize Neon mask");
    assert_eq!(decoded.effect, MaskEffect::Neon);
    assert_eq!(decoded.effect_settings.neon.amount, 42.0);
    assert_eq!(decoded.effect_settings.neon.color, [1.0, 0.2, 0.7]);
    assert_eq!(decoded.adjustments.exposure, 1.25);
}

#[test]
fn glow_settings_round_trip_without_touching_local_adjustments() {
    let mut mask = LocalMask::new(MaskKind::Fullscreen, 1);
    mask.effect = MaskEffect::Glow;
    mask.effect_settings.glow.amount = 72.0;
    mask.effect_settings.glow.radius = 84.0;
    mask.effect_settings.glow.core = 55.0;
    mask.effect_settings.glow.color = [0.2, 0.8, 1.0];
    mask.adjustments.exposure = 1.25;

    let encoded = serde_json::to_string(&mask).expect("serialize Glow mask");
    let decoded: LocalMask = serde_json::from_str(&encoded).expect("deserialize Glow mask");
    assert_eq!(decoded.effect, MaskEffect::Glow);
    assert_eq!(decoded.effect_settings.glow.amount, 72.0);
    assert_eq!(decoded.effect_settings.glow.radius, 84.0);
    assert_eq!(decoded.effect_settings.glow.core, 55.0);
    assert_eq!(decoded.effect_settings.glow.color, [0.2, 0.8, 1.0]);
    assert_eq!(decoded.adjustments.exposure, 1.25);
}

#[test]
fn light_rays_settings_round_trip_without_touching_local_adjustments() {
    let mut mask = LocalMask::new(MaskKind::Fullscreen, 1);
    mask.effect = MaskEffect::LightRays;
    mask.effect_settings.light_rays.amount = 68.0;
    mask.effect_settings.light_rays.length = 145.0;
    mask.effect_settings.light_rays.source = [24.0, -12.0];
    mask.effect_settings.light_rays.color = [1.0, 0.72, 0.35];
    mask.adjustments.exposure = 1.25;

    let encoded = serde_json::to_string(&mask).expect("serialize Light Rays mask");
    let decoded: LocalMask =
        serde_json::from_str(&encoded).expect("deserialize Light Rays mask");
    assert_eq!(decoded.effect, MaskEffect::LightRays);
    assert_eq!(decoded.effect_settings.light_rays.amount, 68.0);
    assert_eq!(decoded.effect_settings.light_rays.length, 145.0);
    assert_eq!(decoded.effect_settings.light_rays.source, [24.0, -12.0]);
    assert_eq!(decoded.effect_settings.light_rays.color, [1.0, 0.72, 0.35]);
    assert_eq!(decoded.adjustments.exposure, 1.25);
}

#[test]
fn blur_edge_glow_and_pixelate_settings_round_trip_independently() {
    let mut mask = LocalMask::new(MaskKind::Fullscreen, 1);
    mask.effect = MaskEffect::Pixelate;
    mask.effect_settings.blur.amount = 37.0;
    mask.effect_settings.blur.radius = 11.5;
    mask.effect_settings.edge_glow.amount = 64.0;
    mask.effect_settings.edge_glow.edge_width = 3.25;
    mask.effect_settings.edge_glow.color = [0.2, 0.7, 1.0];
    mask.effect_settings.pixelate.amount = 82.0;
    mask.effect_settings.pixelate.block_size = 23.0;
    mask.adjustments.exposure = 1.25;

    let encoded = serde_json::to_string(&mask).expect("serialize creative mask effects");
    let decoded: LocalMask =
        serde_json::from_str(&encoded).expect("deserialize creative mask effects");
    assert_eq!(decoded.effect, MaskEffect::Pixelate);
    assert_eq!(decoded.effect_settings.blur.amount, 37.0);
    assert_eq!(decoded.effect_settings.blur.radius, 11.5);
    assert_eq!(decoded.effect_settings.edge_glow.amount, 64.0);
    assert_eq!(decoded.effect_settings.edge_glow.edge_width, 3.25);
    assert_eq!(decoded.effect_settings.edge_glow.color, [0.2, 0.7, 1.0]);
    assert_eq!(decoded.effect_settings.pixelate.amount, 82.0);
    assert_eq!(decoded.effect_settings.pixelate.block_size, 23.0);
    assert_eq!(decoded.adjustments.exposure, 1.25);
}

#[test]
fn focus_blur_settings_round_trip_independently() {
    let mut mask = LocalMask::new(MaskKind::Fullscreen, 1);
    mask.effect = MaskEffect::TiltShift;
    mask.effect_settings.lens_blur.radius = 21.5;
    mask.effect_settings.lens_blur.blades = 7.0;
    mask.effect_settings.motion_blur.distance = 54.0;
    mask.effect_settings.motion_blur.angle = -32.0;
    mask.effect_settings.radial_blur.mode = RadialBlurMode::Spin;
    mask.effect_settings.radial_blur.center = [42.0, 63.0];
    mask.effect_settings.tilt_shift.radius = 18.5;
    mask.effect_settings.tilt_shift.center = [48.0, 61.0];
    mask.effect_settings.tilt_shift.angle = 17.0;
    mask.effect_settings.tilt_shift.focus_width = 31.0;

    let encoded = serde_json::to_string(&mask).expect("serialize focus blur effects");
    let decoded: LocalMask =
        serde_json::from_str(&encoded).expect("deserialize focus blur effects");
    assert_eq!(decoded.effect, MaskEffect::TiltShift);
    assert_eq!(decoded.effect_settings.lens_blur.radius, 21.5);
    assert_eq!(decoded.effect_settings.lens_blur.blades, 7.0);
    assert_eq!(decoded.effect_settings.motion_blur.distance, 54.0);
    assert_eq!(decoded.effect_settings.motion_blur.angle, -32.0);
    assert_eq!(
        decoded.effect_settings.radial_blur.mode,
        RadialBlurMode::Spin
    );
    assert_eq!(decoded.effect_settings.radial_blur.center, [42.0, 63.0]);
    assert_eq!(decoded.effect_settings.tilt_shift.radius, 18.5);
    assert_eq!(decoded.effect_settings.tilt_shift.center, [48.0, 61.0]);
    assert_eq!(decoded.effect_settings.tilt_shift.angle, 17.0);
    assert_eq!(decoded.effect_settings.tilt_shift.focus_width, 31.0);
}

#[test]
fn atmosphere_settings_round_trip_without_touching_local_adjustments() {
    let mut mask = LocalMask::new(MaskKind::Fullscreen, 1);
    mask.effect = MaskEffect::Smoke;
    mask.effect_settings.fog.density = 73.0;
    mask.effect_settings.fog.seed = 217.0;
    mask.effect_settings.fog.color = [0.72, 0.81, 0.93];
    mask.effect_settings.smoke.turbulence = 84.0;
    mask.effect_settings.smoke.angle = 32.0;
    mask.effect_settings.smoke.seed = 491.0;
    mask.effect_settings.smoke.color = [0.18, 0.21, 0.24];
    mask.adjustments.exposure = 1.25;

    let encoded = serde_json::to_string(&mask).expect("serialize atmosphere effects");
    let decoded: LocalMask =
        serde_json::from_str(&encoded).expect("deserialize atmosphere effects");
    assert_eq!(decoded.effect, MaskEffect::Smoke);
    assert_eq!(decoded.effect_settings.fog.density, 73.0);
    assert_eq!(decoded.effect_settings.fog.seed, 217.0);
    assert_eq!(decoded.effect_settings.fog.color, [0.72, 0.81, 0.93]);
    assert_eq!(decoded.effect_settings.smoke.turbulence, 84.0);
    assert_eq!(decoded.effect_settings.smoke.angle, 32.0);
    assert_eq!(decoded.effect_settings.smoke.seed, 491.0);
    assert_eq!(decoded.effect_settings.smoke.color, [0.18, 0.21, 0.24]);
    assert_eq!(decoded.adjustments.exposure, 1.25);
}

#[test]
fn brush_opacity_is_captured_only_when_enabled_for_paint_and_erase() {
    assert_eq!(BrushMode::Paint.dab_opacity(false, 0.25), 1.0);
    assert_eq!(BrushMode::Erase.dab_opacity(false, 0.25), -1.0);
    assert_eq!(BrushMode::Paint.dab_opacity(true, 0.25), 0.25);
    assert_eq!(BrushMode::Erase.dab_opacity(true, 0.25), -0.25);
}

#[test]
fn legacy_brush_geometry_defaults_to_full_strength_opacity() {
    let geometry: MaskGeometry =
        serde_json::from_str(r#"{"Brush":{"size":0.055,"feather":0.55,"dabs":[]}}"#).unwrap();
    let MaskGeometry::Brush {
        opacity_enabled,
        opacity,
        overlap_enabled,
        stroke_starts,
        ..
    } = geometry
    else {
        panic!("legacy brush JSON must decode as brush geometry");
    };
    assert!(!opacity_enabled);
    assert_eq!(opacity, 1.0);
    assert!(overlap_enabled);
    assert!(stroke_starts.is_empty());
}

#[test]
fn overlap_builds_between_strokes_but_not_between_dabs_in_one_stroke() {
    let dabs = [
        BrushDab {
            center: [0.5, 0.5],
            opacity: 0.1,
            size: 0.2,
            feather: 0.2,
        },
        BrushDab {
            center: [0.5, 0.5],
            opacity: 0.1,
            size: 0.2,
            feather: 0.2,
        },
    ];
    let center = 16 * 32 + 16;

    let one_stroke = rasterize_recorded_brush(
        MaskRasterSpace::new(32, 32, 100, 100),
        &dabs,
        true,
        &[0],
    );
    assert!((one_stroke[center] - 0.1).abs() < 0.01);

    let overlapping_strokes = rasterize_recorded_brush(
        MaskRasterSpace::new(32, 32, 100, 100),
        &dabs,
        true,
        &[0, 1],
    );
    assert!((overlapping_strokes[center] - 0.19).abs() < 0.01);

    let overlap_disabled = rasterize_recorded_brush(
        MaskRasterSpace::new(32, 32, 100, 100),
        &dabs,
        false,
        &[0, 1],
    );
    assert!((overlap_disabled[center] - 0.1).abs() < 0.01);
}

#[test]
fn eraser_opacity_builds_between_strokes_not_between_dabs() {
    let dabs = [
        BrushDab {
            center: [0.5, 0.5],
            opacity: 1.0,
            size: 0.2,
            feather: 0.2,
        },
        BrushDab {
            center: [0.5, 0.5],
            opacity: -0.1,
            size: 0.2,
            feather: 0.2,
        },
        BrushDab {
            center: [0.5, 0.5],
            opacity: -0.1,
            size: 0.2,
            feather: 0.2,
        },
    ];
    let center = 16 * 32 + 16;

    let one_eraser_stroke = rasterize_recorded_brush(
        MaskRasterSpace::new(32, 32, 100, 100),
        &dabs,
        true,
        &[0, 1],
    );
    assert!((one_eraser_stroke[center] - 0.9).abs() < 0.01);

    let two_eraser_strokes = rasterize_recorded_brush(
        MaskRasterSpace::new(32, 32, 100, 100),
        &dabs,
        true,
        &[0, 1, 2],
    );
    assert!((two_eraser_strokes[center] - 0.81).abs() < 0.01);
}

#[test]
fn cropped_mask_remaps_geometry_to_the_visible_region() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Radial);
    if let MaskGeometry::Radial {
        center,
        radius,
        initialized,
        ..
    } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *center = [0.75, 0.5];
        *radius = [0.1, 0.2];
        *initialized = true;
    }

    let cropped = stack.cropped_for_region(50, 0, 50, 100, 100, 100);
    let MaskGeometry::Radial { center, radius, .. } =
        &cropped.selected_component().unwrap().geometry
    else {
        panic!("expected radial mask");
    };
    assert!((center[0] - 0.5).abs() < 1e-6);
    assert!((center[1] - 0.5).abs() < 1e-6);
    assert!((radius[0] - 0.2).abs() < 1e-6);
    assert!((radius[1] - 0.2).abs() < 1e-6);
}

#[test]
fn cropped_ai_mask_keeps_full_frame_feather_width() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Subject);
    let mut pixels = vec![0; 128 * 128];
    for y in 32..96 {
        for x in 40..88 {
            pixels[y * 128 + x] = 255;
        }
    }
    if let MaskGeometry::Ai { mask, feather, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = MaskImage::new(128, 128, pixels);
        *feather = 0.8;
    }

    let full = stack.rasterize_layer(0, 128, 128, 128, 128);
    // Partial-raster callers retain the shaping halo around the viewport.
    let cropped = stack.cropped_for_region(24, 24, 80, 80, 128, 128);
    let crop = cropped.rasterize_layer(0, 80, 80, 80, 80);
    for y in 0..80 {
        let full_start = (y + 24) * 128 + 24;
        assert_eq!(
            &crop[y * 80..(y + 1) * 80],
            &full[full_start..full_start + 80]
        );
    }
}

#[test]
fn cropped_low_resolution_matte_keeps_full_frame_subpixel_alignment() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Subject);
    let pixels = (0..29)
        .flat_map(|y| (0..37).map(move |x| ((x * 17 + y * 31) % 256) as u8))
        .collect();
    if let MaskGeometry::Ai { mask, .. } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = MaskImage::new(37, 29, pixels);
    }

    let full = stack.rasterize_layer(0, 128, 96, 128, 96);
    let cropped = stack.cropped_for_region(24, 18, 80, 60, 128, 96);
    let crop = cropped.rasterize_layer(0, 80, 60, 80, 60);
    for y in 0..60 {
        for x in 0..80 {
            let expected = full[(y + 18) * 128 + x + 24];
            assert!(crop[y * 80 + x].abs_diff(expected) <= 1);
        }
    }
}

#[test]
fn radial_layer_has_soft_center_and_clear_corners() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Radial);
    if let MaskGeometry::Radial { initialized, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        *initialized = true;
    }
    let layer = stack.rasterize_layer(0, 64, 64, 100, 100);
    assert!(layer[32 * 64 + 32] > 240);
    assert!(layer[0] < 8);
}

#[test]
fn centered_brush_is_symmetric_on_even_atlas() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Brush);
    if let MaskGeometry::Brush { dabs, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        dabs.push(BrushDab {
            center: [0.5, 0.5],
            size: 0.2,
            feather: 0.5,
            opacity: 1.0,
        });
    }
    let layer = stack.rasterize_layer(0, 32, 32, 100, 100);
    assert_eq!(layer[15 * 32 + 15], layer[15 * 32 + 16]);
    assert_eq!(layer[16 * 32 + 15], layer[16 * 32 + 16]);
}

#[test]
fn inpaint_brush_mask_is_binary_and_ignores_feather() {
    let hard = rasterize_inpaint_dabs_binary(
        64,
        64,
        64,
        64,
        &[BrushDab {
            center: [0.5, 0.5],
            size: 0.2,
            feather: 0.0,
            opacity: 1.0,
        }],
    );
    let formerly_soft = rasterize_inpaint_dabs_binary(
        64,
        64,
        64,
        64,
        &[BrushDab {
            center: [0.5, 0.5],
            size: 0.2,
            feather: 1.0,
            opacity: 1.0,
        }],
    );
    assert_eq!(hard, formerly_soft);
    assert!(hard.iter().all(|&value| value == 0 || value == 255));
    assert_eq!(hard[32 * 64 + 32], 255);
    assert_eq!(hard[0], 0);
}

#[test]
fn new_linear_inpaint_patch_preserves_soft_composite_alpha() {
    let rgba16f = vec![0u16; 4];
    let patch = InpaintPatch::new_linear(1, 1, 0, 0, 1, 1, rgba16f, vec![128]).unwrap();
    let (_, alpha) = patch.sample_linear_rec2020_bilinear(0.0, 0.0).unwrap();
    assert!((alpha - 128.0 / 255.0).abs() < 1e-6);
}

#[test]
fn resampled_inpaint_patch_maps_native_raster_over_full_resolution_extent() {
    use half::f16;

    let pixel = |red: f32| {
        [
            f16::from_f32(red).to_bits(),
            f16::from_f32(0.0).to_bits(),
            f16::from_f32(0.0).to_bits(),
            f16::from_f32(1.0).to_bits(),
        ]
    };
    let rgba16f = pixel(0.25).into_iter().chain(pixel(0.75)).collect();
    let patch = InpaintPatch::new_linear_resampled(
        [10, 10],
        [2, 3],
        [4, 2],
        [2, 1],
        rgba16f,
        vec![255, 255],
    )
    .unwrap();
    assert_eq!(patch.raster_dimensions(), [2, 1]);
    assert!((patch.sample_linear_rec2020_bilinear(2.0, 3.0).unwrap().0[0] - 0.25).abs() < 1e-3);
    assert!((patch.sample_linear_rec2020_bilinear(5.0, 4.0).unwrap().0[0] - 0.75).abs() < 1e-3);
    assert!(patch.sample_linear_rec2020_bilinear(6.0, 4.0).is_none());
}

#[test]
fn missing_resampled_dimensions_keep_legacy_patch_layout() {
    let patch =
        InpaintPatch::new_linear(2, 2, 0, 0, 2, 2, vec![0u16; 16], vec![255; 4]).unwrap();
    let mut document = serde_json::to_value(&patch).unwrap();
    document.as_object_mut().unwrap().remove("raster_width");
    document.as_object_mut().unwrap().remove("raster_height");
    let restored: InpaintPatch = serde_json::from_value(document).unwrap();
    assert_eq!(restored.raster_dimensions(), [2, 2]);
    assert!(restored.is_valid());
}

#[test]
fn inpaint_patch_rejects_partial_or_non_finite_linear_payloads() {
    use half::f16;

    let mut partial =
        InpaintPatch::new_linear(1, 1, 0, 0, 1, 1, vec![0u16; 4], vec![255]).unwrap();
    partial.rgba16f = vec![0u16; 3].into();
    assert!(!partial.is_valid());

    let mut incomplete_raster =
        InpaintPatch::new_linear(1, 1, 0, 0, 1, 1, vec![0u16; 4], vec![255]).unwrap();
    incomplete_raster.raster_width = 1;
    assert!(!incomplete_raster.is_valid());

    let mut non_finite =
        InpaintPatch::new_linear(1, 1, 0, 0, 1, 1, vec![0u16; 4], vec![255]).unwrap();
    Arc::make_mut(&mut non_finite.rgba16f)[0] = f16::NAN.to_bits();
    assert!(!non_finite.is_valid());
    assert!(non_finite
        .sample_linear_rec2020_bilinear(0.0, 0.0)
        .is_none());
}

#[test]
fn legacy_linear_inpaint_patch_is_mapped_from_camera_rgb() {
    let mut patch =
        InpaintPatch::new_linear(1, 1, 0, 0, 1, 1, vec![0u16; 4], vec![255]).unwrap();
    let matrix = [
        [2.0, 0.0, 0.0, 0.0],
        [0.0, 3.0, 0.0, 0.0],
        [0.0, 0.0, 4.0, 0.0],
    ];
    let current = patch.resolve_neutral_working_rgb([0.1, 0.2, 0.3], matrix);
    assert_eq!(current, [0.1, 0.2, 0.3]);

    patch.working_space_version = 0;
    let migrated = patch.resolve_neutral_working_rgb([0.1, 0.2, 0.3], matrix);
    assert_eq!(migrated, [0.2, 0.6, 1.2]);
}

#[test]
fn missing_inpaint_working_space_version_loads_as_legacy() {
    let patch = InpaintPatch::new_linear(1, 1, 0, 0, 1, 1, vec![0u16; 4], vec![255]).unwrap();
    let mut document = serde_json::to_value(patch).unwrap();
    document
        .as_object_mut()
        .unwrap()
        .remove("working_space_version");
    let legacy: InpaintPatch = serde_json::from_value(document).unwrap();
    assert!(legacy.needs_legacy_camera_to_working());
}

#[test]
fn brush_eraser_removes_existing_coverage() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Brush);
    if let MaskGeometry::Brush { dabs, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        dabs.push(BrushDab {
            center: [0.5, 0.5],
            size: 0.25,
            feather: 0.2,
            opacity: 1.0,
        });
        dabs.push(BrushDab {
            center: [0.5, 0.5],
            size: 0.1,
            feather: 0.2,
            opacity: -1.0,
        });
    }
    let layer = stack.rasterize_layer(0, 64, 64, 100, 100);
    assert!(layer[32 * 64 + 32] < 8);
    assert!(layer[32 * 64 + 40] > 200);
}

#[test]
fn partial_brush_and_eraser_dabs_change_only_stored_stroke_coverage() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Brush);
    if let MaskGeometry::Brush { dabs, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        dabs.push(BrushDab {
            center: [0.5, 0.5],
            size: 0.25,
            feather: 0.2,
            opacity: 0.4,
        });
    }
    let painted = stack.rasterize_layer_coverage(0, 64, 64, 100, 100);
    assert!((painted[32 * 64 + 32] - 0.4).abs() < 0.01);
    assert_eq!(stack.masks[0].opacity, 1.0);

    if let MaskGeometry::Brush { dabs, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        dabs.push(BrushDab {
            center: [0.5, 0.5],
            size: 0.25,
            feather: 0.2,
            opacity: -0.5,
        });
    }
    let erased = stack.rasterize_layer_coverage(0, 64, 64, 100, 100);
    assert!((erased[32 * 64 + 32] - 0.2).abs() < 0.01);
    assert_eq!(stack.masks[0].opacity, 1.0);
}

#[test]
fn reordering_tracks_selected_mask_and_component() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Brush);
    stack.add_mask(MaskKind::Radial);
    stack.add_mask(MaskKind::Linear);
    assert!(stack.move_mask(2, 0));
    assert_eq!(stack.selected_mask, Some(0));
    assert_eq!(stack.masks[0].components[0].kind, MaskKind::Linear);

    stack.add_component(MaskKind::Brush, MaskCombineMode::Subtract);
    assert!(stack.move_component(1, 0));
    assert_eq!(stack.selected_component, Some(0));
    assert_eq!(stack.masks[0].components[0].kind, MaskKind::Brush);
}

#[test]
fn background_reuses_and_inverts_subject_probability() {
    let subject = MaskImage::new(2, 1, vec![0, 255]).unwrap();
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Subject);
    if let MaskGeometry::Ai { mask, .. } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = Some(subject.clone());
    }
    stack.add_mask(MaskKind::Background);
    if let MaskGeometry::Ai { mask, .. } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = Some(subject);
    }
    let foreground = stack.rasterize_layer(0, 2, 1, 2, 1);
    let background = stack.rasterize_layer(1, 2, 1, 2, 1);
    assert_eq!(foreground, vec![0, 255]);
    assert_eq!(background, vec![255, 0]);
}

#[test]
fn feathered_background_is_the_exact_subject_complement() {
    let mut pixels = vec![0u8; 8 * 8];
    for y in 2..6 {
        for x in 2..6 {
            pixels[y * 8 + x] = 255;
        }
    }
    let subject = MaskImage::new(8, 8, pixels).unwrap();
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Subject);
    if let MaskGeometry::Ai { mask, feather, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = Some(subject.clone());
        *feather = 0.65;
    }
    stack.add_mask(MaskKind::Background);
    if let MaskGeometry::Ai { mask, feather, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = Some(subject);
        *feather = 0.65;
    }

    let foreground = stack.rasterize_layer(0, 96, 64, 800, 533);
    let background = stack.rasterize_layer(1, 96, 64, 800, 533);
    assert!(foreground
        .iter()
        .zip(background.iter())
        .all(|(subject, not_subject)| *subject as u16 + *not_subject as u16 == 255));
}

#[test]
fn shared_subject_refinement_updates_subject_and_background_as_exact_inverses() {
    let raw = MaskImage::new(32, 32, vec![128; 32 * 32]).unwrap();
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Subject);
    if let MaskGeometry::Ai { mask, .. } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = Some(raw.clone());
    }
    stack.add_mask(MaskKind::Background);
    if let MaskGeometry::Ai { mask, .. } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = Some(raw);
    }
    stack.subject_refinement.stroke_starts.push(0);
    stack.subject_refinement.dabs.push(BrushDab {
        center: [0.5, 0.5],
        opacity: 0.5,
        size: 0.25,
        feather: 0.5,
    });

    let subject = stack.rasterize_layer(0, 32, 32, 32, 32);
    let background = stack.rasterize_layer(1, 32, 32, 32, 32);
    assert!(subject[16 * 32 + 16] > 128);
    assert!(subject
        .iter()
        .zip(background.iter())
        .all(|(subject, not_subject)| *subject as u16 + *not_subject as u16 == 255));

    stack.subject_refinement.stroke_starts.push(1);
    stack.subject_refinement.dabs.push(BrushDab {
        center: [0.5, 0.5],
        opacity: -1.0,
        size: 0.12,
        feather: 0.0,
    });
    let subject_after_subtract = stack.rasterize_layer(0, 32, 32, 32, 32);
    assert!(subject_after_subtract[16 * 32 + 16] < subject[16 * 32 + 16]);

    // Replacing the raw BiRefNet result (for example after a quality-tier
    // switch) must not consume or clear the shared refinement history.
    let regenerated = MaskImage::new(32, 32, vec![64; 32 * 32]).unwrap();
    for mask in &mut stack.masks {
        if let MaskGeometry::Ai { mask: target, .. } = &mut mask.components[0].geometry {
            *target = Some(regenerated.clone());
        }
    }
    let regenerated_subject = stack.rasterize_layer(0, 32, 32, 32, 32);
    let regenerated_background = stack.rasterize_layer(1, 32, 32, 32, 32);
    assert!(regenerated_subject
        .iter()
        .zip(regenerated_background.iter())
        .all(|(subject, not_subject)| *subject as u16 + *not_subject as u16 == 255));

    stack.add_mask(MaskKind::Subject);
    if let MaskGeometry::Ai { mask, .. } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = Some(regenerated);
    }
    let inherited = stack.rasterize_layer(2, 32, 32, 32, 32);
    assert_eq!(inherited, regenerated_subject);
}

#[test]
fn subject_refinement_composite_applies_signed_delta_to_raw_probability() {
    let raw = MaskImage::new(16, 16, vec![128; 16 * 16]).unwrap();
    let refinement = SubjectRefinement {
        stroke_starts: vec![0],
        dabs: vec![BrushDab {
            center: [0.5, 0.5],
            opacity: 0.4,
            size: 0.3,
            feather: 0.0,
        }],
        ..Default::default()
    };
    let refined = refinement.composite(&raw).unwrap();
    assert!(refined.pixels[8 * 16 + 8] > 128);
    assert_eq!(refined.pixels[0], 128);
}

#[test]
fn legacy_mask_stack_without_subject_refinement_deserializes_empty_layer() {
    let stack: MaskStack =
        serde_json::from_str(r#"{"masks":[],"selected_mask":null,"selected_component":null}"#)
            .unwrap();
    assert!(stack.subject_refinement.is_empty());
    assert_eq!(stack.subject_refinement, SubjectRefinement::default());
}

#[test]
fn local_adjustments_without_hue_deserialize_to_a_neutral_rotation() {
    let mut serialized =
        serde_json::to_value(LocalAdjustments::default()).expect("serialize adjustments");
    serialized
        .as_object_mut()
        .expect("local adjustments are a JSON object")
        .remove("hue");
    let decoded: LocalAdjustments =
        serde_json::from_value(serialized).expect("deserialize legacy adjustments");
    assert_eq!(decoded.hue, 0.0);
    assert!(decoded.is_neutral());
}

#[test]
fn grow_expands_ai_mask_coverage() {
    let mut coverage = vec![0.0; 64 * 64];
    for y in 28..36 {
        for x in 28..36 {
            coverage[y * 64 + x] = 1.0;
        }
    }
    let original_covered = coverage.iter().filter(|value| **value >= 0.5).count();
    shape_probability_mask(&mut coverage, 64, 64, 0.5, 0.0);
    let grown_covered = coverage.iter().filter(|value| **value >= 0.5).count();
    assert!(grown_covered > original_covered);
}

#[test]
fn negative_grow_contracts_ai_mask_coverage() {
    let mut coverage = vec![0.0; 64 * 64];
    for y in 20..44 {
        for x in 20..44 {
            coverage[y * 64 + x] = 1.0;
        }
    }
    let original_covered = coverage.iter().filter(|value| **value >= 0.5).count();
    shape_probability_mask(&mut coverage, 64, 64, -0.5, 0.0);
    let contracted_covered = coverage.iter().filter(|value| **value >= 0.5).count();
    assert!(contracted_covered < original_covered);
}

#[test]
fn feather_preserves_a_soft_transition_after_growing() {
    let mut coverage = vec![0.0; 64 * 64];
    for y in 20..44 {
        for x in 20..44 {
            coverage[y * 64 + x] = 1.0;
        }
    }

    shape_probability_mask(&mut coverage, 64, 64, 0.3, 0.7);
    assert!(coverage.iter().any(|value| *value > 0.0 && *value < 1.0));
}

#[test]
fn ai_feather_preserves_the_half_alpha_contour() {
    let mut hard = vec![0.0; 96 * 64];
    for y in 12..52 {
        for x in 23..73 {
            hard[y * 96 + x] = 1.0;
        }
    }
    let original_selected = hard.iter().filter(|value| **value >= 0.5).count();
    shape_probability_mask(&mut hard, 96, 64, 0.0, 1.0);
    let feathered_selected = hard.iter().filter(|value| **value >= 0.5).count();

    assert_eq!(feathered_selected, original_selected);
    assert_eq!(hard[32 * 96 + 48], 1.0);
    assert_eq!(hard[2 * 96 + 2], 0.0);
    assert!(hard.iter().any(|value| *value > 0.0 && *value < 1.0));
}

#[test]
fn luminance_and_color_ranges_use_the_cached_preview() {
    let source = MaskRgbImage::new(2, 1, vec![0, 0, 0, 255, 255, 0, 0, 255]).unwrap();
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::LuminanceRange);
    if let MaskGeometry::LuminanceRange {
        source: target,
        low,
        high,
        ..
    } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *target = Some(source.clone());
        *low = 0.1;
        *high = 0.4;
    }
    let luminance = stack.rasterize_layer(0, 2, 1, 2, 1);
    assert!(luminance[0] < 8);
    assert!(luminance[1] > 240);

    stack.add_mask(MaskKind::ColorRange);
    if let MaskGeometry::ColorRange {
        source: target,
        sample,
        tolerance,
        sampled,
        ..
    } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *target = Some(source);
        *sample = [1.0, 0.0, 0.0];
        *tolerance = 0.1;
        *sampled = true;
    }
    let color = stack.rasterize_layer(1, 2, 1, 2, 1);
    assert!(color[0] < 8);
    assert!(color[1] > 240);
}

#[test]
fn grow_expands_luminance_and_color_range_masks() {
    let width = 64;
    let height = 64;
    let mut rgba = vec![0; width * height * 4];
    for pixel in rgba.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    for y in 28..36 {
        for x in 28..36 {
            let index = (y * width + x) * 4;
            rgba[index] = 255;
        }
    }
    let source = MaskRgbImage::new(width as u32, height as u32, rgba).unwrap();
    let covered = |coverage: Vec<u8>| coverage.iter().filter(|value| **value >= 128).count();

    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::LuminanceRange);
    if let MaskGeometry::LuminanceRange {
        source: target,
        low,
        high,
        grow,
        feather,
    } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *target = Some(source.clone());
        *low = 0.15;
        *high = 0.3;
        *grow = 0.0;
        *feather = 0.0;
    }
    let original_luminance = covered(stack.rasterize_layer(0, 64, 64, 64, 64));
    if let MaskGeometry::LuminanceRange { grow, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        *grow = 0.5;
    }
    let grown_luminance = covered(stack.rasterize_layer(0, 64, 64, 64, 64));
    assert!(grown_luminance > original_luminance);

    stack.add_mask(MaskKind::ColorRange);
    if let MaskGeometry::ColorRange {
        source: target,
        sample,
        tolerance,
        grow,
        feather,
        sampled,
    } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *target = Some(source);
        *sample = [1.0, 0.0, 0.0];
        *tolerance = 0.05;
        *grow = 0.0;
        *feather = 0.0;
        *sampled = true;
    }
    let original_color = covered(stack.rasterize_layer(1, 64, 64, 64, 64));
    if let MaskGeometry::ColorRange { grow, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        *grow = 0.5;
    }
    let grown_color = covered(stack.rasterize_layer(1, 64, 64, 64, 64));
    assert!(grown_color > original_color);
}

#[test]
fn missing_range_grow_values_default_to_zero() {
    let luminance: MaskGeometry =
        serde_json::from_str(r#"{"LuminanceRange":{"low":0.2,"high":0.8,"feather":0.15}}"#)
            .unwrap();
    assert!(matches!(
        luminance,
        MaskGeometry::LuminanceRange { grow: 0.0, .. }
    ));

    let color: MaskGeometry = serde_json::from_str(
        r#"{"ColorRange":{"sample":[0.5,0.5,0.5],"tolerance":0.18,"feather":0.12,"sampled":true}}"#,
    )
    .unwrap();
    assert!(matches!(color, MaskGeometry::ColorRange { grow: 0.0, .. }));
}

#[test]
fn group_invert_is_the_exact_final_mask_complement() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Radial);
    if let MaskGeometry::Radial { initialized, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        *initialized = true;
    }
    let normal = stack.rasterize_layer(0, 64, 64, 100, 100);
    stack.masks[0].invert = true;
    let inverted = stack.rasterize_layer(0, 64, 64, 100, 100);
    assert!(normal
        .iter()
        .zip(inverted.iter())
        .all(|(normal, inverted)| *normal as u16 + *inverted as u16 == 255));
}

#[test]
fn subtract_component_removes_coverage() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Radial);
    if let MaskGeometry::Radial { initialized, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        *initialized = true;
    }
    stack.add_component(MaskKind::Brush, MaskCombineMode::Subtract);
    if let MaskGeometry::Brush { dabs, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        dabs.push(BrushDab::default());
    }
    let layer = stack.rasterize_layer(0, 64, 64, 100, 100);
    assert!(layer[32 * 64 + 32] < 32);
}
#[test]
fn object_prompt_overlay_uses_a_hard_edged_brush() {
    let point = [0.45, 0.6];
    let size = 0.12;

    let mut hard_brush_stack = MaskStack::default();
    hard_brush_stack.add_mask(MaskKind::Brush);
    if let MaskGeometry::Brush { dabs, .. } =
        &mut hard_brush_stack.selected_component_mut().unwrap().geometry
    {
        dabs.push(BrushDab {
            center: point,
            size,
            feather: 0.0,
            opacity: 1.0,
        });
    }

    let mut object_stack = MaskStack::default();
    object_stack.add_mask(MaskKind::Object);
    if let MaskGeometry::Object {
        mask,
        brush_size,
        strokes,
        ..
    } = &mut object_stack.selected_component_mut().unwrap().geometry
    {
        *mask = None;
        *brush_size = size;
        strokes.push(ObjectStroke {
            points: vec![point],
            positive: true,
            brush_size: 0.0,
        });
    }

    let hard_brush = hard_brush_stack.rasterize_component_layer(0, 0, 96, 64, 960, 640);
    let object = object_stack.rasterize_component_layer(0, 0, 96, 64, 960, 640);
    assert_eq!(object, hard_brush);
}

#[test]
fn object_masks_are_available_and_rasterize_soft_probabilities() {
    let mut stack = MaskStack::default();
    assert!(MaskKind::Object.is_available());
    stack.add_mask(MaskKind::Object);
    if let MaskGeometry::Object {
        mask,
        feather,
        strokes,
        ..
    } = &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = MaskImage::new(2, 1, vec![0, 255]);
        *feather = 0.0;
        strokes.push(ObjectStroke {
            points: vec![[0.75, 0.5]],
            positive: true,
            brush_size: 0.0,
        });
    } else {
        panic!("object mask used unexpected geometry");
    }
    assert!(stack
        .selected_component()
        .unwrap()
        .geometry
        .is_initialized());
    let layer = stack.rasterize_layer(0, 2, 1, 2, 1);
    assert_eq!(layer, [0, 255]);
}

#[test]
fn zero_feather_object_mask_preserves_refined_alpha() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Object);
    if let MaskGeometry::Object { mask, feather, .. } =
        &mut stack.selected_component_mut().unwrap().geometry
    {
        *mask = MaskImage::new(5, 1, vec![0, 32, 127, 128, 255]);
        *feather = 0.0;
    } else {
        panic!("object mask used unexpected geometry");
    }

    let layer = stack.rasterize_layer(0, 5, 1, 5, 1);
    assert_eq!(layer, [0, 32, 127, 128, 255]);
}

#[test]
fn inpaint_patches_remain_sparse_and_full_resolution() {
    use half::f16;
    let rgba16f = vec![f16::from_f32(0.5).to_bits(); 8];
    let patch = InpaintPatch::new_linear(6000, 4000, 2500, 1800, 2, 1, rgba16f, vec![255, 255])
        .unwrap();
    let stroke = InpaintStroke::from_result(vec![BrushDab::default()], patch.clone()).unwrap();
    let composed = compose_inpaint_strokes(&[stroke]).unwrap();
    assert_eq!(composed.patches.len(), 1);
    assert_eq!(composed.patches[0].source_width, 6000);
    assert_eq!(composed.patches[0].source_height, 4000);
    assert_eq!(composed.patches[0].x, 2500);
    assert_eq!(composed.patches[0].rgba16f, patch.rgba16f);
}

#[test]
fn later_inpaint_stroke_remains_last_for_overwrite_order() {
    use half::f16;
    let make_stroke = |value: f32| {
        let rgba16f = vec![f16::from_f32(value).to_bits(); 4];
        let patch = InpaintPatch::new_linear(2, 2, 1, 1, 1, 1, rgba16f, vec![255]).unwrap();
        InpaintStroke::from_result(Vec::new(), patch).unwrap()
    };
    let first = make_stroke(0.25);
    let second = make_stroke(0.75);
    let both = compose_inpaint_strokes(&[first.clone(), second.clone()]).unwrap();
    assert_eq!(both.patches.len(), 2);
    assert_eq!(both.patches[1], second.patch);
    let after_delete = compose_inpaint_strokes(std::slice::from_ref(&first)).unwrap();
    assert_eq!(after_delete.patches[0], first.patch);
}

#[test]
fn clone_patch_copies_source_and_keeps_sparse_coverage() {
    let edge = 16u32;
    let mut destination = vec![0.0f32; (edge * edge * 3) as usize];
    let mut source = vec![0.0f32; (edge * edge * 3) as usize];
    for y in 0..edge {
        for x in 0..edge {
            let index = ((y * edge + x) * 3) as usize;
            destination[index..index + 3].copy_from_slice(&[0.1, 0.2, 0.3]);
            source[index..index + 3].copy_from_slice(&[
                x as f32 / edge as f32,
                y as f32 / edge as f32,
                0.75,
            ]);
        }
    }
    let dabs = [BrushDab {
        center: [0.4, 0.5],
        opacity: 1.0,
        size: 0.12,
        feather: 0.0,
    }];
    let patch = build_retouch_patch(
        InpaintStrokeKind::Clone,
        [100, 100],
        [0, 0],
        [100, 100],
        &destination,
        [0, 0],
        [100, 100],
        &source,
        [edge, edge],
        [0.2, 0.0],
        &dabs,
    )
    .unwrap();
    assert!(patch.width < 100);
    assert!(patch.height < 100);
    assert!(patch.mask.contains(&255));
    let center = (patch.raster_dimensions()[0] * patch.raster_dimensions()[1] / 2) as usize;
    let rgb = patch.linear_rgba16f_at(center).unwrap();
    assert!((f16::from_bits(rgb[0]).to_f32() - 0.6).abs() < 0.08);
    assert!((f16::from_bits(rgb[2]).to_f32() - 0.75).abs() < 0.01);
}

#[test]
fn clone_patch_can_sample_pixels_painted_by_earlier_dabs() {
    let mut image = Vec::new();
    for x in 0..8 {
        image.extend_from_slice(&[x as f32 / 10.0, 0.0, 0.0]);
    }
    let dabs = [
        BrushDab {
            center: [2.5 / 8.0, 0.5],
            opacity: 1.0,
            size: 0.49,
            feather: 0.0,
        },
        BrushDab {
            center: [3.5 / 8.0, 0.5],
            opacity: 1.0,
            size: 0.49,
            feather: 0.0,
        },
    ];
    let patch = build_retouch_patch(
        InpaintStrokeKind::Clone,
        [8, 1],
        [0, 0],
        [8, 1],
        &image,
        [0, 0],
        [8, 1],
        &image,
        [8, 1],
        [-1.0 / 8.0, 0.0],
        &dabs,
    )
    .unwrap();
    let (sample, alpha) = patch.sample_linear_rec2020_bilinear(3.0, 0.0).unwrap();
    assert!(alpha > 0.99);
    assert!((sample[0] - 0.1).abs() < 0.01, "sample was {sample:?}");
}

#[test]
fn heal_patch_transfers_texture_onto_destination_color() {
    let edge = 24u32;
    let pixels = (edge * edge) as usize;
    let destination = vec![0.6f32; pixels * 3];
    let mut source = vec![0.2f32; pixels * 3];
    for (index, pixel) in source.chunks_exact_mut(3).enumerate() {
        let detail = if (index + index / edge as usize).is_multiple_of(2) {
            0.04
        } else {
            -0.04
        };
        pixel.iter_mut().for_each(|channel| *channel += detail);
    }
    let dabs = [BrushDab {
        center: [0.5, 0.5],
        opacity: 1.0,
        size: 0.18,
        feather: 0.0,
    }];
    let patch = build_retouch_patch(
        InpaintStrokeKind::Heal,
        [100, 100],
        [0, 0],
        [100, 100],
        &destination,
        [0, 0],
        [100, 100],
        &source,
        [edge, edge],
        [0.0, 0.0],
        &dabs,
    )
    .unwrap();
    let values = patch
        .rgba16f
        .chunks_exact(4)
        .map(|pixel| f16::from_bits(pixel[0]).to_f32())
        .collect::<Vec<_>>();
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    assert!((mean - 0.6).abs() < 0.03, "healed mean was {mean}");
    let range = values.iter().copied().fold(f32::MIN, f32::max)
        - values.iter().copied().fold(f32::MAX, f32::min);
    assert!(range > 0.03, "source texture was lost: range {range}");
}

#[test]
fn submask_components_can_be_reordered_with_insertion_indices() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Brush);
    stack.add_component(MaskKind::Radial, MaskCombineMode::Add);
    stack.add_component(MaskKind::Linear, MaskCombineMode::Subtract);

    assert_eq!(stack.masks[0].components[1].kind, MaskKind::Radial);
    assert_eq!(stack.move_submask_component(0, 1, 0, 3), Some((0, 2)));
    assert_eq!(stack.masks[0].components[2].kind, MaskKind::Radial);
    assert_eq!(stack.selected_component, Some(2));
}

#[test]
fn submask_components_can_move_between_nonempty_groups() {
    let mut stack = MaskStack::default();
    stack.add_mask(MaskKind::Brush);
    stack.add_component(MaskKind::Radial, MaskCombineMode::Add);
    stack.add_mask(MaskKind::Linear);

    assert_eq!(stack.move_submask_component(0, 1, 1, 1), Some((1, 1)));
    assert_eq!(stack.masks[0].components.len(), 1);
    assert_eq!(stack.masks[1].components[1].kind, MaskKind::Radial);
    assert_eq!(stack.selected_mask, Some(1));
    assert_eq!(stack.selected_component, Some(1));
    assert_eq!(stack.move_submask_component(0, 0, 1, 0), None);
}

#[test]
fn landscape_masks_are_available_persisted_and_rasterized() {
    let mut stack = MaskStack::default();
    assert_eq!(stack.add_mask(MaskKind::Landscape), Some((0, 0)));
    let geometry = &mut stack.masks[0].components[0].geometry;
    let MaskGeometry::Landscape {
        mask,
        category,
        feather,
        ..
    } = geometry
    else {
        panic!("landscape mask used unexpected geometry");
    };
    *category = LandscapeCategory::Water;
    *mask = MaskImage::new(2, 1, vec![0, 255]);
    *feather = 0.0;
    assert_eq!(stack.rasterize_layer(0, 2, 1, 2, 1), [0, 255]);

    let json = serde_json::to_string(&stack).unwrap();
    let restored: MaskStack = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        restored.masks[0].components[0].geometry,
        MaskGeometry::Landscape {
            category: LandscapeCategory::Water,
            ..
        }
    ));
}

#[test]
fn mask_image_dimensions_are_checked_before_buffer_comparison() {
    assert!(MaskImage::new(0, 0, Vec::new()).is_some());
    assert!(MaskImage::new(2, 3, vec![0; 6]).is_some());
    assert!(MaskImage::new(2, 3, vec![0; 5]).is_none());
    assert!(MaskImage::new(u32::MAX, u32::MAX, Vec::new()).is_none());
}

#[test]
fn rgba_mask_image_dimensions_are_checked_before_buffer_comparison() {
    assert!(MaskRgbImage::new(0, 0, Vec::new()).is_some());
    assert!(MaskRgbImage::new(2, 3, vec![0; 24]).is_some());
    assert!(MaskRgbImage::new(2, 3, vec![0; 23]).is_none());
    assert!(MaskRgbImage::new(u32::MAX, u32::MAX, Vec::new()).is_none());
}
