use eframe::egui::{
    self, Align, Color32, Frame, InnerResponse, Layout, Margin, Response, RichText, Stroke, Ui,
    Vec2,
};
use serde::{Deserialize, Serialize};

const DESKTOP_CONTROL_HEIGHT: f32 = 32.0;
const ANDROID_CONTROL_HEIGHT: f32 = 40.0;
pub(crate) const CONTROL_HEIGHT: f32 = platform_control_height(cfg!(target_os = "android"));
pub(crate) const TOOLBAR_HEIGHT: f32 = if cfg!(target_os = "android") {
    ANDROID_CONTROL_HEIGHT
} else {
    DESKTOP_CONTROL_HEIGHT
};
pub(crate) const TOOLBAR_ICON_EDGE: f32 = CONTROL_HEIGHT;
#[cfg(not(target_os = "android"))]
pub(crate) const TOOL_RAIL_ICON_EDGE: f32 = 40.0;
pub(crate) const SPACE_XS: f32 = 4.0;
pub(crate) const SPACE_SM: f32 = 8.0;
pub(crate) const SPACE_MD: f32 = 12.0;
pub(crate) const SPACE_LG: f32 = 16.0;
pub(crate) const CARD_GAP: f32 = SPACE_SM;
pub(crate) const CONTENT_MARGIN: i8 = 12;
pub(crate) const CARD_RADIUS: f32 = 8.0;
const COMPACT_PORTRAIT_CARD_GAP: f32 = SPACE_SM;
const COMPACT_PORTRAIT_CONTENT_MARGIN: i8 = 8;
pub(crate) const FORM_STACK_BREAKPOINT: f32 = 520.0;
pub(crate) const HELP_BUTTON_EDGE: f32 = if cfg!(target_os = "android") {
    CONTROL_HEIGHT
} else {
    28.0
};
#[cfg(test)]
pub(crate) const PANEL_TITLE_HEIGHT: f32 = 40.0;
pub(crate) const PANEL_TITLE_TEXT_SIZE: f32 = 16.0;
#[cfg(any(target_os = "android", test))]
pub(crate) const FLOATING_ACTION_EDGE: f32 =
    platform_floating_action_edge(cfg!(target_os = "android"));
#[cfg(any(target_os = "android", test))]
pub(crate) const FLOATING_ACTION_MARGIN: f32 = 12.0;

pub(crate) const CANVAS_BACKDROP: Color32 = Color32::from_rgb(13, 15, 18);
pub(crate) const STATUS_WARNING: Color32 = Color32::from_rgb(244, 142, 48);
pub(crate) const MASK_ADD: Color32 = Color32::from_rgb(78, 163, 255);
pub(crate) const MASK_SUBTRACT: Color32 = Color32::from_rgb(255, 105, 105);
pub(crate) const DROP_TARGET: Color32 = Color32::from_rgb(225, 62, 62);
pub(crate) fn inpaint_stroke_highlight() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 96, 78, 62)
}

pub(crate) fn inpaint_stroke_active() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 120, 84, 84)
}
pub(crate) const CHANNEL_RED: Color32 = Color32::from_rgb(238, 84, 84);
pub(crate) const CHANNEL_GREEN: Color32 = Color32::from_rgb(92, 210, 116);
pub(crate) const CHANNEL_BLUE: Color32 = Color32::from_rgb(88, 150, 245);

pub(crate) const MASK_COMPONENT_COLORS: [Color32; 8] = [
    MASK_ADD,
    Color32::from_rgb(255, 116, 102),
    Color32::from_rgb(83, 211, 146),
    Color32::from_rgb(242, 192, 75),
    Color32::from_rgb(183, 124, 255),
    Color32::from_rgb(63, 207, 220),
    Color32::from_rgb(255, 133, 196),
    Color32::from_rgb(180, 205, 88),
];

pub(crate) const HSL_CHANNELS: [(&str, Color32); 8] = [
    ("Red", Color32::from_rgb(232, 76, 82)),
    ("Orange", Color32::from_rgb(238, 137, 48)),
    ("Yellow", Color32::from_rgb(224, 193, 57)),
    ("Green", Color32::from_rgb(75, 184, 101)),
    ("Aqua", Color32::from_rgb(52, 184, 184)),
    ("Blue", Color32::from_rgb(73, 130, 232)),
    ("Purple", Color32::from_rgb(153, 94, 218)),
    ("Magenta", Color32::from_rgb(219, 79, 163)),
];

pub(crate) const BRIGHTNESS_SHADOW: Color32 = Color32::from_gray(18);
pub(crate) const BRIGHTNESS_MID: Color32 = Color32::from_gray(118);
pub(crate) const BRIGHTNESS_HIGHLIGHT: Color32 = Color32::from_gray(245);
pub(crate) const TEMPERATURE_COOL: Color32 = Color32::from_rgb(72, 128, 235);
pub(crate) const TEMPERATURE_NEUTRAL: Color32 = Color32::from_gray(208);
pub(crate) const TEMPERATURE_WARM: Color32 = Color32::from_rgb(244, 157, 62);
pub(crate) const TINT_GREEN: Color32 = Color32::from_rgb(76, 181, 112);
pub(crate) const TINT_NEUTRAL: Color32 = Color32::from_gray(202);
pub(crate) const TINT_MAGENTA: Color32 = Color32::from_rgb(222, 84, 174);
pub(crate) const COLORFULNESS_SHADOW: Color32 = Color32::from_gray(92);
pub(crate) const COLORFULNESS_MID: Color32 = Color32::from_gray(178);
pub(crate) const LUMINANCE_BLACK: Color32 = Color32::from_rgb(10, 10, 10);
pub(crate) const LUMINANCE_WHITE: Color32 = Color32::from_rgb(246, 246, 246);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UiDesign {
    #[default]
    #[serde(rename = "midnight_pink")]
    ObsidianBlue,
    #[serde(rename = "graphite_mint")]
    ObsidianRed,
    Porcelain,
    DaylightBlue,
}

impl UiDesign {
    pub(crate) const ALL: [Self; 4] = [
        Self::ObsidianBlue,
        Self::ObsidianRed,
        Self::Porcelain,
        Self::DaylightBlue,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::ObsidianBlue => "Obsidian Blue · Dark",
            Self::ObsidianRed => "Obsidian Red · Dark",
            Self::Porcelain => "Porcelain · Light",
            Self::DaylightBlue => "Daylight · Light",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::ObsidianBlue => "Deep neutral surfaces with a restrained blue accent.",
            Self::ObsidianRed => "Warm near-black surfaces with a restrained ruby accent.",
            Self::Porcelain => "Warm paper-like surfaces with a restrained coral accent.",
            Self::DaylightBlue => "Clean cool surfaces with a focused blue accent.",
        }
    }

    pub(crate) const fn is_dark(self) -> bool {
        matches!(self, Self::ObsidianBlue | Self::ObsidianRed)
    }

    const fn palette(self) -> ThemePalette {
        match self {
            Self::ObsidianBlue => ThemePalette {
                accent: Color32::from_rgb(79, 132, 185),
                accent_bright: Color32::from_rgb(126, 170, 213),
                hyperlink: Color32::from_rgb(111, 162, 210),
                border: Color32::from_rgb(48, 52, 60),
                panel: Color32::from_rgb(24, 26, 31),
                window: Color32::from_rgb(18, 20, 24),
                faint: Color32::from_rgb(31, 34, 40),
                extreme: Color32::from_rgb(12, 14, 17),
                inactive: Color32::from_rgb(34, 37, 43),
                inactive_stroke: Color32::from_rgb(58, 63, 72),
                hovered: Color32::from_rgb(43, 47, 55),
                hovered_stroke: Color32::from_rgb(79, 86, 98),
                open: Color32::from_rgb(39, 43, 50),
                open_stroke: Color32::from_rgb(69, 76, 88),
            },
            Self::ObsidianRed => ThemePalette {
                accent: Color32::from_rgb(166, 70, 86),
                accent_bright: Color32::from_rgb(211, 111, 125),
                hyperlink: Color32::from_rgb(222, 125, 137),
                border: Color32::from_rgb(55, 45, 49),
                panel: Color32::from_rgb(25, 22, 24),
                window: Color32::from_rgb(18, 16, 18),
                faint: Color32::from_rgb(34, 29, 31),
                extreme: Color32::from_rgb(11, 10, 11),
                inactive: Color32::from_rgb(39, 33, 35),
                inactive_stroke: Color32::from_rgb(67, 52, 56),
                hovered: Color32::from_rgb(49, 40, 43),
                hovered_stroke: Color32::from_rgb(99, 69, 76),
                open: Color32::from_rgb(44, 36, 39),
                open_stroke: Color32::from_rgb(88, 61, 67),
            },
            Self::Porcelain => ThemePalette {
                accent: Color32::from_rgb(232, 132, 169),
                accent_bright: Color32::from_rgb(242, 166, 194),
                hyperlink: Color32::from_rgb(173, 36, 92),
                border: Color32::from_rgb(205, 195, 199),
                panel: Color32::from_rgb(247, 243, 244),
                window: Color32::from_rgb(255, 251, 252),
                faint: Color32::from_rgb(241, 235, 237),
                extreme: Color32::from_rgb(225, 216, 219),
                inactive: Color32::from_rgb(237, 229, 232),
                inactive_stroke: Color32::from_rgb(196, 183, 188),
                hovered: Color32::from_rgb(230, 218, 223),
                hovered_stroke: Color32::from_rgb(176, 157, 165),
                open: Color32::from_rgb(226, 213, 218),
                open_stroke: Color32::from_rgb(168, 149, 157),
            },
            Self::DaylightBlue => ThemePalette {
                accent: Color32::from_rgb(116, 170, 242),
                accent_bright: Color32::from_rgb(151, 195, 250),
                hyperlink: Color32::from_rgb(28, 91, 193),
                border: Color32::from_rgb(190, 199, 211),
                panel: Color32::from_rgb(243, 247, 252),
                window: Color32::from_rgb(251, 253, 255),
                faint: Color32::from_rgb(235, 241, 248),
                extreme: Color32::from_rgb(216, 225, 236),
                inactive: Color32::from_rgb(229, 236, 245),
                inactive_stroke: Color32::from_rgb(178, 190, 205),
                hovered: Color32::from_rgb(217, 227, 239),
                hovered_stroke: Color32::from_rgb(151, 170, 193),
                open: Color32::from_rgb(211, 223, 237),
                open_stroke: Color32::from_rgb(143, 163, 187),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreviewBackdrop {
    Black,
    #[default]
    DarkGrey,
    LightGrey,
    White,
    MatchPhoto,
}

impl PreviewBackdrop {
    pub(crate) const ALL: [Self; 5] = [
        Self::Black,
        Self::DarkGrey,
        Self::MatchPhoto,
        Self::LightGrey,
        Self::White,
    ];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Black => "Black",
            Self::DarkGrey => "Dark grey",
            Self::LightGrey => "Light grey",
            Self::White => "White",
            Self::MatchPhoto => "Match photo",
        }
    }

    pub(crate) const fn color(self, adaptive: Color32) -> Color32 {
        match self {
            Self::Black => Color32::BLACK,
            Self::DarkGrey => CANVAS_BACKDROP,
            Self::LightGrey => Color32::from_gray(168),
            Self::White => Color32::WHITE,
            Self::MatchPhoto => adaptive,
        }
    }
}

pub(crate) fn adaptive_backdrop_from_rgba(rgba: &[u8]) -> Color32 {
    let pixel_count = rgba.len() / 4;
    if pixel_count == 0 {
        return CANVAS_BACKDROP;
    }

    let sample_stride = (pixel_count / 4096).max(1);
    let mut sums = [0_u64; 3];
    let mut weight = 0_u64;
    for pixel in rgba.chunks_exact(4).step_by(sample_stride) {
        let alpha = u64::from(pixel[3]);
        if alpha == 0 {
            continue;
        }
        for channel in 0..3 {
            sums[channel] += u64::from(pixel[channel]) * alpha;
        }
        weight += alpha;
    }
    if weight == 0 {
        return CANVAS_BACKDROP;
    }

    let average = sums.map(|sum| sum as f32 / weight as f32);
    let luma = average[0] * 0.2126 + average[1] * 0.7152 + average[2] * 0.0722;
    let offsets = average.map(|channel| channel - luma);
    let largest_offset = offsets
        .iter()
        .map(|offset| offset.abs())
        .fold(0.0_f32, f32::max);
    let chroma_scale = if largest_offset > 0.0 {
        (14.0 / largest_offset).min(0.18)
    } else {
        0.0
    };
    let muted = offsets.map(|offset| (32.0 + offset * chroma_scale).clamp(18.0, 50.0) as u8);
    Color32::from_rgb(muted[0], muted[1], muted[2])
}

pub(crate) fn text_on_backdrop(color: Color32) -> Color32 {
    let luminance = f32::from(color.r()) * 0.2126
        + f32::from(color.g()) * 0.7152
        + f32::from(color.b()) * 0.0722;
    if luminance >= 145.0 {
        Color32::from_rgb(24, 25, 28)
    } else {
        Color32::from_rgb(242, 243, 246)
    }
}

#[derive(Clone, Copy)]
struct ThemePalette {
    accent: Color32,
    accent_bright: Color32,
    hyperlink: Color32,
    border: Color32,
    panel: Color32,
    window: Color32,
    faint: Color32,
    extreme: Color32,
    inactive: Color32,
    inactive_stroke: Color32,
    hovered: Color32,
    hovered_stroke: Color32,
    open: Color32,
    open_stroke: Color32,
}

const fn platform_control_height(android: bool) -> f32 {
    if android {
        ANDROID_CONTROL_HEIGHT
    } else {
        DESKTOP_CONTROL_HEIGHT
    }
}

#[cfg(any(target_os = "android", test))]
const fn platform_floating_action_edge(android: bool) -> f32 {
    if android {
        52.0
    } else {
        46.0
    }
}

pub(crate) fn toolbar_icon_size() -> Vec2 {
    Vec2::splat(TOOLBAR_ICON_EDGE)
}

pub(crate) fn is_compact_portrait(ui: &Ui) -> bool {
    compact_portrait_for_platform(ui.ctx().content_rect().size(), cfg!(target_os = "android"))
}

fn compact_portrait_for_platform(viewport: Vec2, android: bool) -> bool {
    android && viewport.x < viewport.y
}

fn content_margin(ui: &Ui) -> i8 {
    if is_compact_portrait(ui) {
        COMPACT_PORTRAIT_CONTENT_MARGIN
    } else {
        CONTENT_MARGIN
    }
}

#[cfg(not(target_os = "android"))]
pub(crate) fn tool_rail_icon_size() -> Vec2 {
    Vec2::splat(TOOL_RAIL_ICON_EDGE)
}

pub(crate) fn prepare_toolbar(ui: &mut Ui) {
    ui.set_min_height(TOOLBAR_HEIGHT);
    ui.spacing_mut().interact_size.y = CONTROL_HEIGHT;
    ui.spacing_mut().item_spacing = egui::vec2(SPACE_SM, SPACE_XS);
}

pub(crate) fn toolbar_row<R>(
    ui: &mut Ui,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    let size = egui::vec2(ui.available_width().max(1.0), TOOLBAR_HEIGHT);
    ui.allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| {
        prepare_toolbar(ui);
        add_contents(ui)
    })
}

pub(crate) fn toolbar_title(ui: &mut Ui, title: impl Into<RichText>) -> Response {
    ui.label(title.into().strong().size(PANEL_TITLE_TEXT_SIZE))
}

#[cfg(test)]
pub(crate) fn panel_title(ui: &mut Ui, title: impl Into<RichText>) -> InnerResponse<Response> {
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

pub(crate) fn toolbar_frame(ui: &Ui) -> Frame {
    let compact = is_compact_portrait(ui);
    Frame::new()
        .fill(ui.visuals().panel_fill)
        .inner_margin(Margin::symmetric(
            if compact { 10 } else { CONTENT_MARGIN },
            if compact { 4 } else { 6 },
        ))
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .corner_radius(0.0)
}

pub(crate) fn panel_frame(ui: &Ui) -> Frame {
    Frame::new()
        .fill(ui.visuals().panel_fill)
        .inner_margin(Margin::same(if is_compact_portrait(ui) { 6 } else { 10 }))
        .stroke(Stroke::NONE)
}

pub(crate) fn workspace_frame(ui: &Ui) -> Frame {
    Frame::new()
        .fill(ui.visuals().window_fill)
        .inner_margin(Margin::same(content_margin(ui)))
        .stroke(Stroke::NONE)
}

pub(crate) fn card_frame(ui: &Ui) -> Frame {
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(Margin::same(content_margin(ui) + 2))
        .corner_radius(CARD_RADIUS)
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
}

pub(crate) fn content_card<R>(
    ui: &mut Ui,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    let frame_width = f32::from(content_margin(ui)) * 2.0 + 6.0;
    let inner_width = (ui.available_width() - frame_width).max(1.0);
    card_frame(ui).show(ui, |ui| {
        ui.set_width(inner_width);
        ui.set_max_width(inner_width);
        add_contents(ui)
    })
}

pub(crate) fn card_header<R>(
    ui: &mut Ui,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    let horizontal_margin = content_margin(ui) + 2;
    let frame_width = f32::from(content_margin(ui)) * 2.0 + 6.0;
    let inner_width = (ui.available_width() - frame_width).max(1.0);
    Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(Margin::symmetric(horizontal_margin, 10))
        .corner_radius(CARD_RADIUS)
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .show(ui, |ui| {
            ui.set_width(inner_width);
            ui.set_max_width(inner_width);
            add_contents(ui)
        })
}

pub(crate) fn section_card<R>(
    ui: &mut Ui,
    title: impl Into<RichText>,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    content_card(ui, |ui| {
        ui.strong(title);
        add_contents(ui)
    })
}

pub(crate) fn section_card_with_help<R>(
    ui: &mut Ui,
    title: impl Into<RichText>,
    help: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    content_card(ui, |ui| {
        strong_with_help(ui, title, help);
        add_contents(ui)
    })
}

pub(crate) fn heading_with_help(ui: &mut Ui, title: impl Into<RichText>, help: &str) {
    ui.heading(title).on_hover_text(help);
}

pub(crate) fn strong_with_help(ui: &mut Ui, title: impl Into<RichText>, help: &str) {
    ui.label(title.into().strong()).on_hover_text(help);
}

pub(crate) fn checkbox_with_help(
    ui: &mut Ui,
    checked: &mut bool,
    label: impl Into<egui::WidgetText>,
    help: &str,
) -> Response {
    let width = ui.available_width().max(1.0);
    ui.allocate_ui_with_layout(
        egui::vec2(width, CONTROL_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| ui.checkbox(checked, label).on_hover_text(help),
    )
    .inner
}

#[cfg(not(target_os = "android"))]
pub(crate) fn tab_button(ui: &mut Ui, label: &str, selected: bool, width: f32) -> Response {
    segmented_button(ui, RichText::new(label).strong(), selected, width)
}

pub(crate) fn segmented_button(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    selected: bool,
    width: f32,
) -> Response {
    ui.add_sized(
        [width, CONTROL_HEIGHT],
        egui::Button::new(label.into())
            .selected(selected)
            .frame(true)
            .truncate()
            .corner_radius(CARD_RADIUS),
    )
}

pub(crate) fn toolbar_button(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    width: f32,
) -> Response {
    ui.add_sized([width, CONTROL_HEIGHT], egui::Button::new(label.into()))
}

pub(crate) fn primary_button(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    width: f32,
) -> Response {
    let visuals = &ui.visuals().widgets.active;
    ui.add_sized(
        [width, CONTROL_HEIGHT],
        egui::Button::new(label.into().color(Color32::WHITE))
            .fill(visuals.weak_bg_fill)
            .stroke(visuals.bg_stroke)
            .corner_radius(CARD_RADIUS),
    )
}

pub(crate) fn toggle_button(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    selected: bool,
) -> Response {
    ui.add(
        egui::Button::new(label.into())
            .selected(selected)
            .frame(true)
            .corner_radius(CARD_RADIUS),
    )
}

pub(crate) fn navigation_row(
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

pub(crate) fn action_row<R>(
    ui: &mut Ui,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> InnerResponse<R> {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().interact_size.y = CONTROL_HEIGHT;
        ui.spacing_mut().item_spacing = egui::vec2(SPACE_SM, SPACE_SM);
        add_contents(ui)
    })
}

pub(crate) fn card_gap(ui: &mut Ui) {
    let gap = if is_compact_portrait(ui) {
        COMPACT_PORTRAIT_CARD_GAP
    } else {
        CARD_GAP
    };
    let explicit_space = (gap - ui.spacing().item_spacing.y).max(0.0);
    ui.add_space(explicit_space);
}

pub(crate) fn singleline_text_edit<'a>(text: &'a mut dyn egui::TextBuffer) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text)
        .vertical_align(Align::Center)
        .margin(Margin::symmetric(8, 4))
        .min_size(egui::vec2(0.0, CONTROL_HEIGHT))
}

#[cfg(any(target_os = "android", test))]
pub(crate) fn floating_action_rect(bounds: egui::Rect) -> egui::Rect {
    let size = Vec2::splat(FLOATING_ACTION_EDGE);
    let inset = Vec2::splat(FLOATING_ACTION_MARGIN);
    egui::Rect::from_min_size(bounds.right_bottom() - inset - size, size)
}

#[cfg(target_os = "android")]
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

pub(crate) fn form_combo(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    id_salt: impl egui::AsIdSalt,
    selected_text: impl Into<egui::WidgetText>,
    preferred_width: f32,
    add_contents: impl FnOnce(&mut Ui),
) {
    if ui.available_width() < FORM_STACK_BREAKPOINT {
        ui.vertical(|ui| {
            ui.label(label);
            let width = ui.available_width().max(1.0);
            egui::ComboBox::from_id_salt(id_salt)
                .selected_text(selected_text)
                .width(width)
                .truncate()
                .show_ui(ui, add_contents);
        });
    } else {
        ui.horizontal(|ui| {
            ui.label(label);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let width = preferred_width.min(ui.available_width().max(1.0));
                egui::ComboBox::from_id_salt(id_salt)
                    .selected_text(selected_text)
                    .width(width)
                    .truncate()
                    .show_ui(ui, add_contents);
            });
        });
    }
}

pub(crate) fn responsive_combo_box<R>(
    ui: &mut Ui,
    id_salt: impl egui::AsIdSalt,
    selected_text: impl Into<egui::WidgetText>,
    width: f32,
    item_count: usize,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let popup_style = ui.ctx().global_style();
    let spacing = &popup_style.spacing;
    let item_count_f32 = item_count as f32;
    let item_spacing_count = item_count.saturating_sub(1) as f32;
    let popup_height = item_count_f32 * spacing.interact_size.y
        + item_spacing_count * spacing.item_spacing.y
        + spacing.menu_margin.sum().y
        + 2.0 * ui.visuals().window_stroke.width
        + 4.0;
    let content_height = ui.ctx().content_rect().height();
    let popup_fits_viewport = content_height >= popup_height;

    let context = ui.ctx().clone();
    let theme = context.theme();
    let original_style = context.style_of(theme);
    if original_style.spacing.default_area_size.y < popup_height {
        context.style_mut_of(theme, |style| {
            style.spacing.default_area_size.y = popup_height;
        });
    }

    let response = egui::ComboBox::from_id_salt((id_salt, popup_fits_viewport))
        .selected_text(selected_text)
        .width(width)
        .height(content_height)
        .truncate()
        .show_ui(ui, add_contents);

    context.set_style_of(theme, original_style);
    response
}

/// A right-click menu that retains the regular popup/widget styling used by
/// combo boxes instead of egui's compact frameless menu override.
pub(crate) fn context_menu<R>(
    response: &Response,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> Option<InnerResponse<R>> {
    egui::Popup::context_menu(response)
        .style(egui::style::StyleModifier::default())
        .show(add_contents)
}

pub(crate) fn context_menu_item<'a>(
    ui: &mut Ui,
    enabled: bool,
    label: impl egui::IntoAtoms<'a>,
) -> Response {
    ui.add_enabled(enabled, egui::Button::selectable(false, label))
}

pub(crate) fn form_combo_with_help(
    ui: &mut Ui,
    label: &str,
    id_salt: impl egui::AsIdSalt,
    selected_text: impl Into<egui::WidgetText>,
    preferred_width: f32,
    help: &str,
    add_contents: impl FnOnce(&mut Ui),
) {
    if ui.available_width() < FORM_STACK_BREAKPOINT {
        ui.vertical(|ui| {
            let width = ui.available_width().max(1.0);
            ui.allocate_ui_with_layout(
                egui::vec2(width, HELP_BUTTON_EDGE),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.label(label).on_hover_text(help);
                },
            );
            let width = ui.available_width().max(1.0);
            egui::ComboBox::from_id_salt(id_salt)
                .selected_text(selected_text)
                .width(width)
                .truncate()
                .show_ui(ui, add_contents)
                .response
                .on_hover_text(help);
        });
    } else {
        ui.horizontal(|ui| {
            ui.label(label).on_hover_text(help);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let width = preferred_width.min(ui.available_width().max(1.0));
                egui::ComboBox::from_id_salt(id_salt)
                    .selected_text(selected_text)
                    .width(width)
                    .truncate()
                    .show_ui(ui, add_contents)
                    .response
                    .on_hover_text(help);
            });
        });
    }
}

pub(crate) fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    apply(ctx, UiDesign::default());
}

pub(crate) fn apply(ctx: &egui::Context, design: UiDesign) {
    let palette = design.palette();
    let theme = if design.is_dark() {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };

    let mut visuals = if design.is_dark() {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    visuals.panel_fill = palette.panel;
    visuals.window_fill = palette.window;
    visuals.faint_bg_color = palette.faint;
    visuals.extreme_bg_color = palette.extreme;
    visuals.code_bg_color = palette.faint;
    visuals.selection.bg_fill = palette.accent;
    let active_text = if design.is_dark() {
        Color32::WHITE
    } else {
        Color32::from_rgb(30, 32, 37)
    };
    visuals.selection.stroke = Stroke::new(1.0, active_text);
    visuals.hyperlink_color = palette.hyperlink;
    visuals.window_stroke = Stroke::new(1.0, palette.border);
    visuals.window_corner_radius = 10.0.into();
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(if design.is_dark() { 72 } else { 30 }),
    };
    visuals.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(if design.is_dark() { 82 } else { 34 }),
    };
    visuals.menu_corner_radius = CARD_RADIUS.into();

    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
    visuals.widgets.noninteractive.corner_radius = CARD_RADIUS.into();
    visuals.widgets.inactive.bg_fill = palette.inactive;
    visuals.widgets.inactive.weak_bg_fill = palette.inactive;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.inactive_stroke);
    visuals.widgets.inactive.corner_radius = CARD_RADIUS.into();
    visuals.widgets.hovered.bg_fill = palette.hovered;
    visuals.widgets.hovered.weak_bg_fill = palette.hovered;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.hovered_stroke);
    visuals.widgets.hovered.corner_radius = CARD_RADIUS.into();
    visuals.widgets.active.bg_fill = palette.accent;
    visuals.widgets.active.weak_bg_fill = palette.accent;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.accent_bright);
    visuals.widgets.active.fg_stroke.color = active_text;
    visuals.widgets.active.corner_radius = CARD_RADIUS.into();
    visuals.widgets.open.bg_fill = palette.open;
    visuals.widgets.open.weak_bg_fill = palette.open;
    visuals.widgets.open.bg_stroke = Stroke::new(1.0, palette.open_stroke);
    visuals.widgets.open.corner_radius = CARD_RADIUS.into();
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);

    let mut style = (*ctx.style_of(theme)).clone();
    style.visuals = visuals;
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(20.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(11.0));
    style.spacing.slider_width = 220.0;
    style.spacing.item_spacing = egui::vec2(SPACE_SM, SPACE_SM);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.interact_size.y = CONTROL_HEIGHT;
    style.spacing.window_margin = Margin::same(if cfg!(target_os = "android") {
        16
    } else {
        CONTENT_MARGIN
    });
    style.spacing.menu_margin = Margin::same(SPACE_SM as i8);
    style.spacing.indent = SPACE_LG;
    ctx.set_style_of(theme, style);
    ctx.set_theme(theme);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::{
        platform_control_height, platform_floating_action_edge, UiDesign, ANDROID_CONTROL_HEIGHT,
        CONTROL_HEIGHT, DESKTOP_CONTROL_HEIGHT, FLOATING_ACTION_EDGE, FLOATING_ACTION_MARGIN,
    };

    #[test]
    fn obsidian_blue_is_the_default_and_dark_accents_are_distinct() {
        assert_eq!(UiDesign::default(), UiDesign::ObsidianBlue);
        assert_eq!(
            serde_json::from_str::<UiDesign>(r#""midnight_pink""#).unwrap(),
            UiDesign::ObsidianBlue
        );
        assert_eq!(
            serde_json::from_str::<UiDesign>(r#""graphite_mint""#).unwrap(),
            UiDesign::ObsidianRed
        );

        let blue = UiDesign::ObsidianBlue.palette().accent;
        let red = UiDesign::ObsidianRed.palette().accent;
        assert!(blue.b() > blue.r() && blue.b() > blue.g());
        assert!(red.r() > red.g() && red.r() > red.b());
        assert_ne!(blue, red);
    }

    #[test]
    fn android_widgets_request_the_full_touch_target_height() {
        assert_eq!(platform_control_height(true), ANDROID_CONTROL_HEIGHT);
        assert_eq!(platform_control_height(false), DESKTOP_CONTROL_HEIGHT);
    }

    #[test]
    fn compact_density_is_limited_to_android_portrait() {
        let portrait = eframe::egui::vec2(411.0, 891.0);
        let landscape = eframe::egui::vec2(891.0, 411.0);
        assert!(super::compact_portrait_for_platform(portrait, true));
        assert!(!super::compact_portrait_for_platform(landscape, true));
        assert!(!super::compact_portrait_for_platform(portrait, false));
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

    #[test]
    fn photo_matched_backdrops_are_distinct_but_quiet() {
        let red = super::adaptive_backdrop_from_rgba(&[240, 30, 20, 255].repeat(32));
        let blue = super::adaptive_backdrop_from_rgba(&[20, 50, 240, 255].repeat(32));

        assert_ne!(red, blue);
        for color in [red, blue] {
            assert!(color.r() >= 18 && color.r() <= 50);
            assert!(color.g() >= 18 && color.g() <= 50);
            assert!(color.b() >= 18 && color.b() <= 50);
        }
    }
}
