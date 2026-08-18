use super::{effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::motion_blur, MaskEffect, MotionBlurEffectSettings};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut MotionBlurEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::MotionBlur, settings);
    super::super::Sidebar::adjustment_section(
        ui,
        MaskEffect::MotionBlur.label(),
        true,
        false,
        |ui| {
            changed |= effect_slider(ui, &mut settings.amount, motion_blur::AMOUNT);
            changed |= effect_slider(ui, &mut settings.distance, motion_blur::DISTANCE);
            changed |= effect_slider(ui, &mut settings.angle, motion_blur::ANGLE);
        },
    );
    changed
}
