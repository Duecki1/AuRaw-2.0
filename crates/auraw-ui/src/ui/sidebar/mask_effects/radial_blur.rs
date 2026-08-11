use super::super::{
    adjustment_slider_with_reset, RadialBlurEffectSettings, RadialBlurMode, Sidebar, Ui,
};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut RadialBlurEffectSettings) -> bool {
    let defaults = RadialBlurEffectSettings::default();
    let mut changed = false;
    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Radial Blur Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Radial Blur settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);
    Sidebar::adjustment_section(ui, "Radial Blur", true, false, |ui| {
        ui.horizontal(|ui| {
            ui.label("Mode");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                egui::ComboBox::from_id_salt("radial-blur-mode")
                    .selected_text(settings.mode.label())
                    .show_ui(ui, |ui| {
                        for mode in RadialBlurMode::ALL {
                            changed |= ui
                                .selectable_value(&mut settings.mode, mode, mode.label())
                                .changed();
                        }
                    });
            });
        });
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Blends the radial trail into the developed image."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Strength",
            &mut settings.strength,
            0.0..=96.0,
            1,
            0.1,
            Some("Sets the maximum trail length in reference-image pixels."),
            defaults.strength,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Center X",
            &mut settings.center[0],
            -50.0..=150.0,
            0,
            1.0,
            Some("Horizontal origin in the full image; values may extend beyond the frame."),
            defaults.center[0],
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Center Y",
            &mut settings.center[1],
            -50.0..=150.0,
            0,
            1.0,
            Some("Vertical origin in the full image; values may extend beyond the frame."),
            defaults.center[1],
        );
    });
    changed
}
