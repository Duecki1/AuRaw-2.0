pub mod components;
pub mod layout;
pub mod library;
pub mod preview;
pub mod settings;
pub mod sidebar;
pub mod top_bar;

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
