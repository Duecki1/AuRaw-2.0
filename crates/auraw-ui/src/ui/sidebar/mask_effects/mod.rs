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

fn effect_description(effect: MaskEffect) -> Option<&'static str> {
    match effect {
        MaskEffect::LensBlur => {
            Some("Uses an aperture-shaped scene-linear blur for natural bokeh.")
        }
        MaskEffect::LightRays => Some(
            "The mask is the light source. Rays converge on the source point and travel beyond the mask.",
        ),
        MaskEffect::Fog => Some(
            "Fog is generated in full-image coordinates and blended through the editable mask.",
        ),
        MaskEffect::Smoke => Some(
            "Smoke is generated in full-image coordinates and blended through the editable mask.",
        ),
        _ => None,
    }
}

fn effect_toolbar<T: Default>(ui: &mut Ui, effect: MaskEffect, settings: &mut T) -> bool {
    let mut reset = false;
    let help = effect_description(effect);
    crate::ui::theme::toolbar_row(ui, |ui| {
        let title = ui.strong(format!("{} Effect", effect.label()));
        if let Some(help) = help {
            title.on_hover_text(help);
        }
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
