pub mod components;
#[cfg(not(target_os = "android"))]
pub mod develop;
pub mod icons;
pub mod layout;
pub mod library;
pub mod preview;
pub mod settings;
pub mod sidebar;
pub mod theme;
pub mod top_bar;

#[cfg(not(target_os = "android"))]
pub(crate) fn choose_export_file_path(
    format: crate::pipeline::ExportFormat,
    default_name: &str,
    initial_directory: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .add_filter(format!("{} image", format.label()), format.extensions())
        .set_file_name(default_name);
    if let Some(directory) = initial_directory.filter(|path| !path.as_os_str().is_empty()) {
        dialog = dialog.set_directory(directory);
    }
    let mut path = dialog.save_file()?;
    let valid_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| format.matches_extension(extension));
    if !valid_extension {
        path.set_extension(format.extension());
    }
    Some(path)
}

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

#[cfg(any(target_os = "android", test))]
fn android_overflow_button(
    ui: &mut eframe::egui::Ui,
    anchor_rect: eframe::egui::Rect,
    id: eframe::egui::Id,
    edge: f32,
) -> eframe::egui::Response {
    use eframe::egui::{self, Align2, Sense, StrokeKind};

    let button_rect = android_overflow_button_rect(anchor_rect, edge);
    let touch_edge = ui
        .spacing()
        .interact_size
        .x
        .max(ui.spacing().interact_size.y)
        .max(edge);
    let hit_rect = egui::Rect::from_min_size(
        egui::pos2(anchor_rect.right() - touch_edge, anchor_rect.top()),
        egui::vec2(touch_edge, touch_edge),
    )
    .intersect(anchor_rect);
    let response = ui.interact(hit_rect, id, Sense::click());
    let visuals = ui.style().interact(&response);
    let radius = (edge * 0.22).clamp(3.0, 7.0);
    let painter = ui.painter_at(button_rect);
    painter.rect_filled(button_rect, radius, visuals.weak_bg_fill);
    painter.rect_stroke(button_rect, radius, visuals.bg_stroke, StrokeKind::Inside);
    painter.text(
        button_rect.center(),
        Align2::CENTER_CENTER,
        egui_phosphor::regular::DOTS_THREE_VERTICAL,
        egui::FontId::proportional(edge * 0.52),
        visuals.fg_stroke.color,
    );
    response
}

#[cfg(target_os = "android")]
pub(crate) fn android_overflow_menu<R>(
    ui: &mut eframe::egui::Ui,
    anchor_rect: eframe::egui::Rect,
    id: eframe::egui::Id,
    edge: f32,
    add_contents: impl FnOnce(&mut eframe::egui::Ui) -> R,
) -> eframe::egui::Response {
    use eframe::egui::Popup;

    let response = android_overflow_button(ui, anchor_rect, id, edge);
    Popup::menu(&response).show(add_contents);

    response.on_hover_text("More actions")
}

pub(crate) fn mask_component_color(index: usize) -> eframe::egui::Color32 {
    use crate::ui::theme::MASK_COMPONENT_COLORS;
    MASK_COMPONENT_COLORS[index % MASK_COMPONENT_COLORS.len()]
}

#[cfg(test)]
mod tests {
    use super::{android_overflow_button, android_overflow_button_rect, ANDROID_OVERFLOW_INSET};
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

    #[test]
    fn android_overflow_button_does_not_rewind_vertical_layout() {
        egui::__run_test_ui(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let (card_rect, _) =
                ui.allocate_exact_size(egui::vec2(68.0, 72.0), egui::Sense::click());
            let next_position = ui.next_widget_position();

            let overflow =
                android_overflow_button(ui, card_rect, egui::Id::new("test-overflow"), 22.0);

            assert_eq!(ui.next_widget_position(), next_position);
            assert!(card_rect.contains(overflow.rect.min));
            assert!(card_rect.contains(overflow.rect.max));

            let (submask_rect, _) =
                ui.allocate_exact_size(egui::vec2(56.0, 62.0), egui::Sense::click());
            assert!(submask_rect.top() >= card_rect.bottom());
        });
    }
}
