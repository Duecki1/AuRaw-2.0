use super::super::*;

impl Preview {
    pub(in crate::ui::preview) fn handle_white_balance_picker(
        ui: &Ui,
        app: &mut CalibRawApp,
        image_rect: Rect,
        preview_rect: Rect,
        source_width: u32,
        source_height: u32,
        response: &egui::Response,
    ) {
        let lens_geometry = app
            .develop
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());
        let pointer = response
            .interact_pointer_pos()
            .filter(|position| preview_rect.contains(*position));
        let (pressed, down, released) = ui.input(|input| {
            (
                input.pointer.primary_pressed(),
                input.pointer.primary_down(),
                input.pointer.primary_released(),
            )
        });
        let pointer_uv = pointer.and_then(|position| {
            editable_source_uv(final_geometry_screen_to_native_source(
                image_rect,
                app.develop.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
                position,
            ))
        });

        if pressed {
            if let Some(uv) = pointer_uv {
                app.develop_ui.white_balance_picker_drag = Some([uv, uv]);
            }
        } else if down {
            if let (Some(area), Some(uv)) = (
                app.develop_ui.white_balance_picker_drag.as_mut(),
                pointer_uv,
            ) {
                area[1] = uv;
                ui.ctx().request_repaint();
            }
        }

        if released {
            if let Some(mut area) = app.develop_ui.white_balance_picker_drag.take() {
                if let Some(uv) = pointer_uv {
                    area[1] = uv;
                }
                app.apply_white_balance_area(area);
            }
        }
    }

    pub(in crate::ui::preview) fn paint_white_balance_picker(
        ui: &Ui,
        app: &CalibRawApp,
        image_rect: Rect,
        preview_rect: Rect,
        source_width: u32,
        source_height: u32,
    ) {
        let painter = ui.painter_at(preview_rect);
        painter.text(
            preview_rect.left_top() + egui::vec2(12.0, 12.0),
            egui::Align2::LEFT_TOP,
            "Drag over a neutral gray or white area",
            egui::FontId::proportional(13.0),
            Color32::WHITE,
        );
        let Some(area) = app.develop_ui.white_balance_picker_drag else {
            return;
        };
        let lens_geometry = app
            .develop
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.as_deref());
        let start = final_geometry_native_source_to_screen(
            image_rect,
            app.develop.geometry,
            lens_geometry,
            source_width,
            source_height,
            area[0],
        );
        let current = final_geometry_native_source_to_screen(
            image_rect,
            app.develop.geometry,
            lens_geometry,
            source_width,
            source_height,
            area[1],
        );
        let rect = Rect::from_two_pos(start, current).intersect(preview_rect);
        if rect.width() > 0.0 && rect.height() > 0.0 {
            painter.rect_filled(rect, 0.0, Color32::from_white_alpha(24));
            painter.rect_stroke(
                rect,
                0.0,
                Stroke::new(1.5, Color32::WHITE),
                egui::StrokeKind::Inside,
            );
        } else {
            painter.circle_stroke(start, 6.0, Stroke::new(1.5, Color32::WHITE));
        }
    }
}
