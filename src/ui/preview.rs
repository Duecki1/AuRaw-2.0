use crate::app::{AurawApp, SidebarTab};
use crate::pipeline::{
    ellipse_outline_points, BrushDab, BrushMode, MaskGeometry, MaskKind,
};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Ui};

pub struct Preview;

impl Preview {
    pub fn show(ui: &mut Ui, app: &mut AurawApp) {
        let Some(pipeline) = &app.gpu_pipeline else {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("No image open");
                    ui.label("Use Open RAW… to load an image.");
                });
            });
            return;
        };
        let Some(texture_id) = pipeline.egui_texture_id else {
            ui.centered_and_justified(|ui| {
                ui.spinner();
                ui.label("Preparing preview…");
            });
            return;
        };

        let available = ui.available_size();
        if available.x <= 0.0 || available.y <= 0.0 || pipeline.height == 0 {
            return;
        }

        let image_aspect = pipeline.width as f32 / pipeline.height as f32;
        let available_aspect = available.x / available.y;
        let size = if available_aspect > image_aspect {
            egui::vec2(available.y * image_aspect, available.y)
        } else {
            egui::vec2(available.x, available.x / image_aspect)
        };

        let (outer_rect, _) = ui.allocate_exact_size(available, Sense::hover());
        let image_rect = Rect::from_center_size(outer_rect.center(), size);
        let response = ui.interact(
            image_rect,
            ui.id().with("develop-preview-mask-interaction"),
            // Drag-only sensing starts immediately on touch-down. This avoids
            // the click-vs-drag threshold that otherwise makes Android masks
            // begin several pixels away from the user's finger.
            Sense::drag(),
        );
        let painter = ui.painter_at(outer_rect);
        painter.image(
            texture_id,
            image_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        if app.sidebar_tab == SidebarTab::Masks {
            Self::handle_mask_interaction(ui, app, image_rect, &response);
            // Keep the selected mask's handles visible even when the colored
            // coverage overlay is hidden.
            Self::paint_mask_overlay(ui, app, image_rect);
            Self::paint_tool_hint(ui, app, image_rect);
        }
    }

    fn handle_mask_interaction(
        ui: &Ui,
        app: &mut AurawApp,
        image_rect: Rect,
        response: &egui::Response,
    ) {
        let Some(kind) = app.active_mask_tool else {
            return;
        };
        let Some(mask_index) = app.masks.selected_mask else {
            app.active_mask_tool = None;
            return;
        };
        let Some(component_index) = app.masks.selected_component else {
            app.active_mask_tool = None;
            return;
        };
        let pointer = response.interact_pointer_pos();
        let primary_down = response.is_pointer_button_down_on();
        let drawing = primary_down;
        let released = !primary_down;

        if released {
            app.last_brush_point = None;
            let completed_shape_drag = app.mask_drag_start.take().is_some();
            let completed_shape = app
                .masks
                .masks
                .get(mask_index)
                .and_then(|mask| mask.components.get(component_index))
                .is_some_and(|component| component.geometry.is_initialized());
            // A tap without a meaningful drag should not cancel the tool on
            // touch devices. Keep Radial/Linear armed until a usable shape has
            // actually been created.
            if completed_shape_drag
                && completed_shape
                && matches!(kind, MaskKind::Radial | MaskKind::Linear)
            {
                app.active_mask_tool = None;
            }
            return;
        }
        if !drawing {
            return;
        }
        let Some(pointer) = pointer else {
            return;
        };
        let uv = screen_to_normalized(image_rect, pointer);
        let mut changed = false;

        if let Some(component) = app
            .masks
            .masks
            .get_mut(mask_index)
            .and_then(|mask| mask.components.get_mut(component_index))
        {
            match (&mut component.geometry, kind) {
                (
                    MaskGeometry::Brush {
                        size,
                        feather,
                        dabs,
                    },
                    MaskKind::Brush,
                ) => {
                    let opacity = match app.brush_mode {
                        BrushMode::Paint => 1.0,
                        BrushMode::Erase => -1.0,
                    };
                    let previous = app.last_brush_point.unwrap_or(uv);
                    let dx = uv[0] - previous[0];
                    let dy = uv[1] - previous[1];
                    // Space dabs in screen/image pixels rather than raw UV
                    // units so strokes remain continuous on wide and tall
                    // images on both mouse and touch input.
                    let distance_px = ((dx * image_rect.width()).powi(2)
                        + (dy * image_rect.height()).powi(2))
                    .sqrt();
                    let radius_px = *size * image_rect.width().min(image_rect.height());
                    let spacing_px = (radius_px * 0.22).clamp(0.75, 24.0);
                    let steps = (distance_px / spacing_px).ceil().max(1.0) as usize;
                    for step in 1..=steps {
                        if dabs.len() >= 8192 {
                            break;
                        }
                        let t = step as f32 / steps as f32;
                        dabs.push(BrushDab {
                            center: [previous[0] + dx * t, previous[1] + dy * t],
                            opacity,
                            size: *size,
                            feather: *feather,
                        });
                    }
                    app.last_brush_point = Some(uv);
                    changed = true;
                }
                (
                    MaskGeometry::Radial {
                        center,
                        radius,
                        rotation,
                        initialized,
                        ..
                    },
                    MaskKind::Radial,
                ) => {
                    let start = *app.mask_drag_start.get_or_insert(uv);
                    let mut rx = (uv[0] - start[0]).abs();
                    let mut ry = (uv[1] - start[1]).abs();
                    if rx < 0.01 && ry >= 0.01 {
                        rx = ry * 0.66;
                    }
                    if ry < 0.01 && rx >= 0.01 {
                        ry = rx * 0.66;
                    }
                    *center = start;
                    *radius = [rx.max(0.005), ry.max(0.005)];
                    *rotation = 0.0;
                    *initialized = rx > 0.008 || ry > 0.008;
                    changed = true;
                }
                (
                    MaskGeometry::Linear {
                        start,
                        end,
                        initialized,
                        ..
                    },
                    MaskKind::Linear,
                ) => {
                    let origin = *app.mask_drag_start.get_or_insert(uv);
                    *start = origin;
                    *end = uv;
                    let dx = end[0] - start[0];
                    let dy = end[1] - start[1];
                    *initialized = dx * dx + dy * dy > 0.000_025;
                    changed = true;
                }
                _ => {}
            }
        }

        if changed {
            app.mark_mask_geometry_dirty(mask_index);
            ui.ctx().request_repaint();
        }
    }

    fn paint_mask_overlay(ui: &Ui, app: &AurawApp, image_rect: Rect) {
        let Some(mask_index) = app.masks.selected_mask else {
            return;
        };
        let Some(mask) = app.masks.masks.get(mask_index) else {
            return;
        };
        let selected_component = app.masks.selected_component;
        let accent = Color32::from_rgb(78, 163, 255);
        let subtract = Color32::from_rgb(255, 105, 105);
        let painter = ui.painter_at(image_rect);

        for (component_index, component) in mask.components.iter().enumerate() {
            if !component.enabled {
                continue;
            }
            let selected = selected_component == Some(component_index);
            if !selected && !app.masks.show_overlay {
                continue;
            }
            let color = if component.combine == crate::pipeline::MaskCombineMode::Subtract {
                subtract
            } else {
                accent
            };
            let width = if selected { 2.0 } else { 1.0 };
            match &component.geometry {
                MaskGeometry::Brush { dabs, .. } => {
                    if app.masks.show_overlay {
                        let fill =
                            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 22);
                        for dab in dabs.iter().rev().take(700) {
                            let center = normalized_to_screen(image_rect, dab.center);
                            let radius = dab.size * image_rect.width().min(image_rect.height());
                            if dab.opacity >= 0.0 {
                                painter.circle_filled(center, radius, fill);
                            } else {
                                painter.circle_stroke(center, radius, Stroke::new(1.0, subtract));
                            }
                        }
                    }
                }
                MaskGeometry::Radial {
                    center,
                    radius,
                    rotation,
                    feather,
                    initialized: true,
                } => {
                    let outer = ellipse_outline_points(*center, *radius, *rotation, 72)
                        .into_iter()
                        .map(|point| normalized_to_screen(image_rect, point))
                        .collect::<Vec<_>>();
                    painter.add(egui::Shape::line(outer, Stroke::new(width, color)));
                    let inner_scale = 1.0 - feather.clamp(0.0, 1.0) * 0.98;
                    let inner = ellipse_outline_points(
                        *center,
                        [radius[0] * inner_scale, radius[1] * inner_scale],
                        *rotation,
                        72,
                    )
                    .into_iter()
                    .map(|point| normalized_to_screen(image_rect, point))
                    .collect::<Vec<_>>();
                    painter.add(egui::Shape::line(
                        inner,
                        Stroke::new(1.0, color.gamma_multiply(0.65)),
                    ));
                    painter.circle_filled(normalized_to_screen(image_rect, *center), 4.0, color);
                }
                MaskGeometry::Linear {
                    start,
                    end,
                    feather,
                    initialized: true,
                } => {
                    let a = normalized_to_screen(image_rect, *start);
                    let b = normalized_to_screen(image_rect, *end);
                    painter.line_segment([a, b], Stroke::new(width, color));
                    painter.circle_filled(a, 4.0, color);
                    painter.circle_filled(b, 4.0, color);
                    let direction = b - a;
                    let length = direction.length().max(1.0);
                    let normal = egui::vec2(-direction.y, direction.x) / length;
                    let span = image_rect.width().max(image_rect.height());
                    let middle = a + direction * 0.5;
                    let half_transition = direction * (0.5 * feather.clamp(0.02, 1.0));
                    for center in [middle - half_transition, middle + half_transition] {
                        painter.line_segment(
                            [center - normal * span, center + normal * span],
                            Stroke::new(1.0, color.gamma_multiply(0.65)),
                        );
                    }
                }
                _ => {}
            }
        }

        if app.active_mask_tool == Some(MaskKind::Brush) {
            if let Some(pointer) = ui.ctx().pointer_hover_pos().filter(|p| image_rect.contains(*p)) {
                if let Some(component) = app.masks.selected_component() {
                    if let MaskGeometry::Brush { size, .. } = &component.geometry {
                        let radius = *size * image_rect.width().min(image_rect.height());
                        let cursor_color = match app.brush_mode {
                            BrushMode::Paint => Color32::WHITE,
                            BrushMode::Erase => subtract,
                        };
                        painter.circle_stroke(pointer, radius, Stroke::new(1.5, cursor_color));
                    }
                }
            }
        }
    }

    fn paint_tool_hint(ui: &Ui, app: &AurawApp, image_rect: Rect) {
        let Some(kind) = app.active_mask_tool else {
            return;
        };
        let text = match kind {
            MaskKind::Brush => {
                if app.brush_mode == BrushMode::Paint {
                    "Drag to paint the mask"
                } else {
                    "Drag to erase from the mask"
                }
            }
            MaskKind::Radial => "Drag from the center to create a radial gradient",
            MaskKind::Linear => "Drag across the image to create a linear gradient",
            _ => return,
        };
        let painter = ui.painter_at(image_rect);
        let position = image_rect.left_top() + egui::vec2(12.0, 12.0);
        painter.text(
            position,
            egui::Align2::LEFT_TOP,
            text,
            egui::FontId::proportional(13.0),
            Color32::WHITE,
        );
    }
}

fn screen_to_normalized(rect: Rect, point: Pos2) -> [f32; 2] {
    [
        ((point.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
        ((point.y - rect.top()) / rect.height()).clamp(0.0, 1.0),
    ]
}

fn normalized_to_screen(rect: Rect, point: [f32; 2]) -> Pos2 {
    Pos2::new(
        rect.left() + point[0] * rect.width(),
        rect.top() + point[1] * rect.height(),
    )
}
