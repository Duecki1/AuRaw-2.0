use eframe::egui::{self, Response, RichText, Ui, Vec2};
use egui_phosphor::regular;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UiIcon {
    #[cfg(not(target_os = "android"))]
    Adjustments,
    #[cfg(not(target_os = "android"))]
    Crop,
    #[cfg(not(target_os = "android"))]
    Mask,
    #[cfg(not(target_os = "android"))]
    Heal,
    #[cfg(not(target_os = "android"))]
    Export,
    #[cfg(not(target_os = "android"))]
    Info,
    #[cfg(not(target_os = "android"))]
    Sidebar,
    #[cfg(not(target_os = "android"))]
    Filmstrip,
    RotateLeft,
    RotateRight,
}

impl UiIcon {
    fn glyph(self) -> &'static str {
        match self {
            #[cfg(not(target_os = "android"))]
            Self::Adjustments => regular::SLIDERS_HORIZONTAL,
            #[cfg(not(target_os = "android"))]
            Self::Crop => regular::CROP,
            #[cfg(not(target_os = "android"))]
            Self::Mask => regular::SELECTION,
            #[cfg(not(target_os = "android"))]
            Self::Heal => regular::BANDAIDS,
            #[cfg(not(target_os = "android"))]
            Self::Export => regular::EXPORT,
            #[cfg(not(target_os = "android"))]
            Self::Info => regular::INFO,
            #[cfg(not(target_os = "android"))]
            Self::Sidebar => regular::SIDEBAR_SIMPLE,
            #[cfg(not(target_os = "android"))]
            Self::Filmstrip => regular::IMAGE,
            Self::RotateLeft => regular::ARROW_COUNTER_CLOCKWISE,
            Self::RotateRight => regular::ARROW_CLOCKWISE,
        }
    }
}

#[cfg(not(target_os = "android"))]
pub(crate) fn icon_toggle_button(
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

pub(crate) fn icon_button(ui: &mut Ui, icon: UiIcon, size: Vec2, tooltip: &str) -> Response {
    ui.add_sized(
        size,
        egui::Button::new(RichText::new(icon.glyph()).size(size.y * 0.52)).frame(true),
    )
    .on_hover_text(tooltip)
}

pub(crate) fn phosphor_icon_button(
    ui: &mut Ui,
    glyph: &str,
    size: Vec2,
    tooltip: &str,
) -> Response {
    ui.add_sized(
        size,
        egui::Button::new(RichText::new(glyph).size(size.y * 0.55)).frame(true),
    )
    .on_hover_text(tooltip)
}

pub(crate) fn phosphor_icon_toggle_button(
    ui: &mut Ui,
    glyph: &str,
    selected: bool,
    size: Vec2,
    tooltip: &str,
) -> Response {
    ui.add_sized(
        size,
        egui::Button::new(RichText::new(glyph).size(size.y * 0.55)).selected(selected),
    )
    .on_hover_text(tooltip)
}

#[cfg(not(target_os = "android"))]
pub(crate) fn phosphor_icon_toggle_button_enabled(
    ui: &mut Ui,
    enabled: bool,
    glyph: &str,
    selected: bool,
    size: Vec2,
    tooltip: &str,
) -> Response {
    ui.add_enabled_ui(enabled, |ui| {
        ui.add_sized(
            size,
            egui::Button::new(RichText::new(glyph).size(size.y * 0.55)).selected(selected),
        )
    })
    .inner
    .on_hover_text(tooltip)
}

pub(crate) fn phosphor_icon_button_enabled(
    ui: &mut Ui,
    enabled: bool,
    glyph: &str,
    size: Vec2,
    tooltip: &str,
) -> Response {
    ui.add_enabled_ui(enabled, |ui| {
        ui.add_sized(
            size,
            egui::Button::new(RichText::new(glyph).size(size.y * 0.55)).frame(true),
        )
    })
    .inner
    .on_hover_text(tooltip)
}
