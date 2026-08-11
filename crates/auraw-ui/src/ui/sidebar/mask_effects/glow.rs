use super::super::{adjustment_slider_with_reset, GlowEffectSettings, Sidebar, Ui};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut GlowEffectSettings) -> bool {
    let defaults = GlowEffectSettings::default();
    let mut changed = false;

    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Glow Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Glow settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);

    Sidebar::adjustment_section(ui, "Glow", true, false, |ui| {
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls the strength of the bright core and emitted halo."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Radius",
            &mut settings.radius,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls how far the glow spreads beyond the mask."),
            defaults.radius,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Core",
            &mut settings.core,
            0.0..=100.0,
            0,
            0.5,
            Some("Makes the masked source brighter and more white-hot."),
            defaults.core,
        );

        ui.horizontal(|ui| {
            ui.label("Color");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                changed |= ui
                    .color_edit_button_rgb(&mut settings.color)
                    .on_hover_text("Choose the color emitted by the Glow effect.")
                    .changed();
            });
        });
    });

    changed
}
