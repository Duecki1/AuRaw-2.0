use super::*;
use crate::pipeline::MaskKind;
use std::time::{SystemTime, UNIX_EPOCH};

fn sample_edits() -> EditState {
    let mut exposure = ExposureParams::scene_referred_default();
    exposure.dehaze = 27.0;
    let mut masks = MaskStack::default();
    masks.add_mask(MaskKind::Radial);
    EditState {
        exposure,
        geometry: GeometryTransform::default(),
        camera_profile: None,
        masks: Arc::new(masks),
        subject_refinement: None,
        inpainting: Arc::new(Vec::new()),
        lens: LensEditState {
            enabled: true,
            maker: "Test Optics".to_owned(),
            model: "35 mm f/2".to_owned(),
        },
        ai_masks_need_update: false,
    }
}

#[test]
fn copied_adjustments_respect_category_settings_and_mark_ai_masks_stale() {
    let mut source = sample_edits();
    source.exposure.dehaze = 61.0;
    source.lens.enabled = false;
    let mut source_masks = MaskStack::default();
    source_masks.add_mask(MaskKind::Subject);
    if let MaskGeometry::Ai { mask, .. } = &mut source_masks.masks[0].components[0].geometry {
        *mask = Some(crate::pipeline::MaskImage::new(2, 2, vec![0, 64, 192, 255]).unwrap());
    }
    source.masks = Arc::new(source_masks);

    let mut destination = sample_edits();
    destination.exposure.dehaze = 7.0;
    let original_exposure = destination.exposure;
    let original_lens = destination.lens.clone();

    apply_copied_adjustments(
        &mut destination,
        &source,
        AdjustmentCopySettings {
            adjustments: false,
            geometry: false,
            camera_profile: false,
            masks: false,
            ai_masks: true,
            inpainting: false,
            lens_correction: false,
        },
    );

    assert_eq!(destination.exposure, original_exposure);
    assert_eq!(destination.lens, original_lens);
    // Merge mode preserves the destination's manual radial mask while
    // replacing only the selected AI category.
    assert_eq!(destination.masks.masks.len(), 2);
    assert_eq!(
        destination.masks.masks[1].components[0].kind,
        MaskKind::Subject
    );
    assert!(destination.ai_masks_need_update);
}

#[test]
fn copied_uncached_ai_masks_are_still_marked_stale() {
    let mut source = default_edit_state();
    let mut source_masks = MaskStack::default();
    source_masks.add_mask(MaskKind::Subject);
    // A generated bitmap is a cache. Pasted AI masks may not include one,
    // but their semantic component still has to be regenerated for the
    // destination image.
    assert!(matches!(
        &source_masks.masks[0].components[0].geometry,
        MaskGeometry::Ai { mask: None, .. }
    ));
    source.masks = Arc::new(source_masks);

    let mut destination = default_edit_state();
    apply_copied_adjustments(
        &mut destination,
        &source,
        AdjustmentCopySettings {
            adjustments: false,
            geometry: false,
            camera_profile: false,
            masks: false,
            ai_masks: true,
            inpainting: false,
            lens_correction: false,
        },
    );

    assert!(destination.ai_masks_need_update);
}

#[test]
fn manual_and_ai_masks_can_be_copied_independently() {
    let mut source = sample_edits();
    let mut source_masks = MaskStack::default();
    source_masks.add_mask(MaskKind::Brush);
    source_masks.add_mask(MaskKind::Subject);
    if let MaskGeometry::Ai { mask, .. } = &mut source_masks.masks[1].components[0].geometry {
        *mask = Some(crate::pipeline::MaskImage::new(2, 2, vec![0, 64, 192, 255]).unwrap());
    }
    source_masks.subject_refinement.stroke_starts.push(0);
    source_masks
        .subject_refinement
        .dabs
        .push(crate::pipeline::BrushDab {
            center: [0.5, 0.5],
            opacity: 0.6,
            size: 0.08,
            feather: 0.4,
        });
    source.subject_refinement = Some(source_masks.subject_refinement.clone());
    source.masks = Arc::new(source_masks);

    let mut destination = default_edit_state();
    apply_copied_adjustments(
        &mut destination,
        &source,
        AdjustmentCopySettings {
            adjustments: false,
            geometry: false,
            camera_profile: false,
            masks: true,
            ai_masks: false,
            inpainting: false,
            lens_correction: false,
        },
    );

    assert_eq!(destination.masks.masks.len(), 1);
    assert_eq!(
        destination.masks.masks[0].components[0].kind,
        MaskKind::Brush
    );
    assert!(!destination.ai_masks_need_update);
    assert!(destination.masks.subject_refinement.is_empty());
    assert!(destination.subject_refinement.is_none());

    apply_copied_adjustments(
        &mut destination,
        &source,
        AdjustmentCopySettings {
            adjustments: false,
            geometry: false,
            camera_profile: false,
            masks: false,
            ai_masks: true,
            inpainting: false,
            lens_correction: false,
        },
    );

    assert_eq!(destination.masks.masks.len(), 2);
    assert!(destination
        .masks
        .masks
        .iter()
        .any(|mask| mask.components[0].kind == MaskKind::Brush));
    assert!(destination
        .masks
        .masks
        .iter()
        .any(|mask| mask.components[0].kind == MaskKind::Subject));
    assert_eq!(
        destination.masks.subject_refinement,
        source.masks.subject_refinement
    );
    assert_eq!(destination.subject_refinement, source.subject_refinement);
    assert!(destination.ai_masks_need_update);
}

#[test]
fn mixed_mask_groups_do_not_copy_disabled_manual_components() {
    let mut source = default_edit_state();
    let mut masks = MaskStack::default();
    masks.add_mask(MaskKind::Brush);
    masks.add_component(MaskKind::Subject, crate::pipeline::MaskCombineMode::Add);
    source.masks = Arc::new(masks);

    let mut destination = default_edit_state();
    apply_copied_adjustments(
        &mut destination,
        &source,
        AdjustmentCopySettings {
            adjustments: false,
            geometry: false,
            camera_profile: false,
            masks: false,
            ai_masks: true,
            inpainting: false,
            lens_correction: false,
        },
    );

    assert_eq!(destination.masks.masks.len(), 1);
    assert_eq!(destination.masks.masks[0].components.len(), 1);
    assert_eq!(
        destination.masks.masks[0].components[0].kind,
        MaskKind::Subject
    );
    assert!(destination.ai_masks_need_update);

    apply_copied_adjustments(
        &mut destination,
        &source,
        AdjustmentCopySettings {
            adjustments: false,
            geometry: false,
            camera_profile: false,
            masks: true,
            ai_masks: false,
            inpainting: false,
            lens_correction: false,
        },
    );

    assert!(destination
        .masks
        .masks
        .iter()
        .any(|mask| mask.components.len() == 1 && mask.components[0].kind == MaskKind::Brush));
    assert!(destination
        .masks
        .masks
        .iter()
        .all(|mask| mask.components.len() == 1));
}

#[test]
fn copied_adjustments_include_camera_profile_and_replace_clears_other_categories() {
    let mut source = sample_edits();
    source.camera_profile = Some(PathBuf::from("Adobe/Camera Standard.dcp"));
    source.exposure.dehaze = 48.0;

    let mut destination = sample_edits();

    apply_copied_adjustments_with_mode(
        &mut destination,
        &source,
        AdjustmentCopySettings {
            adjustments: true,
            geometry: false,
            camera_profile: true,
            masks: false,
            ai_masks: false,
            inpainting: false,
            lens_correction: false,
        },
        AdjustmentPasteMode::Replace,
    );

    assert_eq!(destination.exposure.dehaze, 48.0);
    assert_eq!(
        destination.camera_profile,
        Some(PathBuf::from("Adobe/Camera Standard.dcp"))
    );
    assert!(destination.masks.masks.is_empty());
    assert!(destination.inpainting.is_empty());
    assert_eq!(destination.lens, LensEditState::default());
}

#[test]
fn stale_ai_mask_metadata_alone_is_not_an_edit_conflict() {
    let mut edits = default_edit_state();
    edits.ai_masks_need_update = true;
    assert!(!edit_state_has_adjustments(&edits));
}

#[test]
fn legacy_copy_settings_use_safe_category_defaults() {
    let settings: AdjustmentCopySettings = serde_json::from_str("{}").unwrap();
    assert_eq!(settings, AdjustmentCopySettings::default());
}

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "auraw-sidecar-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    directory
}

#[test]
fn sidecar_round_trip_preserves_edit_state() {
    let mut edits = sample_edits();
    edits.exposure.hue = 37.5;
    Arc::make_mut(&mut edits.masks).masks[0].adjustments.hue = -82.25;
    let encoded = encode(edits.clone()).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
    assert!(!loaded.migrated);
}

#[test]
fn fullscreen_effect_mask_round_trips_through_the_sidecar() {
    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Fullscreen).unwrap();
    masks.masks.last_mut().unwrap().effect = crate::pipeline::MaskEffect::Cartoon;

    let encoded = encode(edits.clone()).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
    let fullscreen = loaded.edits.masks.masks.last().unwrap();
    assert_eq!(fullscreen.components[0].kind, MaskKind::Fullscreen);
    assert!(matches!(
        fullscreen.components[0].geometry,
        MaskGeometry::Fullscreen
    ));
    assert_eq!(fullscreen.effect, crate::pipeline::MaskEffect::Cartoon);
}

#[test]
fn neon_mask_settings_round_trip_through_the_sidecar() {
    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Fullscreen).unwrap();
    let neon_mask = masks.masks.last_mut().unwrap();
    neon_mask.effect = crate::pipeline::MaskEffect::Neon;
    neon_mask.effect_settings.neon.amount = 48.0;
    neon_mask.effect_settings.neon.edge_width = 4.5;
    neon_mask.effect_settings.neon.color = [1.0, 0.15, 0.65];
    neon_mask.adjustments.exposure = 1.0;

    let encoded = encode(edits.clone()).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
    let neon_mask = loaded.edits.masks.masks.last().unwrap();
    assert_eq!(neon_mask.effect, crate::pipeline::MaskEffect::Neon);
    assert_eq!(neon_mask.effect_settings.neon.amount, 48.0);
    assert_eq!(neon_mask.effect_settings.neon.edge_width, 4.5);
    assert_eq!(neon_mask.effect_settings.neon.color, [1.0, 0.15, 0.65]);
    assert_eq!(neon_mask.adjustments.exposure, 1.0);
}

#[test]
fn glow_mask_settings_round_trip_through_the_sidecar() {
    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Fullscreen).unwrap();
    let glow_mask = masks.masks.last_mut().unwrap();
    glow_mask.effect = crate::pipeline::MaskEffect::Glow;
    glow_mask.effect_settings.glow.amount = 74.0;
    glow_mask.effect_settings.glow.radius = 62.0;
    glow_mask.effect_settings.glow.core = 81.0;
    glow_mask.effect_settings.glow.color = [0.1, 0.55, 1.0];
    glow_mask.adjustments.exposure = 1.0;

    let encoded = encode(edits.clone()).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
    let glow_mask = loaded.edits.masks.masks.last().unwrap();
    assert_eq!(glow_mask.effect, crate::pipeline::MaskEffect::Glow);
    assert_eq!(glow_mask.effect_settings.glow.amount, 74.0);
    assert_eq!(glow_mask.effect_settings.glow.radius, 62.0);
    assert_eq!(glow_mask.effect_settings.glow.core, 81.0);
    assert_eq!(glow_mask.effect_settings.glow.color, [0.1, 0.55, 1.0]);
    assert_eq!(glow_mask.adjustments.exposure, 1.0);
}

#[test]
fn light_rays_mask_settings_round_trip_through_the_sidecar() {
    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Fullscreen).unwrap();
    let light_rays_mask = masks.masks.last_mut().unwrap();
    light_rays_mask.effect = crate::pipeline::MaskEffect::LightRays;
    light_rays_mask.effect_settings.light_rays.amount = 76.0;
    light_rays_mask.effect_settings.light_rays.length = 165.0;
    light_rays_mask.effect_settings.light_rays.source = [22.0, -15.0];
    light_rays_mask.effect_settings.light_rays.color = [1.0, 0.68, 0.25];
    light_rays_mask.adjustments.exposure = 1.0;

    let encoded = encode(edits.clone()).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
    let light_rays_mask = loaded.edits.masks.masks.last().unwrap();
    assert_eq!(
        light_rays_mask.effect,
        crate::pipeline::MaskEffect::LightRays
    );
    assert_eq!(light_rays_mask.effect_settings.light_rays.amount, 76.0);
    assert_eq!(light_rays_mask.effect_settings.light_rays.length, 165.0);
    assert_eq!(
        light_rays_mask.effect_settings.light_rays.source,
        [22.0, -15.0]
    );
    assert_eq!(
        light_rays_mask.effect_settings.light_rays.color,
        [1.0, 0.68, 0.25]
    );
    assert_eq!(light_rays_mask.adjustments.exposure, 1.0);
}

#[test]
fn blur_edge_glow_and_pixelate_settings_round_trip_through_the_sidecar() {
    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Fullscreen).unwrap();
    let mask = masks.masks.last_mut().unwrap();
    mask.effect = crate::pipeline::MaskEffect::EdgeGlow;
    mask.effect_settings.blur.amount = 36.0;
    mask.effect_settings.blur.radius = 12.0;
    mask.effect_settings.edge_glow.amount = 71.0;
    mask.effect_settings.edge_glow.edge_width = 3.5;
    mask.effect_settings.edge_glow.color = [0.15, 0.7, 1.0];
    mask.effect_settings.pixelate.amount = 88.0;
    mask.effect_settings.pixelate.block_size = 24.0;
    mask.adjustments.exposure = 1.0;

    let encoded = encode(edits.clone()).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
    let mask = loaded.edits.masks.masks.last().unwrap();
    assert_eq!(mask.effect, crate::pipeline::MaskEffect::EdgeGlow);
    assert_eq!(mask.effect_settings.blur.radius, 12.0);
    assert_eq!(mask.effect_settings.edge_glow.amount, 71.0);
    assert_eq!(mask.effect_settings.edge_glow.color, [0.15, 0.7, 1.0]);
    assert_eq!(mask.effect_settings.pixelate.block_size, 24.0);
    assert_eq!(mask.adjustments.exposure, 1.0);
}

#[test]
fn focus_blur_settings_round_trip_through_the_sidecar() {
    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Fullscreen).unwrap();
    let mask = masks.masks.last_mut().unwrap();
    mask.effect = crate::pipeline::MaskEffect::TiltShift;
    mask.effect_settings.lens_blur.radius = 22.0;
    mask.effect_settings.lens_blur.blades = 8.0;
    mask.effect_settings.motion_blur.distance = 62.0;
    mask.effect_settings.motion_blur.angle = -27.0;
    mask.effect_settings.radial_blur.mode = crate::pipeline::RadialBlurMode::Spin;
    mask.effect_settings.radial_blur.center = [37.0, 64.0];
    mask.effect_settings.tilt_shift.radius = 19.0;
    mask.effect_settings.tilt_shift.center = [51.0, 58.0];
    mask.effect_settings.tilt_shift.angle = 13.0;

    let encoded = encode(edits.clone()).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
    let mask = loaded.edits.masks.masks.last().unwrap();
    assert_eq!(mask.effect, crate::pipeline::MaskEffect::TiltShift);
    assert_eq!(mask.effect_settings.lens_blur.radius, 22.0);
    assert_eq!(mask.effect_settings.motion_blur.distance, 62.0);
    assert_eq!(
        mask.effect_settings.radial_blur.mode,
        crate::pipeline::RadialBlurMode::Spin
    );
    assert_eq!(mask.effect_settings.tilt_shift.center, [51.0, 58.0]);
}

#[test]
fn atmosphere_settings_round_trip_through_the_sidecar() {
    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Fullscreen).unwrap();
    let mask = masks.masks.last_mut().unwrap();
    mask.effect = crate::pipeline::MaskEffect::Smoke;
    mask.effect_settings.fog.amount = 67.0;
    mask.effect_settings.fog.seed = 241.0;
    mask.effect_settings.fog.color = [0.73, 0.84, 0.96];
    mask.effect_settings.smoke.turbulence = 79.0;
    mask.effect_settings.smoke.angle = 34.0;
    mask.effect_settings.smoke.seed = 613.0;
    mask.effect_settings.smoke.color = [0.16, 0.19, 0.23];
    mask.adjustments.exposure = 1.0;

    let encoded = encode(edits.clone()).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
    let mask = loaded.edits.masks.masks.last().unwrap();
    assert_eq!(mask.effect, crate::pipeline::MaskEffect::Smoke);
    assert_eq!(mask.effect_settings.fog.amount, 67.0);
    assert_eq!(mask.effect_settings.fog.seed, 241.0);
    assert_eq!(mask.effect_settings.fog.color, [0.73, 0.84, 0.96]);
    assert_eq!(mask.effect_settings.smoke.turbulence, 79.0);
    assert_eq!(mask.effect_settings.smoke.angle, 34.0);
    assert_eq!(mask.effect_settings.smoke.seed, 613.0);
    assert_eq!(mask.effect_settings.smoke.color, [0.16, 0.19, 0.23]);
    assert_eq!(mask.adjustments.exposure, 1.0);
}

#[test]
fn sidecar_round_trip_preserves_shared_subject_refinement() {
    let mut edits = sample_edits();
    let refinement = {
        let masks = Arc::make_mut(&mut edits.masks);
        masks.subject_refinement.size = 0.07;
        masks.subject_refinement.feather = 0.3;
        masks.subject_refinement.flow = 0.45;
        masks.subject_refinement.stroke_starts.push(0);
        masks
            .subject_refinement
            .dabs
            .push(crate::pipeline::BrushDab {
                center: [0.25, 0.75],
                opacity: -0.45,
                size: 0.07,
                feather: 0.3,
            });
        masks.subject_refinement.clone()
    };
    edits.subject_refinement = Some(refinement);

    let encoded = encode(edits.clone()).unwrap();
    let document: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert!(document.pointer("/edits/subject_refinement").is_some());
    assert!(document
        .pointer("/edits/masks/subject_refinement")
        .is_none());
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
}

#[test]
fn schema_six_sidecar_without_subject_refinement_loads_empty_layer() {
    let edits = sample_edits();
    let encoded = encode(edits).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    document["schema_version"] = 6.into();
    document
        .pointer_mut("/edits")
        .and_then(serde_json::Value::as_object_mut)
        .unwrap()
        .remove("subject_refinement");

    let legacy = serde_json::to_vec(&document).unwrap();
    let loaded = decode(&legacy).unwrap();
    assert!(loaded.migrated);
    assert!(loaded.edits.masks.subject_refinement.is_empty());
}

#[test]
fn schema_seven_mask_defaults_to_adjustment_effect() {
    let edits = sample_edits();
    let encoded = encode(edits).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    document["schema_version"] = 7.into();
    document
        .pointer_mut("/edits/masks/masks/0")
        .and_then(serde_json::Value::as_object_mut)
        .unwrap()
        .remove("effect");

    let legacy = serde_json::to_vec(&document).unwrap();
    let loaded = decode(&legacy).unwrap();
    assert!(loaded.migrated);
    assert_eq!(
        loaded.edits.masks.masks[0].effect,
        crate::pipeline::MaskEffect::Adjustment
    );
}

#[test]
fn generated_masks_are_deduplicated_compressed_and_copy_on_write_after_loading() {
    let mut edits = sample_edits();
    let mut masks = MaskStack::default();
    masks.add_mask(MaskKind::Object);
    let pixels = (0..64 * 64)
        .map(|index| if index % 17 < 8 { 255 } else { 0 })
        .collect::<Vec<_>>();
    if let MaskGeometry::Object { mask, .. } = &mut masks.masks[0].components[0].geometry {
        *mask = MaskImage::new(64, 64, pixels);
    }
    masks.masks.push(masks.masks[0].clone());
    edits.masks = Arc::new(masks);

    let encoded = encode(edits.clone()).unwrap();
    let document: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(document["schema_version"], SIDECAR_SCHEMA_VERSION);
    assert_eq!(document["mask_assets"].as_array().unwrap().len(), 1);
    assert_eq!(document["mask_asset_refs"].as_array().unwrap().len(), 2);
    let compressed = document["mask_assets"][0]["png"].as_str().unwrap();
    assert!(compressed.len() < base64_json_string_bytes(64 * 64).unwrap() as usize);
    assert!(document
        .pointer("/edits/masks/masks/0/components/0/geometry/Object/mask")
        .unwrap()
        .is_null());

    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
    let mut restored = loaded.edits;
    let restored_masks = Arc::make_mut(&mut restored.masks);
    let [first_group, second_group, ..] = restored_masks.masks.as_mut_slice() else {
        panic!("expected duplicated object-mask groups");
    };
    let MaskGeometry::Object {
        mask: Some(first), ..
    } = &mut first_group.components[0].geometry
    else {
        panic!("first object mask was not restored");
    };
    let MaskGeometry::Object {
        mask: Some(second), ..
    } = &mut second_group.components[0].geometry
    else {
        panic!("second object mask was not restored");
    };
    assert!(Arc::ptr_eq(&first.pixels, &second.pixels));
    let unchanged = second.pixels[0];
    Arc::make_mut(&mut first.pixels)[0] = unchanged ^ 0xff;
    assert_eq!(second.pixels[0], unchanged);
    assert!(!Arc::ptr_eq(&first.pixels, &second.pixels));
}

#[test]
fn beta_inline_mask_sidecar_migrates_to_asset_layout_without_losing_masks() {
    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Subject);
    if let MaskGeometry::Ai { mask, .. } = &mut masks.masks[1].components[0].geometry {
        *mask = MaskImage::new(8, 8, vec![127; 8 * 8]);
    }
    let legacy = SidecarDocument {
        format: SIDECAR_FORMAT.to_owned(),
        schema_version: 4,
        edits: edits.clone(),
        mask_assets: Vec::new(),
        mask_asset_refs: Vec::new(),
    };
    let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
    assert!(legacy_bytes
        .windows(b"\"pixels\"".len())
        .any(|part| part == b"\"pixels\""));

    let loaded = decode(&legacy_bytes).unwrap();
    assert!(loaded.migrated);
    assert_eq!(loaded.edits, edits);

    let rewritten = encode(loaded.edits).unwrap();
    let current: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
    assert_eq!(current["schema_version"], SIDECAR_SCHEMA_VERSION);
    assert_eq!(current["mask_assets"].as_array().unwrap().len(), 1);
    assert_eq!(current["mask_asset_refs"].as_array().unwrap().len(), 1);
}

#[test]
fn malformed_current_mask_assets_are_rejected() {
    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Subject);
    if let MaskGeometry::Ai { mask, .. } = &mut masks.masks[1].components[0].geometry {
        *mask = MaskImage::new(8, 8, vec![255; 8 * 8]);
    }
    let encoded = encode(edits).unwrap();

    let mut invalid_reference: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    invalid_reference["mask_asset_refs"][0]["asset_index"] = 99.into();
    assert!(matches!(
        decode(&serde_json::to_vec(&invalid_reference).unwrap()),
        Err(SidecarError::Invalid(message)) if message.contains("asset index")
    ));

    let mut invalid_png: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    invalid_png["mask_assets"][0]["png"] = "AAAA".into();
    assert!(matches!(
        decode(&serde_json::to_vec(&invalid_png).unwrap()),
        Err(SidecarError::Invalid(message)) if message.contains("mask PNG")
    ));
}

#[test]
fn reset_all_adjustments_removes_sidecar_masks_and_thumbnail_caches() {
    let directory = temporary_directory("reset-all");
    let raw = directory.join("masked.CR3");
    fs::write(&raw, b"raw").unwrap();

    let mut edits = sample_edits();
    let masks = Arc::make_mut(&mut edits.masks);
    masks.add_mask(MaskKind::Subject).unwrap();
    edits.ai_masks_need_update = true;
    save_desktop(&raw, edits).unwrap();

    let cache_paths = [
        developed_thumbnail_path_for_raw(&raw),
        developed_thumbnail_fingerprint_path_for_raw(&raw),
        legacy_developed_thumbnail_path_for_raw(&raw),
        legacy_developed_thumbnail_fingerprint_path_for_raw(&raw),
    ];
    for path in &cache_paths {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"stale").unwrap();
    }

    assert!(reset_desktop_adjustments(&raw).unwrap());
    assert!(!sidecar_path_for_raw(&raw).exists());
    assert!(cache_paths.iter().all(|path| !path.exists()));
    assert!(load_desktop(&raw).unwrap().is_none());

    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn inpainting_round_trip_preserves_individual_strokes() {
    use crate::pipeline::{BrushDab, InpaintPatch, InpaintStroke};
    use half::f16;

    let mut edits = sample_edits();
    let rgba16f = vec![f16::from_f32(0.25).to_bits(); 16 * 16 * 4];
    let patch = InpaintPatch::new_linear_resampled(
        [32, 32],
        [8, 8],
        [16, 16],
        [16, 16],
        rgba16f,
        vec![255; 16 * 16],
    )
    .unwrap();
    let stroke = InpaintStroke::from_result(vec![BrushDab::default()], patch).unwrap();
    edits.inpainting = Arc::new(vec![stroke]);

    let encoded = encode(edits.clone()).unwrap();
    let document: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert!(document["edits"]["inpainting"][0].get("kind").is_none());
    assert!(document["edits"]["inpainting"][0]
        .get("source_offset")
        .is_none());
    assert!(document["edits"]["inpainting"][0]["patch"]["rgba16f"]
        .as_str()
        .unwrap()
        .starts_with("z1:"));
    assert!(document["edits"]["inpainting"][0]["patch"]["mask"]
        .as_str()
        .unwrap()
        .starts_with("z1:"));
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits.inpainting, edits.inpainting);
}

#[test]
fn source_based_inpainting_round_trip_preserves_tool_and_offset() {
    use crate::pipeline::{BrushDab, InpaintPatch, InpaintStroke, InpaintStrokeKind};
    use half::f16;

    let mut edits = sample_edits();
    let patch = InpaintPatch::new_linear(
        32,
        32,
        8,
        8,
        2,
        2,
        vec![f16::from_f32(0.5).to_bits(); 16],
        vec![255; 4],
    )
    .unwrap();
    edits.inpainting = Arc::new(vec![InpaintStroke::from_tool_result(
        InpaintStrokeKind::Heal,
        Some([0.25, -0.125]),
        vec![BrushDab::default()],
        patch,
    )
    .unwrap()]);

    let encoded = encode(edits.clone()).unwrap();
    let document: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(document["edits"]["inpainting"][0]["kind"], "Heal");
    assert_eq!(
        document["edits"]["inpainting"][0]["source_offset"],
        serde_json::json!([0.25, -0.125])
    );
    assert_eq!(decode(&encoded).unwrap().edits, edits);
}

#[test]
fn schema_five_raw_inpainting_payload_migrates_to_compressed_schema_six() {
    use crate::pipeline::{BrushDab, InpaintPatch, InpaintStroke};
    use base64::Engine as _;
    use half::f16;

    let mut edits = sample_edits();
    let rgba16f = vec![f16::from_f32(0.25).to_bits(); 16 * 16 * 4];
    let patch = InpaintPatch::new_linear_resampled(
        [32, 32],
        [8, 8],
        [16, 16],
        [16, 16],
        rgba16f.clone(),
        vec![255; 16 * 16],
    )
    .unwrap();
    edits.inpainting = Arc::new(vec![InpaintStroke::from_result(
        vec![BrushDab::default()],
        patch,
    )
    .unwrap()]);

    let encoded = encode(edits.clone()).unwrap();
    let mut document: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    document["schema_version"] = 5.into();
    let rgba_bytes = rgba16f
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect::<Vec<_>>();
    document["edits"]["inpainting"][0]["patch"]["rgba16f"] =
        base64::engine::general_purpose::STANDARD
            .encode(rgba_bytes)
            .into();
    document["edits"]["inpainting"][0]["patch"]["mask"] =
        base64::engine::general_purpose::STANDARD
            .encode(vec![255u8; 16 * 16])
            .into();

    let legacy = serde_json::to_vec(&document).unwrap();
    let loaded = decode(&legacy).unwrap();
    assert!(loaded.migrated);
    assert_eq!(loaded.edits, edits);

    let rewritten = encode(loaded.edits).unwrap();
    let current: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
    assert_eq!(current["schema_version"], SIDECAR_SCHEMA_VERSION);
    assert!(current["edits"]["inpainting"][0]["patch"]["rgba16f"]
        .as_str()
        .unwrap()
        .starts_with("z1:"));
}

#[test]
fn prospective_inpaint_budget_counts_existing_persisted_payloads() {
    use crate::pipeline::{BrushDab, InpaintPatch, InpaintStroke, MaskImage};

    fn stroke(edge: u32, value: u16) -> InpaintStroke {
        let pixels = edge as usize * edge as usize;
        let patch = InpaintPatch::new_linear(
            edge + 2,
            edge + 2,
            1,
            1,
            edge,
            edge,
            vec![value; pixels * 4],
            vec![255; pixels],
        )
        .unwrap();
        InpaintStroke::from_result(vec![BrushDab::default(); 3], patch).unwrap()
    }

    let mut masks = MaskStack::default();
    masks.add_mask(MaskKind::Subject);
    masks.masks[0].name = "subject \"mask\"".to_owned();
    if let MaskGeometry::Ai { mask, .. } = &mut masks.masks[0].components[0].geometry {
        *mask = MaskImage::new(32, 32, vec![127; 32 * 32]);
    } else {
        panic!("subject mask should use AI geometry");
    }

    let existing = stroke(16, 1);
    let candidate = stroke(8, 2);
    let candidate_only =
        estimate_sidecar_bytes(&MaskStack::default(), std::iter::once(&candidate)).unwrap();
    let prospective = estimate_sidecar_bytes(&masks, [&existing, &candidate]).unwrap();
    let measured = measure_sidecar_dynamic_bytes(&masks, [&existing, &candidate]).unwrap();
    assert!(prospective > candidate_only);
    assert!(prospective > measured);

    assert!(preflight_inpaint_addition_with_limit(
        &MaskStack::default(),
        &[],
        &candidate,
        prospective - 1,
    )
    .is_ok());
    assert!(preflight_inpaint_addition_with_limit(
        &masks,
        std::slice::from_ref(&existing),
        &candidate,
        prospective - 1,
    )
    .is_ok());
    assert!(matches!(
        preflight_inpaint_addition_with_limit(
            &masks,
            std::slice::from_ref(&existing),
            &candidate,
            measured - 1,
        ),
        Err(SidecarError::TooLarge(bytes)) if bytes == measured
    ));

    let mut edits = sample_edits();
    edits.masks = Arc::new(masks);
    edits.inpainting = Arc::new(vec![existing, candidate]);
    let encoded = encode(edits).unwrap();
    assert!((encoded.len() as u64) <= prospective);
}

#[test]
fn compressed_mask_measurement_prevents_false_inpaint_budget_rejection() {
    use crate::pipeline::{BrushDab, InpaintPatch, InpaintStroke, MaskImage};

    let mut masks = MaskStack::default();
            _ => panic!("generated mask kind should have generated mask storage"),
        }
    }

    let patch = InpaintPatch::new_linear_resampled(
        [6000, 4000],
        [100, 100],
        [64, 64],
        [16, 16],
        vec![0; 16 * 16 * 4],
        vec![255; 16 * 16],
    )
    .unwrap();
    let candidate = InpaintStroke::from_result(vec![BrushDab::default()], patch).unwrap();
    let conservative = estimate_sidecar_bytes(&masks, std::iter::once(&candidate)).unwrap();
    let measured = measure_sidecar_dynamic_bytes(&masks, std::iter::once(&candidate)).unwrap();
    assert!(conservative > measured);
    let limit = measured + (conservative - measured) / 2;
    assert!(preflight_inpaint_addition_with_limit(&masks, &[], &candidate, limit).is_ok());
}

#[test]
fn compressed_native_resolution_patches_exceed_old_android_stroke_ceiling() {
    use crate::pipeline::{BrushDab, InpaintPatch, InpaintStroke};

    let raster_pixels = 256usize * 256;
    let patch = InpaintPatch::new_linear_resampled(
        [6000, 4000],
        [500, 500],
        [800, 800],
        [256, 256],
        vec![0u16; raster_pixels * 4],
        vec![255; raster_pixels],
    )
    .unwrap();
    let stroke = InpaintStroke::from_result(vec![BrushDab::default()], patch).unwrap();
    let android_limit = 32 * 1024 * 1024;
    let strokes = (0..48)
        .map(|index| {
            let mut candidate = stroke.clone();
            candidate.patch.x += index * 10;
            candidate.dabs[0].center[0] = index as f32 / 48.0;
            candidate
        })
        .collect::<Vec<_>>();
    let old_raw_bound = estimate_sidecar_bytes(&MaskStack::default(), strokes.iter()).unwrap();
    assert!(old_raw_bound > android_limit);
    preflight_inpaint_addition_with_limit(
        &MaskStack::default(),
        &strokes[..47],
        &strokes[47],
        android_limit,
    )
    .unwrap();

    let mut edits = sample_edits();
    edits.inpainting = Arc::new(strokes.clone());
    let encoded = encode(edits).unwrap();
    assert!((encoded.len() as u64) <= android_limit);
    let decoded = decode(&encoded).unwrap();
    assert_eq!(decoded.edits.inpainting.as_ref(), strokes.as_slice());
}

#[test]
fn schema_one_sidecar_without_inpainting_loads_as_empty() {
    let document = SidecarDocument {
        format: SIDECAR_FORMAT.to_owned(),
        schema_version: 1,
        edits: sample_edits(),
        mask_assets: Vec::new(),
        mask_asset_refs: Vec::new(),
    };
    let mut value = serde_json::to_value(document).unwrap();
    value["edits"].as_object_mut().unwrap().remove("inpainting");
    let encoded = serde_json::to_vec(&value).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert!(loaded.edits.inpainting.is_empty());
    assert!(loaded.migrated);
}

#[test]
fn schema_two_full_resolution_inpaint_patch_remains_compatible() {
    use crate::pipeline::{InpaintPatch, InpaintStroke};

    let mut edits = sample_edits();
    let patch =
        InpaintPatch::new_linear(4, 4, 1, 1, 2, 2, vec![0u16; 16], vec![255; 4]).unwrap();
    edits.inpainting = Arc::new(vec![InpaintStroke::from_result(Vec::new(), patch).unwrap()]);
    let encoded = serde_json::to_vec(&SidecarDocument {
        format: SIDECAR_FORMAT.to_owned(),
        schema_version: 2,
        edits,
        mask_assets: Vec::new(),
        mask_asset_refs: Vec::new(),
    })
    .unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits.inpainting[0].patch.raster_dimensions(), [2, 2]);
    assert!(loaded.migrated);
}

#[test]
fn corrupt_and_future_sidecars_are_rejected() {
    assert!(matches!(
        decode(br#"{"schema_version":1,"#),
        Err(SidecarError::Invalid(_))
    ));

    let edits = sample_edits();
    let future = SidecarDocument {
        format: SIDECAR_FORMAT.to_owned(),
        schema_version: SIDECAR_SCHEMA_VERSION + 1,
        edits,
        mask_assets: Vec::new(),
        mask_asset_refs: Vec::new(),
    };
    assert!(matches!(
        decode(&serde_json::to_vec(&future).unwrap()),
        Err(SidecarError::Unsupported(_))
    ));

    let mut non_finite = sample_edits();
    non_finite.exposure.exposure = f32::NAN;
    assert!(matches!(
        encode(non_finite),
        Err(SidecarError::Invalid(message)) if message.contains("non-finite")
    ));

    let mut unsafe_geometry = sample_edits();
    if let MaskGeometry::Radial { radius, .. } =
        &mut Arc::make_mut(&mut unsafe_geometry.masks).masks[0].components[0].geometry
    {
        radius[0] = 1.0e30;
    }
    assert!(matches!(
        encode(unsafe_geometry),
        Err(SidecarError::Invalid(message)) if message.contains("safe range")
    ));
}

#[test]
fn tiff_sidecars_keep_the_source_extension() {
    assert_eq!(
        sidecar_path_for_raw(Path::new("photo.tif"))
            .file_name()
            .unwrap(),
        "photo.tif.auraw"
    );
    assert_eq!(
        sidecar_path_for_raw(Path::new("photo.TIFF"))
            .file_name()
            .unwrap(),
        "photo.TIFF.auraw"
    );
}

#[test]
fn desktop_save_is_atomic_and_uses_appended_suffix() {
    let directory = temporary_directory("atomic");
    let raw = directory.join("photo.CR3");
    fs::write(&raw, b"raw").unwrap();
    let edits = sample_edits();
    let path = save_desktop(&raw, edits.clone()).unwrap();
    assert_eq!(path.file_name().unwrap(), "photo.CR3.auraw");
    assert_eq!(load_desktop(&raw).unwrap().unwrap().edits, edits);
    assert_eq!(
        fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count(),
        0
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn reconstructible_range_source_is_not_persisted() {
    use crate::pipeline::{MaskCombineMode, MaskComponent, MaskRgbImage};

    let mut edits = sample_edits();
    let width = 2048;
    let height = 2048;
    let source = MaskRgbImage::new(
        width,
        height,
        vec![127; width as usize * height as usize * 4],
    )
    .unwrap();
    Arc::make_mut(&mut edits.masks).masks[0].components[0] = MaskComponent {
        name: "Luminance Range".to_owned(),
        kind: MaskKind::LuminanceRange,
        combine: MaskCombineMode::Add,
        enabled: true,
        invert: false,
        geometry: MaskGeometry::LuminanceRange {
            source: Some(source),
            low: 0.2,
            high: 0.8,
            grow: 0.0,
            feather: 0.15,
        },
    };
    let encoded = encode(edits).unwrap();
    assert!(encoded.len() < 64 * 1024);
    let loaded = decode(&encoded).unwrap();
    assert!(matches!(
        &loaded.edits.masks.masks[0].components[0].geometry,
        MaskGeometry::LuminanceRange { source: None, .. }
    ));
}

#[test]
fn object_mask_round_trip_preserves_prompts_and_soft_mask() {
    use crate::pipeline::{MaskCombineMode, MaskComponent, MaskImage, ObjectStroke};

    let mut edits = sample_edits();
    let object = MaskComponent {
        name: "Object".to_owned(),
        kind: MaskKind::Object,
        combine: MaskCombineMode::Add,
        enabled: true,
        invert: false,
        geometry: MaskGeometry::Object {
            mask: Some(MaskImage::new(2, 2, vec![0, 64, 192, 255]).unwrap()),
            grow: 0.0,
            feather: 0.1,
            brush_size: 0.08,
            edge_refine: 0.7,
            strokes: vec![
                ObjectStroke {
                    points: vec![[0.25, 0.25], [0.5, 0.5]],
                    positive: true,
                    brush_size: 0.0,
                },
                ObjectStroke {
                    points: vec![[0.75, 0.75]],
                    positive: false,
                    brush_size: 0.0,
                },
            ],
        },
    };
    Arc::make_mut(&mut edits.masks).masks[0].components = vec![object];

    let encoded = encode(edits.clone()).unwrap();
    let loaded = decode(&encoded).unwrap();
    assert_eq!(loaded.edits, edits);
}

#[test]
fn repeated_shared_range_sources_stay_small() {
    use crate::pipeline::{MaskCombineMode, MaskComponent, MaskRgbImage};

    let mut edits = sample_edits();
    let width = 2048;
    let height = 2048;
    let source = MaskRgbImage::new(
        width,
        height,
        vec![63; width as usize * height as usize * 4],
    )
    .unwrap();
    let component = MaskComponent {
        name: "Range".to_owned(),
        kind: MaskKind::LuminanceRange,
        combine: MaskCombineMode::Add,
        enabled: true,
        invert: false,
        geometry: MaskGeometry::LuminanceRange {
            source: Some(source),
            low: 0.2,
            high: 0.8,
            grow: 0.0,
            feather: 0.15,
        },
    };
    Arc::make_mut(&mut edits.masks).masks[0].components = vec![component; 3];
    assert!(encode(edits).unwrap().len() < 64 * 1024);
}

#[cfg(not(target_os = "android"))]
#[test]
fn developed_thumbnail_cache_uses_private_application_directory() {
    let raw = Path::new("photos/photo.CR3");
    let cache = developed_thumbnail_path_for_raw(raw);
    assert!(cache.starts_with(crate::thumbnail_cache::desktop_thumbnail_cache_root()));
    assert!(cache
        .to_string_lossy()
        .ends_with(DEVELOPED_THUMBNAIL_SUFFIX));
    assert_ne!(cache.parent(), raw.parent());
}

#[cfg(not(target_os = "android"))]
#[test]
fn developed_thumbnail_cache_round_trips_and_tracks_sidecar_content() {
    let directory = temporary_directory("developed-thumbnail");
    let raw = directory.join("photo.CR3");
    fs::write(&raw, b"raw").unwrap();
    fs::write(sidecar_path_for_raw(&raw), b"edit-one").unwrap();
    let fingerprint = desktop_sidecar_fingerprint(&raw).unwrap().unwrap();
    let thumbnail = RawThumbnail {
        width: 16,
        height: 16,
        rgba: [10, 20, 30, 255].repeat(16 * 16),
    };

    let cache_path = save_developed_thumbnail_cache(&raw, &thumbnail, fingerprint).unwrap();
    assert!(cache_path.starts_with(crate::thumbnail_cache::desktop_thumbnail_cache_root()));
    assert_ne!(cache_path.parent(), raw.parent());
    let loaded = load_developed_thumbnail_cache(&raw, 512)
        .unwrap()
        .expect("developed thumbnail cache should load");
    assert_eq!(loaded.width, thumbnail.width);
    assert_eq!(loaded.height, thumbnail.height);
    for (actual, expected) in loaded
        .rgba
        .chunks_exact(4)
        .zip(thumbnail.rgba.chunks_exact(4))
    {
        for channel in 0..3 {
            assert!(actual[channel].abs_diff(expected[channel]) <= 3);
        }
        assert_eq!(actual[3], 255);
    }

    fs::write(sidecar_path_for_raw(&raw), b"edit-two").unwrap();
    assert!(!developed_thumbnail_cache_is_fresh(&raw).unwrap());
    let _ = fs::remove_file(developed_thumbnail_path_for_raw(&raw));
    let _ = fs::remove_file(developed_thumbnail_fingerprint_path_for_raw(&raw));
    fs::remove_dir_all(directory).unwrap();
}

#[cfg(unix)]
#[test]
fn non_utf8_raw_paths_keep_their_exact_bytes() {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let raw = PathBuf::from(OsString::from_vec(b"photo-\xff.NEF".to_vec()));
    assert_eq!(
        sidecar_path_for_raw(&raw).as_os_str().as_bytes(),
        b"photo-\xff.NEF.auraw"
    );
}

#[test]
fn relative_sidecar_parent_is_the_current_directory() {
    let path = Path::new("photo.NEF.auraw");
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    assert_eq!(parent, Path::new("."));
}
