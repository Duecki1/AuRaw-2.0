use eframe::egui::{self, Align, Align2, DragValue, FontId, Layout, RichText, Sense, Stroke, Ui};
use std::ops::RangeInclusive;

#[cfg(not(target_os = "android"))]
const VALUE_FIELD_WIDTH: f32 = 72.0;
#[cfg(target_os = "android")]
const VALUE_FIELD_WIDTH: f32 = 60.0;
const HEADER_HEIGHT: f32 = crate::ui::theme::CONTROL_HEIGHT;
#[cfg(not(target_os = "android"))]
const SLIDER_HEIGHT: f32 = 28.0;
#[cfg(target_os = "android")]
const SLIDER_HEIGHT: f32 = 24.0;
const TRACK_HEIGHT: f32 = 4.0;
#[cfg(not(target_os = "android"))]
const HANDLE_RADIUS: f32 = 7.0;
#[cfg(target_os = "android")]
const HANDLE_RADIUS: f32 = 8.0;
const HANDLE_TOUCH_RADIUS: f32 = 18.0;
const TRACK_DRAG_THRESHOLD: f32 = 8.0;
const HANDLE_DRAG_THRESHOLD: f32 = 2.0;
#[cfg(not(target_os = "android"))]
const CONTROL_GAP: f32 = 4.0;
#[cfg(target_os = "android")]
const CONTROL_GAP: f32 = 2.0;
#[cfg(not(target_os = "android"))]
const ROW_BOTTOM_SPACE: f32 = 7.0;
#[cfg(target_os = "android")]
const ROW_BOTTOM_SPACE: f32 = 3.0;
const COMPACT_ROW_GAP: f32 = 4.0;
const COMPACT_ROW_BOTTOM_SPACE: f32 = 1.0;
const COMPACT_LABEL_MIN_WIDTH: f32 = 76.0;
const COMPACT_LABEL_MAX_WIDTH: f32 = 104.0;
const COMPACT_TRACK_MIN_WIDTH: f32 = 64.0;

#[derive(Clone, Copy, Debug)]
pub(crate) enum SliderGradient {
    HueDegrees {
        start: f32,
        end: f32,
    },
    ChannelHue {
        left: egui::Color32,
        center: egui::Color32,
        right: egui::Color32,
    },
    Brightness,
    Temperature,
    Tint,
    CameraTint {
        neutral_fraction: f32,
    },
    Colorfulness,
    Saturation(egui::Color32),
    Luminance(egui::Color32),
}

#[derive(Clone, Copy)]
struct SliderOptions<'a> {
    decimals: usize,
    speed: f64,
    hover_text: Option<&'a str>,
    explicit_reset_value: Option<f64>,
    accent: Option<egui::Color32>,
    gradient: Option<SliderGradient>,
}

fn slider_scroll_lock_id() -> egui::Id {
    egui::Id::new("auraw-adjustment-slider-scroll-lock")
}

pub(crate) fn slider_scroll_locked(ctx: &egui::Context) -> bool {
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
    ctx.set_dragged_id(slider_id);
}

pub(crate) fn adjustment_slider<Num>(
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
    adjustment_slider_impl(
        ui,
        label,
        value,
        range,
        SliderOptions {
            decimals,
            speed,
            hover_text,
            explicit_reset_value: None,
            accent: None,
            gradient: None,
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn gradient_adjustment_slider<Num>(
    ui: &mut Ui,
    label: &str,
    value: &mut Num,
    range: RangeInclusive<Num>,
    decimals: usize,
    speed: f64,
    hover_text: Option<&str>,
    gradient: SliderGradient,
) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    adjustment_slider_impl(
        ui,
        label,
        value,
        range,
        SliderOptions {
            decimals,
            speed,
            hover_text,
            explicit_reset_value: None,
            accent: None,
            gradient: Some(gradient),
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn accented_gradient_adjustment_slider<Num>(
    ui: &mut Ui,
    label: &str,
    value: &mut Num,
    range: RangeInclusive<Num>,
    decimals: usize,
    speed: f64,
    hover_text: Option<&str>,
    accent: egui::Color32,
    gradient: SliderGradient,
) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    adjustment_slider_impl(
        ui,
        label,
        value,
        range,
        SliderOptions {
            decimals,
            speed,
            hover_text,
            explicit_reset_value: None,
            accent: Some(accent),
            gradient: Some(gradient),
        },
    )
}

pub(crate) fn hue_adjustment_slider(
    ui: &mut Ui,
    value: &mut f32,
    hover_text: Option<&str>,
) -> bool {
    let spec = crate::pipeline::effect_params::adjustment::HUE;
    adjustment_slider_impl(
        ui,
        spec.label,
        value,
        spec.range(),
        SliderOptions {
            decimals: spec.decimals,
            speed: spec.step,
            hover_text,
            explicit_reset_value: Some(f64::from(spec.default)),
            accent: None,
            gradient: Some(SliderGradient::HueDegrees {
                start: spec.min,
                end: spec.max,
            }),
        },
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn adjustment_slider_with_reset<Num>(
    ui: &mut Ui,
    label: &str,
    value: &mut Num,
    range: RangeInclusive<Num>,
    decimals: usize,
    speed: f64,
    hover_text: Option<&str>,
    reset_value: Num,
) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    adjustment_slider_impl(
        ui,
        label,
        value,
        range,
        SliderOptions {
            decimals,
            speed,
            hover_text,
            explicit_reset_value: Some(reset_value.to_f64()),
            accent: None,
            gradient: None,
        },
    )
}

fn adjustment_slider_impl<Num>(
    ui: &mut Ui,
    label: &str,
    value: &mut Num,
    range: RangeInclusive<Num>,
    options: SliderOptions<'_>,
) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    let SliderOptions {
        decimals,
        speed,
        hover_text,
        explicit_reset_value,
        accent,
        gradient: _,
    } = options;
    let mut changed = false;

    ui.push_id(label, |ui| {
        let reset_id = ui.id().with("reset-value");
        let reset_value = explicit_reset_value.unwrap_or_else(|| {
            ui.data_mut(|data| {
                if let Some(value) = data.get_temp::<f64>(reset_id) {
                    value
                } else {
                    let value = (*value).to_f64();
                    data.insert_temp(reset_id, value);
                    value
                }
            })
        });
        let control_width = ui.available_width().max(1.0);

        ui.vertical(|ui| {
            ui.set_width(control_width);
            ui.spacing_mut().item_spacing.y = CONTROL_GAP;

            if crate::ui::theme::is_compact_portrait(ui) {
                let (label_width, track_width) = compact_slider_widths(control_width);
                ui.allocate_ui_with_layout(
                    egui::vec2(control_width, HEADER_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = COMPACT_ROW_GAP;
                        let label_tooltip = format!("{label}\n{}", reset_tooltip(hover_text));
                        let label_response = compact_slider_label(ui, label, accent, label_width)
                            .on_hover_text(label_tooltip);
                        if label_response.double_clicked() {
                            changed |= set_numeric(value, reset_value, decimals);
                        }

                        changed |= guarded_slider(
                            ui,
                            value,
                            range.clone(),
                            track_width,
                            reset_value,
                            options,
                        );

                        let mut value_response = if ui.input(|input| input.has_touch_screen()) {
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
                        value_response = value_response.on_hover_text(reset_tooltip(hover_text));
                        if value_response.double_clicked() {
                            changed |= set_numeric(value, reset_value, decimals);
                        }
                    },
                );
                ui.add_space(COMPACT_ROW_BOTTOM_SPACE);
            } else {
                ui.allocate_ui_with_layout(
                    egui::vec2(control_width, HEADER_HEIGHT),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        let mut label_response = if let Some(accent) = accent {
                            let (swatch_rect, swatch_response) =
                                ui.allocate_exact_size(egui::vec2(9.0, 9.0), Sense::hover());
                            ui.painter()
                                .circle_filled(swatch_rect.center(), 4.5, accent);
                            ui.painter().circle_stroke(
                                swatch_rect.center(),
                                4.5,
                                Stroke::new(1.0, egui::Color32::from_white_alpha(90)),
                            );
                            swatch_response.union(ui.label(RichText::new(label)))
                        } else {
                            ui.label(label)
                        };
                        label_response = label_response.on_hover_text(reset_tooltip(hover_text));
                        if label_response.double_clicked() {
                            changed |= set_numeric(value, reset_value, decimals);
                        }

                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let mut value_response = if ui.input(|input| input.has_touch_screen()) {
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
                            value_response =
                                value_response.on_hover_text(reset_tooltip(hover_text));
                            if value_response.double_clicked() {
                                changed |= set_numeric(value, reset_value, decimals);
                            }
                        });
                    },
                );

                changed |= guarded_slider(ui, value, range, control_width, reset_value, options);
                ui.add_space(ROW_BOTTOM_SPACE);
            }
        });
    });

    changed
}

fn compact_slider_widths(control_width: f32) -> (f32, f32) {
    let usable_width = (control_width - VALUE_FIELD_WIDTH - COMPACT_ROW_GAP * 2.0).max(2.0);
    let label_width = (control_width * 0.27)
        .clamp(COMPACT_LABEL_MIN_WIDTH, COMPACT_LABEL_MAX_WIDTH)
        .min((usable_width - COMPACT_TRACK_MIN_WIDTH).max(1.0));
    (label_width, (usable_width - label_width).max(1.0))
}

fn compact_slider_label(
    ui: &mut Ui,
    label: &str,
    accent: Option<egui::Color32>,
    width: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, HEADER_HEIGHT), Sense::click());
    let painter = ui.painter_at(rect);
    let mut text_x = rect.left();
    if let Some(accent) = accent {
        let center = egui::pos2(rect.left() + 5.0, rect.center().y);
        painter.circle_filled(center, 4.5, accent);
        painter.circle_stroke(
            center,
            4.5,
            Stroke::new(1.0, egui::Color32::from_white_alpha(90)),
        );
        text_x += 15.0;
    }
    painter.text(
        egui::pos2(text_x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        egui::TextStyle::Body.resolve(ui.style()),
        ui.visuals().text_color(),
    );
    response
}

fn touch_value_field(ui: &mut Ui, value: f64, decimals: usize) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(VALUE_FIELD_WIDTH, HEADER_HEIGHT), Sense::click());
    let visuals = ui.style().interact(&response);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, visuals.bg_fill);
    painter.rect_stroke(rect, 3.0, visuals.bg_stroke, egui::StrokeKind::Inside);
    let formatted = format!("{value:.decimals$}");
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        formatted,
        FontId::monospace(13.0),
        visuals.fg_stroke.color,
    );
    response
}

fn guarded_slider<Num>(
    ui: &mut Ui,
    value: &mut Num,
    range: RangeInclusive<Num>,
    width: f32,
    reset_value: f64,
    options: SliderOptions<'_>,
) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    let SliderOptions {
        decimals,
        speed: keyboard_step,
        hover_text,
        explicit_reset_value: _,
        accent,
        gradient,
    } = options;
    let start = (*range.start()).to_f64();
    let end = (*range.end()).to_f64();
    let span = end - start;
    let fraction = if span.abs() <= f64::EPSILON {
        0.0
    } else {
        ((value.to_f64() - start) / span).clamp(0.0, 1.0)
    } as f32;

    let slider_height = if crate::ui::theme::is_compact_portrait(ui) {
        HEADER_HEIGHT
    } else {
        SLIDER_HEIGHT
    };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, slider_height), Sense::hover());
    let track_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2((rect.width() - HANDLE_RADIUS * 2.0).max(1.0), TRACK_HEIGHT),
    );
    let handle_x = egui::lerp(track_rect.left()..=track_rect.right(), fraction);
    let handle_center = egui::pos2(handle_x, track_rect.center().y);
    let handle_hit_rect = egui::Rect::from_center_size(
        handle_center,
        egui::vec2(HANDLE_TOUCH_RADIUS * 2.0, HANDLE_TOUCH_RADIUS * 2.0),
    )
    .intersect(rect);

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
    let reset_requested = track_response.double_clicked() || handle_response.double_clicked();
    if reset_requested {
        changed |= set_numeric(value, reset_value, decimals);
    }
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
    if !reset_requested {
        if let (Some(origin), Some(position), true) = pointer {
            if slider_drag_active {
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
                    changed |=
                        set_from_pointer(value, start, end, decimals, track_rect, position.x);
                }
            }
        }
    }

    let focused = track_response.has_focus() || handle_response.has_focus();
    if focused {
        let (decrease, increase) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::ArrowLeft) || input.key_pressed(egui::Key::ArrowDown),
                input.key_pressed(egui::Key::ArrowRight) || input.key_pressed(egui::Key::ArrowUp),
            )
        });
        let direction = (increase as i8 - decrease as i8) as f64;
        if direction != 0.0 {
            let next =
                (value.to_f64() + direction * keyboard_step).clamp(start.min(end), start.max(end));
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
    let gradient_color = gradient.map(|gradient| gradient_color_at(gradient, fraction));
    let visual_accent = gradient_color.or(accent);
    if let Some(gradient) = gradient {
        paint_gradient_track(painter, track_rect, gradient);
    } else {
        painter.rect_filled(
            track_rect,
            TRACK_HEIGHT * 0.5,
            ui.visuals().widgets.inactive.bg_fill,
        );
    }
    let bipolar = start < 0.0 && end > 0.0;
    let fill_origin = if bipolar {
        let zero_fraction = ((-start) / span).clamp(0.0, 1.0) as f32;
        egui::lerp(track_rect.left()..=track_rect.right(), zero_fraction)
    } else {
        track_rect.left()
    };
    let fill_left = fill_origin.min(handle_x);
    let fill_right = fill_origin.max(handle_x);
    if gradient.is_none() && fill_right - fill_left > 0.25 {
        let fill_rect = egui::Rect::from_min_max(
            egui::pos2(fill_left, track_rect.top()),
            egui::pos2(fill_right, track_rect.bottom()),
        );
        painter.rect_filled(
            fill_rect,
            TRACK_HEIGHT * 0.5,
            visual_accent.unwrap_or(ui.visuals().selection.bg_fill),
        );
    }
    if bipolar {
        painter.vline(
            fill_origin,
            (track_rect.center().y - 5.0)..=(track_rect.center().y + 5.0),
            Stroke::new(1.0, egui::Color32::from_white_alpha(75)),
        );
    }
    painter.circle_filled(
        handle_center,
        HANDLE_RADIUS,
        visual_accent.unwrap_or(widget_visuals.bg_fill),
    );
    painter.circle_stroke(
        handle_center,
        HANDLE_RADIUS,
        Stroke::new(1.0, widget_visuals.fg_stroke.color),
    );

    let combined = track_response
        .union(handle_response)
        .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    combined.on_hover_text(reset_tooltip(hover_text));

    changed
}

fn reset_tooltip(hover_text: Option<&str>) -> String {
    match hover_text {
        Some(text) => format!("{text}\nDouble-click to reset."),
        None => "Double-click to reset.".to_owned(),
    }
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
    let fraction = ((pointer_x - track_rect.left()) / track_rect.width().max(1.0)).clamp(0.0, 1.0);
    set_numeric(value, start + (end - start) * fraction as f64, decimals)
}

fn paint_gradient_track(painter: &egui::Painter, rect: egui::Rect, gradient: SliderGradient) {
    const SEGMENTS: usize = 64;
    for segment in 0..SEGMENTS {
        let left_fraction = segment as f32 / SEGMENTS as f32;
        let right_fraction = (segment + 1) as f32 / SEGMENTS as f32;
        let left = egui::lerp(rect.left()..=rect.right(), left_fraction);
        let right = egui::lerp(rect.left()..=rect.right(), right_fraction);
        let color = gradient_color_at(gradient, (left_fraction + right_fraction) * 0.5);
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(left - 0.25, rect.top()),
                egui::pos2(right + 0.25, rect.bottom()),
            ),
            0.0,
            color,
        );
    }
    painter.rect_stroke(
        rect,
        TRACK_HEIGHT * 0.5,
        Stroke::new(0.75, egui::Color32::from_black_alpha(100)),
        egui::StrokeKind::Inside,
    );
}

fn gradient_color_at(gradient: SliderGradient, fraction: f32) -> egui::Color32 {
    let t = fraction.clamp(0.0, 1.0);
    match gradient {
        SliderGradient::HueDegrees { start, end } => {
            hsv_color(egui::lerp(start..=end, t), 0.90, 0.92)
        }
        SliderGradient::ChannelHue {
            left,
            center,
            right,
        } => {
            if t <= 0.5 {
                lerp_hue_color(left, center, t * 2.0)
            } else {
                lerp_hue_color(center, right, (t - 0.5) * 2.0)
            }
        }
        SliderGradient::Brightness => {
            if t <= 0.5 {
                lerp_color(
                    crate::ui::theme::BRIGHTNESS_SHADOW,
                    crate::ui::theme::BRIGHTNESS_MID,
                    t * 2.0,
                )
            } else {
                lerp_color(
                    crate::ui::theme::BRIGHTNESS_MID,
                    crate::ui::theme::BRIGHTNESS_HIGHLIGHT,
                    (t - 0.5) * 2.0,
                )
            }
        }
        SliderGradient::Temperature => {
            if t <= 0.5 {
                lerp_color(
                    crate::ui::theme::TEMPERATURE_COOL,
                    crate::ui::theme::TEMPERATURE_NEUTRAL,
                    t * 2.0,
                )
            } else {
                lerp_color(
                    crate::ui::theme::TEMPERATURE_NEUTRAL,
                    crate::ui::theme::TEMPERATURE_WARM,
                    (t - 0.5) * 2.0,
                )
            }
        }
        SliderGradient::Tint => {
            if t <= 0.5 {
                lerp_color(
                    crate::ui::theme::TINT_GREEN,
                    crate::ui::theme::TINT_NEUTRAL,
                    t * 2.0,
                )
            } else {
                lerp_color(
                    crate::ui::theme::TINT_NEUTRAL,
                    crate::ui::theme::TINT_MAGENTA,
                    (t - 0.5) * 2.0,
                )
            }
        }
        SliderGradient::CameraTint { neutral_fraction } => {
            let neutral = neutral_fraction.clamp(0.0, 1.0);
            if t <= neutral {
                let u = if neutral <= f32::EPSILON {
                    1.0
                } else {
                    t / neutral
                };
                lerp_color(
                    crate::ui::theme::TINT_MAGENTA,
                    crate::ui::theme::TINT_NEUTRAL,
                    u,
                )
            } else {
                let span = 1.0 - neutral;
                let u = if span <= f32::EPSILON {
                    1.0
                } else {
                    (t - neutral) / span
                };
                lerp_color(
                    crate::ui::theme::TINT_NEUTRAL,
                    crate::ui::theme::TINT_GREEN,
                    u,
                )
            }
        }
        SliderGradient::Colorfulness => {
            if t <= 0.5 {
                lerp_color(
                    crate::ui::theme::COLORFULNESS_SHADOW,
                    crate::ui::theme::COLORFULNESS_MID,
                    t * 2.0,
                )
            } else {
                let u = (t - 0.5) * 2.0;
                let hue = u * 360.0;
                let saturation = egui::lerp(0.0..=0.94, u.sqrt());
                let value = egui::lerp(0.70..=0.92, u);
                hsv_color(hue, saturation, value)
            }
        }
        SliderGradient::Saturation(color) => {
            let (hue, saturation, value) = rgb_to_hsv(color);
            let target_saturation = if t <= 0.5 {
                egui::lerp(0.02..=saturation.max(0.35), t * 2.0)
            } else {
                egui::lerp(saturation.max(0.35)..=1.0, (t - 0.5) * 2.0)
            };
            hsv_color(hue, target_saturation, value.max(0.78))
        }
        SliderGradient::Luminance(color) => {
            if t <= 0.5 {
                lerp_color(crate::ui::theme::LUMINANCE_BLACK, color, t * 2.0)
            } else {
                lerp_color(color, crate::ui::theme::LUMINANCE_WHITE, (t - 0.5) * 2.0)
            }
        }
    }
}

fn lerp_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    egui::Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

fn hsv_color(hue_degrees: f32, saturation: f32, value: f32) -> egui::Color32 {
    let hue = hue_degrees.rem_euclid(360.0) / 60.0;
    let sector = hue.floor() as u32;
    let blend = hue - sector as f32;
    let saturation = saturation.clamp(0.0, 1.0);
    let value = value.clamp(0.0, 1.0);
    let low = value * (1.0 - saturation);
    let rise = low + (value - low) * blend;
    let fall = value - (value - low) * blend;
    let (red, green, blue) = match sector % 6 {
        0 => (value, rise, low),
        1 => (fall, value, low),
        2 => (low, value, rise),
        3 => (low, fall, value),
        4 => (rise, low, value),
        _ => (value, low, fall),
    };
    egui::Color32::from_rgb(
        (red * 255.0).round() as u8,
        (green * 255.0).round() as u8,
        (blue * 255.0).round() as u8,
    )
}

fn lerp_hue_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let (hue_a, saturation_a, value_a) = rgb_to_hsv(a);
    let (hue_b, saturation_b, value_b) = rgb_to_hsv(b);
    let delta = (hue_b - hue_a + 180.0).rem_euclid(360.0) - 180.0;
    hsv_color(
        hue_a + delta * t.clamp(0.0, 1.0),
        egui::lerp(saturation_a..=saturation_b, t.clamp(0.0, 1.0)),
        egui::lerp(value_a..=value_b, t.clamp(0.0, 1.0)),
    )
}

fn rgb_to_hsv(color: egui::Color32) -> (f32, f32, f32) {
    let red = color.r() as f32 / 255.0;
    let green = color.g() as f32 / 255.0;
    let blue = color.b() as f32 / 255.0;
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let delta = max - min;
    let hue = if delta <= f32::EPSILON {
        0.0
    } else if max == red {
        60.0 * ((green - blue) / delta).rem_euclid(6.0)
    } else if max == green {
        60.0 * ((blue - red) / delta + 2.0)
    } else {
        60.0 * ((red - green) / delta + 4.0)
    };
    let saturation = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    (hue, saturation, max)
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

#[cfg(test)]
mod tests {
    use super::{
        compact_slider_widths, gradient_color_at, SliderGradient, COMPACT_ROW_GAP, HEADER_HEIGHT,
        VALUE_FIELD_WIDTH,
    };

    #[test]
    fn slider_header_reserves_the_full_themed_control_height() {
        assert_eq!(HEADER_HEIGHT, crate::ui::theme::CONTROL_HEIGHT);
    }

    #[test]
    fn compact_slider_row_uses_the_available_width() {
        let width = 360.0;
        let (label, track) = compact_slider_widths(width);
        assert!((label + track + VALUE_FIELD_WIDTH + COMPACT_ROW_GAP * 2.0 - width).abs() < 0.001);
        assert!(track >= 64.0);
    }

    #[test]
    fn hue_gradient_wraps_to_the_same_color_at_a_full_turn() {
        let gradient = SliderGradient::HueDegrees {
            start: 0.0,
            end: 360.0,
        };
        assert_eq!(
            gradient_color_at(gradient, 0.0),
            gradient_color_at(gradient, 1.0)
        );
    }

    #[test]
    fn brightness_gradient_is_monotonic_in_luma() {
        let gradient = SliderGradient::Brightness;
        let low = gradient_color_at(gradient, 0.0);
        let mid = gradient_color_at(gradient, 0.5);
        let high = gradient_color_at(gradient, 1.0);
        assert!(low.r() < mid.r() && mid.r() < high.r());
    }

    #[test]
    fn camera_tint_gradient_reverses_local_tint_and_uses_requested_neutral() {
        let neutral_fraction = 0.4;
        let camera = SliderGradient::CameraTint { neutral_fraction };
        let local = SliderGradient::Tint;

        let camera_low = gradient_color_at(camera, 0.0);
        let camera_neutral = gradient_color_at(camera, neutral_fraction);
        let camera_high = gradient_color_at(camera, 1.0);
        let local_low = gradient_color_at(local, 0.0);
        let local_high = gradient_color_at(local, 1.0);

        assert_eq!(camera_low, local_high);
        assert_eq!(camera_high, local_low);
        assert_eq!(camera_neutral, eframe::egui::Color32::from_gray(202));
    }
}
