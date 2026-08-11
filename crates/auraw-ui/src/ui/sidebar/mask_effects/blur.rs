use super::super::{adjustment_slider_with_reset, BlurEffectSettings, Sidebar, Ui};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut BlurEffectSettings) -> bool {
    let defaults = BlurEffectSettings::default();
    let mut changed = false;

    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Blur Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Blur settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);

    Sidebar::adjustment_section(ui, "Blur", true, false, |ui| {
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Blends the blurred result into the developed image."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Radius",
            &mut settings.radius,
            0.0..=16.0,
            1,
            0.1,
            Some("Controls the scale-aware blur radius."),
            defaults.radius,
        );
    });

    changed
}
