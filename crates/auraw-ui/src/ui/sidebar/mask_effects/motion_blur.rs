use super::super::{adjustment_slider_with_reset, MotionBlurEffectSettings, Sidebar, Ui};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut MotionBlurEffectSettings) -> bool {
    let defaults = MotionBlurEffectSettings::default();
    let mut changed = false;
    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Motion Blur Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Motion Blur settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);
    Sidebar::adjustment_section(ui, "Motion Blur", true, false, |ui| {
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Blends the directional blur into the developed image."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Distance",
            &mut settings.distance,
            0.0..=96.0,
            1,
            0.1,
            Some("Controls the total shutter trail in reference-image pixels."),
            defaults.distance,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Angle",
            &mut settings.angle,
            -180.0..=180.0,
            0,
            1.0,
            Some("Sets the direction of motion."),
            defaults.angle,
        );
    });
    changed
}
