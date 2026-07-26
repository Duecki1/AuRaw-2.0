//! Shared visual tokens for the editor UI.
//!
//! This is deliberately the only place that sets global egui visuals and text
//! metrics. Screens should use the reusable controls in `ui::icons` instead of
//! compensating for a small or inconsistent default style locally.

use eframe::egui::{self, Context, FontId, Theme};

pub const TOUCH_TARGET: f32 = 40.0;
pub const COMPACT_TARGET: f32 = 32.0;

pub fn install(ctx: &Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    let mut visuals = egui::Visuals::dark();
    let accent = egui::Color32::from_rgb(86, 156, 255);
    visuals.panel_fill = egui::Color32::from_rgb(21, 23, 27);
    visuals.window_fill = egui::Color32::from_rgb(28, 31, 36);
    visuals.faint_bg_color = egui::Color32::from_rgb(40, 44, 51);
    visuals.extreme_bg_color = egui::Color32::from_rgb(13, 15, 18);
    visuals.selection.bg_fill = accent;
    visuals.hyperlink_color = accent;
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(43, 47, 54);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(58, 64, 73);
    visuals.widgets.active.bg_fill = accent;
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style_of(Theme::Dark)).clone();
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(20.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(15.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(14.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(12.5));
    style.spacing.slider_width = 240.0;
    style.spacing.item_spacing = egui::vec2(9.0, 7.0);
    style.spacing.button_padding = egui::vec2(11.0, 7.0);
    style.spacing.interact_size = egui::vec2(COMPACT_TARGET, COMPACT_TARGET);
    style.spacing.indent = 14.0;
    ctx.set_style_of(Theme::Dark, style);
}

pub fn control_size(touch: bool) -> f32 {
    if touch { TOUCH_TARGET } else { COMPACT_TARGET }
}
