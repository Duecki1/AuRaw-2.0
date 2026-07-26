use eframe::egui::{self, Response, RichText, Ui, Vec2};
use egui_phosphor::regular;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiIcon {
    Adjustments,
    Crop,
    Mask,
    Heal,
    Export,
    RotateLeft,
    RotateRight,
}

impl UiIcon {
    fn glyph(self) -> &'static str {
        match self {
            Self::Adjustments => regular::SLIDERS_HORIZONTAL,
            Self::Crop => regular::CROP,
            Self::Mask => regular::SELECTION,
            Self::Heal => regular::BANDAIDS,
            Self::Export => regular::EXPORT,
            Self::RotateLeft => regular::ARROW_COUNTER_CLOCKWISE,
            Self::RotateRight => regular::ARROW_CLOCKWISE,
        }
    }
}

pub fn icon_toggle_button(
    ui: &mut Ui,
    icon: UiIcon,
    selected: bool,
    size: Vec2,
    tooltip: &str,
) -> Response {
    ui.add_sized(
        size,
        egui::Button::new(RichText::new(icon.glyph()).size(size.y * 0.52))
            .selected(selected)
            .frame(selected),
    )
    .on_hover_text(tooltip)
}

pub fn phosphor_icon_button(ui: &mut Ui, glyph: &str, size: Vec2, tooltip: &str) -> Response {
    ui.add_sized(
        size,
        egui::Button::new(RichText::new(glyph).size(size.y * 0.55)),
    )
    .on_hover_text(tooltip)
}

pub fn phosphor_icon_button_enabled(
    ui: &mut Ui,
    enabled: bool,
    glyph: &str,
    size: Vec2,
    tooltip: &str,
) -> Response {
    ui.add_enabled_ui(enabled, |ui| {
        ui.add_sized(
            size,
            egui::Button::new(RichText::new(glyph).size(size.y * 0.55)),
        )
    })
    .inner
    .on_hover_text(tooltip)
}
