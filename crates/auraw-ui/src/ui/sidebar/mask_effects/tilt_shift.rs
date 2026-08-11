use super::super::{adjustment_slider_with_reset, Sidebar, TiltShiftEffectSettings, Ui};
use eframe::egui;

pub(crate) fn show(
    ui: &mut Ui,
    settings: &mut TiltShiftEffectSettings,
    is_fullscreen_mask: bool,
) -> bool {
    let defaults = TiltShiftEffectSettings::default();
    let mut changed = false;
    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Tilt-Shift Effect");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Tilt-Shift settings",
            )
            .clicked()
            {
                settings.reset();
                changed = true;
            }
        });
    });
    ui.add_space(4.0);
    Sidebar::adjustment_section(ui, "Tilt-Shift", true, false, |ui| {
        if !is_fullscreen_mask {
            ui.colored_label(ui.visuals().warn_fg_color, "Best used with a Fullscreen mask. Other masks clip the built-in focus band and can create an abrupt blur boundary.");
            ui.add_space(3.0);
        }
        changed |= adjustment_slider_with_reset(
            ui,
            "Amount",
            &mut settings.amount,
            0.0..=100.0,
            0,
            0.5,
            Some("Controls the maximum defocus strength outside the focus band."),
            defaults.amount,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Radius",
            &mut settings.radius,
            0.0..=48.0,
            1,
            0.1,
            Some("Controls the defocus radius in reference-image pixels."),
            defaults.radius,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Center X",
            &mut settings.center[0],
            -50.0..=150.0,
            0,
            1.0,
            Some("Horizontal position of a point on the sharp band."),
            defaults.center[0],
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Center Y",
            &mut settings.center[1],
            -50.0..=150.0,
            0,
            1.0,
            Some("Vertical position of a point on the sharp band."),
            defaults.center[1],
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Angle",
            &mut settings.angle,
            -180.0..=180.0,
            0,
            1.0,
            Some("Rotates the in-focus band."),
            defaults.angle,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Focus Width",
            &mut settings.focus_width,
            0.0..=100.0,
            0,
            0.5,
            Some("Width of the sharp band as a percentage of the image's shorter edge."),
            defaults.focus_width,
        );
        changed |= adjustment_slider_with_reset(
            ui,
            "Feather",
            &mut settings.feather,
            0.1..=100.0,
            1,
            0.1,
            Some("Softens the transition from sharp to defocused areas."),
            defaults.feather,
        );
    });
    changed
}
