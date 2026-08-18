//! Validation for decoded and newly serialized sidecar edit state.

use super::*;

pub(super) fn validate_edit_state(edits: &EditState) -> Result<(), SidecarError> {
    validate_exposure(&edits.exposure)?;
    if let Some(profile) = &edits.camera_profile {
        if profile.as_os_str().len() > MAX_EDIT_NAME_BYTES * 4 {
            return invalid("camera profile path is unreasonably long");
        }
        if profile.is_absolute()
            || profile.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return invalid("camera profile path must stay inside the configured profile folder");
        }
    }
    let stack = &edits.masks;
    if stack.masks.len() > MAX_LOCAL_MASKS {
        return invalid("sidecar contains too many local masks");
    }
    if stack
        .selected_mask
        .is_some_and(|index| index >= stack.masks.len())
    {
        return invalid("selected mask index is out of range");
    }
    if edits.lens.maker.len() > MAX_EDIT_NAME_BYTES || edits.lens.model.len() > MAX_EDIT_NAME_BYTES
    {
        return invalid("lens name is unreasonably long");
    }

    let refinement = &stack.subject_refinement;
    finite(
        "subject refinement settings",
        &[refinement.size, refinement.feather, refinement.flow],
    )?;
    bounded("subject refinement size", refinement.size, 0.0, 16.0)?;
    bounded("subject refinement feather", refinement.feather, 0.0, 1.0)?;
    bounded("subject refinement flow", refinement.flow, 0.0, 1.0)?;
    if refinement.dabs.len() > MAX_BRUSH_DABS {
        return invalid("subject refinement contains too many dabs");
    }
    let mut previous_start = None;
    for &start in &refinement.stroke_starts {
        if start >= refinement.dabs.len()
            || previous_start.is_some_and(|previous| start <= previous)
        {
            return invalid("subject refinement contains invalid stroke boundaries");
        }
        previous_start = Some(start);
    }
    for dab in &refinement.dabs {
        finite(
            "subject refinement dab",
            &[
                dab.center[0],
                dab.center[1],
                dab.opacity,
                dab.size,
                dab.feather,
            ],
        )?;
        bounded("subject refinement dab x", dab.center[0], -16.0, 16.0)?;
        bounded("subject refinement dab y", dab.center[1], -16.0, 16.0)?;
        bounded("subject refinement dab opacity", dab.opacity, -1.0, 1.0)?;
        bounded("subject refinement dab size", dab.size, 0.0, 16.0)?;
        bounded("subject refinement dab feather", dab.feather, 0.0, 1.0)?;
    }

    for (mask_index, mask) in stack.masks.iter().enumerate() {
        finite("mask opacity", &[mask.opacity])?;
        if !(0.0..=1.0).contains(&mask.opacity) {
            return invalid("mask opacity is outside 0..1");
        }
        validate_local_adjustments(&mask.adjustments)?;
        validate_blur_effect(&mask.effect_settings.blur)?;
        validate_lens_blur_effect(&mask.effect_settings.lens_blur)?;
        validate_motion_blur_effect(&mask.effect_settings.motion_blur)?;
        validate_radial_blur_effect(&mask.effect_settings.radial_blur)?;
        validate_tilt_shift_effect(&mask.effect_settings.tilt_shift)?;
        validate_edge_glow_effect(&mask.effect_settings.edge_glow)?;
        validate_glow_effect(&mask.effect_settings.glow)?;
        validate_light_rays_effect(&mask.effect_settings.light_rays)?;
        validate_neon_effect(&mask.effect_settings.neon)?;
        validate_pixelate_effect(&mask.effect_settings.pixelate)?;
        validate_fog_effect(&mask.effect_settings.fog)?;
        validate_smoke_effect(&mask.effect_settings.smoke)?;
        if mask.name.len() > MAX_EDIT_NAME_BYTES {
            return invalid("mask name is unreasonably long");
        }
        if mask.components.is_empty() || mask.components.len() > MAX_MASK_COMPONENTS {
            return invalid("mask has an invalid component count");
        }
        if stack.selected_mask == Some(mask_index)
            && stack
                .selected_component
                .is_some_and(|index| index >= mask.components.len())
        {
            return invalid("selected mask component index is out of range");
        }
        for component in &mask.components {
            if component.name.len() > MAX_EDIT_NAME_BYTES {
                return invalid("mask component name is unreasonably long");
            }
            if !geometry_matches_kind(component.kind, &component.geometry) {
                return invalid("mask component kind and geometry do not agree");
            }
            match &component.geometry {
                MaskGeometry::Brush {
                    size,
                    feather,
                    opacity,
                    stroke_starts,
                    dabs,
                    ..
                } => {
                    finite("brush geometry", &[*size, *feather, *opacity])?;
                    bounded("brush size", *size, 0.0, 16.0)?;
                    bounded("brush feather", *feather, 0.0, 1.0)?;
                    bounded("brush opacity", *opacity, 0.0, 1.0)?;
                    if dabs.len() > MAX_BRUSH_DABS {
                        return invalid("brush mask contains too many dabs");
                    }
                    let mut previous_start = None;
                    for &start in stroke_starts {
                        if start >= dabs.len()
                            || previous_start.is_some_and(|previous| start <= previous)
                        {
                            return invalid("brush mask contains invalid stroke boundaries");
                        }
                        previous_start = Some(start);
                    }
                    for dab in dabs {
                        finite(
                            "brush dab",
                            &[
                                dab.center[0],
                                dab.center[1],
                                dab.opacity,
                                dab.size,
                                dab.feather,
                            ],
                        )?;
                        bounded("brush dab x", dab.center[0], -16.0, 16.0)?;
                        bounded("brush dab y", dab.center[1], -16.0, 16.0)?;
                        bounded("brush dab opacity", dab.opacity, -1.0, 1.0)?;
                        bounded("brush dab size", dab.size, 0.0, 16.0)?;
                        bounded("brush dab feather", dab.feather, 0.0, 1.0)?;
                    }
                }
                MaskGeometry::Radial {
                    center,
                    radius,
                    rotation,
                    feather,
                    ..
                } => {
                    finite(
                        "radial geometry",
                        &[
                            center[0], center[1], radius[0], radius[1], *rotation, *feather,
                        ],
                    )?;
                    for value in center {
                        bounded("radial center", *value, -16.0, 16.0)?;
                    }
                    for value in radius {
                        bounded("radial radius", *value, 0.0, 16.0)?;
                    }
                    bounded("radial rotation", *rotation, -1_000_000.0, 1_000_000.0)?;
                    bounded("radial feather", *feather, 0.0, 1.0)?;
                }
                MaskGeometry::Linear {
                    start,
                    end,
                    feather,
                    ..
                } => {
                    finite(
                        "linear geometry",
                        &[start[0], start[1], end[0], end[1], *feather],
                    )?;
                    for value in start.iter().chain(end.iter()) {
                        bounded("linear point", *value, -16.0, 16.0)?;
                    }
                    bounded("linear feather", *feather, 0.0, 16.0)?;
                }
                MaskGeometry::Ai {
                    mask,
                    grow,
                    feather,
                } => {
                    finite("AI mask settings", &[*grow, *feather])?;
                    bounded("AI mask grow", *grow, -1.0, 1.0)?;
                    bounded("AI mask feather", *feather, 0.0, 1.0)?;
                    if let Some(image) = mask {
                        validate_image(image.width, image.height, image.pixels.len(), 1)?;
                    }
                }
                MaskGeometry::Landscape {
                    mask,
                    grow,
                    feather,
                    ..
                } => {
                    finite("landscape mask settings", &[*grow, *feather])?;
                    bounded("landscape mask grow", *grow, -1.0, 1.0)?;
                    bounded("landscape mask feather", *feather, 0.0, 1.0)?;
                    if let Some(image) = mask {
                        validate_image(image.width, image.height, image.pixels.len(), 1)?;
                    }
                }
                MaskGeometry::Object {
                    mask,
                    grow,
                    feather,
                    brush_size,
                    edge_refine,
                    strokes,
                    ..
                } => {
                    finite(
                        "object mask settings",
                        &[*grow, *feather, *brush_size, *edge_refine],
                    )?;
                    bounded("object mask grow", *grow, -1.0, 1.0)?;
                    bounded("object mask feather", *feather, 0.0, 1.0)?;
                    bounded("object brush size", *brush_size, 0.0, 16.0)?;
                    bounded("object edge refine", *edge_refine, 0.0, 1.0)?;
                    if strokes.len() > MAX_OBJECT_STROKES {
                        return invalid("object mask contains too many strokes");
                    }
                    let mut point_count = 0usize;
                    for stroke in strokes {
                        point_count =
                            point_count
                                .checked_add(stroke.points.len())
                                .ok_or_else(|| {
                                    SidecarError::Invalid("object prompt count overflow".to_owned())
                                })?;
                        if point_count > MAX_OBJECT_STROKE_POINTS {
                            return invalid("object mask contains too many prompt points");
                        }
                        for point in &stroke.points {
                            finite("object prompt", point)?;
                            bounded("object prompt x", point[0], -16.0, 16.0)?;
                            bounded("object prompt y", point[1], -16.0, 16.0)?;
                        }
                    }
                    if let Some(image) = mask {
                        validate_image(image.width, image.height, image.pixels.len(), 1)?;
                    }
                }
                MaskGeometry::LuminanceRange {
                    source,
                    low,
                    high,
                    grow,
                    feather,
                } => {
                    finite("luminance range mask", &[*low, *high, *grow, *feather])?;
                    bounded("luminance low", *low, -16.0, 16.0)?;
                    bounded("luminance high", *high, -16.0, 16.0)?;
                    bounded("luminance grow", *grow, -1.0, 1.0)?;
                    bounded("luminance feather", *feather, 0.0, 16.0)?;
                    if let Some(image) = source {
                        validate_image(image.width, image.height, image.rgba.len(), 4)?;
                    }
                }
                MaskGeometry::ColorRange {
                    source,
                    sample,
                    tolerance,
                    grow,
                    feather,
                    ..
                } => {
                    finite(
                        "color range mask",
                        &[sample[0], sample[1], sample[2], *tolerance, *grow, *feather],
                    )?;
                    for value in sample {
                        bounded("color sample", *value, -16.0, 16.0)?;
                    }
                    bounded("color tolerance", *tolerance, 0.0, 16.0)?;
                    bounded("color grow", *grow, -1.0, 1.0)?;
                    bounded("color feather", *feather, 0.0, 16.0)?;
                    if let Some(image) = source {
                        validate_image(image.width, image.height, image.rgba.len(), 4)?;
                    }
                }
                _ => {}
            }
        }
    }
    let mut inpaint_dabs = 0usize;
    for stroke in edits.inpainting.iter() {
        inpaint_dabs = inpaint_dabs
            .checked_add(stroke.dabs.len())
            .ok_or_else(|| SidecarError::Invalid("inpainting dab count overflow".to_owned()))?;
        if inpaint_dabs > MAX_INPAINT_DABS {
            return invalid("sidecar contains too many inpainting brush dabs");
        }
        if stroke.kind.requires_source() {
            let Some(source_offset) = stroke.source_offset else {
                return invalid("source-based inpainting stroke has no source offset");
            };
            finite("inpainting source offset", &source_offset)?;
            bounded("inpainting source offset x", source_offset[0], -16.0, 16.0)?;
            bounded("inpainting source offset y", source_offset[1], -16.0, 16.0)?;
        } else if stroke.source_offset.is_some() {
            return invalid("remove stroke unexpectedly contains a source offset");
        }
        for dab in &stroke.dabs {
            finite(
                "inpainting brush dab",
                &[
                    dab.center[0],
                    dab.center[1],
                    dab.opacity,
                    dab.size,
                    dab.feather,
                ],
            )?;
            bounded("inpainting dab x", dab.center[0], -16.0, 16.0)?;
            bounded("inpainting dab y", dab.center[1], -16.0, 16.0)?;
            bounded("inpainting dab opacity", dab.opacity, -1.0, 1.0)?;
            bounded("inpainting dab size", dab.size, 0.0, 16.0)?;
            bounded("inpainting dab feather", dab.feather, 0.0, 1.0)?;
        }

        let patch = &stroke.patch;
        if patch.source_width == 0
            || patch.source_height == 0
            || patch.width == 0
            || patch.height == 0
            || patch
                .x
                .checked_add(patch.width)
                .is_none_or(|right| right > patch.source_width)
            || patch
                .y
                .checked_add(patch.height)
                .is_none_or(|bottom| bottom > patch.source_height)
        {
            return invalid("inpainting patch bounds are invalid");
        }
        if !patch.is_valid() {
            return invalid("inpainting patch storage is invalid");
        }
        let [raster_width, raster_height] = patch.raster_dimensions();
        validate_image(raster_width, raster_height, patch.mask.len(), 1)?;
        let pixels = raster_width as usize * raster_height as usize;
        if !patch.rgba16f.is_empty() {
            if patch.rgba16f.len() != pixels.saturating_mul(4) {
                return invalid("inpainting RGBA16F patch dimensions are invalid");
            }
        } else {
            validate_image(raster_width, raster_height, patch.rgba.len(), 4)?;
        }
    }

    if stack.selected_mask.is_none() && stack.selected_component.is_some() {
        return invalid("a component is selected without a selected mask");
    }
    Ok(())
}

fn geometry_matches_kind(kind: MaskKind, geometry: &MaskGeometry) -> bool {
    matches!(
        (kind, geometry),
        (MaskKind::Fullscreen, MaskGeometry::Fullscreen)
            | (MaskKind::Brush, MaskGeometry::Brush { .. })
            | (MaskKind::Radial, MaskGeometry::Radial { .. })
            | (MaskKind::Linear, MaskGeometry::Linear { .. })
            | (
                MaskKind::Subject | MaskKind::Background,
                MaskGeometry::Ai { .. }
            )
            | (MaskKind::Object, MaskGeometry::Object { .. })
            | (MaskKind::Landscape, MaskGeometry::Landscape { .. })
            | (
                MaskKind::LuminanceRange,
                MaskGeometry::LuminanceRange { .. }
            )
            | (MaskKind::ColorRange, MaskGeometry::ColorRange { .. })
            | (MaskKind::DepthRange, MaskGeometry::Placeholder)
    )
}

fn validate_exposure(exposure: &ExposureParams) -> Result<(), SidecarError> {
    finite(
        "global adjustment",
        &[
            exposure.black_point,
            exposure.exposure,
            exposure.contrast,
            exposure.temperature,
            exposure.tint,
            exposure.hue,
            exposure.saturation,
            exposure.vibrance,
            exposure.chroma_denoise,
            exposure.luminance_denoise,
            exposure.denoise_detail,
            exposure.dual_threshold,
            exposure.frequency_chroma,
            exposure.ca_red,
            exposure.ca_blue,
            exposure.highlight_clip,
            exposure.highlight_reconstruction,
            exposure.highlights,
            exposure.shadows,
            exposure.whites,
            exposure.blacks,
            exposure.texture,
            exposure.clarity,
            exposure.dehaze,
            exposure.sharpen_amount,
            exposure.sharpen_radius,
            exposure.sharpen_detail,
            exposure.sharpen_masking,
            exposure.glow_amount,
            exposure.glow_radius,
            exposure.glow_threshold,
            exposure.vignette_amount,
            exposure.vignette_midpoint,
            exposure.vignette_roundness,
            exposure.vignette_feather,
            exposure.vignette_highlights,
            exposure.sigmoid.contrast,
            exposure.sigmoid.skew,
            exposure.sigmoid.display_white_target,
            exposure.sigmoid.display_black_target,
            exposure.sigmoid.hue_preservation,
        ],
    )?;
    finite("global HSL hue", &exposure.hsl_hue)?;
    finite("global HSL saturation", &exposure.hsl_saturation)?;
    finite("global HSL luminance", &exposure.hsl_luminance)?;
    validate_curves(
        &[
            &exposure.tone_curve,
            &exposure.tone_curve_red,
            &exposure.tone_curve_green,
            &exposure.tone_curve_blue,
        ],
        "global tone curve",
    )?;
    validate_grading(&exposure.color_grading, "global color grading")
}

fn validate_local_adjustments(
    adjustments: &crate::pipeline::LocalAdjustments,
) -> Result<(), SidecarError> {
    finite(
        "local adjustment",
        &[
            adjustments.exposure,
            adjustments.contrast,
            adjustments.highlights,
            adjustments.shadows,
            adjustments.whites,
            adjustments.blacks,
            adjustments.temperature,
            adjustments.tint,
            adjustments.hue,
            adjustments.saturation,
            adjustments.texture,
            adjustments.clarity,
            adjustments.dehaze,
        ],
    )?;
    finite("local HSL hue", &adjustments.hsl_hue)?;
    finite("local HSL saturation", &adjustments.hsl_saturation)?;
    finite("local HSL luminance", &adjustments.hsl_luminance)?;
    validate_curves(
        &[
            &adjustments.tone_curve,
            &adjustments.tone_curve_red,
            &adjustments.tone_curve_green,
            &adjustments.tone_curve_blue,
        ],
        "local tone curve",
    )?;
    validate_grading(&adjustments.color_grading, "local color grading")
}

fn validate_neon_effect(neon: &crate::pipeline::NeonEffectSettings) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::neon;
    validate_effect_params(
        crate::pipeline::MaskEffect::Neon,
        &[
            (neon::AMOUNT, neon.amount),
            (neon::EDGE_WIDTH, neon.edge_width),
            (neon::DETAIL, neon.detail),
            (neon::GLOW, neon.glow),
            (neon::BACKGROUND, neon.background),
        ],
        &neon.color,
    )?;
    validate_effect_color(crate::pipeline::MaskEffect::Neon, neon::COLOR, neon.color)
}

fn validate_blur_effect(blur: &crate::pipeline::BlurEffectSettings) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::blur;
    validate_effect_params(
        crate::pipeline::MaskEffect::Blur,
        &[(blur::AMOUNT, blur.amount), (blur::RADIUS, blur.radius)],
        &[],
    )
}

fn validate_lens_blur_effect(
    lens_blur: &crate::pipeline::LensBlurEffectSettings,
) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::lens_blur;
    validate_effect_params(
        crate::pipeline::MaskEffect::LensBlur,
        &[
            (lens_blur::AMOUNT, lens_blur.amount),
            (lens_blur::RADIUS, lens_blur.radius),
            (lens_blur::BLADES, lens_blur.blades),
            (lens_blur::ROTATION, lens_blur.rotation),
            (lens_blur::HIGHLIGHTS, lens_blur.highlight_boost),
        ],
        &[],
    )
}

fn validate_motion_blur_effect(
    motion_blur: &crate::pipeline::MotionBlurEffectSettings,
) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::motion_blur;
    validate_effect_params(
        crate::pipeline::MaskEffect::MotionBlur,
        &[
            (motion_blur::AMOUNT, motion_blur.amount),
            (motion_blur::DISTANCE, motion_blur.distance),
            (motion_blur::ANGLE, motion_blur.angle),
        ],
        &[],
    )
}

fn validate_radial_blur_effect(
    radial_blur: &crate::pipeline::RadialBlurEffectSettings,
) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::radial_blur;
    validate_effect_params(
        crate::pipeline::MaskEffect::RadialBlur,
        &[
            (radial_blur::AMOUNT, radial_blur.amount),
            (radial_blur::STRENGTH, radial_blur.strength),
            (radial_blur::CENTER_X, radial_blur.center[0]),
            (radial_blur::CENTER_Y, radial_blur.center[1]),
        ],
        &[],
    )
}

fn validate_tilt_shift_effect(
    tilt_shift: &crate::pipeline::TiltShiftEffectSettings,
) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::tilt_shift;
    validate_effect_params(
        crate::pipeline::MaskEffect::TiltShift,
        &[
            (tilt_shift::AMOUNT, tilt_shift.amount),
            (tilt_shift::RADIUS, tilt_shift.radius),
            (tilt_shift::CENTER_X, tilt_shift.center[0]),
            (tilt_shift::CENTER_Y, tilt_shift.center[1]),
            (tilt_shift::ANGLE, tilt_shift.angle),
            (tilt_shift::FOCUS_WIDTH, tilt_shift.focus_width),
            (tilt_shift::FEATHER, tilt_shift.feather),
        ],
        &[],
    )
}

fn validate_edge_glow_effect(
    edge_glow: &crate::pipeline::EdgeGlowEffectSettings,
) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::edge_glow;
    validate_effect_params(
        crate::pipeline::MaskEffect::EdgeGlow,
        &[
            (edge_glow::AMOUNT, edge_glow.amount),
            (edge_glow::EDGE_WIDTH, edge_glow.edge_width),
            (edge_glow::DETAIL, edge_glow.detail),
            (edge_glow::GLOW, edge_glow.glow),
        ],
        &edge_glow.color,
    )?;
    validate_effect_color(
        crate::pipeline::MaskEffect::EdgeGlow,
        edge_glow::COLOR,
        edge_glow.color,
    )
}

fn validate_pixelate_effect(
    pixelate: &crate::pipeline::PixelateEffectSettings,
) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::pixelate;
    validate_effect_params(
        crate::pipeline::MaskEffect::Pixelate,
        &[
            (pixelate::AMOUNT, pixelate.amount),
            (pixelate::BLOCK_SIZE, pixelate.block_size),
        ],
        &[],
    )
}

fn validate_fog_effect(fog: &crate::pipeline::FogEffectSettings) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::fog;
    validate_effect_params(
        crate::pipeline::MaskEffect::Fog,
        &[
            (fog::AMOUNT, fog.amount),
            (fog::DENSITY, fog.density),
            (fog::SCALE, fog.scale),
            (fog::SOFTNESS, fog.softness),
            (fog::VARIATION, fog.variation),
            (fog::SEED, fog.seed),
        ],
        &fog.color,
    )?;
    validate_effect_color(crate::pipeline::MaskEffect::Fog, fog::COLOR, fog.color)
}

fn validate_smoke_effect(smoke: &crate::pipeline::SmokeEffectSettings) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::smoke;
    validate_effect_params(
        crate::pipeline::MaskEffect::Smoke,
        &[
            (smoke::AMOUNT, smoke.amount),
            (smoke::DENSITY, smoke.density),
            (smoke::SCALE, smoke.scale),
            (smoke::TURBULENCE, smoke.turbulence),
            (smoke::SOFTNESS, smoke.softness),
            (smoke::ANGLE, smoke.angle),
            (smoke::SEED, smoke.seed),
        ],
        &smoke.color,
    )?;
    validate_effect_color(crate::pipeline::MaskEffect::Smoke, smoke::COLOR, smoke.color)
}

fn validate_glow_effect(glow: &crate::pipeline::GlowEffectSettings) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::glow;
    validate_effect_params(
        crate::pipeline::MaskEffect::Glow,
        &[
            (glow::AMOUNT, glow.amount),
            (glow::RADIUS, glow.radius),
            (glow::CORE, glow.core),
        ],
        &glow.color,
    )?;
    validate_effect_color(crate::pipeline::MaskEffect::Glow, glow::COLOR, glow.color)
}

fn validate_light_rays_effect(
    light_rays: &crate::pipeline::LightRaysEffectSettings,
) -> Result<(), SidecarError> {
    use crate::pipeline::effect_params::light_rays;
    validate_effect_params(
        crate::pipeline::MaskEffect::LightRays,
        &[
            (light_rays::AMOUNT, light_rays.amount),
            (light_rays::LENGTH, light_rays.length),
            (light_rays::SOURCE_X, light_rays.source[0]),
            (light_rays::SOURCE_Y, light_rays.source[1]),
            (light_rays::SPREAD, light_rays.spread),
            (light_rays::FADE, light_rays.fade),
            (light_rays::RAY_COUNT, light_rays.ray_count),
            (light_rays::VARIATION, light_rays.variation),
            (light_rays::SOFTNESS, light_rays.softness),
        ],
        &light_rays.color,
    )?;
    validate_effect_color(
        crate::pipeline::MaskEffect::LightRays,
        light_rays::COLOR,
        light_rays.color,
    )
}

fn validate_effect_params(
    effect: crate::pipeline::MaskEffect,
    params: &[(crate::pipeline::effect_params::FloatParamSpec, f32)],
    extra_finite: &[f32],
) -> Result<(), SidecarError> {
    if params
        .iter()
        .map(|(_, value)| value)
        .chain(extra_finite)
        .all(|value| value.is_finite())
    {
        for (spec, value) in params {
            bounded(
                &format!("{} {}", effect.label(), spec.label),
                *value,
                spec.min,
                spec.max,
            )?;
        }
        Ok(())
    } else {
        invalid(&format!(
            "{} mask effect contains a non-finite value",
            effect.label()
        ))
    }
}

fn validate_effect_color(
    effect: crate::pipeline::MaskEffect,
    spec: crate::pipeline::effect_params::ColorParamSpec,
    color: [f32; 3],
) -> Result<(), SidecarError> {
    for channel in color {
        bounded(
            &format!("{} {} channel", effect.label(), spec.label),
            channel,
            spec.min,
            spec.max,
        )?;
    }
    Ok(())
}

fn validate_curves(
    curves: &[&crate::pipeline::PointCurve],
    label: &str,
) -> Result<(), SidecarError> {
    for curve in curves {
        if !(2..=crate::pipeline::MAX_POINT_CURVE_POINTS as u32).contains(&curve.len) {
            return invalid("tone curve point count is invalid");
        }
        for point in curve.points {
            finite(label, &point)?;
        }
    }
    Ok(())
}

fn validate_grading(
    grading: &crate::pipeline::ColorGrading,
    label: &str,
) -> Result<(), SidecarError> {
    finite(
        label,
        &[
            grading.shadows.hue,
            grading.shadows.saturation,
            grading.shadows.luminance,
            grading.midtones.hue,
            grading.midtones.saturation,
            grading.midtones.luminance,
            grading.highlights.hue,
            grading.highlights.saturation,
            grading.highlights.luminance,
            grading.global.hue,
            grading.global.saturation,
            grading.global.luminance,
            grading.blending,
            grading.balance,
        ],
    )
}

fn finite(label: &str, values: &[f32]) -> Result<(), SidecarError> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        invalid(&format!("{label} contains a non-finite value"))
    }
}

fn bounded(label: &str, value: f32, minimum: f32, maximum: f32) -> Result<(), SidecarError> {
    if (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        invalid(&format!("{label} is outside the safe range"))
    }
}

pub(super) fn validate_image(
    width: u32,
    height: u32,
    bytes: usize,
    channels: usize,
) -> Result<(), SidecarError> {
    if width == 0 || height == 0 || width > MAX_MASK_IMAGE_EDGE || height > MAX_MASK_IMAGE_EDGE {
        return invalid("mask image dimensions are invalid");
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| SidecarError::Invalid("mask image dimensions overflow".to_owned()))?;
    if bytes != expected {
        return invalid("mask image byte count does not match its dimensions");
    }
    Ok(())
}

pub(super) fn invalid<T>(message: &str) -> Result<T, SidecarError> {
    Err(SidecarError::Invalid(message.to_owned()))
}
