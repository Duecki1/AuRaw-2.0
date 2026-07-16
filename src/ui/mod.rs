pub mod components;
pub mod layout;
pub mod library;
pub mod preview;
pub mod settings;
pub mod sidebar;
pub mod top_bar;

#[cfg(target_os = "android")]
pub(crate) fn android_overflow_menu<R>(
    ui: &mut eframe::egui::Ui,
    anchor_rect: eframe::egui::Rect,
    id: eframe::egui::Id,
    edge: f32,
    add_contents: impl FnOnce(&mut eframe::egui::Ui) -> R,
) -> eframe::egui::Response {
    use eframe::egui::{self, Direction, Layout, UiBuilder};

    const INSET: f32 = 5.0;
    let button_rect = egui::Rect::from_min_size(
        egui::pos2(
            anchor_rect.right() - edge - INSET,
            anchor_rect.top() + INSET,
        ),
        egui::vec2(edge, edge),
    );
    let mut button_ui = ui.new_child(
        UiBuilder::new()
            .id(id)
            .max_rect(button_rect)
            .layout(Layout::centered_and_justified(Direction::LeftToRight)),
    );
    button_ui.spacing_mut().interact_size = button_rect.size();
    let response = button_ui.menu_button("", add_contents).response;

    // Paint the ellipsis ourselves instead of relying on U+22EE. Some Android
    // system-font combinations do not contain that glyph and render a tofu
    // square in its place.
    let visuals = button_ui.style().interact(&response);
    let dot_radius = (edge * 0.055).clamp(1.1, 1.7);
    let dot_spacing = edge * 0.18;
    let center = response.rect.center();
    let painter = button_ui.painter_at(response.rect);
    for offset in [-dot_spacing, 0.0, dot_spacing] {
        painter.circle_filled(
            egui::pos2(center.x, center.y + offset),
            dot_radius,
            visuals.fg_stroke.color,
        );
    }

    response.on_hover_text("More actions")
}

pub(crate) fn mask_component_color(index: usize) -> eframe::egui::Color32 {
    use eframe::egui::Color32;
    const COLORS: [Color32; 8] = [
        Color32::from_rgb(78, 163, 255),
        Color32::from_rgb(255, 116, 102),
        Color32::from_rgb(83, 211, 146),
        Color32::from_rgb(242, 192, 75),
        Color32::from_rgb(183, 124, 255),
        Color32::from_rgb(63, 207, 220),
        Color32::from_rgb(255, 133, 196),
        Color32::from_rgb(180, 205, 88),
    ];
    COLORS[index % COLORS.len()]
}
