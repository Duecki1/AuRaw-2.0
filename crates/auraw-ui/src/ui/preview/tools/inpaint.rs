use super::super::*;
use super::brush::{sample_brush_stroke, STANDARD_BRUSH_MINIMUM_SPACING_FRACTION};

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
        let (primary_is_down, primary_released, set_source_modifier, secondary_is_down) =
            ui.input(|input| {
                (
                    input.pointer.primary_down(),
                    input.pointer.primary_released(),
                    input.modifiers.command || input.modifiers.ctrl,
                    input.pointer.secondary_down(),
                )
            });
        let primary_down =
            pointer.is_some() && response.is_pointer_button_down_on() && primary_is_down;

        if app.inpaint.tool.retouch().is_some()
            && ((primary_down && (set_source_modifier || app.inpaint.source_pick_active))
                || (response.hovered() && secondary_is_down))
        {
            let Some(pointer) = pointer else {
                return;
            };
            let source_uv = final_geometry_screen_to_native_source(
                image_rect,
                app.develop.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
                pointer,
            );
            if let Some(uv) = editable_source_uv(source_uv) {
                app.inpaint.source_point = Some([
                    (uv[0] * source_width as f32)
                        .floor()
                        .clamp(0.0, source_width.saturating_sub(1) as f32),
                    (uv[1] * source_height as f32)
                        .floor()
                        .clamp(0.0, source_height.saturating_sub(1) as f32),
                ]);
                app.inpaint.source_pick_active = false;
                app.inpaint.aligned_offset = None;
                app.inpaint.active_points.clear();
                app.inpaint.last_brush_uv = None;
                app.ui.notice = None;
                ui.ctx().request_repaint();
            }
            return;
        }

        if primary_down {
            if app.inpaint.tool.retouch().is_some() && app.inpaint.source_point.is_none() {
                app.ui.notice = Some(
                    "Set a clone/heal source first with Ctrl-click (Command-click on macOS) or right-click."
                        .to_owned(),
                );
                app.inpaint.last_brush_uv = None;
                return;
            }
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
            let radius_native = stroke.dab_size * source_width.min(source_height).max(1) as f32;
            let mut changed = false;
            for &sample_uv in &stroke.samples {
                if app.inpaint.active_points.len() >= crate::pipeline::REMOVE_MAX_POINTS_PER_STROKE
                {
                    break;
                }
                app.inpaint
                    .active_points
                    .push(crate::pipeline::RemoveBrushPoint {
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
            let brush = crate::pipeline::RemoveBrushStroke {
                points,
                dilation_radius: 0,
            };
            if let Some(tool) = app.inpaint.tool.retouch() {
                let destination = brush
                    .points
                    .first()
                    .map_or([0.0; 2], |point| [point.x.floor(), point.y.floor()]);
                let selected_source = app.inpaint.source_point.unwrap_or(destination);
                let source = match app.inpaint.alignment {
                    crate::pipeline::RetouchAlignment::Aligned => {
                        let offset = app.inpaint.aligned_offset.unwrap_or([
                            selected_source[0] - destination[0],
                            selected_source[1] - destination[1],
                        ]);
                        app.inpaint.aligned_offset = Some(offset);
                        [destination[0] + offset[0], destination[1] + offset[1]]
                    }
                    crate::pipeline::RetouchAlignment::Registered => destination,
                    crate::pipeline::RetouchAlignment::None
                    | crate::pipeline::RetouchAlignment::Fixed => selected_source,
                };
                app.start_retouch_worker(
                    frame,
                    brush,
                    RetouchStroke {
                        tool,
                        alignment: app.inpaint.alignment,
                        source,
                        destination,
                        hardness: app.inpaint.brush_hardness,
                        opacity: app.inpaint.brush_opacity,
                        baked_opacity: None,
                    },
                );
            } else {
                app.start_remove_worker(frame, brush);
            }
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
            if let Some(retouch) = stroke.retouch {
                paint_retouch_source_marker(
                    &painter,
                    image_rect,
                    app.develop.geometry,
                    lens_geometry,
                    source_width,
                    source_height,
                    retouch.source,
                    stroke
                        .brush
                        .points
                        .first()
                        .map(|point| point.radius)
                        .unwrap_or(1.0),
                    crate::ui::theme::inpaint_stroke_highlight(),
                );
            }
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
        if app.inpaint.tool.retouch().is_some() && app.inpaint.brush_hardness > 0.0 {
            let inner = brush_outline_geometry_screen_points(
                image_rect,
                app.develop.geometry,
                lens_geometry,
                source_width,
                source_height,
                uv,
                dab_size * app.inpaint.brush_hardness,
                64,
            );
            painter.add(Shape::line(
                inner,
                Stroke::new(1.0, Color32::from_white_alpha(145)),
            ));
        }

        if let Some(selected_source) = app.inpaint.source_point {
            let hover_native = [uv[0] * source_width as f32, uv[1] * source_height as f32];
            let stroke_origin = app
                .inpaint
                .active_points
                .first()
                .map(|point| [point.x.floor(), point.y.floor()]);
            let marker = retouch_source_marker_position(
                app.inpaint.alignment,
                selected_source,
                app.inpaint.aligned_offset,
                stroke_origin,
                hover_native,
            );
            paint_retouch_source_marker(
                &painter,
                image_rect,
                app.develop.geometry,
                lens_geometry,
                source_width,
                source_height,
                marker,
                dab_size * source_width.min(source_height).max(1) as f32,
                Color32::from_rgb(255, 190, 55),
            );
        }
    }
}

fn retouch_source_marker_position(
    alignment: crate::pipeline::RetouchAlignment,
    selected_source: [f32; 2],
    aligned_offset: Option<[f32; 2]>,
    stroke_origin: Option<[f32; 2]>,
    destination: [f32; 2],
) -> [f32; 2] {
    let follows_current_stroke = || {
        stroke_origin.map_or(selected_source, |origin| {
            [
                selected_source[0] + destination[0] - origin[0],
                selected_source[1] + destination[1] - origin[1],
            ]
        })
    };
    match alignment {
        crate::pipeline::RetouchAlignment::None => follows_current_stroke(),
        crate::pipeline::RetouchAlignment::Aligned => aligned_offset
            .map(|offset| [destination[0] + offset[0], destination[1] + offset[1]])
            .unwrap_or_else(follows_current_stroke),
        crate::pipeline::RetouchAlignment::Registered => destination,
        crate::pipeline::RetouchAlignment::Fixed => selected_source,
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_retouch_source_marker(
    painter: &egui::Painter,
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    source: [f32; 2],
    radius_native: f32,
    color: Color32,
) {
    if source[0] < 0.0
        || source[1] < 0.0
        || source[0] >= source_width as f32
        || source[1] >= source_height as f32
    {
        return;
    }
    let uv = [
        source[0] / source_width.max(1) as f32,
        source[1] / source_height.max(1) as f32,
    ];
    let size = radius_native / source_width.min(source_height).max(1) as f32;
    let outline = brush_outline_geometry_screen_points(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        uv,
        size,
        64,
    );
    painter.add(Shape::line(outline, Stroke::new(1.5, color)));
    let center = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        uv,
    );
    painter.line_segment(
        [center - egui::vec2(5.0, 0.0), center + egui::vec2(5.0, 0.0)],
        Stroke::new(1.5, color),
    );
    painter.line_segment(
        [center - egui::vec2(0.0, 5.0), center + egui::vec2(0.0, 5.0)],
        Stroke::new(1.5, color),
    );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_alignment_source_follows_stroke_then_returns_to_selected_point() {
        let selected = [120.0, 80.0];
        let destination = [260.0, 210.0];
        assert_eq!(
            retouch_source_marker_position(
                crate::pipeline::RetouchAlignment::None,
                selected,
                None,
                None,
                destination,
            ),
            selected
        );
        assert_eq!(
            retouch_source_marker_position(
                crate::pipeline::RetouchAlignment::None,
                selected,
                None,
                Some([200.0, 170.0]),
                destination,
            ),
            [180.0, 120.0]
        );
        assert_eq!(
            retouch_source_marker_position(
                crate::pipeline::RetouchAlignment::None,
                selected,
                None,
                None,
                destination,
            ),
            selected
        );
    }
}
