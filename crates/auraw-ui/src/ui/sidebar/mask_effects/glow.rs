use super::{effect_color, effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::glow, GlowEffectSettings, MaskEffect};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut GlowEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::Glow, settings);
    super::super::Sidebar::adjustment_section(ui, MaskEffect::Glow.label(), true, false, |ui| {
        changed |= effect_slider(ui, &mut settings.amount, glow::AMOUNT);
        changed |= effect_slider(ui, &mut settings.radius, glow::RADIUS);
        changed |= effect_slider(ui, &mut settings.core, glow::CORE);
        changed |= effect_color(ui, "glow-color-picker", &mut settings.color, glow::COLOR);
    });
    changed
}
