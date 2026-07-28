pub mod components;
pub mod icons;
pub mod layout;
pub mod library;
pub mod preview;
pub mod settings;
pub mod sidebar;
pub mod top_bar;

/// Keep desktop dialogs compact while constraining them to the usable viewport.
/// Compact portrait screens additionally enable vertical scrolling so long
/// model-download text remains reachable without extending below system bars.
pub(crate) fn responsive_popup<'a>(
    window: eframe::egui::Window<'a>,
    ctx: &eframe::egui::Context,
    preferred_width: f32,
) -> eframe::egui::Window<'a> {
    let available = ctx.content_rect().size() - eframe::egui::vec2(24.0, 24.0);
    let available = eframe::egui::vec2(available.x.max(1.0), available.y.max(1.0));
    let compact_portrait = available.x < 560.0 && available.y > available.x;
    let window = window
        .default_width(preferred_width.min(available.x))
        .max_width(available.x)
        .max_height(available.y)
        .vscroll(compact_portrait);
    #[cfg(target_os = "android")]
    let window = window.order(eframe::egui::Order::Foreground);
    window
}

#[cfg(any(target_os = "android", test))]
const ANDROID_OVERFLOW_INSET: f32 = 5.0;

#[cfg(any(target_os = "android", test))]
fn android_overflow_button_rect(anchor_rect: eframe::egui::Rect, edge: f32) -> eframe::egui::Rect {
    eframe::egui::Rect::from_min_size(
        eframe::egui::pos2(
            anchor_rect.right() - edge - ANDROID_OVERFLOW_INSET,
            anchor_rect.top() + ANDROID_OVERFLOW_INSET,
        ),
        eframe::egui::vec2(edge, edge),
    )
}

#[cfg(target_os = "android")]
pub(crate) fn android_overflow_menu<R>(
    ui: &mut eframe::egui::Ui,
    anchor_rect: eframe::egui::Rect,
    _id: eframe::egui::Id,
    edge: f32,
    add_contents: impl FnOnce(&mut eframe::egui::Ui) -> R,
) -> eframe::egui::Response {
    use eframe::egui::{self, Popup};

    let response = ui.put(
        android_overflow_button_rect(anchor_rect, edge),
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

#[cfg(test)]
mod tests {
    use super::{android_overflow_button_rect, ANDROID_OVERFLOW_INSET};
    use eframe::egui;

    #[test]
    fn android_overflow_button_stays_inside_the_card_anchor() {
        let anchor = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(120.0, 80.0));
        let button = android_overflow_button_rect(anchor, 22.0);

        assert_eq!(button.size(), egui::vec2(22.0, 22.0));
        assert_eq!(button.right(), anchor.right() - ANDROID_OVERFLOW_INSET);
        assert_eq!(button.top(), anchor.top() + ANDROID_OVERFLOW_INSET);
        assert!(anchor.contains(button.min));
        assert!(anchor.contains(button.max));
    }
}
