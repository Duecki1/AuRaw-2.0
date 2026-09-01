use super::{effect_color, effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::light_rays, LightRaysEffectSettings, MaskEffect};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut LightRaysEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::LightRays, settings);
    super::super::Sidebar::adjustment_section(
        ui,
        MaskEffect::LightRays.label(),
        true,
        false,
        |ui| {
            changed |= effect_slider(ui, &mut settings.amount, light_rays::AMOUNT);
            changed |= effect_slider(ui, &mut settings.length, light_rays::LENGTH);
            changed |= effect_slider(ui, &mut settings.source[0], light_rays::SOURCE_X);
            changed |= effect_slider(ui, &mut settings.source[1], light_rays::SOURCE_Y);
            changed |= effect_slider(ui, &mut settings.spread, light_rays::SPREAD);
            changed |= effect_slider(ui, &mut settings.fade, light_rays::FADE);
            changed |= effect_slider(ui, &mut settings.ray_count, light_rays::RAY_COUNT);
            changed |= effect_slider(ui, &mut settings.variation, light_rays::VARIATION);
            changed |= effect_slider(ui, &mut settings.softness, light_rays::SOFTNESS);
            changed |= effect_color(
                ui,
                "light-rays-color-picker",
                &mut settings.color,
                light_rays::COLOR,
            );
        },
    );
    changed
}
