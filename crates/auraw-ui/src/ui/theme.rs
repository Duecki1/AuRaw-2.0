use eframe::egui::{
    self, Align, Color32, Frame, InnerResponse, Layout, Margin, Response, RichText, Stroke, Ui,
    Vec2,
};

/// Layout dimensions.
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
pub const PANEL_TITLE_HEIGHT: f32 = 42.0;
pub const PANEL_TITLE_TEXT_SIZE: f32 = 18.0;
pub const FLOATING_ACTION_EDGE: f32 = platform_floating_action_edge(cfg!(target_os = "android"));
pub const FLOATING_ACTION_MARGIN: f32 = 12.0;

const fn platform_control_height(android: bool) -> f32 {
    if android {
        ANDROID_CONTROL_HEIGHT
    } else {
        DESKTOP_CONTROL_HEIGHT
    }
}

const fn platform_floating_action_edge(android: bool) -> f32 {
    if android {
        52.0
    } else {
        46.0
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

/// A borderless panel title with a stable vertical center and a flush divider.
pub fn panel_title(ui: &mut Ui, title: impl Into<RichText>) -> InnerResponse<Response> {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let title = ui
            .allocate_ui_with_layout(
                egui::vec2(ui.available_width().max(1.0), PANEL_TITLE_HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| ui.label(title.into().strong().size(PANEL_TITLE_TEXT_SIZE)),
            )
            .inner;
        ui.separator();
        title
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
        egui::Button::new(label.into())
            .selected(selected)
            .truncate(),
    )
}

pub fn toolbar_button(ui: &mut Ui, label: impl Into<egui::WidgetText>, width: f32) -> Response {
    ui.add_sized([width, CONTROL_HEIGHT], egui::Button::new(label.into()))
}

/// A full-width, left-aligned selectable row for navigation trees and lists.
pub fn navigation_row(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    selected: bool,
    sense: egui::Sense,
) -> Response {
    ui.add_sized(
        [ui.available_width().max(1.0), CONTROL_HEIGHT],
        egui::Button::selectable(selected, ())
            .left_text(label)
            .truncate()
            .sense(sense),
    )
}

/// Lay out related Settings actions with the same control height and wrapping
/// rhythm on both desktop and narrow mobile panels.
pub fn action_row<R>(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().interact_size.y = CONTROL_HEIGHT;
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
        add_contents(ui)
    })
}

/// Place a compact floating action inside the same inset used by AuRaw cards.
pub fn floating_action_rect(bounds: egui::Rect) -> egui::Rect {
    let size = Vec2::splat(FLOATING_ACTION_EDGE);
    let inset = Vec2::splat(FLOATING_ACTION_MARGIN);
    egui::Rect::from_min_size(bounds.right_bottom() - inset - size, size)
}

/// A floating action that reuses the active button colors, stroke, and corner
/// radius instead of introducing a separate circular Material-style control.
pub fn floating_action_button(
    ui: &mut Ui,
    rect: egui::Rect,
    glyph: &str,
    tooltip: &str,
) -> Response {
    let active = &ui.visuals().widgets.active;
    let fill = active.weak_bg_fill;
    let stroke = active.bg_stroke;
    let corner_radius = active.corner_radius;
    let icon_color = active.fg_stroke.color;
    ui.put(
        rect,
        egui::Button::new(
            RichText::new(glyph)
                .size(FLOATING_ACTION_EDGE * 0.42)
                .color(icon_color),
        )
        .min_size(rect.size())
        .corner_radius(corner_radius)
        .fill(fill)
        .stroke(stroke),
    )
    .on_hover_text(tooltip)
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
        platform_control_height, platform_floating_action_edge, ANDROID_CONTROL_HEIGHT,
        CONTROL_HEIGHT, DESKTOP_CONTROL_HEIGHT, FLOATING_ACTION_EDGE, FLOATING_ACTION_MARGIN,
    };

    #[test]
    fn android_widgets_request_the_full_touch_target_height() {
        assert_eq!(platform_control_height(true), ANDROID_CONTROL_HEIGHT);
        assert_eq!(platform_control_height(false), DESKTOP_CONTROL_HEIGHT);
    }

    #[test]
    fn floating_actions_use_platform_size_and_standard_inset() {
        assert!(platform_floating_action_edge(true) > ANDROID_CONTROL_HEIGHT);
        assert!(platform_floating_action_edge(false) > DESKTOP_CONTROL_HEIGHT);

        let bounds = eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(10.0, 20.0),
            eframe::egui::vec2(300.0, 400.0),
        );
        let rect = super::floating_action_rect(bounds);
        assert_eq!(rect.size(), eframe::egui::Vec2::splat(FLOATING_ACTION_EDGE));
        assert_eq!(
            rect.right_bottom(),
            bounds.right_bottom() - eframe::egui::Vec2::splat(FLOATING_ACTION_MARGIN)
        );
    }

    #[test]
    fn segmented_buttons_honor_their_assigned_width() {
        eframe::egui::__run_test_ui(|ui| {
            let width = 42.0;
            let response = super::segmented_button(ui, "Long segment label", false, width);
            assert_eq!(response.rect.width(), width);
            assert_eq!(response.rect.height(), CONTROL_HEIGHT);
        });
    }

    #[test]
    fn panel_title_text_is_vertically_centered() {
        eframe::egui::__run_test_ui(|ui| {
            ui.set_width(320.0);
            let row_top = ui.cursor().top();
            let title = super::panel_title(ui, "Edit").inner;
            let expected_center = row_top + super::PANEL_TITLE_HEIGHT * 0.5;
            assert!((title.rect.center().y - expected_center).abs() < 0.001);
        });
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
