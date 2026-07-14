use eframe::egui::{
    self, Align, Align2, DragValue, FontId, Layout, RichText, Sense, Stroke, StrokeKind, Ui,
};
use std::ops::RangeInclusive;

const VALUE_FIELD_WIDTH: f32 = 72.0;
const HEADER_HEIGHT: f32 = 24.0;
const SLIDER_HEIGHT: f32 = 28.0;
const TRACK_HEIGHT: f32 = 4.0;
const HANDLE_RADIUS: f32 = 7.0;
const HANDLE_TOUCH_RADIUS: f32 = 18.0;
const TRACK_DRAG_THRESHOLD: f32 = 8.0;
const HANDLE_DRAG_THRESHOLD: f32 = 2.0;
const LABEL_SIZE: f32 = 12.5;
const CONTROL_GAP: f32 = 4.0;
const ROW_BOTTOM_SPACE: f32 = 7.0;

fn slider_scroll_lock_id() -> egui::Id {
    egui::Id::new("auraw-adjustment-slider-scroll-lock")
}

/// Returns whether an adjustment slider currently owns the pointer drag.
///
/// Scroll areas use this before laying out their contents so touch scrolling,
/// wheel scrolling, and scrollbar movement are frozen until the slider is
/// released.
pub fn slider_scroll_locked(ctx: &egui::Context) -> bool {
    let pointer_down = ctx.input(|input| input.pointer.any_down());
    if !pointer_down {
        ctx.data_mut(|data| data.remove::<egui::Id>(slider_scroll_lock_id()));
        return false;
    }

    ctx.data(|data| data.get_temp::<egui::Id>(slider_scroll_lock_id()).is_some())
}

fn slider_scroll_lock_owner(ctx: &egui::Context) -> Option<egui::Id> {
    ctx.data(|data| data.get_temp::<egui::Id>(slider_scroll_lock_id()))
}

fn lock_slider_scroll(ctx: &egui::Context, slider_id: egui::Id) {
    ctx.data_mut(|data| data.insert_temp(slider_scroll_lock_id(), slider_id));
    // Claim the drag immediately once horizontal intent is established. The
    // containing ScrollArea sees a different dragged id before it applies its
    // drag delta at the end of this pass, so the view stops moving in the same
    // frame rather than one frame later.
    ctx.set_dragged_id(slider_id);
}

/// Reusable adjustment control used by the Develop sidebar and Settings.
///
/// The slider deliberately does not jump on a track tap. A value changes only
/// after a horizontal drag that starts on the handle, or after a deliberate
/// horizontal slide beginning elsewhere on the track. Vertical touch motion is
/// therefore left to the containing ScrollArea instead of changing a setting
/// while the user is trying to scroll.
pub fn adjustment_slider<Num>(
    ui: &mut Ui,
    label: &str,
    value: &mut Num,
    range: RangeInclusive<Num>,
    decimals: usize,
    speed: f64,
    hover_text: Option<&str>,
) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    let mut changed = false;

    ui.push_id(label, |ui| {
        let control_width = ui.available_width().max(VALUE_FIELD_WIDTH + 96.0);

        ui.vertical(|ui| {
            ui.set_width(control_width);
            ui.spacing_mut().item_spacing.y = CONTROL_GAP;

            ui.allocate_ui_with_layout(
                egui::vec2(control_width, HEADER_HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| {
                    let label_response = ui.label(RichText::new(label).size(LABEL_SIZE));
                    if let Some(text) = hover_text {
                        label_response.on_hover_text(text);
                    }

                    ui.allocate_ui_with_layout(
                        ui.available_size(),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            let value_response = if ui.input(|input| input.has_touch_screen()) {
                                touch_value_field(ui, (*value).to_f64(), decimals)
                            } else {
                                let response = ui.add_sized(
                                    [VALUE_FIELD_WIDTH, HEADER_HEIGHT],
                                    DragValue::new(value)
                                        .range(range.clone())
                                        .speed(speed)
                                        .fixed_decimals(decimals),
                                );
                                changed |= response.changed();
                                response
                            };
                            if let Some(text) = hover_text {
                                value_response.on_hover_text(text);
                            }
                        },
                    );
                },
            );

            changed |= guarded_slider(ui, value, range, decimals, speed, control_width, hover_text);
            ui.add_space(ROW_BOTTOM_SPACE);
        });
    });

    changed
}

fn touch_value_field(ui: &mut Ui, value: f64, decimals: usize) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(VALUE_FIELD_WIDTH, HEADER_HEIGHT),
        Sense::hover(),
    );
    let visuals = ui.style().interact(&response);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, visuals.bg_fill);
    painter.rect_stroke(rect, 3.0, visuals.bg_stroke, StrokeKind::Inside);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        format!("{value:.decimals$}"),
        FontId::monospace(12.0),
        visuals.fg_stroke.color,
    );
    response
}

fn guarded_slider<Num>(
    ui: &mut Ui,
    value: &mut Num,
    range: RangeInclusive<Num>,
    decimals: usize,
    keyboard_step: f64,
    width: f32,
    hover_text: Option<&str>,
) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    let start = (*range.start()).to_f64();
    let end = (*range.end()).to_f64();
    let span = end - start;
    let fraction = if span.abs() <= f64::EPSILON {
        0.0
    } else {
        ((value.to_f64() - start) / span).clamp(0.0, 1.0)
    } as f32;

    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, SLIDER_HEIGHT), Sense::hover());
    let track_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2((rect.width() - HANDLE_RADIUS * 2.0).max(1.0), TRACK_HEIGHT),
    );
    let handle_x = egui::lerp(track_rect.left()..=track_rect.right(), fraction);
    let handle_center = egui::pos2(handle_x, track_rect.center().y);
    let handle_hit_rect = egui::Rect::from_center_size(
        handle_center,
        egui::vec2(HANDLE_TOUCH_RADIUS * 2.0, HANDLE_TOUCH_RADIUS * 2.0),
    );

    // Both overlapping hit regions only request click sensing. In egui this
    // lets a parent ScrollArea claim a vertical drag, while we still inspect
    // the global pointer displacement to recognize an intentional horizontal
    // slider gesture.
    let track_response = ui.interact(rect, ui.id().with("guarded-track"), Sense::click());
    let handle_response = ui.interact(
        handle_hit_rect,
        ui.id().with("guarded-handle"),
        Sense::click(),
    );

    if track_response.clicked() {
        track_response.request_focus();
    }
    if handle_response.clicked() {
        handle_response.request_focus();
    }

    let mut changed = false;
    let slider_drag_id = ui.id().with("guarded-slider-drag");
    let pointer = ui.input(|input| {
        (
            input.pointer.press_origin(),
            input.pointer.interact_pos(),
            input.pointer.any_down(),
        )
    });
    let mut slider_drag_active = pointer.2
        && slider_scroll_lock_owner(ui.ctx()).is_some_and(|owner| owner == slider_drag_id);

    if let (Some(origin), Some(position), true) = pointer {
        if slider_drag_active {
            // Once a horizontal slider drag has started it keeps ownership even
            // if the finger moves vertically or leaves the original hit rect.
            // This prevents the parent ScrollArea from waking up mid-drag.
            lock_slider_scroll(ui.ctx(), slider_drag_id);
            changed |= set_from_pointer(value, start, end, decimals, track_rect, position.x);
        } else if rect.contains(origin) {
            let delta = position - origin;
            let began_on_handle = handle_hit_rect.contains(origin);
            let threshold = if began_on_handle {
                HANDLE_DRAG_THRESHOLD
            } else {
                TRACK_DRAG_THRESHOLD
            };
            let horizontal_intent =
                delta.x.abs() >= threshold && delta.x.abs() >= delta.y.abs() * 1.15;
            if horizontal_intent {
                slider_drag_active = true;
                lock_slider_scroll(ui.ctx(), slider_drag_id);
                track_response.request_focus();
                changed |= set_from_pointer(value, start, end, decimals, track_rect, position.x);
            }
        }
    }

    let focused = track_response.has_focus() || handle_response.has_focus();
    if focused {
        let (decrease, increase) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowLeft)
                    || input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::ArrowRight)
                    || input.key_pressed(egui::Key::ArrowUp),
            )
        });
        let direction = (increase as i8 - decrease as i8) as f64;
        if direction != 0.0 {
            let next = (value.to_f64() + direction * keyboard_step)
                .clamp(start.min(end), start.max(end));
            changed |= set_numeric(value, next, decimals);
        }
    }

    let active = slider_drag_active
        || handle_response.is_pointer_button_down_on()
        || (track_response.is_pointer_button_down_on()
            && pointer.0.is_some_and(|origin| rect.contains(origin)));
    let hovered = track_response.hovered() || handle_response.hovered();
    let widget_visuals = if active {
        &ui.visuals().widgets.active
    } else if hovered {
        &ui.visuals().widgets.hovered
    } else {
        &ui.visuals().widgets.inactive
    };

    let painter = ui.painter();
    painter.rect_filled(track_rect, TRACK_HEIGHT * 0.5, ui.visuals().widgets.inactive.bg_fill);
    let fill_rect = egui::Rect::from_min_max(
        track_rect.left_top(),
        egui::pos2(handle_x, track_rect.bottom()),
    );
    painter.rect_filled(fill_rect, TRACK_HEIGHT * 0.5, ui.visuals().selection.bg_fill);
    painter.circle_filled(handle_center, HANDLE_RADIUS, widget_visuals.bg_fill);
    painter.circle_stroke(
        handle_center,
        HANDLE_RADIUS,
        Stroke::new(1.0, widget_visuals.fg_stroke.color),
    );

    let combined = track_response
        .union(handle_response)
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    if let Some(text) = hover_text {
        combined.on_hover_text(text);
    }

    changed
}

fn set_from_pointer<Num>(
    value: &mut Num,
    start: f64,
    end: f64,
    decimals: usize,
    track_rect: egui::Rect,
    pointer_x: f32,
) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    let fraction =
        ((pointer_x - track_rect.left()) / track_rect.width().max(1.0)).clamp(0.0, 1.0);
    set_numeric(value, start + (end - start) * fraction as f64, decimals)
}

fn set_numeric<Num>(value: &mut Num, raw: f64, decimals: usize) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    let scale = 10_f64.powi(decimals.min(12) as i32);
    let rounded = (raw * scale).round() / scale;
    let next = Num::from_f64(rounded);
    if next == *value {
        false
    } else {
        *value = next;
        true
    }
}
