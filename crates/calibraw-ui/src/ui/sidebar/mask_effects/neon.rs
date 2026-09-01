use super::{effect_color, effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::neon, MaskEffect, NeonEffectSettings};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut NeonEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::Neon, settings);
    super::super::Sidebar::adjustment_section(ui, MaskEffect::Neon.label(), true, false, |ui| {
        changed |= effect_slider(ui, &mut settings.amount, neon::AMOUNT);
        changed |= effect_slider(ui, &mut settings.edge_width, neon::EDGE_WIDTH);
        changed |= effect_slider(ui, &mut settings.detail, neon::DETAIL);
        changed |= effect_slider(ui, &mut settings.glow, neon::GLOW);
        changed |= effect_slider(ui, &mut settings.background, neon::BACKGROUND);
        changed |= effect_color(ui, "neon-color-picker", &mut settings.color, neon::COLOR);
    });
    changed
}
