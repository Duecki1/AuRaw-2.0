use super::{effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::lens_blur, LensBlurEffectSettings, MaskEffect};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut LensBlurEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::LensBlur, settings);
    super::super::Sidebar::adjustment_section(ui, MaskEffect::LensBlur.label(), true, false, |ui| {
        ui.small("Uses an aperture-shaped scene-linear blur for natural bokeh.");
        ui.add_space(3.0);
        changed |= effect_slider(ui, &mut settings.amount, lens_blur::AMOUNT);
        changed |= effect_slider(ui, &mut settings.radius, lens_blur::RADIUS);
        changed |= effect_slider(ui, &mut settings.blades, lens_blur::BLADES);
        changed |= effect_slider(ui, &mut settings.rotation, lens_blur::ROTATION);
        changed |= effect_slider(ui, &mut settings.highlight_boost, lens_blur::HIGHLIGHTS);
    });
    changed
}
