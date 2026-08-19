use super::{effect_color, effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::smoke, MaskEffect, SmokeEffectSettings};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut SmokeEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::Smoke, settings);
    super::super::Sidebar::adjustment_section(ui, MaskEffect::Smoke.label(), true, false, |ui| {
        ui.small(
            "Smoke is generated in full-image coordinates and blended through the editable mask.",
        );
        ui.add_space(3.0);
        changed |= effect_slider(ui, &mut settings.amount, smoke::AMOUNT);
        changed |= effect_slider(ui, &mut settings.density, smoke::DENSITY);
        changed |= effect_slider(ui, &mut settings.scale, smoke::SCALE);
        changed |= effect_slider(ui, &mut settings.turbulence, smoke::TURBULENCE);
        changed |= effect_slider(ui, &mut settings.softness, smoke::SOFTNESS);
        changed |= effect_slider(ui, &mut settings.angle, smoke::ANGLE);
        changed |= effect_slider(ui, &mut settings.seed, smoke::SEED);
        changed |= effect_color(ui, "smoke-color-picker", &mut settings.color, smoke::COLOR);
    });
    changed
}
