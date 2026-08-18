use super::*;

impl Sidebar {
    pub(super) fn show_mask_effect_picker(ui: &mut Ui, effect: &mut MaskEffect) -> bool {
        let before = *effect;
        crate::ui::theme::section_card(ui, "Mask type", |ui| {
            ui.add_space(4.0);
            let button = egui::Button::new(effect.label())
                .right_text(egui_phosphor::regular::CARET_DOWN)
                .min_size(egui::vec2(ui.available_width(), crate::ui::theme::CONTROL_HEIGHT));
            egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
                ui.set_min_width(190.0);
                if ui
                    .selectable_label(*effect == MaskEffect::Adjustment, "Adjustment")
                    .on_hover_text("Use the mask with the existing local adjustment controls.")
                    .clicked()
                {
                    *effect = MaskEffect::Adjustment;
                    ui.close();
                }

                ui.separator();
                for category in MaskEffectCategory::ALL {
                    ui.menu_button(category.label(), |ui| {
                        ui.set_min_width(180.0);
                        for candidate in MaskEffect::ALL {
                            if candidate.category() != Some(category) {
                                continue;
                            }
                            if ui
                                .selectable_label(*effect == candidate, candidate.label())
                                .clicked()
                            {
                                *effect = candidate;
                                ui.close();
                            }
                        }
                    });
                }
            });
        });
        before != *effect
    }

    pub(super) fn show_mask_effect_settings(ui: &mut Ui, mask: &mut LocalMask) -> bool {
        match mask.effect {
            MaskEffect::Blur => mask_effects::blur::show(ui, &mut mask.effect_settings.blur),
            MaskEffect::LensBlur => {
                mask_effects::lens_blur::show(ui, &mut mask.effect_settings.lens_blur)
            }
            MaskEffect::MotionBlur => {
                mask_effects::motion_blur::show(ui, &mut mask.effect_settings.motion_blur)
            }
            MaskEffect::RadialBlur => {
                mask_effects::radial_blur::show(ui, &mut mask.effect_settings.radial_blur)
            }
            MaskEffect::TiltShift => {
                let is_fullscreen_mask = Self::is_plain_fullscreen_mask(mask);
                mask_effects::tilt_shift::show(
                    ui,
                    &mut mask.effect_settings.tilt_shift,
                    is_fullscreen_mask,
                )
            }
            MaskEffect::EdgeGlow => {
                mask_effects::edge_glow::show(ui, &mut mask.effect_settings.edge_glow)
            }
            MaskEffect::Glow => mask_effects::glow::show(ui, &mut mask.effect_settings.glow),
            MaskEffect::LightRays => {
                mask_effects::light_rays::show(ui, &mut mask.effect_settings.light_rays)
            }
            MaskEffect::Neon => mask_effects::neon::show(ui, &mut mask.effect_settings.neon),
            MaskEffect::Pixelate => {
                mask_effects::pixelate::show(ui, &mut mask.effect_settings.pixelate)
            }
            MaskEffect::Fog => mask_effects::fog::show(ui, &mut mask.effect_settings.fog),
            MaskEffect::Smoke => mask_effects::smoke::show(ui, &mut mask.effect_settings.smoke),
            MaskEffect::Adjustment => false,
        }
    }

    pub(super) fn is_plain_fullscreen_mask(mask: &LocalMask) -> bool {
        !mask.invert
            && matches!(
                mask.components.as_slice(),
                [component]
                    if component.enabled
                        && !component.invert
                        && component.kind == MaskKind::Fullscreen
            )
    }

    pub(super) fn apply_mask_geometry_change(ui: &Ui, app: &mut AurawApp, mask_index: usize, changed: bool) {
        if changed && ui.input(|input| input.pointer.primary_down()) {
            app.note_mask_geometry_interaction(mask_index);
        } else if changed {
            app.finish_mask_geometry_interaction();
            app.mark_mask_geometry_dirty(mask_index);
        } else if !ui.input(|input| input.pointer.primary_down()) {
            // The last value of a drag may arrive in the frame after its final
            // movement. Commit it as soon as the pointer is released.
            app.finish_mask_geometry_interaction();
        }
    }

    fn mask_grow_slider(ui: &mut Ui, grow: &mut f32) -> bool {
        adjustment_slider(
            ui,
            "Grow",
            grow,
            -1.0..=1.0,
            2,
            0.01,
            Some("Positive values expand the mask; negative values shrink it inward."),
        )
    }

    fn mask_feather_slider(
        ui: &mut Ui,
        label: &str,
        feather: &mut f32,
        range: std::ops::RangeInclusive<f32>,
        help: &str,
        reset: f32,
    ) -> bool {
        adjustment_slider_with_reset(ui, label, feather, range, 2, 0.01, Some(help), reset)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn show_vertical_mask_properties(
        ui: &mut Ui,
        mask: &mut crate::pipeline::LocalMask,
        component_index: usize,
        brush_mode: &mut BrushMode,
        subject_controls: (&mut bool, crate::ai_masks::BiRefNetQuality, bool),
        refinement_controls: (&mut bool, &mut f32, &mut f32, &mut f32, &mut bool),
        request_object: &mut bool,
        request_landscape: &mut bool,
    ) -> bool {
        let (request_subject, birefnet_quality, birefnet_quality_change_enabled) = subject_controls;
        let (
            refinement_active,
            refinement_size,
            refinement_feather,
            refinement_flow,
            clear_refinement,
        ) = refinement_controls;
        let mut opacity = mask.opacity;
        let mut geometry_changed = adjustment_slider(
            ui,
            "Mask opacity",
            &mut opacity,
            0.0..=1.0,
            2,
            0.01,
            Some("Controls the strength of the entire mask before its selected type is applied."),
        );
        if geometry_changed {
            mask.set_opacity(opacity);
        }

        let Some(component) = mask.components.get_mut(component_index) else {
            return geometry_changed;
        };

        ui.add_space(4.0);
        ui.scope(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.strong(component.name.as_str());
                ui.weak(component.kind.label());
                let mut inverted = component.invert;
                if ui.checkbox(&mut inverted, "Invert").changed() {
                    component.common.toggle_invert();
                    geometry_changed = true;
                }
                if component_index > 0 {
                    let mut combine = component.combine;
                    egui::ComboBox::from_id_salt("vertical-mask-combine")
                        .selected_text(combine.label())
                        .show_ui(ui, |ui| {
                            for mode in [
                                MaskCombineMode::Add,
                                MaskCombineMode::Subtract,
                                MaskCombineMode::Intersect,
                            ] {
                                ui.selectable_value(&mut combine, mode, mode.label());
                            }
                        });
                    geometry_changed |= component.set_combine(combine);
                }
            });

            match &mut component.geometry {
                MaskGeometry::Fullscreen => {
                    ui.label("Covers the complete image with uniform mask strength.");
                }
                MaskGeometry::Brush {
                    size,
                    feather,
                    opacity_enabled,
                    opacity,
                    overlap_enabled,
                    stroke_starts,
                    dabs,
                } => {
                    ui.horizontal(|ui| {
                        let width = ((ui.available_width() - ui.spacing().item_spacing.x) * 0.5)
                            .max(1.0);
                        if crate::ui::theme::segmented_button(
                            ui,
                            "Brush",
                            *brush_mode == BrushMode::Paint,
                            width,
                        )
                        .clicked()
                        {
                            *brush_mode = BrushMode::Paint;
                        }
                        if crate::ui::theme::segmented_button(
                            ui,
                            "Eraser",
                            *brush_mode == BrushMode::Erase,
                            width,
                        )
                        .clicked()
                        {
                            *brush_mode = BrushMode::Erase;
                        }
                    });
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Size",
                        size,
                        0.0025..=0.25,
                        3,
                        0.0025,
                        Some("Brush stays the same size on screen; zoom in for finer image-space detail."),
                    );
                    geometry_changed |= Self::mask_feather_slider(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        "Softness from the brush core to its edge.",
                        0.55,
                    );
                    ui.horizontal(|ui| {
                        geometry_changed |= ui
                            .checkbox(opacity_enabled, "Opacity")
                            .on_hover_text(
                                "Use the opacity setting for newly drawn brush and eraser strokes. \
                                 Disabled strokes always use 100% opacity.",
                            )
                            .changed();
                        geometry_changed |= ui
                            .checkbox(overlap_enabled, "Overlapping")
                            .on_hover_text(
                                "Allow separate brush strokes to build opacity where they overlap. \
                                 For example, 10% over 10% produces about 19% coverage.",
                            )
                            .changed();
                    });
                    ui.add_enabled_ui(*opacity_enabled, |ui| {
                        geometry_changed |= adjustment_slider(
                            ui,
                            "Stroke opacity",
                            opacity,
                            0.0..=1.0,
                            2,
                            0.01,
                            Some(
                                "Controls only newly drawn brush and eraser strokes. Existing \
                                 strokes and the whole-mask opacity are unchanged.",
                            ),
                        );
                    });
                    if crate::ui::icons::phosphor_icon_button(
                        ui,
                        egui_phosphor::regular::ERASER,
                        crate::ui::theme::toolbar_icon_size(),
                        "Clear brush strokes",
                    )
                    .clicked()
                    {
                        dabs.clear();
                        stroke_starts.clear();
                        geometry_changed = true;
                    }
                    ui.small(format!("{} brush dabs", dabs.len()));
                }
                MaskGeometry::Radial { feather, .. } => {
                    geometry_changed |= Self::mask_feather_slider(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        "Soft transition from the ellipse interior to its edge.",
                        0.55,
                    );
                }
                MaskGeometry::Linear { feather, .. } => {
                    geometry_changed |= Self::mask_feather_slider(
                        ui,
                        "Feather",
                        feather,
                        0.02..=1.0,
                        "Controls the width of the gradient transition.",
                        1.0,
                    );
                }
                MaskGeometry::Ai {
                    mask: generated_mask,
                    grow,
                    feather,
                } => {
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(
                                *refinement_active,
                                if *refinement_active { "Done" } else { "Refine" },
                            )
                            .on_hover_text(
                                "Fine-tune the shared Subject / Not Subject boundary with a brush.",
                            )
                            .clicked()
                        {
                            *refinement_active = !*refinement_active;
                        }
                    });
                    if *refinement_active {
                        crate::ui::theme::section_card(ui, "Subject refinement", |ui| {
                            ui.horizontal(|ui| {
                                let width = ((ui.available_width()
                                    - ui.spacing().item_spacing.x)
                                    * 0.5)
                                    .max(1.0);
                                if crate::ui::theme::segmented_button(
                                    ui,
                                    "Add subject",
                                    *brush_mode == BrushMode::Paint,
                                    width,
                                )
                                .clicked()
                                {
                                    *brush_mode = BrushMode::Paint;
                                }
                                if crate::ui::theme::segmented_button(
                                    ui,
                                    "Subtract subject",
                                    *brush_mode == BrushMode::Erase,
                                    width,
                                )
                                .clicked()
                                {
                                    *brush_mode = BrushMode::Erase;
                                }
                            });
                            adjustment_slider(
                                ui,
                                "Size",
                                refinement_size,
                                0.0025..=0.25,
                                3,
                                0.0025,
                                Some(
                                    "Brush stays the same size on screen; zoom in for finer image-space detail.",
                                ),
                            );
                            Self::mask_feather_slider(
                                ui,
                                "Feather",
                                refinement_feather,
                                0.0..=1.0,
                                "Softness of newly painted refinement strokes.",
                                0.55,
                            );
                            adjustment_slider_with_reset(
                                ui,
                                "Flow / opacity",
                                refinement_flow,
                                0.01..=1.0,
                                2,
                                0.01,
                                Some("Strength captured by newly painted add/subtract strokes."),
                                1.0,
                            );
                            if crate::ui::icons::phosphor_icon_button(
                                ui,
                                egui_phosphor::regular::ERASER,
                                    crate::ui::theme::toolbar_icon_size(),
                                "Clear subject refinement",
                            )
                            .clicked()
                            {
                                *clear_refinement = true;
                            }
                        });
                    }
                    let has_generated_mask = generated_mask.is_some();
                    let action = if has_generated_mask {
                        "Rerun"
                    } else {
                        "Generate"
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!(
                            "{action} in {} quality",
                            birefnet_quality.label()
                        ));
                        if ui
                            .add_enabled(
                                birefnet_quality_change_enabled,
                                egui::Button::new(format!("{action} subject mask")),
                            )
                            .clicked()
                        {
                            *request_subject = true;
                        }
                        if !birefnet_quality_change_enabled {
                            ui.spinner();
                        }
                    });
                    geometry_changed |= Self::mask_grow_slider(ui, grow);
                    geometry_changed |= Self::mask_feather_slider(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        "Softens the BiRefNet subject boundary.",
                        0.0,
                    );
                }
                MaskGeometry::Object {
                    mask: generated_mask,
                    grow,
                    feather,
                    brush_size,
                    edge_refine,
                    strokes,
                } => {
                    *brush_mode = BrushMode::Paint;
                    ui.label(if generated_mask.is_some() {
                        "Draw again on the image to replace this object selection from scratch."
                    } else {
                        "Paint through the middle of the object part you want to select."
                    });
                    ui.strong("Selection brush");
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Size",
                        brush_size,
                        0.0025..=0.25,
                        3,
                        0.0025,
                        Some("Controls the hard-edged selection brush. Its on-screen size stays constant while zooming for finer detail."),
                    );
                    ui.add_space(4.0);
                    geometry_changed |= Self::mask_grow_slider(ui, grow);
                    geometry_changed |= Self::mask_feather_slider(
                        ui,
                        "Mask feather",
                        feather,
                        0.0..=1.0,
                        "Softens the final object mask after SAM selection.",
                        0.0,
                    );
                    let refine_changed = adjustment_slider(
                        ui,
                        "Edge refine",
                        edge_refine,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Aligns uncertain SAM boundaries to local image edges."),
                    );
                    geometry_changed |= refine_changed;
                    if refine_changed && !strokes.is_empty() {
                        *request_object = true;
                    }
                    ui.horizontal_wrapped(|ui| {
                        if crate::ui::icons::phosphor_icon_button(
                            ui,
                            egui_phosphor::regular::ARROW_CLOCKWISE,
                            crate::ui::theme::toolbar_icon_size(),
                            "Recalculate object selection",
                        )
                        .clicked()
                        {
                            *request_object = true;
                        }
                        if crate::ui::icons::phosphor_icon_button(
                            ui,
                            egui_phosphor::regular::X,
                            crate::ui::theme::toolbar_icon_size(),
                            "Clear object selection",
                        )
                        .clicked()
                        {
                            strokes.clear();
                            *generated_mask = None;
                            geometry_changed = true;
                        }
                    });
                    ui.small(format!("{} selection stroke(s)", strokes.len()));
                }
                MaskGeometry::Landscape {
                    mask: generated_mask,
                    category,
                    grow,
                    feather,
                } => {
                    ui.label("Choose a landscape element, then generate its semantic mask.");
                    let before = *category;
                    egui::ComboBox::from_id_salt("landscape-mask-category")
                        .selected_text(category.label())
                        .show_ui(ui, |ui| {
                            for option in crate::pipeline::LandscapeCategory::ALL {
                                ui.selectable_value(category, option, option.label());
                            }
                        });
                    if before != *category {
                        *generated_mask = None;
                        geometry_changed = true;
                    }
                    if ui.button("Generate Mask").clicked() {
                        *request_landscape = true;
                    }
                    geometry_changed |= Self::mask_grow_slider(ui, grow);
                    geometry_changed |= Self::mask_feather_slider(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        "Softens the semantic boundary after generation.",
                        0.0,
                    );
                }
                MaskGeometry::LuminanceRange {
                    low,
                    high,
                    grow,
                    feather,
                    ..
                } => {
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Range low",
                        low,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Lowest included scene luminance."),
                    );
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Range high",
                        high,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Highest included scene luminance."),
                    );
                    geometry_changed |= Self::mask_grow_slider(ui, grow);
                    geometry_changed |= Self::mask_feather_slider(
                        ui,
                        "Range feather",
                        feather,
                        0.0..=1.0,
                        "Softens both luminance-range boundaries.",
                        0.15,
                    );
                }
                MaskGeometry::ColorRange {
                    tolerance,
                    grow,
                    feather,
                    sampled,
                    ..
                } => {
                    ui.label(if *sampled {
                        "Drag on the image to choose another color."
                    } else {
                        "Drag on the image to sample a color."
                    });
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Tolerance",
                        tolerance,
                        0.005..=1.0,
                        3,
                        0.005,
                        Some("Expands the selected color region in perceptual OkLab space."),
                    );
                    geometry_changed |= Self::mask_grow_slider(ui, grow);
                    geometry_changed |= Self::mask_feather_slider(
                        ui,
                        "Color feather",
                        feather,
                        0.0..=1.0,
                        "Softens the color-distance cutoff.",
                        0.12,
                    );
                }
                MaskGeometry::Placeholder => {
                    ui.label("This mask type is not implemented yet.");
                }
            }
        });

        geometry_changed
    }
}
