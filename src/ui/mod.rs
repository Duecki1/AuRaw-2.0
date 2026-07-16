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
    use eframe::egui::{self, Color32, Popup, Sense};

    const INSET: f32 = 5.0;
    let button_rect = egui::Rect::from_min_size(
        egui::pos2(
            anchor_rect.right() - edge - INSET,
            anchor_rect.top() + INSET,
        ),
        egui::vec2(edge, edge),
    );

    // Do not use `Ui::menu_button` here. Its normal button layout can grow to
    // the global Android touch target and it also lays out menu-button atoms,
    // which may render as a missing-glyph square with some system fonts.
    // A raw interaction keeps the visible control at the requested exact size.
    let response = ui.interact(button_rect, id, Sense::click());
    Popup::menu(&response).show(add_contents);

    let painter = ui.painter_at(button_rect);
    let background = if response.is_pointer_button_down_on() {
        Color32::from_black_alpha(220)
    } else if response.hovered() {
        Color32::from_black_alpha(195)
    } else {
        Color32::from_black_alpha(165)
    };
    painter.rect_filled(button_rect, (edge * 0.22).clamp(3.0, 7.0), background);

    // Draw the overflow mark geometrically so it never depends on font glyphs.
    let dot_radius = (edge * 0.065).clamp(1.0, 1.6);
    let dot_spacing = edge * 0.20;
    let center = button_rect.center();
    for offset in [-dot_spacing, 0.0, dot_spacing] {
        painter.circle_filled(
            egui::pos2(center.x, center.y + offset),
            dot_radius,
            Color32::WHITE,
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
