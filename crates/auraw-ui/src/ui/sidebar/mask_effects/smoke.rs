use super::super::{adjustment_slider_with_reset, Sidebar, SmokeEffectSettings, Ui};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut SmokeEffectSettings) -> bool {
    let defaults = SmokeEffectSettings::default();
    let mut changed = false;

    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Smoke Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Smoke settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);

    Sidebar::adjustment_section(ui, "Smoke", true, false, |ui| {
        ui.small(
            "Smoke is generated in full-image coordinates and blended through the editable mask.",
        );
        ui.add_space(3.0);
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls the overall strength of the smoke overlay."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Density",
            &mut settings.density,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls the opacity and body of the plumes."),
            defaults.density,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Scale",
            &mut settings.scale,
            1.0..=100.0,
            0,
            0.5,
            Some("Higher values create larger smoke plumes."),
            defaults.scale,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Turbulence",
            &mut settings.turbulence,
            0.0..=100.0,
            0,
            0.5,
            Some("Adds curls and distortion to the smoke."),
            defaults.turbulence,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Softness",
            &mut settings.softness,
            0.0..=100.0,
            0,
            0.5,
            Some("Softens the boundaries of individual plumes."),
            defaults.softness,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Angle",
            &mut settings.angle,
            -180.0..=180.0,
            0,
            1.0,
            Some("Rotates the direction of the smoke flow."),
            defaults.angle,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Seed",
            &mut settings.seed,
            0.0..=1_000.0,
            0,
            1.0,
            Some("Chooses another deterministic smoke pattern."),
            defaults.seed,
        );

        ui.horizontal(|ui| {
            ui.label("Color");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                changed |= crate::ui::components::effect_color_picker::effect_color_picker(
                    ui,
                    "smoke-color-picker",
                    &mut settings.color,
                    "Smoke color",
                    "Choose the color of the smoke plumes.",
                );
            });
        });
    });

    changed
}
