use super::super::{adjustment_slider_with_reset, LightRaysEffectSettings, Sidebar, Ui};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut LightRaysEffectSettings) -> bool {
    let defaults = LightRaysEffectSettings::default();
    let mut changed = false;

    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Light Rays Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Light Rays settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);

    Sidebar::adjustment_section(ui, "Light Rays", true, false, |ui| {
        ui.small(
            "The mask is the light source. Rays converge on the source point and travel beyond the mask.",
        );
        ui.add_space(3.0);
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls the strength of the emitted light shafts."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Length",
            &mut settings.length,
            0.0..=200.0,
            0,
            1.0,
            Some("Ray reach as a percentage of the image's shorter edge."),
            defaults.length,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Source X",
            &mut settings.source[0],
            -50.0..=150.0,
            0,
            1.0,
            Some("Horizontal source position in the full image; values outside 0–100 place it beyond the frame."),
            defaults.source[0],
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Source Y",
            &mut settings.source[1],
            -50.0..=150.0,
            0,
            1.0,
            Some("Vertical source position in the full image; values outside 0–100 place it beyond the frame."),
            defaults.source[1],
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Spread",
            &mut settings.spread,
            0.0..=45.0,
            1,
            0.25,
            Some("Widens the cone sampled around each radial shaft."),
            defaults.spread,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Fade",
            &mut settings.fade,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls how quickly ray intensity falls off with distance."),
            defaults.fade,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Ray Count",
            &mut settings.ray_count,
            4.0..=96.0,
            0,
            1.0,
            Some("Controls the approximate number of broad shafts around the source."),
            defaults.ray_count,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Variation",
            &mut settings.variation,
            0.0..=100.0,
            0,
            0.5,
            Some("Breaks uniform emission into stronger and weaker god rays."),
            defaults.variation,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Softness",
            &mut settings.softness,
            0.0..=100.0,
            0,
            0.5,
            Some("Softens shaft edges and blends neighbouring source directions."),
            defaults.softness,
        );

        ui.horizontal(|ui| {
            ui.label("Color");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                changed |= crate::ui::components::effect_color_picker::effect_color_picker(
                    ui,
                    "light-rays-color-picker",
                    &mut settings.color,
                    "Light Rays color",
                    "Choose the color emitted by the Light Rays effect.",
                );
            });
        });
    });

    changed
}
