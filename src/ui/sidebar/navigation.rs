impl Sidebar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, layout: ScreenLayout, frame: &eframe::Frame) {
        let available_width = ui.available_width().max(1.0);
        let content_width = match layout {
            ScreenLayout::Horizontal => (available_width - Self::SCROLLBAR_GUTTER)
                .max(220.0)
                .min(available_width),
            ScreenLayout::Vertical => (available_width - Self::SCROLLBAR_GUTTER).max(1.0),
        };
        ui.set_width(content_width);
        ui.set_max_width(content_width);
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

        egui::ScrollArea::horizontal()
            .id_salt("develop-sidebar-tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (SidebarTab::Adjustments, "Adjustments"),
                        (SidebarTab::Masks, "Masks"),
                        (SidebarTab::Inpainting, "Inpainting"),
                        (SidebarTab::Export, "Export"),
                    ] {
                        ui.selectable_value(&mut app.sidebar_tab, tab, label);
                    }
                });
            });
        ui.add_space(2.0);
        ui.separator();

        if layout == ScreenLayout::Vertical {
            Self::show_vertical_section_tabs(ui, app);
        }

        let sidebar_scroll_source = if slider_scroll_locked(ui.ctx()) {
            egui::scroll_area::ScrollSource::NONE
        } else {
            egui::scroll_area::ScrollSource::default()
        };
        egui::ScrollArea::vertical()
            .id_salt("develop-sidebar-content")
            .scroll_source(sidebar_scroll_source)
            .auto_shrink([false, false])
            .show(ui, |ui| match app.sidebar_tab {
                SidebarTab::Adjustments => Self::show_adjustments(ui, app, layout, frame),
                SidebarTab::Masks => Self::show_masks(ui, app, layout, frame),
                SidebarTab::Inpainting => Self::show_inpainting(ui, app, layout, frame),
                SidebarTab::Export => Self::show_export(ui, app, frame),
            });
    }

    fn show_adjustments(
        ui: &mut Ui,
        app: &mut AurawApp,
        layout: ScreenLayout,
        frame: &eframe::Frame,
    ) {
        ui.horizontal(|ui| {
            ui.heading("Adjustments");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Reset all").clicked() {
                    app.reset_develop_adjustments();
                }
            });
        });
        ui.label(
            egui::RichText::new("Scene-referred RAW controls")
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(2.0);
        ui.separator();

        #[cfg(not(target_os = "android"))]
        Self::show_camera_profile_selector(ui, app, frame);

        let mut changed = false;
        let mut lens_changed = false;
        if layout == ScreenLayout::Vertical {
            match app.adjustment_section {
                AdjustmentSection::Light => {
                    changed |= Self::show_basic(ui, &mut app.exposure, false);
                }
                AdjustmentSection::ToneCurve => {
                    changed |= Self::show_tone_curve(
                        ui,
                        &mut app.exposure,
                        &mut app.tone_curve_tab,
                        false,
                    );
                }
                AdjustmentSection::Color => {
                    changed |= Self::show_color(ui, &mut app.exposure, false);
                }
                AdjustmentSection::ColorGrading => {
                    changed |= Self::show_color_grading(
                        ui,
                        &mut app.exposure.color_grading,
                        &mut app.color_grade_tab,
                        false,
                    );
                }
                AdjustmentSection::Effects => {
                    changed |= Self::show_presence(
                        ui,
                        &mut app.exposure,
                        app.expert_mode,
                        false,
                    );
                }
                AdjustmentSection::ColorMixer => {
                    changed |= Self::show_hsl(ui, &mut app.exposure, false);
                }
                AdjustmentSection::Optics => {
                    lens_changed |= Self::show_optics(ui, app, false);
                }
                AdjustmentSection::AdvancedRendering if app.expert_mode => {
                    changed |= Self::show_rendering(ui, &mut app.exposure, false);
                }
                AdjustmentSection::Raw if app.expert_mode => {
                    changed |= Self::show_raw(ui, &mut app.exposure, false);
                }
                _ => {}
            }
        } else {
            changed |= Self::show_basic(ui, &mut app.exposure, true);
            changed |= Self::show_tone_curve(
                ui,
                &mut app.exposure,
                &mut app.tone_curve_tab,
                true,
            );
            changed |= Self::show_color(ui, &mut app.exposure, true);
            changed |= Self::show_color_grading(
                ui,
                &mut app.exposure.color_grading,
                &mut app.color_grade_tab,
                true,
            );
            changed |= Self::show_presence(ui, &mut app.exposure, app.expert_mode, true);
            changed |= Self::show_hsl(ui, &mut app.exposure, true);
            lens_changed |= Self::show_optics(ui, app, true);
            if app.expert_mode {
                changed |= Self::show_rendering(ui, &mut app.exposure, true);
                changed |= Self::show_raw(ui, &mut app.exposure, true);
            }
        }

        if changed {
            app.exposure.sanitize_tone_curves();
            app.mark_pipeline_dirty();
        }
        if lens_changed {
            app.mark_lens_correction_dirty();
        }
    }

    #[cfg(not(target_os = "android"))]
    fn show_camera_profile_selector(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        if app.camera_profile_mode == crate::pipeline::CameraProfileMode::MatrixOnly {
            return;
        }
        let Some(raw) = app.loaded_raw.as_ref() else {
            return;
        };
        let candidates = raw.available_camera_profiles.clone();
        if candidates.is_empty() {
            return;
        }
        let active_source = raw.camera_profile_source.clone();
        let active_name = active_source
            .as_ref()
            .and_then(|active| {
                candidates
                    .iter()
                    .find(|candidate| candidate.path == *active)
                    .map(|candidate| candidate.name.clone())
            })
            .or_else(|| raw.camera_profile.name.clone())
            .unwrap_or_else(|| "Camera matrix".to_owned());

        if candidates.len() == 1 {
            ui.horizontal_wrapped(|ui| {
                ui.strong("Camera profile");
                ui.label(&active_name);
            });
            ui.separator();
            return;
        }

        let previous = app.selected_camera_profile.clone();
        let mut selection = previous.clone();
        let selected_text = previous
            .as_ref()
            .and_then(|selected| {
                candidates
                    .iter()
                    .find(|candidate| candidate.path == *selected)
                    .map(|candidate| candidate.name.clone())
            })
            .unwrap_or_else(|| format!("Automatic — {active_name}"));

        ui.horizontal(|ui| {
            ui.strong("Camera profile");
            egui::ComboBox::from_id_salt("current-image-camera-profile")
                .selected_text(selected_text)
                .width(ui.available_width().max(140.0))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selection, None, "Automatic (recommended)")
                        .on_hover_text("Use AuRaw's preferred matching profile for this camera.");
                    ui.separator();
                    for candidate in &candidates {
                        ui.selectable_value(
                            &mut selection,
                            Some(candidate.path.clone()),
                            &candidate.name,
                        )
                        .on_hover_text(candidate.path.display().to_string());
                    }
                });
        });
        ui.label(
            egui::RichText::new(format!(
                "{} matching DCP profiles found for {} {}.",
                candidates.len(), raw.camera_make, raw.camera_model
            ))
            .size(11.0)
            .color(ui.visuals().weak_text_color()),
        );
        ui.separator();

        if selection != previous {
            app.select_camera_profile_for_current(selection, frame);
        }
    }

    fn show_vertical_section_tabs(ui: &mut Ui, app: &mut AurawApp) {
        match app.sidebar_tab {
            SidebarTab::Adjustments => {
                if !app.expert_mode
                    && matches!(
                        app.adjustment_section,
                        AdjustmentSection::AdvancedRendering | AdjustmentSection::Raw
                    )
                {
                    app.adjustment_section = AdjustmentSection::Light;
                }
                Self::show_adjustment_tabs(ui, app);
            }
            SidebarTab::Masks => Self::show_mask_tabs(ui, app),
            SidebarTab::Inpainting | SidebarTab::Export => {}
        }
    }

    fn show_adjustment_tabs(ui: &mut Ui, app: &mut AurawApp) {
        egui::ScrollArea::horizontal()
            .id_salt("adjustment-section-tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (section, label) in [
                        (AdjustmentSection::Light, "Light"),
                        (AdjustmentSection::ToneCurve, "Tone Curve"),
                        (AdjustmentSection::Color, "Color"),
                        (AdjustmentSection::ColorGrading, "Color Grading"),
                        (AdjustmentSection::Effects, "Effects"),
                        (AdjustmentSection::ColorMixer, "Color Mixer"),
                        (AdjustmentSection::Optics, "Optics"),
                    ] {
                        ui.selectable_value(&mut app.adjustment_section, section, label);
                    }
                    if app.expert_mode {
                        ui.selectable_value(
                            &mut app.adjustment_section,
                            AdjustmentSection::AdvancedRendering,
                            "Advanced",
                        );
                        ui.selectable_value(
                            &mut app.adjustment_section,
                            AdjustmentSection::Raw,
                            "Raw",
                        );
                    }
                });
            });
        ui.separator();
    }

    fn show_mask_tabs(ui: &mut Ui, app: &mut AurawApp) {
        egui::ScrollArea::horizontal()
            .id_salt("mask-section-tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (section, label) in [
                        (MaskSection::Properties, "Mask Properties"),
                        (MaskSection::Light, "Light"),
                        (MaskSection::ToneCurve, "Tone Curve"),
                        (MaskSection::Color, "Color"),
                        (MaskSection::ColorGrading, "Color Grading"),
                        (MaskSection::Effects, "Effects"),
                        (MaskSection::ColorMixer, "Color Mixer"),
                    ] {
                        ui.selectable_value(&mut app.mask_section, section, label);
                    }
                });
            });
        ui.separator();
    }

    fn adjustment_section(
        ui: &mut Ui,
        title: &'static str,
        default_open: bool,
        foldable: bool,
        contents: impl FnOnce(&mut Ui),
    ) {
        if foldable {
            egui::CollapsingHeader::new(title)
                .default_open(default_open)
                .show(ui, contents);
        } else {
            contents(ui);
        }
    }
}
