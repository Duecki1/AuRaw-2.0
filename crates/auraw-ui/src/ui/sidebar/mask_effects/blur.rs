use super::{effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::blur, BlurEffectSettings, MaskEffect};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut BlurEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::Blur, settings);
    super::super::Sidebar::adjustment_section(ui, MaskEffect::Blur.label(), true, false, |ui| {
        changed |= effect_slider(ui, &mut settings.amount, blur::AMOUNT);
        changed |= effect_slider(ui, &mut settings.radius, blur::RADIUS);
    });
    changed
}
