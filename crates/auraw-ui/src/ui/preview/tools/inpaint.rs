use super::super::*;
use crate::app::UiInpaintStroke;

impl Preview {
    pub(in crate::ui::preview) fn handle_inpaint_interaction(
        ui: &Ui,
        app: &mut AurawApp,
        _frame: &eframe::Frame,
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
        let (primary_is_down, primary_released, alt_down) = ui.input(|input| {
            (
                input.pointer.primary_down(),
                input.pointer.primary_released(),
                input.modifiers.alt,
            )
        });
        let primary_down =
            pointer.is_some() && response.is_pointer_button_down_on() && primary_is_down;

        if app.inpaint.tool.requires_source()
            && (app.inpaint.source_pick_active || (alt_down && primary_down))
        {
            if alt_down && primary_down {
                app.inpaint.source_pick_active = true;
            }
            app.inpaint.active_dab_count = 0;
            if primary_down {
                if let Some(pointer) = pointer {
                    let source_uv = final_geometry_screen_to_native_source(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        pointer,
                    );
                    if let Some(uv) = editable_source_uv(source_uv) {
                        app.inpaint.source_anchor = Some(uv);
                        ui.ctx().request_repaint();
                    }
                }
            }
            if primary_released {
                app.inpaint.source_pick_active = false;
                ui.ctx().request_repaint();
            }
            return;
        }

        if app.inpaint.tool.requires_source() && app.inpaint.source_anchor.is_none() {
            app.inpaint.source_pick_active = true;
            return;
        }

        if primary_down {
            app.inpaint.active_dab_count = app.inpaint.active_dab_count.saturating_add(1).min(8192);
            ui.ctx().request_repaint();
        } else if primary_released && app.inpaint.active_dab_count > 0 {
            app.inpaint.strokes.push(UiInpaintStroke {
                kind: app.inpaint.tool,
                dab_count: app.inpaint.active_dab_count,
            });
            app.inpaint.active_dab_count = 0;
            app.inpaint.selected_stroke = None;
            app.inpaint.hovered_stroke = None;
            ui.ctx().request_repaint();
        }
    }

    pub(in crate::ui::preview) fn paint_inpaint_overlay(
        ui: &Ui,
        app: &mut AurawApp,
        image_rect: Rect,
        preview_rect: Rect,
        source_width: u32,
        source_height: u32,
    ) {
        if app.ui.sidebar_tab != SidebarTab::Inpainting {
            return;
        }
        let Some(pointer) = ui
            .ctx()
            .pointer_hover_pos()
            .filter(|position| preview_rect.contains(*position))
        else {
            return;
        };
        let lens_geometry = app
            .develop
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.as_deref());
        let source_uv = final_geometry_screen_to_native_source(
            image_rect,
            app.develop.geometry,
            lens_geometry,
            source_width,
            source_height,
            pointer,
        );
        let Some(uv) = editable_source_uv(source_uv) else {
            return;
        };
        let dab_size = zoom_scaled_brush_size(
            app.inpaint.brush_size,
            app.preview.zoom,
            app.preferences.image_relative_brush_size,
        );
        let outline = brush_outline_geometry_screen_points(
            image_rect,
            app.develop.geometry,
            lens_geometry,
            source_width,
            source_height,
            uv,
            dab_size,
            64,
        );
        let cursor_color = if app.inpaint.source_pick_active {
            Color32::from_rgb(95, 225, 155)
        } else {
            Color32::WHITE
        };
        let painter = ui.painter_at(preview_rect);
        painter.add(Shape::line(outline, Stroke::new(1.5, cursor_color)));
        if app.inpaint.source_pick_active {
            painter.text(
                pointer + egui::vec2(10.0, 10.0),
                egui::Align2::LEFT_TOP,
                "Set source",
                egui::FontId::proportional(11.0),
                cursor_color,
            );
        }
    }
}
