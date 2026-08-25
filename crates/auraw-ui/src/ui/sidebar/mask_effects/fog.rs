use super::{effect_color, effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::fog, FogEffectSettings, MaskEffect};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut FogEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::Fog, settings);
    super::super::Sidebar::adjustment_section(ui, MaskEffect::Fog.label(), true, false, |ui| {
        changed |= effect_slider(ui, &mut settings.amount, fog::AMOUNT);
        changed |= effect_slider(ui, &mut settings.density, fog::DENSITY);
        changed |= effect_slider(ui, &mut settings.scale, fog::SCALE);
        changed |= effect_slider(ui, &mut settings.softness, fog::SOFTNESS);
        changed |= effect_slider(ui, &mut settings.variation, fog::VARIATION);
        changed |= effect_slider(ui, &mut settings.seed, fog::SEED);
        changed |= effect_color(ui, "fog-color-picker", &mut settings.color, fog::COLOR);
    });
    changed
}
