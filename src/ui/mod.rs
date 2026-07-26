pub mod components;
pub mod icons;
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
    _id: eframe::egui::Id,
    edge: f32,
    add_contents: impl FnOnce(&mut eframe::egui::Ui) -> R,
) -> eframe::egui::Response {
    use eframe::egui::{self, Popup};

    const INSET: f32 = 5.0;
    let button_rect = egui::Rect::from_min_size(
        egui::pos2(
            anchor_rect.right() - edge - INSET,
            anchor_rect.top() + INSET,
        ),
        egui::vec2(edge, edge),
    );

    let response = ui.put(
        button_rect,
        egui::Button::new(
            egui::RichText::new(egui_phosphor::regular::DOTS_THREE_VERTICAL).size(edge * 0.52),
        )
        .corner_radius((edge * 0.22).clamp(3.0, 7.0)),
    );
    Popup::menu(&response).show(add_contents);

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
