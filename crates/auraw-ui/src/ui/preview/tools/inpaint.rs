use super::brush::{sample_brush_stroke, STANDARD_BRUSH_MINIMUM_SPACING_FRACTION};
use super::super::*;

impl Preview {
    pub(in crate::ui::preview) fn handle_inpaint_interaction(
        ui: &Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
        image_rect: Rect,
        preview_rect: Rect,
        source_width: u32,
        source_height: u32,
        response: &egui::Response,
    ) {
        if app.inpaint.processing() {
            return;
        }
        let lens_geometry = app
            .develop
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());
        let pointer = response
            .interact_pointer_pos()
            .filter(|position| preview_rect.contains(*position));
        let (primary_is_down, primary_released) = ui.input(|input| {
            (
                input.pointer.primary_down(),
                input.pointer.primary_released(),
            )
        });
        let primary_down =
            pointer.is_some() && response.is_pointer_button_down_on() && primary_is_down;

        if primary_down {
            let Some(pointer) = pointer else {
                return;
            };
            let Some(stroke) = sample_brush_stroke(
                image_rect,
                app.develop.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
                pointer,
                app.inpaint.brush_size,
                app.preview.zoom,
                app.preferences.image_relative_brush_size,
                &mut app.inpaint.last_brush_uv,
                STANDARD_BRUSH_MINIMUM_SPACING_FRACTION,
            ) else {
                return;
            };
            let radius_native =
                stroke.dab_size * source_width.min(source_height).max(1) as f32;
            let mut changed = false;
            for &sample_uv in &stroke.samples {
                if app.inpaint.active_points.len()
                    >= crate::pipeline::REMOVE_MAX_POINTS_PER_STROKE
                {
                    break;
                }
                app.inpaint.active_points.push(crate::pipeline::RemoveBrushPoint {
                    x: sample_uv[0] * source_width as f32,
                    y: sample_uv[1] * source_height as f32,
                    radius: radius_native,
                });
                changed = true;
            }
            if changed {
                app.inpaint.last_brush_uv = Some(stroke.uv);
                ui.ctx().request_repaint();
            }
            return;
        }

        if primary_is_down {
            app.inpaint.last_brush_uv = None;
            return;
        }

        if primary_released && !app.inpaint.active_points.is_empty() {
            let points = std::mem::take(&mut app.inpaint.active_points);
            app.inpaint.last_brush_uv = None;
            app.start_remove_worker(
                frame,
                crate::pipeline::RemoveBrushStroke {
                    points,
                    dilation_radius: 0,
                },
            );
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
        let lens_geometry = app
            .develop
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.as_deref());
        let painter = ui.painter_at(preview_rect);

        let highlighted = app
            .inpaint
            .hovered_stroke
            .or(app.inpaint.selected_stroke)
            .and_then(|index| app.inpaint.edits.strokes.get(index));
        if let Some(stroke) = highlighted {
            paint_remove_brush_geometry(
                &painter,
                image_rect,
                app.develop.geometry,
                lens_geometry,
                source_width,
                source_height,
                &stroke.brush.points,
                crate::ui::theme::inpaint_stroke_highlight(),
            );
        }
        if let Some(stroke) = app.inpaint.pending_brush.as_ref() {
            paint_remove_brush_geometry(
                &painter,
                image_rect,
                app.develop.geometry,
                lens_geometry,
                source_width,
                source_height,
                &stroke.points,
                crate::ui::theme::inpaint_stroke_active(),
            );
        }
        if !app.inpaint.active_points.is_empty() {
            paint_remove_brush_geometry(
                &painter,
                image_rect,
                app.develop.geometry,
                lens_geometry,
                source_width,
                source_height,
                &app.inpaint.active_points,
                crate::ui::theme::inpaint_stroke_active(),
            );
        }

        let Some(pointer) = ui
            .ctx()
            .pointer_hover_pos()
            .filter(|position| preview_rect.contains(*position))
        else {
            return;
        };
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
        let cursor_color = if app.inpaint.processing() {
            Color32::from_white_alpha(110)
        } else {
            Color32::WHITE
        };
        painter.add(Shape::line(outline, Stroke::new(1.5, cursor_color)));
    }
}

fn paint_remove_brush_geometry(
    painter: &egui::Painter,
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    points: &[crate::pipeline::RemoveBrushPoint],
    fill: Color32,
) {
    let shortest = source_width.min(source_height).max(1) as f32;
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    for point in points {
        let uv = [point.x / width, point.y / height];
        let size = point.radius.max(0.0) / shortest;
        let outline = brush_outline_geometry_screen_points(
            image_rect,
            geometry,
            lens_geometry,
            source_width,
            source_height,
            uv,
            size,
            24,
        );
        if outline.len() >= 3 {
            painter.add(Shape::convex_polygon(outline, fill, Stroke::NONE));
        }
    }
}
