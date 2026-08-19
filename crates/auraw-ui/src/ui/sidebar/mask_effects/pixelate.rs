use super::{effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::pixelate, MaskEffect, PixelateEffectSettings};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut PixelateEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::Pixelate, settings);
    super::super::Sidebar::adjustment_section(ui, MaskEffect::Pixelate.label(), true, false, |ui| {
        changed |= effect_slider(ui, &mut settings.amount, pixelate::AMOUNT);
        changed |= effect_slider(ui, &mut settings.block_size, pixelate::BLOCK_SIZE);
    });
    changed
}
