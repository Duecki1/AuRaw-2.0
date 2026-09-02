use crate::app::CalibRawApp;
#[cfg(not(target_os = "android"))]
use crate::ui::library::library_image_context_menu;
use crate::ui::library::{
    apply_library_action, load_desktop_reference_preview, DesktopFilmstripItem,
};
use crate::ui::preview::Preview;
use eframe::egui::{self, Align2, Color32, FontId, Sense, Stroke, StrokeKind, Ui};
use std::collections::HashSet;
use std::sync::{mpsc, Mutex, OnceLock};

pub(crate) const FILMSTRIP_HEIGHT: f32 = 128.0;
const FILMSTRIP_CARD_HEIGHT: f32 = 96.0;
const FILMSTRIP_GAP: f32 = 8.0;
const FILMSTRIP_PRELOAD_POINTS: f32 = 360.0;
const FILMSTRIP_HOVER_OVERLAY_ALPHA: u8 = 156;
const FILMSTRIP_HOVER_ANIMATION_SECONDS: f32 = 0.18;
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
        app: &mut CalibRawApp,
        frame: &eframe::Frame,
    ) {
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

    pub(crate) fn show_filmstrip(ui: &mut Ui, app: &mut CalibRawApp, frame: &eframe::Frame) {
        app.library.poll(ui.ctx());
        sync_reference_texture(app, ui.ctx());
        let shelf_rect = ui.max_rect();
        ui.painter().hline(
            shelf_rect.x_range(),
            shelf_rect.top(),
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        );

        egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .inner_margin(egui::Margin::symmetric(12, 10))
            .show(ui, |ui| {
                ui.set_min_height(FILMSTRIP_CARD_HEIGHT);
                show_filmstrip_contents(ui, app, frame);
            });
    }

    pub(crate) fn show_preview(ui: &mut Ui, app: &mut CalibRawApp, frame: &eframe::Frame) {
        if app.develop_ui.reference.path.is_none() {
            Preview::show(ui, app, frame);
            return;
        }

        sync_reference_texture(app, ui.ctx());
        let available = ui.available_size();
        if available.x <= 0.0 || available.y <= 0.0 {
            return;
        }

        let (split_rect, _) = ui.allocate_exact_size(available, Sense::hover());
        let usable_width = (split_rect.width() - SPLIT_GAP).max(0.0);
        if usable_width < 2.0 {
            return;
        }
        let min_ratio = (SPLIT_MIN_PANE_WIDTH / usable_width).min(0.45);
        let max_ratio = 1.0 - min_ratio;
        app.develop_ui.reference.split_ratio = app
            .develop_ui
            .reference
            .split_ratio
            .clamp(min_ratio, max_ratio);

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

fn show_filmstrip_contents(ui: &mut Ui, app: &mut CalibRawApp, frame: &eframe::Frame) {
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
    let mut cards_width = 0.0;
    let filmstrip_cards = (0..count)
        .map(|index| {
            let width = app.library.filmstrip_item_aspect(index) * FILMSTRIP_CARD_HEIGHT;
            let card = (cards_width, width);
            cards_width += width + FILMSTRIP_GAP;
            card
        })
        .collect::<Vec<_>>();
    cards_width = (cards_width - FILMSTRIP_GAP).max(0.0);

    ui.scope(|ui| {
        ui.style_mut().always_scroll_the_only_direction = true;
        let mut scroll_style = egui::style::ScrollStyle::solid();
        scroll_style.bar_width = 7.0;
        scroll_style.bar_inner_margin = 7.0;
        ui.spacing_mut().scroll = scroll_style;

        egui::ScrollArea::horizontal()
            .scroll_source(egui::scroll_area::ScrollSource::default())
            .wheel_scroll_multiplier(egui::vec2(1.35, 1.0))
            .id_salt("develop-filmstrip-scroll")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                let content_height = ui.available_height().max(FILMSTRIP_CARD_HEIGHT);
                let (content_rect, _) = ui.allocate_exact_size(
                    egui::vec2(cards_width.max(1.0), content_height),
                    Sense::hover(),
                );
                let items_left = content_rect.left();

                if let Some((index, path)) = center_request.as_ref() {
                    let (offset, width) = filmstrip_cards[*index];
                    let x = items_left + offset;
                    let y = content_rect.center().y - FILMSTRIP_CARD_HEIGHT * 0.5;
                    let active_rect = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(width, FILMSTRIP_CARD_HEIGHT),
                    );
                    ui.scroll_to_rect(active_rect, Some(egui::Align::Center));
                    centered_path = Some(path.clone());
                    protected_indices.insert(*index);
                    app.library.touch_and_request_thumbnail(*index, ui.ctx());
                    ui.ctx().request_repaint();
                }

                let items_origin_in_content = items_left - ui.max_rect().left();
                let preload = viewport.expand(FILMSTRIP_PRELOAD_POINTS);
                let relative_left =
                    (preload.left() - items_origin_in_content).clamp(0.0, cards_width.max(0.0));
                let relative_right =
                    (preload.right() - items_origin_in_content).clamp(0.0, cards_width.max(0.0));
                let first = filmstrip_cards.partition_point(|(offset, width)| {
                    offset + width + FILMSTRIP_GAP < relative_left
                });
                let last = filmstrip_cards
                    .partition_point(|(offset, _)| *offset <= relative_right)
                    .min(count);

                for (index, &(offset, width)) in
                    filmstrip_cards.iter().enumerate().take(last).skip(first)
                {
                    protected_indices.insert(index);
                    app.library.touch_and_request_thumbnail(index, ui.ctx());
                    let Some(item) = app.library.filmstrip_item(index) else {
                        continue;
                    };

                    let x = items_left + offset;
                    let y = content_rect.center().y - FILMSTRIP_CARD_HEIGHT * 0.5;
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(width, FILMSTRIP_CARD_HEIGHT),
                    );
                    let active = active_path.as_deref() == Some(item.path.as_path());
                    let reference = reference_path.as_deref() == Some(item.path.as_path());
                    let response = filmstrip_thumbnail(ui, &item, rect, active, reference);

                    if response.clicked() && !response.secondary_clicked() && !active {
                        open_path = Some(item.path.clone());
                    }

                    crate::ui::theme::context_menu(&response, |ui| {
                        let context_assets = [item.asset.clone()];
                        if let Some(action) =
                            library_image_context_menu(ui, app, &item.asset, &context_assets)
                        {
                            library_action = Some(action);
                        }
                        ui.separator();
                        if reference {
                            if crate::ui::theme::context_menu_item(
                                ui,
                                true,
                                "Clear Reference Image",
                            )
                            .clicked()
                            {
                                app.develop_ui.reference.clear();
                                ui.close();
                            }
                        } else if crate::ui::theme::context_menu_item(
                            ui,
                            true,
                            "Set as Reference Image",
                        )
                        .clicked()
                        {
                            set_reference_image(app, &item, ui.ctx());
                            ui.close();
                        }
                    });
                }
            });
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

fn set_reference_image(
    app: &mut CalibRawApp,
    item: &DesktopFilmstripItem,
    context: &egui::Context,
) {
    let path = item.path.clone();
    app.develop_ui.reference.path = Some(path);
    app.develop_ui.reference.label = Some(item.asset.display_name.clone());
    app.develop_ui.reference.texture = item.texture.clone();
    app.develop_ui.reference.texture_size = item.thumbnail_size;
    app.develop_ui.reference.high_quality = false;
    app.develop_ui.reference.error = None;
    app.develop_ui.reference.loading_path = None;
    app.develop_ui.reference.preview_receiver = None;
    start_reference_preview_load(app, context);
}

fn start_reference_preview_load(app: &mut CalibRawApp, context: &egui::Context) {
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
        .name("calibraw-reference-preview".to_owned())
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

fn sync_reference_texture(app: &mut CalibRawApp, context: &egui::Context) {
    let Some(path) = app.develop_ui.reference.path.clone() else {
        return;
    };

    let event = app
        .develop_ui
        .reference
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

fn show_reference_pane(ui: &mut Ui, app: &mut CalibRawApp) {
    let backdrop = app.preview_backdrop_color();
    egui::Frame::new()
        .fill(backdrop)
        .stroke(Stroke::new(
            1.0,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        ))
        .show(ui, |ui| {
            let available = ui.available_size();
            let (rect, _) = ui.allocate_exact_size(available, Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, backdrop);

            if let Some(texture) = app.develop_ui.reference.texture.as_ref() {
                let texture_size = app
                    .develop_ui
                    .reference
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
                    crate::ui::theme::text_on_backdrop(backdrop),
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
    let painter = ui.painter_at(rect);
    let card_radius = crate::ui::theme::CARD_RADIUS;

    if let Some(texture) = item.texture.as_ref() {
        painter.add(
            egui::epaint::RectShape::filled(rect, card_radius, Color32::WHITE)
                .with_texture(texture.id(), cover_uv(item.thumbnail_size, rect.size())),
        );
    } else {
        painter.rect_filled(rect, card_radius, ui.visuals().extreme_bg_color);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "RAW",
            FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
    }

    filmstrip_name_hover_overlay(ui, &response, rect, &item.asset.display_name);

    if item.developed_thumbnail_pending {
        let center = rect.right_top() + egui::vec2(-13.0, 13.0);
        crate::ui::components::pending_indicator(&painter, center, 10.0, 13.0);
    }

    if active {
        painter.rect_stroke(
            rect,
            card_radius,
            Stroke::new(2.0, ui.visuals().selection.bg_fill),
            StrokeKind::Inside,
        );
    } else {
        painter.rect_stroke(
            rect,
            card_radius,
            Stroke::new(
                1.0,
                if response.hovered() {
                    ui.visuals().widgets.hovered.bg_stroke.color
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke.color
                },
            ),
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

    response
}

fn filmstrip_name_hover_overlay(ui: &Ui, response: &egui::Response, rect: egui::Rect, name: &str) {
    let hover_progress = ui.ctx().animate_bool_with_time_and_easing(
        response.id.with("overlay"),
        response.hovered(),
        FILMSTRIP_HOVER_ANIMATION_SECONDS,
        egui::emath::easing::cubic_out,
    );
    if hover_progress <= 0.0 {
        return;
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(
        rect,
        crate::ui::theme::CARD_RADIUS,
        Color32::from_black_alpha(
            (f32::from(FILMSTRIP_HOVER_OVERLAY_ALPHA) * hover_progress).round() as u8,
        ),
    );
    let font_size = 14.5;
    let text_width = (rect.width() - 24.0).max(1.0);
    let maximum_chars = (text_width / (font_size * 0.55)).floor().max(6.0) as usize;
    let title = elide_middle(name, maximum_chars);
    let slide = egui::vec2(0.0, 7.0 * (1.0 - hover_progress));
    let text_center = rect.center() + slide;
    let text_color = Color32::from_white_alpha((255.0 * hover_progress).round() as u8);
    let font = FontId::proportional(font_size);
    if rect.height() > rect.width() {
        let galley = painter.layout_no_wrap(title, font, text_color);
        let text_pos = Align2::CENTER_CENTER
            .anchor_size(text_center, galley.size())
            .min;
        painter.add(
            egui::epaint::TextShape::new(text_pos, galley, text_color)
                .with_angle_and_anchor(std::f32::consts::FRAC_PI_2, Align2::CENTER_CENTER),
        );
    } else {
        painter.text(text_center, Align2::CENTER_CENTER, title, font, text_color);
    }
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

fn elide_middle(value: &str, maximum_chars: usize) -> String {
    let count = value.chars().count();
    if count <= maximum_chars || maximum_chars < 5 {
        return value.to_owned();
    }
    let left = (maximum_chars - 1) / 2;
    let right = maximum_chars - 1 - left;
    let prefix = value.chars().take(left).collect::<String>();
    let suffix = value.chars().skip(count - right).collect::<String>();
    format!("{prefix}…{suffix}")
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
