use eframe::egui::{
    self, Color32, FontId, Mesh, Pos2, Response, RichText, Sense, Shape, Stroke, StrokeKind, Ui,
};

const PICKER_MAX_WIDTH: f32 = 360.0;
const PICKER_MIN_WIDTH: f32 = 120.0;
const PICKER_SCREEN_MARGIN: f32 = 48.0;
const PICKER_VERTICAL_CHROME: f32 = 260.0;
const PICKER_MIN_PLANE_EDGE: f32 = 96.0;
const PLANE_SEGMENTS: usize = 16;
const HUE_SEGMENTS: usize = 36;

#[derive(Clone, Copy)]
struct PickerState {
    open: bool,
    hue: f32,
}

pub fn effect_color_picker(
    ui: &mut Ui,
    id_salt: impl egui::AsIdSalt,
    color: &mut [f32; 3],
    title: &str,
    tooltip: &str,
) -> bool {
    let picker_id = ui.make_persistent_id(id_salt);
    let (color_hue, mut saturation, mut value) = rgb_to_hsv(*color);
    let mut state = ui
        .data(|data| data.get_temp::<PickerState>(picker_id))
        .unwrap_or(PickerState {
            open: false,
            hue: color_hue,
        });
    if saturation > 1e-5 {
        state.hue = color_hue;
    }

    let display_color = display_color(*color);
    let button = color_swatch_button(ui, display_color, state.open).on_hover_text(tooltip);
    if button.clicked() {
        state.open = true;
    }

    let mut changed = false;
    if state.open {
        let content_size = ui.ctx().content_rect().size();
        let picker_width = picker_width(content_size.x);
        let plane_edge = picker_plane_edge(picker_width, content_size.y);
        let frame = egui::Frame::new()
            .fill(ui.visuals().window_fill)
            .inner_margin(egui::Margin::same(if cfg!(target_os = "android") {
                14
            } else {
                12
            }))
            .corner_radius(ui.visuals().window_corner_radius)
            .stroke(ui.visuals().window_stroke)
            .shadow(ui.visuals().window_shadow);

        let modal = egui::Modal::new(picker_id.with("modal"))
            .frame(frame)
            .backdrop_color(Color32::from_black_alpha(150))
            .show(ui.ctx(), |ui| {
                ui.set_width(picker_width);
                ui.set_max_width(picker_width);

                let mut close_requested = false;
                crate::ui::theme::toolbar_row(ui, |ui| {
                    ui.strong(title);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::ui::icons::phosphor_icon_button(
                            ui,
                            egui_phosphor::regular::X,
                            crate::ui::theme::toolbar_icon_size(),
                            "Close color picker",
                        )
                        .clicked()
                        {
                            close_requested = true;
                        }
                    });
                });

                ui.add_space(4.0);
                ui.label(RichText::new("Saturation / brightness").weak());
                let plane = ui
                    .vertical_centered(|ui| {
                        saturation_value_plane(
                            ui,
                            state.hue,
                            &mut saturation,
                            &mut value,
                            plane_edge,
                        )
                    })
                    .inner;
                if plane.changed() {
                    *color = hsv_to_rgb(state.hue, saturation, value);
                    changed = true;
                }

                ui.add_space(2.0);
                ui.label(RichText::new("Hue").weak());
                let hue = hue_slider(ui, &mut state.hue);
                if hue.changed() {
                    *color = hsv_to_rgb(state.hue, saturation, value);
                    changed = true;
                }

                ui.add_space(4.0);
                selected_color_row(ui, *color);

                ui.add_space(2.0);
                if ui
                    .add_sized(
                        [ui.available_width(), crate::ui::theme::CONTROL_HEIGHT],
                        egui::Button::new("Done"),
                    )
                    .clicked()
                {
                    close_requested = true;
                }

                close_requested
            });

        if modal.inner || modal.should_close() {
            state.open = false;
        }
    }

    ui.data_mut(|data| data.insert_temp(picker_id, state));
    changed
}

fn color_swatch_button(ui: &mut Ui, color: Color32, open: bool) -> Response {
    let desired_size = egui::vec2(92.0, crate::ui::theme::CONTROL_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(desired_size, Sense::click());
    let visuals = if open {
        &ui.visuals().widgets.open
    } else {
        ui.style().interact(&response)
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, visuals.corner_radius, color);
    painter.rect_stroke(
        rect,
        visuals.corner_radius,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        color_hex(color),
        FontId::monospace(12.0),
        contrast_text_color(color),
    );
    response
}

fn saturation_value_plane(
    ui: &mut Ui,
    hue: f32,
    saturation: &mut f32,
    value: &mut f32,
    edge: f32,
) -> Response {
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(edge, edge), Sense::click_and_drag());

    if pointer_is_operating(&response) {
        if let Some(pointer) = response.interact_pointer_pos() {
            let next_saturation = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let next_value = (1.0 - (pointer.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
            if (*saturation - next_saturation).abs() > f32::EPSILON
                || (*value - next_value).abs() > f32::EPSILON
            {
                *saturation = next_saturation;
                *value = next_value;
                response.mark_changed();
            }
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 5.0, ui.visuals().extreme_bg_color);
    painter.add(Shape::mesh(saturation_value_mesh(rect, hue)));
    painter.rect_stroke(
        rect,
        5.0,
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        StrokeKind::Inside,
    );

    let marker = egui::pos2(
        egui::lerp(rect.left()..=rect.right(), *saturation),
        egui::lerp(rect.bottom()..=rect.top(), *value),
    );
    let marker_color = hsv_display_color(hue, *saturation, *value);
    painter.circle_filled(marker, 7.0, marker_color);
    painter.circle_stroke(
        marker,
        8.5,
        Stroke::new(2.0, contrast_text_color(marker_color)),
    );
    response.on_hover_text("Drag to choose saturation and brightness")
}

fn hue_slider(ui: &mut Ui, hue: &mut f32) -> Response {
    let desired_size = egui::vec2(
        ui.available_width().max(1.0),
        crate::ui::theme::CONTROL_HEIGHT,
    );
    let (touch_rect, mut response) = ui.allocate_exact_size(desired_size, Sense::click_and_drag());

    if pointer_is_operating(&response) {
        if let Some(pointer) = response.interact_pointer_pos() {
            let next_hue = ((pointer.x - touch_rect.left()) / touch_rect.width()).clamp(0.0, 1.0);
            if (*hue - next_hue).abs() > f32::EPSILON {
                *hue = next_hue;
                response.mark_changed();
            }
        }
    }

    let vertical_inset = ((touch_rect.height() - 20.0) * 0.5).max(0.0);
    let bar_rect = touch_rect.shrink2(egui::vec2(0.0, vertical_inset));
    let painter = ui.painter_at(touch_rect);
    painter.rect_filled(bar_rect, 5.0, ui.visuals().extreme_bg_color);
    painter.add(Shape::mesh(hue_mesh(bar_rect)));
    painter.rect_stroke(
        bar_rect,
        5.0,
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        StrokeKind::Inside,
    );

    let marker_x = egui::lerp(bar_rect.left()..=bar_rect.right(), *hue);
    let marker_rect = egui::Rect::from_center_size(
        egui::pos2(marker_x, bar_rect.center().y),
        egui::vec2(8.0, bar_rect.height() + 6.0),
    );
    painter.rect_filled(marker_rect, 4.0, hsv_display_color(*hue, 1.0, 1.0));
    painter.rect_stroke(
        marker_rect,
        4.0,
        Stroke::new(2.0, Color32::WHITE),
        StrokeKind::Inside,
    );
    response.on_hover_text("Drag to choose hue")
}

fn selected_color_row(ui: &mut Ui, color: [f32; 3]) {
    let display = display_color(color);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(
            ui.available_width().max(1.0),
            crate::ui::theme::CONTROL_HEIGHT,
        ),
        Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 5.0, display);
    painter.rect_stroke(
        rect,
        5.0,
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        color_hex(display),
        FontId::monospace(12.0),
        contrast_text_color(display),
    );
}

fn pointer_is_operating(response: &Response) -> bool {
    response.clicked() || response.dragged() || response.is_pointer_button_down_on()
}

fn saturation_value_mesh(rect: egui::Rect, hue: f32) -> Mesh {
    let mut mesh = Mesh::default();
    for row in 0..=PLANE_SEGMENTS {
        let value = 1.0 - row as f32 / PLANE_SEGMENTS as f32;
        for column in 0..=PLANE_SEGMENTS {
            let saturation = column as f32 / PLANE_SEGMENTS as f32;
            mesh.vertices.push(egui::epaint::Vertex {
                pos: Pos2::new(
                    egui::lerp(rect.left()..=rect.right(), saturation),
                    egui::lerp(
                        rect.top()..=rect.bottom(),
                        row as f32 / PLANE_SEGMENTS as f32,
                    ),
                ),
                uv: Pos2::ZERO,
                color: hsv_display_color(hue, saturation, value),
            });
        }
    }

    let stride = PLANE_SEGMENTS + 1;
    for row in 0..PLANE_SEGMENTS {
        for column in 0..PLANE_SEGMENTS {
            let a = (row * stride + column) as u32;
            let b = a + 1;
            let c = ((row + 1) * stride + column) as u32;
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    mesh
}

fn hue_mesh(rect: egui::Rect) -> Mesh {
    let mut mesh = Mesh::default();
    for segment in 0..=HUE_SEGMENTS {
        let hue = segment as f32 / HUE_SEGMENTS as f32;
        let x = egui::lerp(rect.left()..=rect.right(), hue);
        let color = hsv_display_color(hue, 1.0, 1.0);
        mesh.colored_vertex(Pos2::new(x, rect.top()), color);
        mesh.colored_vertex(Pos2::new(x, rect.bottom()), color);
        if segment < HUE_SEGMENTS {
            let index = (segment * 2) as u32;
            mesh.indices.extend_from_slice(&[
                index,
                index + 1,
                index + 2,
                index + 1,
                index + 3,
                index + 2,
            ]);
        }
    }
    mesh
}

fn rgb_to_hsv(rgb: [f32; 3]) -> (f32, f32, f32) {
    egui::ecolor::hsv_from_rgb([
        rgb[0].clamp(0.0, 1.0),
        rgb[1].clamp(0.0, 1.0),
        rgb[2].clamp(0.0, 1.0),
    ])
}

fn hsv_to_rgb(hue: f32, saturation: f32, value: f32) -> [f32; 3] {
    egui::ecolor::rgb_from_hsv((
        hue.rem_euclid(1.0),
        saturation.clamp(0.0, 1.0),
        value.clamp(0.0, 1.0),
    ))
}

fn hsv_display_color(hue: f32, saturation: f32, value: f32) -> Color32 {
    display_color(hsv_to_rgb(hue, saturation, value))
}

fn display_color(rgb: [f32; 3]) -> Color32 {
    Color32::from(egui::Rgba::from_rgb(
        rgb[0].clamp(0.0, 1.0),
        rgb[1].clamp(0.0, 1.0),
        rgb[2].clamp(0.0, 1.0),
    ))
}

fn color_hex(color: Color32) -> String {
    format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b())
}

fn contrast_text_color(color: Color32) -> Color32 {
    let luminance = 0.2126 * f32::from(color.r())
        + 0.7152 * f32::from(color.g())
        + 0.0722 * f32::from(color.b());
    if luminance > 150.0 {
        Color32::BLACK
    } else {
        Color32::WHITE
    }
}

fn picker_width(screen_width: f32) -> f32 {
    (screen_width - PICKER_SCREEN_MARGIN).clamp(PICKER_MIN_WIDTH, PICKER_MAX_WIDTH)
}

fn picker_plane_edge(picker_width: f32, screen_height: f32) -> f32 {
    picker_width
        .min(screen_height - PICKER_VERTICAL_CHROME)
        .max(PICKER_MIN_PLANE_EDGE)
}

#[cfg(test)]
mod tests {
    use super::{hsv_to_rgb, picker_plane_edge, picker_width, rgb_to_hsv};

    #[test]
    fn rgb_and_hsv_round_trip_for_effect_colors() {
        for original in [
            [1.0, 0.0, 0.0],
            [0.12, 0.73, 0.41],
            [0.5, 0.5, 0.5],
            [0.03, 0.05, 0.92],
        ] {
            let (hue, saturation, value) = rgb_to_hsv(original);
            let round_trip = hsv_to_rgb(hue, saturation, value);
            for channel in 0..3 {
                assert!((round_trip[channel] - original[channel]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn picker_geometry_is_bounded_for_desktop_and_compact_screens() {
        assert_eq!(picker_width(1200.0), 360.0);
        assert_eq!(picker_width(360.0), 312.0);
        assert_eq!(picker_width(100.0), 120.0);
        assert_eq!(picker_plane_edge(360.0, 800.0), 360.0);
        assert_eq!(picker_plane_edge(312.0, 420.0), 160.0);
        assert_eq!(picker_plane_edge(120.0, 200.0), 96.0);
    }
}
