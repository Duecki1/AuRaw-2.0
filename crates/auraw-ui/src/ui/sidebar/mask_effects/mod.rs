pub(super) mod blur;
pub(super) mod edge_glow;
pub(super) mod fog;
pub(super) mod glow;
pub(super) mod lens_blur;
pub(super) mod light_rays;
pub(super) mod motion_blur;
pub(super) mod neon;
pub(super) mod pixelate;
pub(super) mod radial_blur;
pub(super) mod smoke;
pub(super) mod tilt_shift;

use super::{adjustment_slider_with_reset, MaskEffect, Ui};
use crate::pipeline::effect_params::{ColorParamSpec, FloatParamSpec};
use eframe::egui;

fn effect_toolbar<T: Default>(ui: &mut Ui, effect: MaskEffect, settings: &mut T) -> bool {
    let mut reset = false;
    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong(format!("{} Effect", effect.label()));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            reset = crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                &format!("Reset {} settings", effect.label()),
            )
            .clicked();
        });
    });
    ui.add_space(4.0);
    if reset {
        *settings = T::default();
    }
    reset
}

fn effect_slider(ui: &mut Ui, value: &mut f32, spec: FloatParamSpec) -> bool {
    adjustment_slider_with_reset(
        ui,
        spec.label,
        value,
        spec.range(),
        spec.decimals,
        spec.step,
        spec.tooltip,
        spec.default,
    )
}

fn effect_color(
    ui: &mut Ui,
    id_salt: &'static str,
    color: &mut [f32; 3],
    spec: ColorParamSpec,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(spec.label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            changed |= crate::ui::components::effect_color_picker::effect_color_picker(
                ui,
                id_salt,
                color,
                spec.title,
                spec.tooltip,
            );
        });
    });
    changed
}
