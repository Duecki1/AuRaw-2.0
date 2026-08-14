use super::*;

impl Sidebar {
    pub(super) fn prepare_content_mask(app: &mut AurawApp, frame: &eframe::Frame, kind: MaskKind) {
        match kind {
            MaskKind::Subject | MaskKind::Background => app.request_subject_mask(frame),
            MaskKind::Object => {
                if let Err(error) = app.capture_mask_source(frame) {
                    app.report_ai_mask_error(error);
                }
            }
            MaskKind::LuminanceRange | MaskKind::ColorRange => {
                if let Err(error) = app.capture_mask_source(frame) {
                    app.status = error;
                    return;
                }
                let source = app.mask_source_cache.clone();
                if let Some(component) = app.masks.selected_component_mut() {
                    match &mut component.geometry {
                        MaskGeometry::LuminanceRange { source: target, .. }
                        | MaskGeometry::ColorRange { source: target, .. } => *target = source,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn show_local_mask_adjustment_section(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        section: MaskSection,
        selected_tab: &mut ToneCurveTab,
        selected_grade_tab: &mut ColorGradeTab,
        selected_hsl_color: &mut HslMixerColor,
    ) -> (bool, bool) {
        match section {
            MaskSection::Properties => (false, false),
            MaskSection::Light => Self::show_local_mask_light(ui, adjustment),
            MaskSection::ToneCurve => (
                Self::show_local_mask_tone_curve(ui, adjustment, selected_tab),
                false,
            ),
            MaskSection::Color => (Self::show_local_mask_color(ui, adjustment), false),
            MaskSection::ColorGrading => (
                Self::show_local_mask_color_grading(ui, adjustment, selected_grade_tab),
                false,
            ),
            MaskSection::Effects => (Self::show_local_mask_effects(ui, adjustment), false),
            MaskSection::ColorMixer => (
                Self::show_local_mask_color_mixer(ui, adjustment, selected_hsl_color),
                false,
            ),
        }
    }

    fn show_local_mask_light(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> (bool, bool) {
        let mut changed = false;
        let shadows_before = adjustment.shadows;
        let blacks_before = adjustment.blacks;
        changed |= gradient_adjustment_slider(
            ui,
            "Exposure",
            &mut adjustment.exposure,
            -5.0..=5.0,
            2,
            0.05,
            None,
            SliderGradient::Brightness,
        );
        changed |= gradient_adjustment_slider(
            ui,
            "Contrast",
            &mut adjustment.contrast,
            -100.0..=100.0,
            0,
            1.0,
            None,
            SliderGradient::Brightness,
        );
        changed |= gradient_adjustment_slider(
            ui,
            "Highlights",
            &mut adjustment.highlights,
            -100.0..=100.0,
            0,
            1.0,
            None,
            SliderGradient::Brightness,
        );
        changed |= gradient_adjustment_slider(
            ui,
            "Shadows",
            &mut adjustment.shadows,
            -100.0..=100.0,
            0,
            1.0,
            None,
            SliderGradient::Brightness,
        );
        changed |= gradient_adjustment_slider(
            ui,
            "Whites",
            &mut adjustment.whites,
            -100.0..=100.0,
            0,
            1.0,
            None,
            SliderGradient::Brightness,
        );
        changed |= gradient_adjustment_slider(
            ui,
            "Blacks",
            &mut adjustment.blacks,
            -100.0..=100.0,
            0,
            1.0,
            None,
            SliderGradient::Brightness,
        );
        (
            changed,
            adjustment.shadows != shadows_before || adjustment.blacks != blacks_before,
        )
    }

    fn show_local_mask_color(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        let mut changed = false;
        changed |= gradient_adjustment_slider(
            ui,
            "Temperature",
            &mut adjustment.temperature,
            -100.0..=100.0,
            0,
            1.0,
            None,
            SliderGradient::Temperature,
        );
        changed |= gradient_adjustment_slider(
            ui,
            "Tint",
            &mut adjustment.tint,
            -100.0..=100.0,
            0,
            1.0,
            None,
            SliderGradient::Tint,
        );
        changed |= hue_adjustment_slider(
            ui,
            &mut adjustment.hue,
            Some("Rotates colors inside the mask around the perceptual color wheel."),
        );
        changed |= gradient_adjustment_slider(
            ui,
            "Saturation",
            &mut adjustment.saturation,
            -100.0..=100.0,
            0,
            1.0,
            None,
            SliderGradient::Colorfulness,
        );
        changed
    }

    fn show_local_mask_effects(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        let mut changed = false;
        changed |= adjustment_slider(
            ui,
            "Texture",
            &mut adjustment.texture,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Clarity",
            &mut adjustment.clarity,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Dehaze",
            &mut adjustment.dehaze,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed
    }

    fn show_local_mask_color_grading(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        selected_grade_tab: &mut ColorGradeTab,
    ) -> bool {
        color_grading_editor(ui, &mut adjustment.color_grading, selected_grade_tab)
    }

    fn show_local_mask_tone_curve(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        selected_tab: &mut ToneCurveTab,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            let spacing = ui.spacing().item_spacing.x;
            let segment_width =
                ((ui.available_width() - crate::ui::theme::TOOLBAR_ICON_EDGE - spacing * 4.0)
                    / 4.0)
                    .max(32.0);
            for (tab, label, color) in [
                (ToneCurveTab::Rgb, "RGB", egui::Color32::WHITE),
                (ToneCurveTab::Red, "R", egui::Color32::from_rgb(238, 84, 84)),
                (
                    ToneCurveTab::Green,
                    "G",
                    egui::Color32::from_rgb(92, 210, 116),
                ),
                (
                    ToneCurveTab::Blue,
                    "B",
                    egui::Color32::from_rgb(88, 150, 245),
                ),
            ] {
                if crate::ui::theme::segmented_button(
                    ui,
                    egui::RichText::new(label).color(color),
                    *selected_tab == tab,
                    segment_width,
                )
                .clicked()
                {
                    *selected_tab = tab;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::ui::icons::phosphor_icon_button(
                    ui,
                    egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                    crate::ui::theme::toolbar_icon_size(),
                    "Reset the selected tone curve",
                )
                .clicked()
                {
                    match *selected_tab {
                        ToneCurveTab::Rgb => adjustment.tone_curve.reset(),
                        ToneCurveTab::Red => adjustment.tone_curve_red.reset(),
                        ToneCurveTab::Green => adjustment.tone_curve_green.reset(),
                        ToneCurveTab::Blue => adjustment.tone_curve_blue.reset(),
                    }
                    changed = true;
                }
            });
        });
        let (curve, color, description) = match *selected_tab {
            ToneCurveTab::Rgb => (
                &mut adjustment.tone_curve,
                egui::Color32::WHITE,
                "Composite luminance curve",
            ),
            ToneCurveTab::Red => (
                &mut adjustment.tone_curve_red,
                egui::Color32::from_rgb(238, 84, 84),
                "Red channel curve",
            ),
            ToneCurveTab::Green => (
                &mut adjustment.tone_curve_green,
                egui::Color32::from_rgb(92, 210, 116),
                "Green channel curve",
            ),
            ToneCurveTab::Blue => (
                &mut adjustment.tone_curve_blue,
                egui::Color32::from_rgb(88, 150, 245),
                "Blue channel curve",
            ),
        };
        ui.label(
            egui::RichText::new(description)
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        changed |= tone_curve_editor(ui, curve, color);
        if changed {
            adjustment.sanitize_tone_curves();
        }
        changed
    }

    fn show_local_mask_color_mixer(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        selected_color: &mut HslMixerColor,
    ) -> bool {
        hsl_mixer(
            ui,
            selected_color,
            &mut adjustment.hsl_hue,
            &mut adjustment.hsl_saturation,
            &mut adjustment.hsl_luminance,
        )
    }
}
