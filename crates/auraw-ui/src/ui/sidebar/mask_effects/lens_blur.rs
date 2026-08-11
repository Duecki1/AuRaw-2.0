use super::super::{adjustment_slider_with_reset, LensBlurEffectSettings, Sidebar, Ui};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut LensBlurEffectSettings) -> bool {
    let defaults = LensBlurEffectSettings::default();
    let mut changed = false;

    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Lens Blur Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Lens Blur settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);

    Sidebar::adjustment_section(ui, "Lens Blur", true, false, |ui| {
        ui.small("Uses an aperture-shaped scene-linear blur for natural bokeh.");
        ui.add_space(3.0);
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Blends the lens-blurred result into the developed image."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Radius",
            &mut settings.radius,
            0.0..=48.0,
            1,
            0.1,
            Some("Controls the aperture radius in reference-image pixels."),
            defaults.radius,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Blades",
            &mut settings.blades,
            3.0..=12.0,
            0,
            1.0,
            Some("Sets the number of sides in the simulated aperture."),
            defaults.blades,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Rotation",
            &mut settings.rotation,
            -180.0..=180.0,
            0,
            1.0,
            Some("Rotates the simulated aperture."),
            defaults.rotation,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Highlights",
            &mut settings.highlight_boost,
            0.0..=100.0,
            0,
            0.5,
            Some("Gives bright samples more weight so bokeh highlights stand out."),
            defaults.highlight_boost,
        );
    });
    changed
}
