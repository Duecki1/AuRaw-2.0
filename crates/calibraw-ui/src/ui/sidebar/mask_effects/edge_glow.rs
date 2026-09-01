use super::{effect_color, effect_slider, effect_toolbar};
use crate::pipeline::{effect_params::edge_glow, EdgeGlowEffectSettings, MaskEffect};
use eframe::egui::Ui;

pub(crate) fn show(ui: &mut Ui, settings: &mut EdgeGlowEffectSettings) -> bool {
    let mut changed = effect_toolbar(ui, MaskEffect::EdgeGlow, settings);
    super::super::Sidebar::adjustment_section(
        ui,
        MaskEffect::EdgeGlow.label(),
        true,
        false,
        |ui| {
            changed |= effect_slider(ui, &mut settings.amount, edge_glow::AMOUNT);
            changed |= effect_slider(ui, &mut settings.edge_width, edge_glow::EDGE_WIDTH);
            changed |= effect_slider(ui, &mut settings.detail, edge_glow::DETAIL);
            changed |= effect_slider(ui, &mut settings.glow, edge_glow::GLOW);
            changed |= effect_color(
                ui,
                "edge-glow-color-picker",
                &mut settings.color,
                edge_glow::COLOR,
            );
        },
    );
    changed
}
