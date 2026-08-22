use crate::app::AurawApp;
use crate::ui::library::{
    apply_library_action, library_image_context_menu, load_desktop_reference_preview,
    DesktopFilmstripItem,
};
use crate::ui::preview::Preview;
use eframe::egui::{self, Align2, Color32, FontId, Sense, Stroke, StrokeKind, Ui};
use std::collections::HashSet;
use std::sync::{mpsc, Mutex, OnceLock};

pub(crate) const FILMSTRIP_HEIGHT: f32 = 112.0;
const FILMSTRIP_CARD_WIDTH: f32 = 118.0;
const FILMSTRIP_CARD_HEIGHT: f32 = 88.0;
const FILMSTRIP_GAP: f32 = 6.0;
const FILMSTRIP_PRELOAD_POINTS: f32 = 360.0;
const SPLIT_GAP: f32 = 8.0;
const SPLIT_MIN_PANE_WIDTH: f32 = 160.0;
const SPLIT_HANDLE_HIT_SLOP: f32 = 5.0;
const REFERENCE_BADGE: Color32 = Color32::from_rgb(214, 171, 72);
const REFERENCE_PREVIEW_EDGE: u32 = 2048;
static REFERENCE_PREVIEW_SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

pub(crate) struct Develop;

impl Develop {
    pub(crate) fn handle_image_navigation_shortcuts(
        context: &egui::Context,
        app: &mut AurawApp,
        frame: &eframe::Frame,
    ) {
        // Do not steal arrow keys from focused text fields/sliders or other
        // widgets that currently own keyboard input. Outside such focused
        // controls, Left/Right mirrors clicking the previous/next filmstrip
        // thumbnail and therefore follows the normal document-switch path.
        if context.egui_wants_keyboard_input() {
            return;
        }

        let previous = egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::ArrowLeft);
        let next = egui::KeyboardShortcut::new(egui::Modifiers::NONE, egui::Key::ArrowRight);
        let direction = if context.input_mut(|input| input.consume_shortcut(&previous)) {
            -1_i8
        } else if context.input_mut(|input| input.consume_shortcut(&next)) {
            1_i8
        } else {
            return;
        };

        let Some(current_path) = app.develop.current_path.as_deref() else {
            return;
        };
        let Some(current_index) = app.library.filmstrip_index_for_path(current_path) else {
            return;
        };
        let target_index = if direction < 0 {
            current_index.checked_sub(1)
        } else {
            current_index
                .checked_add(1)
                .filter(|index| *index < app.library.filmstrip_len())
        };
        let Some(target_index) = target_index else {
            return;
        };
        let Some(item) = app.library.filmstrip_item(target_index) else {
            return;
        };

        app.open_path(item.path, frame);
    }

    pub(crate) fn show_filmstrip(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        // Develop normally pauses catalog-wide thumbnail decoding. Polling here
        // installs only work that was already queued or explicitly requested by
        // visible filmstrip/reference items; the worker keeps ordinary catalog
        // background work paused.
        app.library.poll(ui.ctx());
        sync_reference_texture(app, ui.ctx());

        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .show(ui, |ui| {
                ui.set_min_height(FILMSTRIP_HEIGHT - 4.0);
                show_filmstrip_contents(ui, app, frame);
            });
    }

    pub(crate) fn show_preview(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        if app.develop_ui.reference.path.is_none() {
            Preview::show(ui, app, frame);
            return;
        }

        sync_reference_texture(app, ui.ctx());
        let available = ui.available_size();
        if available.x <= 0.0 || available.y <= 0.0 {
            return;
        }

        // Reserve the complete split canvas once, then construct each pane with
        // an explicit max rect AND clip rect. `allocate_ui` constrains layout but
        // its child painter inherits the parent's clip, which lets a zoomed
        // Develop mesh spill across the divider. Shrinking the child clip here
        // gives the edited pane a hard egui/wgpu scissor boundary.
        let (split_rect, _) = ui.allocate_exact_size(available, Sense::hover());
        let usable_width = (split_rect.width() - SPLIT_GAP).max(0.0);
        if usable_width < 2.0 {
            return;
        }
        let min_ratio = (SPLIT_MIN_PANE_WIDTH / usable_width).min(0.45);
        let max_ratio = 1.0 - min_ratio;
        app.develop_ui.reference.split_ratio = app.develop_ui.reference
            .split_ratio
            .clamp(min_ratio, max_ratio);

        // Treat the center gutter as a real drag handle. Using the pointer's
        // absolute x position avoids accumulating `drag_delta()` across frames,
        // and the ratio is retained when the reference image changes.
        let divider_left = split_rect.left() + usable_width * app.develop_ui.reference.split_ratio;
        let initial_divider_rect = egui::Rect::from_min_max(
            egui::pos2(divider_left, split_rect.top()),
            egui::pos2(divider_left + SPLIT_GAP, split_rect.bottom()),
        );
        let divider_response = ui
            .interact(
                egui::Rect::from_min_max(
                    egui::pos2(
                        initial_divider_rect.left() - SPLIT_HANDLE_HIT_SLOP,
                        initial_divider_rect.top(),
                    ),
                    egui::pos2(
                        initial_divider_rect.right() + SPLIT_HANDLE_HIT_SLOP,
                        initial_divider_rect.bottom(),
                    ),
                ),
                ui.make_persistent_id("develop-reference-split-divider"),
                Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if divider_response.dragged() {
            if let Some(pointer) = ui.ctx().input(|input| input.pointer.interact_pos()) {
                let ratio = (pointer.x - split_rect.left() - SPLIT_GAP * 0.5) / usable_width;
                app.develop_ui.reference.split_ratio = ratio.clamp(min_ratio, max_ratio);
            }
        }
        if divider_response.double_clicked() {
            app.develop_ui.reference.split_ratio = 0.5;
        }

        let pane_width = usable_width * app.develop_ui.reference.split_ratio;
        let left_rect = egui::Rect::from_min_max(
            split_rect.min,
            egui::pos2(split_rect.left() + pane_width, split_rect.bottom()),
        );
        let right_rect = egui::Rect::from_min_max(
            egui::pos2(left_rect.right() + SPLIT_GAP, split_rect.top()),
            split_rect.max,
        );
        let divider_rect = egui::Rect::from_min_max(
            egui::pos2(left_rect.right(), split_rect.top()),
            egui::pos2(right_rect.left(), split_rect.bottom()),
        );
        let divider_color = if divider_response.hovered() || divider_response.dragged() {
            ui.visuals().widgets.hovered.bg_stroke.color
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke.color
        };
        ui.painter().rect_filled(divider_rect, 0.0, divider_color);

        let mut reference_ui = ui.new_child(egui::UiBuilder::new().max_rect(left_rect));
        reference_ui.shrink_clip_rect(left_rect);
        show_reference_pane(&mut reference_ui, app);

        let mut develop_ui = ui.new_child(egui::UiBuilder::new().max_rect(right_rect));
        develop_ui.shrink_clip_rect(right_rect);
        Preview::show(&mut develop_ui, app, frame);
    }
}

fn show_filmstrip_contents(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
    let count = app.library.filmstrip_len();
    if count == 0 {
        ui.centered_and_justified(|ui| {
            ui.label(
                egui::RichText::new("No RAW images in the active folder")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        });
        return;
    }

    let active_path = app.develop.current_path.clone();
    let reference_path = app.develop_ui.reference.path.clone();
    let center_request = active_path.as_ref().and_then(|path| {
        if app.develop_ui.filmstrip_centered_path.as_ref() == Some(path) {
            None
        } else {
            app.library
                .filmstrip_index_for_path(path)
                .map(|index| (index, path.clone()))
        }
    });
    let mut centered_path = None;
    let mut open_path: Option<std::path::PathBuf> = None;
    let mut library_action = None;
    let mut protected_indices = HashSet::new();
    let stride = FILMSTRIP_CARD_WIDTH + FILMSTRIP_GAP;
    let cards_width = count as f32 * stride - FILMSTRIP_GAP;

    // On desktop, a one-axis horizontal ScrollArea should consume a normal
    // vertical mouse-wheel gesture while the pointer is over it. Keep this
    // style override local to the filmstrip UI.
    ui.style_mut().always_scroll_the_only_direction = true;

    egui::ScrollArea::horizontal()
        .scroll_source(egui::scroll_area::ScrollSource::default())
        .wheel_scroll_multiplier(egui::vec2(1.35, 1.0))
        .id_salt("develop-filmstrip-scroll")
        .auto_shrink([false, false])
        .show_viewport(ui, |ui, viewport| {
            // Keep the content bounds equal to the real thumbnail run. That
            // lets egui clamp a center request naturally at either end: the
            // active image is centered only when there are enough thumbnails
            // on both sides, while first/last-nearby images stay edge-aligned
            // instead of gaining artificial blank padding.
            let content_height = ui.available_height().max(FILMSTRIP_CARD_HEIGHT);
            let (content_rect, _) = ui.allocate_exact_size(
                egui::vec2(cards_width.max(1.0), content_height),
                Sense::hover(),
            );
            let items_left = content_rect.left();

            // This is deliberately a one-shot request. It fires when Develop is
            // first shown for an image, when another thumbnail becomes active,
            // or after the filmstrip is reopened. Align::Center is clamped by
            // the real content bounds above, so manual wheel/drag scrolling
            // remains under user control and edge images are not force-centered.
            if let Some((index, path)) = center_request.as_ref() {
                let x = items_left + *index as f32 * stride;
                let y = content_rect.center().y - FILMSTRIP_CARD_HEIGHT * 0.5;
                let active_rect = egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::vec2(FILMSTRIP_CARD_WIDTH, FILMSTRIP_CARD_HEIGHT),
                );
                ui.scroll_to_rect(active_rect, Some(egui::Align::Center));
                centered_path = Some(path.clone());
                protected_indices.insert(*index);
                app.library.touch_and_request_thumbnail(*index, ui.ctx());
                ui.ctx().request_repaint();
            }

            // `show_viewport` reports `viewport` in scroll-content coordinates,
            // while `content_rect`/`items_left` are screen coordinates after egui
            // has translated the child UI by the current scroll offset. Mixing
            // those spaces makes the offset get counted twice: as the user
            // scrolls, the virtualized range advances faster than the cards and
            // thumbnails disappear one-by-one. Convert the item-run origin back
            // into the viewport's content-relative coordinate space first.
            let items_origin_in_content = items_left - ui.max_rect().left();
            let preload = viewport.expand(FILMSTRIP_PRELOAD_POINTS);
            let relative_left =
                (preload.left() - items_origin_in_content).clamp(0.0, cards_width.max(0.0));
            let relative_right =
                (preload.right() - items_origin_in_content).clamp(0.0, cards_width.max(0.0));
            let first = ((relative_left / stride).floor() as usize).min(count.saturating_sub(1));
            let last = (((relative_right / stride).ceil() as usize) + 1).min(count);

            for index in first..last {
                protected_indices.insert(index);
                app.library.touch_and_request_thumbnail(index, ui.ctx());
                let Some(item) = app.library.filmstrip_item(index) else {
                    continue;
                };

                let x = items_left + index as f32 * stride;
                let y = content_rect.center().y - FILMSTRIP_CARD_HEIGHT * 0.5;
                let rect = egui::Rect::from_min_size(
                    egui::pos2(x, y),
                    egui::vec2(FILMSTRIP_CARD_WIDTH, FILMSTRIP_CARD_HEIGHT),
                );
                let active = active_path.as_deref() == Some(item.path.as_path());
                let reference = reference_path.as_deref() == Some(item.path.as_path());
                let response = filmstrip_thumbnail(ui, &item, rect, active, reference);

                if response.clicked() && !response.secondary_clicked() && !active {
                    open_path = Some(item.path.clone());
                }

                response.context_menu(|ui| {
                    // Develop and both Library UIs use the exact same action menu.
                    let context_assets = [item.asset.clone()];
                    if let Some(action) =
                        library_image_context_menu(ui, app, &item.asset, &context_assets)
                    {
                        library_action = Some(action);
                    }
                    ui.separator();
                    if reference {
                        if ui.button("Clear Reference Image").clicked() {
                            app.develop_ui.reference.clear();
                            ui.close();
                        }
                    } else if ui.button("Set as Reference Image").clicked() {
                        set_reference_image(app, &item, ui.ctx());
                        ui.close();
                    }
                });
            }
        });

    if let Some(path) = centered_path {
        app.develop_ui.filmstrip_centered_path = Some(path);
    }

    app.library.evict_old_textures(&protected_indices);

    if let Some(action) = library_action {
        apply_library_action(ui, app, frame, action);
    }
    if let Some(path) = open_path {
        app.open_path(path, frame);
    }
}

fn set_reference_image(app: &mut AurawApp, item: &DesktopFilmstripItem, context: &egui::Context) {
    let path = item.path.clone();
    app.develop_ui.reference.path = Some(path);
    app.develop_ui.reference.label = Some(item.asset.display_name.clone());
    // Install the existing catalog texture immediately so Reference mode opens
    // without a blank frame. A dedicated high-quality preview replaces it as
    // soon as the background request completes.
    app.develop_ui.reference.texture = item.texture.clone();
    app.develop_ui.reference.texture_size = item.thumbnail_size;
    app.develop_ui.reference.high_quality = false;
    app.develop_ui.reference.error = None;
    app.develop_ui.reference.loading_path = None;
    app.develop_ui.reference.preview_receiver = None;
    start_reference_preview_load(app, context);
}

fn start_reference_preview_load(app: &mut AurawApp, context: &egui::Context) {
    let Some(path) = app.develop_ui.reference.path.clone() else {
        return;
    };
    if app.develop_ui.reference.high_quality
        || app.develop_ui.reference.loading_path.as_ref() == Some(&path)
    {
        return;
    }

    let (sender, receiver) = mpsc::channel();
    let decode_gate = app.library.decode_gate();
    let repaint = context.clone();
    let worker_path = path.clone();
    let spawn_result = std::thread::Builder::new()
        .name("auraw-reference-preview".to_owned())
        .spawn(move || {
            let reference_serial = REFERENCE_PREVIEW_SERIAL.get_or_init(|| Mutex::new(()));
            let _reference_guard = reference_serial
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let result = match decode_gate.read() {
                Ok(_decode_guard) => {
                    load_desktop_reference_preview(&worker_path, REFERENCE_PREVIEW_EDGE)
                }
                Err(_) => Err("reference preview decode gate is poisoned".to_owned()),
            };
            let _ = sender.send((worker_path, result));
            repaint.request_repaint();
        });

    match spawn_result {
        Ok(_) => {
            app.develop_ui.reference.loading_path = Some(path);
            app.develop_ui.reference.preview_receiver = Some(receiver);
        }
        Err(error) => {
            app.develop_ui.reference.error =
                Some(format!("could not start reference preview worker: {error}"));
        }
    }
}

fn sync_reference_texture(app: &mut AurawApp, context: &egui::Context) {
    let Some(path) = app.develop_ui.reference.path.clone() else {
        return;
    };

    let event = app.develop_ui.reference
        .preview_receiver
        .as_ref()
        .and_then(|receiver| match receiver.try_recv() {
            Ok(event) => Some(Ok(event)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(
                "reference preview worker stopped unexpectedly".to_owned(),
            )),
        });

    if let Some(event) = event {
        app.develop_ui.reference.preview_receiver = None;
        app.develop_ui.reference.loading_path = None;
        match event {
            Ok((loaded_path, Ok(thumbnail))) if loaded_path == path => {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [thumbnail.width as usize, thumbnail.height as usize],
                    &thumbnail.rgba,
                );
                app.develop_ui.reference.texture = Some(context.load_texture(
                    format!("develop-reference-high-quality-{}", loaded_path.display()),
                    image,
                    egui::TextureOptions::LINEAR,
                ));
                app.develop_ui.reference.texture_size = Some([thumbnail.width, thumbnail.height]);
                app.develop_ui.reference.high_quality = true;
                app.develop_ui.reference.error = None;
            }
            Ok((loaded_path, Err(error))) if loaded_path == path => {
                app.develop_ui.reference.error = Some(error);
            }
            Ok(_) => {}
            Err(error) => app.develop_ui.reference.error = Some(error),
        }
    }

    // Keep the existing 512 px catalog texture as an immediate placeholder.
    // If it was evicted before Reference mode opened, ask the normal thumbnail
    // worker to repopulate it while the high-quality request is running.
    if app.develop_ui.reference.texture.is_none() {
        if let Some(index) = app.library.filmstrip_index_for_path(&path) {
            app.library.touch_and_request_thumbnail(index, context);
            if let Some(item) = app.library.filmstrip_item(index) {
                app.develop_ui.reference.label = Some(item.asset.display_name);
                app.develop_ui.reference.texture = item.texture;
                app.develop_ui.reference.texture_size = item.thumbnail_size;
            }
        }
    }

    if !app.develop_ui.reference.high_quality
        && app.develop_ui.reference.preview_receiver.is_none()
        && app.develop_ui.reference.error.is_none()
    {
        start_reference_preview_load(app, context);
    }
}

fn show_reference_pane(ui: &mut Ui, app: &mut AurawApp) {
    egui::Frame::new()
        .fill(Color32::from_rgb(15, 16, 18))
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .show(ui, |ui| {
            let available = ui.available_size();
            let (rect, _) = ui.allocate_exact_size(available, Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, Color32::from_rgb(15, 16, 18));

            if let Some(texture) = app.develop_ui.reference.texture.as_ref() {
                let texture_size = app.develop_ui.reference
                    .texture_size
                    .map(|[width, height]| egui::vec2(width as f32, height as f32))
                    .unwrap_or_else(|| texture.size_vec2());
                let image_size = fitted_size(rect.size(), texture_size);
                let image_rect = egui::Rect::from_center_size(rect.center(), image_size);
                painter.image(
                    texture.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                painter.text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    "Loading reference…",
                    FontId::proportional(13.0),
                    ui.visuals().weak_text_color(),
                );
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(80));
            }

            if !app.develop_ui.reference.high_quality
                && app.develop_ui.reference.loading_path.is_some()
                && app.develop_ui.reference.texture.is_some()
            {
                painter.text(
                    rect.right_bottom() - egui::vec2(12.0, 12.0),
                    Align2::RIGHT_BOTTOM,
                    "Loading full reference preview…",
                    FontId::proportional(10.5),
                    Color32::from_white_alpha(180),
                );
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(80));
            } else if let Some(error) = app.develop_ui.reference.error.as_deref() {
                painter.text(
                    rect.right_bottom() - egui::vec2(12.0, 12.0),
                    Align2::RIGHT_BOTTOM,
                    error,
                    FontId::proportional(10.5),
                    ui.visuals().error_fg_color,
                );
            }

            let badge_rect = egui::Rect::from_min_size(
                rect.min + egui::vec2(10.0, 10.0),
                egui::vec2(86.0, 24.0),
            );
            painter.rect_filled(badge_rect, 5.0, Color32::from_black_alpha(176));
            painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                "Reference",
                FontId::proportional(12.0),
                Color32::WHITE,
            );

            let close_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 34.0, rect.top() + 8.0),
                egui::vec2(26.0, 26.0),
            );
            let close = ui.put(
                close_rect,
                egui::Button::new(egui_phosphor::regular::X)
                    .min_size(close_rect.size())
                    .corner_radius(5.0),
            );
            if close.clicked() {
                app.develop_ui.reference.clear();
            }

            if let Some(label) = app.develop_ui.reference.label.as_deref() {
                let text_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 10.0, rect.bottom() - 34.0),
                    egui::pos2(rect.right() - 10.0, rect.bottom() - 8.0),
                );
                painter.rect_filled(text_rect, 4.0, Color32::from_black_alpha(150));
                painter.text(
                    text_rect.left_center() + egui::vec2(8.0, 0.0),
                    Align2::LEFT_CENTER,
                    label,
                    FontId::proportional(11.5),
                    Color32::WHITE,
                );
            }
        });
}

fn filmstrip_thumbnail(
    ui: &mut Ui,
    item: &DesktopFilmstripItem,
    rect: egui::Rect,
    active: bool,
    reference: bool,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("develop-filmstrip-thumbnail", &item.asset.id)),
        Sense::click(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, Color32::from_rgb(17, 18, 20));

    if let Some(texture) = item.texture.as_ref() {
        let image_rect = egui::Rect::from_min_max(
            rect.min + egui::vec2(2.0, 2.0),
            egui::pos2(rect.right() - 2.0, rect.bottom() - 22.0),
        );
        painter.image(
            texture.id(),
            image_rect,
            cover_uv(item.thumbnail_size, image_rect.size()),
            Color32::WHITE,
        );
    } else {
        painter.text(
            egui::pos2(rect.center().x, rect.center().y - 9.0),
            Align2::CENTER_CENTER,
            "RAW",
            FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
    }

    if item.developed_thumbnail_pending {
        let center = rect.right_top() + egui::vec2(-13.0, 13.0);
        painter.circle_filled(center, 10.0, Color32::from_black_alpha(190));
        painter.text(
            center,
            Align2::CENTER_CENTER,
            egui_phosphor::regular::ARROW_CLOCKWISE,
            FontId::proportional(13.0),
            Color32::from_rgb(244, 142, 48),
        );
    }

    let label_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 2.0, rect.bottom() - 22.0),
        rect.max - egui::vec2(2.0, 2.0),
    );
    painter.rect_filled(label_rect, 2.0, Color32::from_black_alpha(180));
    painter.text(
        label_rect.center(),
        Align2::CENTER_CENTER,
        elide_name(&item.asset.display_name, 17),
        FontId::proportional(10.5),
        Color32::WHITE,
    );

    if response.hovered() {
        painter.rect_filled(rect, 3.0, Color32::from_white_alpha(13));
    }
    if active {
        painter.rect_stroke(
            rect,
            3.0,
            Stroke::new(3.0, ui.visuals().selection.bg_fill),
            StrokeKind::Inside,
        );
    } else {
        painter.rect_stroke(
            rect,
            3.0,
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            StrokeKind::Inside,
        );
    }

    if reference {
        let ref_rect =
            egui::Rect::from_min_size(rect.min + egui::vec2(5.0, 5.0), egui::vec2(31.0, 17.0));
        painter.rect_filled(ref_rect, 3.0, Color32::from_black_alpha(190));
        painter.text(
            ref_rect.center(),
            Align2::CENTER_CENTER,
            "REF",
            FontId::proportional(9.5),
            REFERENCE_BADGE,
        );
    }

    response.on_hover_text(item.path.display().to_string())
}

fn cover_uv(source_size: Option<[u32; 2]>, target_size: egui::Vec2) -> egui::Rect {
    let Some([width, height]) = source_size.filter(|[width, height]| *width > 0 && *height > 0)
    else {
        return egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    };
    let source_aspect = width as f32 / height as f32;
    let target_aspect = target_size.x / target_size.y.max(1.0);
    if source_aspect > target_aspect {
        let visible = (target_aspect / source_aspect).clamp(0.0, 1.0);
        let inset = (1.0 - visible) * 0.5;
        egui::Rect::from_min_max(egui::pos2(inset, 0.0), egui::pos2(1.0 - inset, 1.0))
    } else {
        let visible = (source_aspect / target_aspect).clamp(0.0, 1.0);
        let inset = (1.0 - visible) * 0.5;
        egui::Rect::from_min_max(egui::pos2(0.0, inset), egui::pos2(1.0, 1.0 - inset))
    }
}

fn fitted_size(available: egui::Vec2, source: egui::Vec2) -> egui::Vec2 {
    if source.x <= 0.0 || source.y <= 0.0 {
        return egui::Vec2::ZERO;
    }
    let scale = (available.x / source.x)
        .min(available.y / source.y)
        .max(0.0);
    source * scale
}

fn elide_name(name: &str, max_chars: usize) -> String {
    if name.chars().count() <= max_chars {
        return name.to_owned();
    }
    let keep = max_chars.saturating_sub(1);
    let left = keep / 2;
    let right = keep - left;
    let prefix = name.chars().take(left).collect::<String>();
    let suffix = name
        .chars()
        .rev()
        .take(right)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{prefix}…{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitted_reference_size_preserves_aspect() {
        let fitted = fitted_size(egui::vec2(400.0, 300.0), egui::vec2(600.0, 400.0));
        assert!((fitted.x - 400.0).abs() < 0.001);
        assert!((fitted.y - 266.666_66).abs() < 0.001);
    }
}
