use super::super::{adjustment_slider_with_reset, FogEffectSettings, Sidebar, Ui};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut FogEffectSettings) -> bool {
    let defaults = FogEffectSettings::default();
    let mut changed = false;

    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Fog Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Fog settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);

    Sidebar::adjustment_section(ui, "Fog", true, false, |ui| {
        ui.small(
            "Fog is generated in full-image coordinates and blended through the editable mask.",
        );
        ui.add_space(3.0);
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls the overall strength of the atmospheric veil."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Density",
            &mut settings.density,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls how opaque the fog becomes."),
            defaults.density,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Scale",
            &mut settings.scale,
            1.0..=100.0,
            0,
            0.5,
            Some("Higher values create broader fog banks."),
            defaults.scale,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Softness",
            &mut settings.softness,
            0.0..=100.0,
            0,
            0.5,
            Some("Softens transitions between clear and foggy areas."),
            defaults.softness,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Variation",
            &mut settings.variation,
            0.0..=100.0,
            0,
            0.5,
            Some("Varies the fog density across the image."),
            defaults.variation,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Seed",
            &mut settings.seed,
            0.0..=1_000.0,
            0,
            1.0,
            Some("Chooses another deterministic fog pattern."),
            defaults.seed,
        );

        ui.horizontal(|ui| {
            ui.label("Color");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                changed |= crate::ui::components::effect_color_picker::effect_color_picker(
                    ui,
                    "fog-color-picker",
                    &mut settings.color,
                    "Fog color",
                    "Choose the color of the atmospheric veil.",
                );
            });
        });
    });

    changed
}
