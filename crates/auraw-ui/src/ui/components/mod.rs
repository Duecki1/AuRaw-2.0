pub mod adjustment_slider;
pub mod color_grading;
pub mod effect_color_picker;
pub mod hsl_mixer;
pub mod tone_curve_editor;

use eframe::egui::{Align2, Color32, FontId, Painter, Pos2};

pub fn pending_indicator(painter: &Painter, center: Pos2, radius: f32, font_size: f32) {
    painter.circle_filled(center, radius, Color32::from_black_alpha(190));
    painter.text(
        center,
        Align2::CENTER_CENTER,
        egui_phosphor::regular::ARROW_CLOCKWISE,
        FontId::proportional(font_size),
        crate::ui::theme::STATUS_WARNING,
    );
}
