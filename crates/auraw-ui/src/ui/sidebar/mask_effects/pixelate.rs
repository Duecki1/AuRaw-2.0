use super::super::{adjustment_slider_with_reset, PixelateEffectSettings, Sidebar, Ui};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut PixelateEffectSettings) -> bool {
    let defaults = PixelateEffectSettings::default();
    let mut changed = false;

    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Pixelate Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Pixelate settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);

    Sidebar::adjustment_section(ui, "Pixelate", true, false, |ui| {
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Blends the pixelated result into the developed image."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Block Size",
            &mut settings.block_size,
            2.0..=32.0,
            0,
            1.0,
            Some("Controls the scale-aware size of each square pixel block."),
            defaults.block_size,
        );
    });

    changed
}
