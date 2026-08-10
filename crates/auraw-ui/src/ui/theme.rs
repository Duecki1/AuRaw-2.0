use eframe::egui::{
    self, Align, Color32, Frame, InnerResponse, Layout, Margin, Response, RichText, Stroke, Ui,
    Vec2,
};

/// Shared dimensions for the editor chrome.
///
/// Keeping these values in one place prevents toolbars and section actions from
/// slowly acquiring slightly different heights as individual screens evolve.
/// Android's normal interaction height must also be the requested widget
/// height. Requesting 30 points while the style enforces a 40-point touch
/// target makes egui paint outside the allocated rectangle, which misaligns
/// neighboring labels and makes width calculations inaccurate.
const DESKTOP_CONTROL_HEIGHT: f32 = 30.0;
const ANDROID_CONTROL_HEIGHT: f32 = 40.0;
pub const CONTROL_HEIGHT: f32 = platform_control_height(cfg!(target_os = "android"));
pub const TOOLBAR_HEIGHT: f32 = if cfg!(target_os = "android") {
    ANDROID_CONTROL_HEIGHT
} else {
    32.0
};
pub const TOOLBAR_ICON_EDGE: f32 = CONTROL_HEIGHT;
pub const TOOL_RAIL_ICON_EDGE: f32 = 40.0;
pub const CARD_GAP: f32 = 10.0;

const fn platform_control_height(android: bool) -> f32 {
    if android {
        ANDROID_CONTROL_HEIGHT
    } else {
        DESKTOP_CONTROL_HEIGHT
    }
}

pub fn toolbar_icon_size() -> Vec2 {
    Vec2::splat(TOOLBAR_ICON_EDGE)
}

pub fn tool_rail_icon_size() -> Vec2 {
    Vec2::splat(TOOL_RAIL_ICON_EDGE)
}

/// Apply the common rhythm used by top bars and panel headers.
pub fn prepare_toolbar(ui: &mut Ui) {
    ui.set_min_height(TOOLBAR_HEIGHT);
    // Keep the local style and explicit helper sizes in lockstep even when a
    // caller installs a temporary style before constructing a toolbar.
    ui.spacing_mut().interact_size.y = CONTROL_HEIGHT;
    ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);
}

/// Allocate a full-width row with a stable cross-axis center before adding any
/// of its children. This prevents a compact label added first from establishing
/// a different visual center than a trailing icon button.
pub fn toolbar_row<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    let size = egui::vec2(ui.available_width().max(1.0), TOOLBAR_HEIGHT);
    ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
        prepare_toolbar(ui);
        add_contents(ui)
    })
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

/// Show a full-width card using the same surface treatment as Settings and
/// Export. Editor tools should use this instead of open-coded `Frame`s so the
/// padding, border, and background remain consistent as the theme evolves.
pub fn content_card<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    card_frame(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add_contents(ui)
    })
}

/// A content card with the standard compact section title.
pub fn section_card<R>(
    ui: &mut Ui,
    title: impl Into<RichText>,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    content_card(ui, |ui| {
        ui.strong(title);
        add_contents(ui)
    })
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
                .truncate()
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
    style.spacing.interact_size.y = CONTROL_HEIGHT;
    style.spacing.window_margin = Margin::same(if cfg!(target_os = "android") { 12 } else { 10 });
    style.spacing.menu_margin = Margin::same(8);
    style.spacing.indent = 14.0;
    ctx.set_style_of(egui::Theme::Dark, style);
}

#[cfg(test)]
mod tests {
    use super::{
        platform_control_height, ANDROID_CONTROL_HEIGHT, CONTROL_HEIGHT, DESKTOP_CONTROL_HEIGHT,
        TOOLBAR_HEIGHT, TOOLBAR_ICON_EDGE,
    };

    #[test]
    fn android_widgets_request_the_full_touch_target_height() {
        assert_eq!(platform_control_height(true), ANDROID_CONTROL_HEIGHT);
        assert_eq!(platform_control_height(false), DESKTOP_CONTROL_HEIGHT);
    }

    #[test]
    fn toolbar_geometry_never_understates_its_controls() {
        assert_eq!(TOOLBAR_ICON_EDGE, CONTROL_HEIGHT);
        assert!(TOOLBAR_HEIGHT >= CONTROL_HEIGHT);
    }

    #[test]
    fn toolbar_row_centers_labels_and_actions_on_the_same_axis() {
        eframe::egui::__run_test_ui(|ui| {
            ui.set_width(320.0);
            let (label, action) = super::toolbar_row(ui, |ui| {
                let label = ui.strong("Section");
                let action = ui
                    .with_layout(
                        eframe::egui::Layout::right_to_left(eframe::egui::Align::Center),
                        |ui| {
                            ui.add_sized(super::toolbar_icon_size(), eframe::egui::Button::new("R"))
                        },
                    )
                    .inner;
                (label.rect, action.rect)
            })
            .inner;

            assert!((label.center().y - action.center().y).abs() < 0.001);
            assert_eq!(action.size(), super::toolbar_icon_size());
        });
    }
}
