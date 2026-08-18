use super::*;

fn local_param_slider(
    ui: &mut Ui,
    value: &mut f32,
    spec: crate::pipeline::effect_params::FloatParamSpec,
) -> bool {
    adjustment_slider(
        ui,
        spec.label,
        value,
        spec.range(),
        spec.decimals,
        spec.step,
        spec.tooltip,
    )
}

fn local_gradient_param_slider(
    ui: &mut Ui,
    value: &mut f32,
    spec: crate::pipeline::effect_params::FloatParamSpec,
    gradient: SliderGradient,
) -> bool {
    gradient_adjustment_slider(
        ui,
        spec.label,
        value,
        spec.range(),
        spec.decimals,
        spec.step,
        spec.tooltip,
        gradient,
    )
}

impl Sidebar {
    pub(super) fn prepare_content_mask(app: &mut AurawApp, frame: &eframe::Frame, kind: MaskKind) {
        match kind {
            MaskKind::Subject | MaskKind::Background => app.request_subject_mask(frame),
            MaskKind::Object => {
                if let Err(error) = app.capture_mask_source(frame) {
                    app.report_ai_mask_error(error);
                }
            }
            MaskKind::Landscape => {
                if let Err(error) = app.capture_mask_source(frame) {
                    app.report_ai_mask_error(error);
                }
            }
            MaskKind::LuminanceRange | MaskKind::ColorRange => {
                if let Err(error) = app.capture_mask_source(frame) {
                    app.ui.status = error;
                    return;
                }
                let source = app.masks.source_cache.clone();
                if let Some(component) = app.masks.stack.selected_component_mut() {
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
        use crate::pipeline::effect_params::adjustment as params;

        let mut changed = false;
        let shadows_before = adjustment.shadows;
        let blacks_before = adjustment.blacks;
        changed |= local_gradient_param_slider(
            ui,
            &mut adjustment.exposure,
            params::EXPOSURE,
            SliderGradient::Brightness,
        );
        changed |= local_gradient_param_slider(
            ui,
            &mut adjustment.contrast,
            params::CONTRAST,
            SliderGradient::Brightness,
        );
        changed |= local_gradient_param_slider(
            ui,
            &mut adjustment.highlights,
            params::HIGHLIGHTS,
            SliderGradient::Brightness,
        );
        changed |= local_gradient_param_slider(
            ui,
            &mut adjustment.shadows,
            params::SHADOWS,
            SliderGradient::Brightness,
        );
        changed |= local_gradient_param_slider(
            ui,
            &mut adjustment.whites,
            params::WHITES,
            SliderGradient::Brightness,
        );
        changed |= local_gradient_param_slider(
            ui,
            &mut adjustment.blacks,
            params::BLACKS,
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
        use crate::pipeline::effect_params::adjustment as params;

        let mut changed = false;
        changed |= local_gradient_param_slider(
            ui,
            &mut adjustment.temperature,
            params::TEMPERATURE,
            SliderGradient::Temperature,
        );
        changed |= local_gradient_param_slider(
            ui,
            &mut adjustment.tint,
            params::TINT,
            SliderGradient::Tint,
        );
        changed |= hue_adjustment_slider(ui, &mut adjustment.hue, params::HUE.tooltip);
        changed |= local_gradient_param_slider(
            ui,
            &mut adjustment.saturation,
            params::SATURATION,
            SliderGradient::Colorfulness,
        );
        changed
    }

    fn show_local_mask_effects(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        use crate::pipeline::effect_params::adjustment as params;

        let mut changed = false;
        changed |= local_param_slider(ui, &mut adjustment.texture, params::TEXTURE);
        changed |= local_param_slider(ui, &mut adjustment.clarity, params::CLARITY);
        changed |= local_param_slider(ui, &mut adjustment.dehaze, params::DEHAZE);
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
