use super::super::{adjustment_slider_with_reset, EdgeGlowEffectSettings, Sidebar, Ui};
use eframe::egui;

pub(crate) fn show(ui: &mut Ui, settings: &mut EdgeGlowEffectSettings) -> bool {
    let defaults = EdgeGlowEffectSettings::default();
    let mut changed = false;

    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Edge Glow Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Edge Glow settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);

    Sidebar::adjustment_section(ui, "Edge Glow", true, false, |ui| {
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls the strength of the emitted edge light."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Edge Width",
            &mut settings.edge_width,
            0.5..=8.0,
            1,
            0.05,
            Some("Sets the scale used to detect and widen edges."),
            defaults.edge_width,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Detail",
            &mut settings.detail,
            0.0..=100.0,
            0,
            0.5,
            Some("Higher values include finer, lower-contrast edges."),
            defaults.detail,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Glow",
            &mut settings.glow,
            0.0..=100.0,
            0,
            0.5,
            Some("Adds a broader halo around the detected edges."),
            defaults.glow,
        );

        ui.horizontal(|ui| {
            ui.label("Color");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                changed |= crate::ui::components::effect_color_picker::effect_color_picker(
                    ui,
                    "edge-glow-color-picker",
                    &mut settings.color,
                    "Edge Glow color",
                    "Choose the color emitted by the Edge Glow effect.",
                );
            });
        });
    });

    changed
}
