use eframe::egui::{self, Color32, Frame, Margin, Response, RichText, Stroke, Ui, Vec2};

/// Shared dimensions for the desktop editor chrome.
///
/// Keeping these values in one place prevents toolbars and section actions from
/// slowly acquiring slightly different heights as individual screens evolve.
pub const CONTROL_HEIGHT: f32 = 30.0;
pub const TOOLBAR_HEIGHT: f32 = 32.0;
pub const TOOLBAR_ICON_EDGE: f32 = 30.0;
pub const TOOL_RAIL_ICON_EDGE: f32 = 40.0;
pub const CARD_GAP: f32 = 10.0;

pub fn toolbar_icon_size() -> Vec2 {
    Vec2::splat(TOOLBAR_ICON_EDGE)
}

pub fn tool_rail_icon_size() -> Vec2 {
    Vec2::splat(TOOL_RAIL_ICON_EDGE)
}

/// Apply the common rhythm used by top bars and panel headers.
pub fn prepare_toolbar(ui: &mut Ui) {
    ui.set_min_height(TOOLBAR_HEIGHT);
    ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
}

pub fn toolbar_frame(ui: &Ui) -> Frame {
    Frame::new()
        .fill(ui.visuals().panel_fill)
        .inner_margin(Margin::symmetric(10, 5))
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
}

/// A quiet, bordered surface for related settings and export controls.
pub fn card_frame(ui: &Ui) -> Frame {
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(Margin::same(12))
        .corner_radius(6.0)
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
}

pub fn tab_button(ui: &mut Ui, label: &str, selected: bool, width: f32) -> Response {
    segmented_button(ui, RichText::new(label).strong(), selected, width)
}

pub fn segmented_button(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    selected: bool,
    width: f32,
) -> Response {
    ui.add_sized(
        [width, CONTROL_HEIGHT],
        egui::Button::new(label.into()).selected(selected),
    )
}

pub fn toolbar_button(ui: &mut Ui, label: impl Into<egui::WidgetText>, width: f32) -> Response {
    ui.add_sized([width, CONTROL_HEIGHT], egui::Button::new(label.into()))
}

/// A consistent form row: descriptive label on the left, fixed-width control
/// on the right. This avoids egui's default trailing ComboBox labels, which are
/// difficult to scan when several settings are stacked vertically.
pub fn form_combo(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    id_salt: impl egui::AsIdSalt,
    selected_text: impl Into<egui::WidgetText>,
    preferred_width: f32,
    add_contents: impl FnOnce(&mut Ui),
) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let width = preferred_width.min(ui.available_width().max(1.0));
            egui::ComboBox::from_id_salt(id_salt)
                .selected_text(selected_text)
                .width(width)
                .show_ui(ui, add_contents);
        });
    });
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    let mut visuals = egui::Visuals::dark();
    let accent = Color32::from_rgb(62, 142, 247);
    let border = Color32::from_rgb(58, 62, 69);

    visuals.panel_fill = Color32::from_rgb(24, 26, 29);
    visuals.window_fill = Color32::from_rgb(29, 31, 35);
    visuals.faint_bg_color = Color32::from_rgb(31, 34, 38);
    visuals.extreme_bg_color = Color32::from_rgb(16, 18, 21);
    visuals.selection.bg_fill = accent;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    visuals.hyperlink_color = Color32::from_rgb(99, 170, 255);
    visuals.window_stroke = Stroke::new(1.0, border);
    visuals.window_corner_radius = 7.0.into();
    visuals.menu_corner_radius = 6.0.into();

    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    visuals.widgets.noninteractive.corner_radius = 5.0.into();
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(39, 42, 47);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(39, 42, 47);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(63, 68, 76));
    visuals.widgets.inactive.corner_radius = 5.0.into();
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(51, 55, 62);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(51, 55, 62);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(86, 94, 105));
    visuals.widgets.hovered.corner_radius = 5.0.into();
    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.weak_bg_fill = accent;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(115, 181, 255));
    visuals.widgets.active.corner_radius = 5.0.into();
    visuals.widgets.open.bg_fill = Color32::from_rgb(48, 52, 59);
    visuals.widgets.open.weak_bg_fill = Color32::from_rgb(48, 52, 59);
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, Color32::from_rgb(82, 89, 100));
    visuals.widgets.open.corner_radius = 5.0.into();
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(19.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(13.5));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
    style.spacing.slider_width = 220.0;
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    style.spacing.interact_size.y = if cfg!(target_os = "android") {
        40.0
    } else {
        CONTROL_HEIGHT
    };
    style.spacing.window_margin = Margin::same(if cfg!(target_os = "android") { 12 } else { 10 });
    style.spacing.menu_margin = Margin::same(8);
    style.spacing.indent = 14.0;
    ctx.set_style_of(egui::Theme::Dark, style);
}
