use crate::app::{AurawApp, MaskDragState, MaskOverlayBlink, SidebarTab};
use crate::pipeline::{BrushDab, BrushMode, MaskCombineMode, MaskGeometry, MaskKind};
use crate::ui::mask_component_color;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Ui};

pub struct Preview;

impl Preview {
    pub fn show(ui: &mut Ui, app: &mut AurawApp) {
        let Some((texture_id, pipeline_width, pipeline_height)) = app
            .gpu_pipeline
            .as_ref()
            .and_then(|pipeline| {
                pipeline
                    .egui_texture_id
                    .map(|texture_id| (texture_id, pipeline.width, pipeline.height))
            })
        else {
            if app.gpu_pipeline.is_some() {
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                    ui.label("Preparing preview…");
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("No image open");
                        ui.label("Use Open RAW… to load an image.");
                    });
                });
            }
            return;
        };

        let available = ui.available_size();
        if available.x <= 0.0 || available.y <= 0.0 || pipeline_height == 0 {
            return;
        }

        let (outer_rect, _) = ui.allocate_exact_size(available, Sense::hover());
        let base_size = fitted_image_size(
            outer_rect.size(),
            pipeline_width as f32 / pipeline_height as f32,
        );
        app.preview_zoom = app.preview_zoom.clamp(1.0, 32.0);
        clamp_preview_center(
            &mut app.preview_center,
            outer_rect.size(),
            base_size * app.preview_zoom,
        );
        let mut image_rect = zoomed_image_rect(
            outer_rect,
            base_size,
            app.preview_zoom,
            app.preview_center,
        );
        let mut interaction_rect = outer_rect.intersect(image_rect);
        if interaction_rect.width() <= 0.0 || interaction_rect.height() <= 0.0 {
            interaction_rect = outer_rect;
        }
        let interaction_id = if app.sidebar_tab == SidebarTab::Masks {
            ui.id().with("develop-preview-mask-interaction")
        } else {
            ui.id().with("develop-preview-interaction")
        };
        let interaction_sense = if app.sidebar_tab == SidebarTab::Masks {
            Sense::drag()
        } else {
            Sense::click_and_drag()
        };
        let response = ui.interact(interaction_rect, interaction_id, interaction_sense);

        let mut moved = false;
        if response.hovered() {
            let scroll_y = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll_y.abs() > 0.01 {
                let pointer = ui
                    .input(|input| input.pointer.hover_pos())
                    .unwrap_or(outer_rect.center());
                let anchor = [
                    (pointer.x - image_rect.left()) / image_rect.width().max(1.0),
                    (pointer.y - image_rect.top()) / image_rect.height().max(1.0),
                ];
                let previous_zoom = app.preview_zoom;
                app.preview_zoom = (previous_zoom * (scroll_y * 0.0018).exp()).clamp(1.0, 32.0);
                if (app.preview_zoom - previous_zoom).abs() > f32::EPSILON {
                    let new_size = base_size * app.preview_zoom;
                    let new_min = Pos2::new(
                        pointer.x - anchor[0] * new_size.x,
                        pointer.y - anchor[1] * new_size.y,
                    );
                    app.preview_center = [
                        (outer_rect.center().x - new_min.x) / new_size.x.max(1.0),
                        (outer_rect.center().y - new_min.y) / new_size.y.max(1.0),
                    ];
                    clamp_preview_center(&mut app.preview_center, outer_rect.size(), new_size);
                    moved = true;
                }
            }
        }

        let pan_with_primary = app.sidebar_tab != SidebarTab::Masks
            && response.dragged_by(egui::PointerButton::Primary);
        let pan_with_middle = response.dragged_by(egui::PointerButton::Middle);
        if pan_with_primary || pan_with_middle {
            let delta = ui.input(|input| input.pointer.delta());
            let image_size = base_size * app.preview_zoom;
            app.preview_center[0] -= delta.x / image_size.x.max(1.0);
            app.preview_center[1] -= delta.y / image_size.y.max(1.0);
            clamp_preview_center(&mut app.preview_center, outer_rect.size(), image_size);
            moved |= delta.length_sq() > 0.0;
        }

        if response.double_clicked() {
            app.preview_zoom = 1.0;
            app.preview_center = [0.5, 0.5];
            moved = true;
        }

        image_rect = zoomed_image_rect(
            outer_rect,
            base_size,
            app.preview_zoom,
            app.preview_center,
        );
        let visible_screen = outer_rect.intersect(image_rect);
        let visible_uv = crate::app::PreviewUvRect {
            min: [
                ((visible_screen.left() - image_rect.left()) / image_rect.width().max(1.0))
                    .clamp(0.0, 1.0),
                ((visible_screen.top() - image_rect.top()) / image_rect.height().max(1.0))
                    .clamp(0.0, 1.0),
            ],
            max: [
                ((visible_screen.right() - image_rect.left()) / image_rect.width().max(1.0))
                    .clamp(0.0, 1.0),
                ((visible_screen.bottom() - image_rect.top()) / image_rect.height().max(1.0))
                    .clamp(0.0, 1.0),
            ],
        };
        if preview_uv_changed(app.preview_visible_uv, visible_uv) {
            app.preview_visible_uv = visible_uv;
            moved = true;
        }
        if moved {
            app.note_preview_motion();
        }

        let painter = ui.painter_at(outer_rect);
        painter.image(
            texture_id,
            image_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        if let Some(detail) = app
            .preview_detail
            .as_ref()
            .filter(|detail| detail.revision == app.preview_revision)
        {
            if let Some(detail_texture_id) = detail.pipeline.egui_texture_id {
                let detail_rect = Rect::from_min_max(
                    normalized_to_screen(image_rect, detail.uv_rect.min),
                    normalized_to_screen(image_rect, detail.uv_rect.max),
                );
                painter.image(
                    detail_texture_id,
                    detail_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }

        painter.text(
            outer_rect.left_top() + egui::vec2(10.0, 10.0),
            egui::Align2::LEFT_TOP,
            format!("{:.0}% · scroll to zoom · double-click to fit", app.preview_zoom * 100.0),
            egui::FontId::proportional(11.0),
            Color32::from_white_alpha(180),
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
        let Some(mask_index) = app.masks.selected_mask else {
            app.finish_mask_geometry_interaction();
            app.active_mask_tool = None;
            return;
        };
        let Some(component_index) = app.masks.selected_component else {
            app.finish_mask_geometry_interaction();
            app.active_mask_tool = None;
            return;
        };
        let Some(kind) = app
            .masks
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .map(|component| component.kind)
        else {
            app.finish_mask_geometry_interaction();
            app.active_mask_tool = None;
            return;
        };
        if !kind.is_available() {
            return;
        }
        app.active_mask_tool = Some(kind);
        let pointer = response.interact_pointer_pos();
        let primary_down = response.is_pointer_button_down_on()
            && ui.input(|input| input.pointer.primary_down());
        if !primary_down {
            app.finish_mask_geometry_interaction();
            app.last_brush_point = None;
            app.mask_drag = None;
            return;
        }
        let Some(pointer) = pointer else {
            return;
        };
        let uv = screen_to_normalized(image_rect, pointer);
        let color_was_sampled = app
            .masks
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .is_some_and(|component| {
                matches!(
                    &component.geometry,
                    MaskGeometry::ColorRange { sampled: true, .. }
                )
            });

        if app.mask_drag.is_none() && kind != MaskKind::Brush {
            let geometry = &app.masks.masks[mask_index].components[component_index].geometry;
            app.mask_drag = begin_mask_drag(geometry, uv, pointer, image_rect);
        }

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
                ) => match app.mask_drag {
                    Some(MaskDragState::Create(origin)) => {
                        let mut rx = (uv[0] - origin[0]).abs();
                        let mut ry = (uv[1] - origin[1]).abs();
                        if rx < 0.01 && ry >= 0.01 {
                            rx = ry * 0.66;
                        }
                        if ry < 0.01 && rx >= 0.01 {
                            ry = rx * 0.66;
                        }
                        *center = origin;
                        *radius = [rx.max(0.005), ry.max(0.005)];
                        *rotation = 0.0;
                        *initialized = rx > 0.008 || ry > 0.008;
                        changed = true;
                    }
                    Some(MaskDragState::MoveRadial {
                        pointer: origin,
                        center: original_center,
                    }) => {
                        center[0] = (original_center[0] + uv[0] - origin[0]).clamp(0.0, 1.0);
                        center[1] = (original_center[1] + uv[1] - origin[1]).clamp(0.0, 1.0);
                        changed = true;
                    }
                    Some(MaskDragState::ResizeRadial { axis }) => {
                        let center_screen = normalized_to_screen(image_rect, *center);
                        let delta = pointer - center_screen;
                        let cos_r = rotation.cos();
                        let sin_r = rotation.sin();
                        if axis == 0 {
                            radius[0] = ((cos_r * delta.x + sin_r * delta.y).abs()
                                / image_rect.width().max(1.0))
                            .max(0.005);
                        } else {
                            radius[1] = ((-sin_r * delta.x + cos_r * delta.y).abs()
                                / image_rect.height().max(1.0))
                            .max(0.005);
                        }
                        changed = true;
                    }
                    Some(MaskDragState::RotateRadial {
                        pointer_angle,
                        rotation: original_rotation,
                    }) => {
                        let center_screen = normalized_to_screen(image_rect, *center);
                        let current_angle = angle_from(center_screen, pointer);
                        *rotation =
                            original_rotation + shortest_angle_delta(pointer_angle, current_angle);
                        changed = true;
                    }
                    _ => {}
                },
                (
                    MaskGeometry::Linear {
                        start,
                        end,
                        initialized,
                        ..
                    },
                    MaskKind::Linear,
                ) => match app.mask_drag {
                    Some(MaskDragState::Create(origin)) => {
                        *start = origin;
                        *end = uv;
                        let dx = end[0] - start[0];
                        let dy = end[1] - start[1];
                        *initialized = dx * dx + dy * dy > 0.000_025;
                        changed = true;
                    }
                    Some(MaskDragState::LinearStart) => {
                        *start = uv;
                        changed = true;
                    }
                    Some(MaskDragState::LinearEnd) => {
                        *end = uv;
                        changed = true;
                    }
                    Some(MaskDragState::MoveLinear {
                        pointer: origin,
                        start: original_start,
                        end: original_end,
                    }) => {
                        let min_x = original_start[0].min(original_end[0]);
                        let max_x = original_start[0].max(original_end[0]);
                        let min_y = original_start[1].min(original_end[1]);
                        let max_y = original_start[1].max(original_end[1]);
                        let dx = (uv[0] - origin[0]).clamp(-min_x, 1.0 - max_x);
                        let dy = (uv[1] - origin[1]).clamp(-min_y, 1.0 - max_y);
                        *start = [original_start[0] + dx, original_start[1] + dy];
                        *end = [original_end[0] + dx, original_end[1] + dy];
                        changed = true;
                    }
                    Some(MaskDragState::RotateLinear {
                        pointer_angle,
                        start: original_start,
                        end: original_end,
                    }) => {
                        let a = normalized_to_screen(image_rect, original_start);
                        let b = normalized_to_screen(image_rect, original_end);
                        let midpoint = a + (b - a) * 0.5;
                        let original_vector = b - a;
                        let original_angle = original_vector.y.atan2(original_vector.x);
                        let current_angle = angle_from(midpoint, pointer);
                        let angle =
                            original_angle + shortest_angle_delta(pointer_angle, current_angle);
                        let half_length = original_vector.length() * 0.5;
                        let half_vector = egui::vec2(angle.cos(), angle.sin()) * half_length;
                        *start = screen_to_normalized(image_rect, midpoint - half_vector);
                        *end = screen_to_normalized(image_rect, midpoint + half_vector);
                        changed = true;
                    }
                    _ => {}
                },
                (
                    MaskGeometry::ColorRange {
                        source: Some(source),
                        sample,
                        sampled,
                        ..
                    },
                    MaskKind::ColorRange,
                ) => {
                    let x = (uv[0] * source.width.saturating_sub(1) as f32).round() as usize;
                    let y = (uv[1] * source.height.saturating_sub(1) as f32).round() as usize;
                    let index = (y * source.width as usize + x) * 4;
                    *sample = [
                        source.rgba[index] as f32 / 255.0,
                        source.rgba[index + 1] as f32 / 255.0,
                        source.rgba[index + 2] as f32 / 255.0,
                    ];
                    *sampled = true;
                    changed = true;
                }
                _ => {}
            }
        }

        if changed {
            app.note_mask_geometry_interaction(mask_index);
            if kind == MaskKind::ColorRange && !color_was_sampled {
                app.blink_selected_component();
            }
            ui.ctx().request_repaint();
        }
    }

    fn paint_mask_overlay(ui: &Ui, app: &mut AurawApp, image_rect: Rect) {
        let Some(mask_index) = app.masks.selected_mask else {
            return;
        };
        let Some(mask) = app.masks.masks.get(mask_index) else {
            return;
        };
        let selected_component = app.masks.selected_component;
        let neutral = mask.adjustments.is_neutral();
        let accent = selected_component
            .map(mask_component_color)
            .unwrap_or(Color32::from_rgb(78, 163, 255));
        let subtract = Color32::from_rgb(255, 105, 105);
        let painter = ui.painter_at(image_rect);

        // An untouched mask remains visible after its selection flashes. Once
        // local adjustments exist, selection still flashes for orientation but
        // the overlay returns to hidden so it cannot obscure the edit.
        let steady_target: Option<Option<usize>> = neutral.then_some(None);
        let mut coverage_target = steady_target;
        if let Some((started, blink)) = app.mask_overlay_blink {
            let elapsed = started.elapsed().as_secs_f32();
            coverage_target = match blink {
                MaskOverlayBlink::GroupTwice if elapsed < 0.18 => Some(None),
                MaskOverlayBlink::GroupTwice if elapsed < 0.32 => None,
                MaskOverlayBlink::GroupTwice if elapsed < 0.50 => Some(None),
                MaskOverlayBlink::GroupTwice if elapsed < 0.64 => None,
                MaskOverlayBlink::ComponentThenGroup if elapsed < 0.22 => {
                    selected_component.map(Some)
                }
                MaskOverlayBlink::ComponentThenGroup if elapsed < 0.35 => None,
                MaskOverlayBlink::ComponentThenGroup if elapsed < 0.57 => Some(None),
                MaskOverlayBlink::ComponentThenGroup if elapsed < 0.70 => None,
                _ => {
                    app.mask_overlay_blink = None;
                    steady_target
                }
            };
            if app.mask_overlay_blink.is_some() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(25));
            }
        }
        let pointer_editing = ui.input(|input| input.pointer.primary_down())
            && ui
                .ctx()
                .pointer_interact_pos()
                .is_some_and(|position| image_rect.contains(position));
        if pointer_editing {
            let editing_live_mask = neutral
                && selected_component.is_some_and(|index| {
                    app.masks.masks[mask_index]
                        .components
                        .get(index)
                        .is_some_and(|component| {
                            !matches!(component.kind, MaskKind::Subject | MaskKind::Background)
                        })
                });
            coverage_target = if editing_live_mask {
                selected_component.map(Some)
            } else {
                None
            };
        }
        if mask.enabled {
            if let Some(component) = coverage_target {
                Self::paint_coverage_texture(ui, app, image_rect, mask_index, component);
            }
        }

        if let Some(component) = selected_component.and_then(|index| {
            app.masks
                .masks
                .get(mask_index)
                .and_then(|mask| mask.components.get(index))
        }) {
            if !component.enabled {
                return;
            }
            let color = accent;
            match &component.geometry {
                MaskGeometry::Brush { .. } => {}
                MaskGeometry::Radial {
                    center,
                    radius,
                    rotation,
                    feather,
                    initialized: true,
                } => {
                    let outer =
                        radial_outline_screen_points(image_rect, *center, *radius, *rotation, 72);
                    painter.add(egui::Shape::line(outer, Stroke::new(2.0, color)));
                    let inner_scale = 1.0 - feather.clamp(0.0, 1.0) * 0.98;
                    let inner = radial_outline_screen_points(
                        image_rect,
                        *center,
                        [radius[0] * inner_scale, radius[1] * inner_scale],
                        *rotation,
                        72,
                    );
                    painter.add(egui::Shape::line(
                        inner,
                        Stroke::new(1.0, color.gamma_multiply(0.65)),
                    ));
                    let center_screen = normalized_to_screen(image_rect, *center);
                    painter.circle_filled(center_screen, 5.0, color);
                    for handle in radial_handles_screen(image_rect, *center, *radius, *rotation) {
                        painter.circle_filled(handle, 4.0, color);
                    }
                    let major_handle =
                        radial_handles_screen(image_rect, *center, *radius, *rotation)[0];
                    let rotation_handle =
                        radial_rotation_handle(image_rect, *center, *radius, *rotation);
                    painter.line_segment(
                        [major_handle, rotation_handle],
                        Stroke::new(1.0, color.gamma_multiply(0.72)),
                    );
                    painter.circle_stroke(rotation_handle, 6.0, Stroke::new(2.0, color));
                }
                MaskGeometry::Linear {
                    start,
                    end,
                    feather,
                    initialized: true,
                } => {
                    let a = normalized_to_screen(image_rect, *start);
                    let b = normalized_to_screen(image_rect, *end);
                    painter.line_segment([a, b], Stroke::new(2.0, color));
                    painter.circle_filled(a, 5.0, color);
                    painter.circle_filled(b, 5.0, color);
                    let direction = b - a;
                    let length = direction.length().max(1.0);
                    let normal = egui::vec2(-direction.y, direction.x) / length;
                    let rotation_handle = linear_rotation_handle(a, b);
                    let middle = a + direction * 0.5;
                    painter.line_segment(
                        [middle, rotation_handle],
                        Stroke::new(1.0, color.gamma_multiply(0.72)),
                    );
                    painter.circle_stroke(rotation_handle, 6.0, Stroke::new(2.0, color));
                    let span = image_rect.width().max(image_rect.height());
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

        if app
            .masks
            .selected_component()
            .is_some_and(|component| component.kind == MaskKind::Brush && component.enabled)
        {
            if let Some(pointer) = ui
                .ctx()
                .pointer_hover_pos()
                .filter(|p| image_rect.contains(*p))
            {
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

    fn paint_coverage_texture(
        ui: &Ui,
        app: &mut AurawApp,
        image_rect: Rect,
        mask_index: usize,
        component_index: Option<usize>,
    ) {
        let Some(pipeline) = app.gpu_pipeline.as_ref() else {
            return;
        };
        let max_edge = if cfg!(target_os = "android") {
            384.0
        } else {
            512.0
        };
        let scale = (max_edge / image_rect.width().max(image_rect.height())).min(1.0);
        let width = (image_rect.width() * scale).round().max(1.0) as u32;
        let height = (image_rect.height() * scale).round().max(1.0) as u32;
        let key = (
            mask_index,
            component_index,
            app.mask_overlay_revision,
            width,
            height,
        );

        if app.mask_overlay_texture_key != Some(key) {
            let rgba = if let Some(component_index) = component_index {
                let coverage = app.masks.rasterize_component_layer(
                    mask_index,
                    component_index,
                    width,
                    height,
                    pipeline.width,
                    pipeline.height,
                );
                coverage_rgba(coverage, mask_component_color(component_index))
            } else {
                group_coverage_rgba(
                    app,
                    mask_index,
                    width,
                    height,
                    pipeline.width,
                    pipeline.height,
                )
            };
            let image =
                egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
            if let Some(texture) = app.mask_overlay_texture.as_mut() {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                app.mask_overlay_texture = Some(ui.ctx().load_texture(
                    "selected-mask-coverage",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
            app.mask_overlay_texture_key = Some(key);
        }

        if let Some(texture) = &app.mask_overlay_texture {
            painter_image(ui, texture.id(), image_rect);
        }
    }

    fn paint_tool_hint(ui: &Ui, app: &AurawApp, image_rect: Rect) {
        let Some(kind) = app.active_mask_tool else {
            return;
        };
        let text = match kind {
            MaskKind::Brush => return,
            MaskKind::Radial
                if !app
                    .masks
                    .selected_component()
                    .is_some_and(|component| component.geometry.is_initialized()) =>
            {
                "Drag from the center to create a radial gradient"
            }
            MaskKind::Linear
                if !app
                    .masks
                    .selected_component()
                    .is_some_and(|component| component.geometry.is_initialized()) =>
            {
                "Drag across the image to create a linear gradient"
            }
            MaskKind::ColorRange
                if !app.masks.selected_component().is_some_and(|component| {
                    matches!(
                        &component.geometry,
                        MaskGeometry::ColorRange { sampled: true, .. }
                    )
                }) =>
            {
                "Drag on the image to sample a color"
            }
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

fn begin_mask_drag(
    geometry: &MaskGeometry,
    uv: [f32; 2],
    pointer: Pos2,
    image_rect: Rect,
) -> Option<MaskDragState> {
    match geometry {
        MaskGeometry::Radial {
            center,
            radius,
            rotation,
            initialized,
            ..
        } => {
            if !initialized {
                return Some(MaskDragState::Create(uv));
            }
            let rotation_handle = radial_rotation_handle(image_rect, *center, *radius, *rotation);
            if rotation_handle.distance(pointer) <= 24.0 {
                return Some(MaskDragState::RotateRadial {
                    pointer_angle: angle_from(normalized_to_screen(image_rect, *center), pointer),
                    rotation: *rotation,
                });
            }
            for (index, handle) in radial_handles_screen(image_rect, *center, *radius, *rotation)
                .into_iter()
                .enumerate()
            {
                if handle.distance(pointer) <= 22.0 {
                    return Some(MaskDragState::ResizeRadial { axis: index / 2 });
                }
            }

            let center_screen = normalized_to_screen(image_rect, *center);
            let delta = pointer - center_screen;
            let cos_r = rotation.cos();
            let sin_r = rotation.sin();
            let local_x = (cos_r * delta.x + sin_r * delta.y)
                / (radius[0].abs().max(0.005) * image_rect.width().max(1.0));
            let local_y = (-sin_r * delta.x + cos_r * delta.y)
                / (radius[1].abs().max(0.005) * image_rect.height().max(1.0));
            if local_x * local_x + local_y * local_y <= 1.0 {
                Some(MaskDragState::MoveRadial {
                    pointer: uv,
                    center: *center,
                })
            } else {
                None
            }
        }
        MaskGeometry::Linear {
            start,
            end,
            initialized,
            ..
        } => {
            if !initialized {
                return Some(MaskDragState::Create(uv));
            }
            let a = normalized_to_screen(image_rect, *start);
            let b = normalized_to_screen(image_rect, *end);
            let rotation_handle = linear_rotation_handle(a, b);
            if rotation_handle.distance(pointer) <= 24.0 {
                Some(MaskDragState::RotateLinear {
                    pointer_angle: angle_from(a + (b - a) * 0.5, pointer),
                    start: *start,
                    end: *end,
                })
            } else if a.distance(pointer) <= 22.0 {
                Some(MaskDragState::LinearStart)
            } else if b.distance(pointer) <= 22.0 {
                Some(MaskDragState::LinearEnd)
            } else if distance_to_segment(pointer, a, b) <= 18.0 {
                Some(MaskDragState::MoveLinear {
                    pointer: uv,
                    start: *start,
                    end: *end,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn angle_from(center: Pos2, pointer: Pos2) -> f32 {
    let delta = pointer - center;
    delta.y.atan2(delta.x)
}

fn shortest_angle_delta(from: f32, to: f32) -> f32 {
    let mut delta = to - from;
    while delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    while delta < -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    delta
}

fn radial_rotation_handle(
    image_rect: Rect,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
) -> Pos2 {
    let center_screen = normalized_to_screen(image_rect, center);
    let major_screen = radial_handles_screen(image_rect, center, radius, rotation)[0];
    let direction = (major_screen - center_screen).normalized();
    major_screen + direction * 30.0
}

fn linear_rotation_handle(start: Pos2, end: Pos2) -> Pos2 {
    let direction = end - start;
    let length = direction.length().max(1.0);
    let normal = egui::vec2(-direction.y, direction.x) / length;
    start + direction * 0.5 + normal * 34.0
}

fn radial_handles_screen(
    image_rect: Rect,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
) -> [Pos2; 4] {
    let center = normalized_to_screen(image_rect, center);
    let cos_r = rotation.cos();
    let sin_r = rotation.sin();
    let major = egui::vec2(
        cos_r * radius[0] * image_rect.width(),
        sin_r * radius[0] * image_rect.width(),
    );
    let minor = egui::vec2(
        -sin_r * radius[1] * image_rect.height(),
        cos_r * radius[1] * image_rect.height(),
    );
    [
        center + major,
        center - major,
        center + minor,
        center - minor,
    ]
}

fn radial_outline_screen_points(
    image_rect: Rect,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
    segments: usize,
) -> Vec<Pos2> {
    let center = normalized_to_screen(image_rect, center);
    let radius_x = radius[0] * image_rect.width();
    let radius_y = radius[1] * image_rect.height();
    let cos_r = rotation.cos();
    let sin_r = rotation.sin();
    let segments = segments.max(12);
    (0..=segments)
        .map(|index| {
            let angle = std::f32::consts::TAU * index as f32 / segments as f32;
            let x = radius_x * angle.cos();
            let y = radius_y * angle.sin();
            center + egui::vec2(cos_r * x - sin_r * y, sin_r * x + cos_r * y)
        })
        .collect()
}

fn distance_to_segment(point: Pos2, start: Pos2, end: Pos2) -> f32 {
    let segment = end - start;
    let length_sq = segment.length_sq();
    if length_sq <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(segment) / length_sq).clamp(0.0, 1.0);
    point.distance(start + segment * t)
}

fn fitted_image_size(available: egui::Vec2, image_aspect: f32) -> egui::Vec2 {
    let available_aspect = available.x / available.y.max(1.0);
    if available_aspect > image_aspect {
        egui::vec2(available.y * image_aspect, available.y)
    } else {
        egui::vec2(available.x, available.x / image_aspect.max(f32::EPSILON))
    }
}

fn zoomed_image_rect(
    outer_rect: Rect,
    base_size: egui::Vec2,
    zoom: f32,
    center: [f32; 2],
) -> Rect {
    let size = base_size * zoom;
    let min = Pos2::new(
        outer_rect.center().x - center[0] * size.x,
        outer_rect.center().y - center[1] * size.y,
    );
    Rect::from_min_size(min, size)
}

fn clamp_preview_center(center: &mut [f32; 2], viewport: egui::Vec2, image: egui::Vec2) {
    for axis in 0..2 {
        let viewport_axis = if axis == 0 { viewport.x } else { viewport.y };
        let image_axis = if axis == 0 { image.x } else { image.y };
        if image_axis <= viewport_axis + 0.5 {
            center[axis] = 0.5;
        } else {
            let half_visible = (viewport_axis / (2.0 * image_axis)).clamp(0.0, 0.5);
            center[axis] = center[axis].clamp(half_visible, 1.0 - half_visible);
        }
    }
}

fn preview_uv_changed(left: crate::app::PreviewUvRect, right: crate::app::PreviewUvRect) -> bool {
    left.min
        .into_iter()
        .chain(left.max)
        .zip(right.min.into_iter().chain(right.max))
        .any(|(left, right)| (left - right).abs() > 0.0005)
}

fn painter_image(ui: &Ui, texture_id: egui::TextureId, rect: Rect) {
    ui.painter_at(rect).image(
        texture_id,
        rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
}

fn coverage_rgba(coverage: Vec<u8>, color: Color32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(coverage.len() * 4);
    for alpha in coverage {
        rgba.extend_from_slice(&[
            color.r(),
            color.g(),
            color.b(),
            ((alpha as u16 * 92) / 255) as u8,
        ]);
    }
    rgba
}

fn group_coverage_rgba(
    app: &AurawApp,
    mask_index: usize,
    width: u32,
    height: u32,
    image_width: u32,
    image_height: u32,
) -> Vec<u8> {
    let final_coverage =
        app.masks
            .rasterize_layer(mask_index, width, height, image_width, image_height);
    let component_count = app
        .masks
        .masks
        .get(mask_index)
        .map_or(0, |mask| mask.components.len());

    // combined coverage, weighted red, green, blue, and total color weight.
    // Keeping these together avoids allocating one full image per component.
    let mut composite = vec![[0.0_f32; 5]; final_coverage.len()];
    let mut has_component = false;

    for component_index in 0..component_count {
        let Some((combine, enabled, initialized)) = app
            .masks
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .map(|component| {
                (
                    component.combine,
                    component.enabled,
                    component.geometry.is_initialized(),
                )
            })
        else {
            continue;
        };
        if !enabled || !initialized {
            continue;
        }

        let coverage = app.masks.rasterize_component_layer(
            mask_index,
            component_index,
            width,
            height,
            image_width,
            image_height,
        );
        let color = mask_component_color(component_index);
        let rgb = [color.r() as f32, color.g() as f32, color.b() as f32];

        if !has_component {
            has_component = true;
            if combine != MaskCombineMode::Add {
                continue;
            }
        }

        for (pixel, alpha) in composite.iter_mut().zip(coverage) {
            let source = alpha as f32 / 255.0;
            match combine {
                MaskCombineMode::Add => {
                    pixel[0] = pixel[0].max(source);
                    pixel[1] += rgb[0] * source;
                    pixel[2] += rgb[1] * source;
                    pixel[3] += rgb[2] * source;
                    pixel[4] += source;
                }
                MaskCombineMode::Subtract => {
                    let remaining = 1.0 - source;
                    for value in pixel.iter_mut() {
                        *value *= remaining;
                    }
                }
                MaskCombineMode::Intersect => {
                    pixel[0] *= source;
                    pixel[1] *= source;
                    pixel[2] *= source;
                    pixel[3] *= source;
                    pixel[4] *= source;

                    // An intersection belongs visually to both operands. Give
                    // the intersecting component an equal contribution in the
                    // portion of the group mask that survives it.
                    let contribution = pixel[0];
                    pixel[1] += rgb[0] * contribution;
                    pixel[2] += rgb[1] * contribution;
                    pixel[3] += rgb[2] * contribution;
                    pixel[4] += contribution;
                }
            }
        }
    }

    let fallback = Color32::from_rgb(78, 163, 255);
    let mut rgba = Vec::with_capacity(final_coverage.len() * 4);
    for (alpha, pixel) in final_coverage.into_iter().zip(composite) {
        let (red, green, blue) = if pixel[4] > f32::EPSILON {
            (
                (pixel[1] / pixel[4]).round().clamp(0.0, 255.0) as u8,
                (pixel[2] / pixel[4]).round().clamp(0.0, 255.0) as u8,
                (pixel[3] / pixel[4]).round().clamp(0.0, 255.0) as u8,
            )
        } else {
            (fallback.r(), fallback.g(), fallback.b())
        };
        rgba.extend_from_slice(&[red, green, blue, ((alpha as u16 * 92) / 255) as u8]);
    }
    rgba
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
