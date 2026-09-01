use super::super::*;
use super::brush::{
    sample_brush_stroke, BrushStrokeSamples, OBJECT_BRUSH_MINIMUM_SPACING_FRACTION,
    STANDARD_BRUSH_MINIMUM_SPACING_FRACTION,
};

impl Preview {
    pub(in crate::ui::preview) fn handle_mask_interaction(
        ui: &Ui,
        app: &mut CalibRawApp,
        image_rect: Rect,
        preview_rect: Rect,
        overlay_rect: Rect,
        source_width: u32,
        source_height: u32,
        response: &egui::Response,
    ) {
        let lens_geometry = app
            .develop
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());
        let Some(mask_index) = app.masks.stack.selected_mask else {
            app.finish_mask_geometry_interaction();
            app.masks.active_tool = None;
            return;
        };
        let Some(component_index) = app.masks.stack.selected_component else {
            app.finish_mask_geometry_interaction();
            app.masks.active_tool = None;
            return;
        };
        let Some(kind) = app
            .masks
            .stack
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .map(|component| component.kind)
        else {
            app.finish_mask_geometry_interaction();
            app.masks.active_tool = None;
            return;
        };
        if !kind.is_available() {
            return;
        }
        if kind == MaskKind::Fullscreen {
            app.finish_mask_geometry_interaction();
            app.masks.active_tool = None;
            return;
        }
        let subject_refining = app.masks.subject_refinement_active
            && matches!(kind, MaskKind::Subject | MaskKind::Background);
        app.masks.active_tool = Some(kind);
        let geometry_can_leave_image = matches!(kind, MaskKind::Radial | MaskKind::Linear)
            && (app.masks.drag.is_some()
                || app
                    .masks
                    .stack
                    .masks
                    .get(mask_index)
                    .and_then(|mask| mask.components.get(component_index))
                    .is_some_and(|component| component.geometry.is_initialized()));
        let pointer_bounds = if geometry_can_leave_image {
            overlay_rect
        } else {
            preview_rect
        };
        let pointer = response
            .interact_pointer_pos()
            .filter(|position| pointer_bounds.contains(*position));
        let (primary_is_down, primary_released) = ui.input(|input| {
            (
                input.pointer.primary_down(),
                input.pointer.primary_released(),
            )
        });
        let primary_down =
            pointer.is_some() && response.is_pointer_button_down_on() && primary_is_down;
        if !primary_down {
            if primary_is_down {
                if subject_refining || matches!(kind, MaskKind::Brush | MaskKind::Object) {
                    app.masks.last_brush_point = None;
                }
                return;
            }

            let object_stroke_finished = primary_released
                && !subject_refining
                && kind == MaskKind::Object
                && app
                    .masks
                    .stack
                    .masks
                    .get(mask_index)
                    .and_then(|mask| mask.components.get(component_index))
                    .is_some_and(|component| {
                        matches!(
                            &component.geometry,
                            MaskGeometry::Object { mask: None, strokes, .. }
                                if strokes.iter().any(|stroke| !stroke.points.is_empty())
                        )
                    });
            app.finish_mask_geometry_interaction();
            app.masks.last_brush_point = None;
            app.masks.drag = None;
            app.commit_mask_touch_gesture();
            if object_stroke_finished {
                app.request_object_mask(mask_index, component_index);
            }
            return;
        }
        let Some(pointer) = pointer else {
            return;
        };
        if ui.input(|input| input.any_touches()) {
            app.begin_mask_touch_gesture(mask_index, component_index);
        }
        let brush_tool_size = if subject_refining {
            Some(app.masks.stack.subject_refinement.size)
        } else {
            app.masks
                .stack
                .masks
                .get(mask_index)
                .and_then(|mask| mask.components.get(component_index))
                .and_then(|component| match (&component.geometry, kind) {
                    (MaskGeometry::Brush { size, .. }, MaskKind::Brush) => Some(*size),
                    (MaskGeometry::Object { brush_size, .. }, MaskKind::Object) => {
                        Some(*brush_size)
                    }
                    _ => None,
                })
        };
        let brush_samples: Option<BrushStrokeSamples> = if let Some(tool_size) = brush_tool_size {
            let sampled = sample_brush_stroke(
                image_rect,
                app.develop.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
                pointer,
                tool_size,
                app.preview.zoom,
                app.preferences.image_relative_brush_size,
                &mut app.masks.last_brush_point,
                if kind == MaskKind::Object {
                    OBJECT_BRUSH_MINIMUM_SPACING_FRACTION
                } else {
                    STANDARD_BRUSH_MINIMUM_SPACING_FRACTION
                },
            );
            if sampled.is_none() {
                return;
            }
            sampled
        } else {
            None
        };
        let uv = if let Some(stroke) = brush_samples.as_ref() {
            stroke.uv
        } else {
            let source_uv = final_geometry_screen_to_native_source(
                image_rect,
                app.develop.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
                pointer,
            );
            if geometry_can_leave_image {
                source_uv
            } else if let Some(uv) = editable_source_uv(source_uv) {
                uv
            } else {
                return;
            }
        };

        if subject_refining {
            let Some(stroke) = brush_samples.as_ref() else {
                return;
            };
            let refinement = &mut app.masks.stack.subject_refinement;
            let opacity = app.masks.brush_mode.dab_opacity(true, refinement.flow);
            let mut changed = false;
            if stroke.first && !stroke.samples.is_empty() && refinement.dabs.len() < 65_536 {
                refinement.stroke_starts.push(refinement.dabs.len());
            }
            for &center in &stroke.samples {
                if refinement.dabs.len() >= 65_536 {
                    break;
                }
                refinement.dabs.push(BrushDab {
                    center,
                    opacity,
                    size: stroke.dab_size,
                    feather: refinement.feather,
                });
                changed = true;
            }
            if changed {
                app.masks.last_brush_point = Some(stroke.uv);
                app.note_subject_refinement_interaction();
                ui.ctx().request_repaint();
            }
            return;
        }
        let color_was_sampled = app
            .masks
            .stack
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .is_some_and(|component| {
                matches!(
                    &component.geometry,
                    MaskGeometry::ColorRange { sampled: true, .. }
                )
            });

        if app.masks.drag.is_none() && kind != MaskKind::Brush && kind != MaskKind::Object {
            let geometry = &app.masks.stack.masks[mask_index].components[component_index].geometry;
            app.masks.drag = begin_mask_drag(
                geometry,
                uv,
                pointer,
                image_rect,
                app.develop.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
            );
        }

        let mut changed = false;

        if kind == MaskKind::Object && app.masks.last_brush_point.is_none() {
            changed |= app.restart_refined_object_mask_for_stroke(mask_index, component_index);
        }

        if let Some(component) = app
            .masks
            .stack
            .masks
            .get_mut(mask_index)
            .and_then(|mask| mask.components.get_mut(component_index))
        {
            match (&mut component.geometry, kind) {
                (
                    MaskGeometry::Brush {
                        feather,
                        opacity_enabled,
                        opacity: brush_opacity,
                        stroke_starts,
                        dabs,
                        ..
                    },
                    MaskKind::Brush,
                ) => {
                    let opacity = app
                        .masks
                        .brush_mode
                        .dab_opacity(*opacity_enabled, *brush_opacity);
                    let Some(stroke) = brush_samples.as_ref() else {
                        return;
                    };
                    if stroke.first && !stroke.samples.is_empty() && dabs.len() < 8192 {
                        stroke_starts.push(dabs.len());
                    }
                    for &center in &stroke.samples {
                        if dabs.len() >= 8192 {
                            break;
                        }
                        dabs.push(BrushDab {
                            center,
                            opacity,
                            size: stroke.dab_size,
                            feather: *feather,
                        });
                        changed = true;
                    }
                    if changed {
                        app.masks.last_brush_point = Some(stroke.uv);
                    }
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
                ) => match app.masks.drag {
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
                        center[0] = original_center[0] + uv[0] - origin[0];
                        center[1] = original_center[1] + uv[1] - origin[1];
                        changed = true;
                    }
                    Some(MaskDragState::ResizeRadial { axis }) => {
                        let dx = (uv[0] - center[0]) * source_width.max(1) as f32;
                        let dy = (uv[1] - center[1]) * source_height.max(1) as f32;
                        let cos_r = rotation.cos();
                        let sin_r = rotation.sin();
                        if axis == 0 {
                            radius[0] = ((cos_r * dx + sin_r * dy).abs()
                                / source_width.max(1) as f32)
                                .max(0.005);
                        } else {
                            radius[1] = ((-sin_r * dx + cos_r * dy).abs()
                                / source_height.max(1) as f32)
                                .max(0.005);
                        }
                        changed = true;
                    }
                    Some(MaskDragState::RotateRadial {
                        pointer_angle,
                        rotation: original_rotation,
                    }) => {
                        let current_angle =
                            source_angle_from(*center, uv, source_width, source_height);
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
                ) => match app.masks.drag {
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
                        let dx = uv[0] - origin[0];
                        let dy = uv[1] - origin[1];
                        *start = [original_start[0] + dx, original_start[1] + dy];
                        *end = [original_end[0] + dx, original_end[1] + dy];
                        changed = true;
                    }
                    Some(MaskDragState::RotateLinear {
                        pointer_angle,
                        start: original_start,
                        end: original_end,
                    }) => {
                        let midpoint = [
                            (original_start[0] + original_end[0]) * 0.5,
                            (original_start[1] + original_end[1]) * 0.5,
                        ];
                        let vector_x =
                            (original_end[0] - original_start[0]) * source_width.max(1) as f32;
                        let vector_y =
                            (original_end[1] - original_start[1]) * source_height.max(1) as f32;
                        let original_angle = vector_y.atan2(vector_x);
                        let current_angle =
                            source_angle_from(midpoint, uv, source_width, source_height);
                        let angle =
                            original_angle + shortest_angle_delta(pointer_angle, current_angle);
                        let half_length = (vector_x * vector_x + vector_y * vector_y).sqrt() * 0.5;
                        let half_x = angle.cos() * half_length;
                        let half_y = angle.sin() * half_length;
                        *start = [
                            midpoint[0] - half_x / source_width.max(1) as f32,
                            midpoint[1] - half_y / source_height.max(1) as f32,
                        ];
                        *end = [
                            midpoint[0] + half_x / source_width.max(1) as f32,
                            midpoint[1] + half_y / source_height.max(1) as f32,
                        ];
                        changed = true;
                    }
                    _ => {}
                },
                (MaskGeometry::Object { strokes, .. }, MaskKind::Object) => {
                    let Some(sampled) = brush_samples.as_ref() else {
                        return;
                    };
                    if sampled.first {
                        if let Some(&first) = sampled.samples.first() {
                            strokes.push(ObjectStroke {
                                points: vec![first],
                                positive: true,
                                brush_size: sampled.dab_size,
                            });
                            changed = true;
                        }
                    } else if let Some(stroke) = strokes.last_mut() {
                        let before = stroke.points.len();
                        for &point in &sampled.samples {
                            if stroke.points.len() >= 8192 {
                                break;
                            }
                            stroke.points.push(point);
                        }
                        changed |= stroke.points.len() != before;
                    }
                    if changed {
                        app.masks.last_brush_point = Some(sampled.uv);
                    }
                }
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

    pub(in crate::ui::preview) fn paint_mask_overlay(
        ui: &Ui,
        app: &mut CalibRawApp,
        image_rect: Rect,
        preview_rect: Rect,
        overlay_rect: Rect,
        source_width: u32,
        source_height: u32,
    ) {
        let lens_geometry = app
            .develop
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());
        let Some(mask_index) = app.masks.stack.selected_mask else {
            return;
        };
        let Some(mask) = app.masks.stack.masks.get(mask_index) else {
            return;
        };
        let selected_component = app.masks.stack.selected_component;
        let neutral = match mask.effect {
            crate::pipeline::MaskEffect::Adjustment => mask.adjustments.is_neutral(),
            crate::pipeline::MaskEffect::Blur => !mask.effect_settings.blur.is_active(),
            crate::pipeline::MaskEffect::LensBlur => !mask.effect_settings.lens_blur.is_active(),
            crate::pipeline::MaskEffect::MotionBlur => {
                !mask.effect_settings.motion_blur.is_active()
            }
            crate::pipeline::MaskEffect::RadialBlur => {
                !mask.effect_settings.radial_blur.is_active()
            }
            crate::pipeline::MaskEffect::TiltShift => !mask.effect_settings.tilt_shift.is_active(),
            crate::pipeline::MaskEffect::EdgeGlow => !mask.effect_settings.edge_glow.is_active(),
            crate::pipeline::MaskEffect::Glow => !mask.effect_settings.glow.is_active(),
            crate::pipeline::MaskEffect::LightRays => !mask.effect_settings.light_rays.is_active(),
            crate::pipeline::MaskEffect::Neon => !mask.effect_settings.neon.is_active(),
            crate::pipeline::MaskEffect::Pixelate => !mask.effect_settings.pixelate.is_active(),
            crate::pipeline::MaskEffect::Fog => !mask.effect_settings.fog.is_active(),
            crate::pipeline::MaskEffect::Smoke => !mask.effect_settings.smoke.is_active(),
        };
        let accent = selected_component
            .map(mask_component_color)
            .unwrap_or(crate::ui::theme::MASK_ADD);
        let subtract = crate::ui::theme::MASK_SUBTRACT;
        let painter = ui.painter_at(overlay_rect);

        let steady_target: Option<Option<usize>> = neutral.then_some(None);
        let mut coverage_target = steady_target;
        if let Some((started, blink)) = app.masks.overlay_blink {
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
                    app.masks.overlay_blink = None;
                    steady_target
                }
            };
            if app.masks.overlay_blink.is_some() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(25));
            }
        }
        let pointer_editing = ui.input(|input| input.pointer.primary_down())
            && ui
                .ctx()
                .pointer_interact_pos()
                .is_some_and(|position| preview_rect.contains(position));
        if pointer_editing {
            let editing_live_mask = selected_component.is_some_and(|index| {
                app.masks.stack.masks[mask_index]
                    .components
                    .get(index)
                    .is_some_and(|component| {
                        component.kind == MaskKind::Object
                            || (app.masks.subject_refinement_active
                                && matches!(
                                    component.kind,
                                    MaskKind::Subject | MaskKind::Background
                                ))
                            || (neutral
                                && !matches!(
                                    component.kind,
                                    MaskKind::Subject | MaskKind::Background
                                ))
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
                Self::paint_coverage_texture(
                    ui,
                    app,
                    image_rect,
                    preview_rect,
                    mask_index,
                    component,
                    source_width,
                    source_height,
                );
            }
        }

        if let Some(component) = selected_component.and_then(|index| {
            app.masks
                .stack
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
                    let outer = radial_outline_geometry_screen_points(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *center,
                        *radius,
                        *rotation,
                        72,
                    );
                    painter.add(egui::Shape::line(outer, Stroke::new(2.0, color)));
                    let inner_scale = 1.0 - feather.clamp(0.0, 1.0) * 0.98;
                    let inner = radial_outline_geometry_screen_points(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *center,
                        [radius[0] * inner_scale, radius[1] * inner_scale],
                        *rotation,
                        72,
                    );
                    painter.add(egui::Shape::line(
                        inner,
                        Stroke::new(1.0, color.gamma_multiply(0.65)),
                    ));
                    let center_screen = final_geometry_native_source_to_screen(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *center,
                    );
                    painter.circle_filled(center_screen, 5.0, color);
                    for handle in radial_handles_geometry_screen(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *center,
                        *radius,
                        *rotation,
                    ) {
                        painter.circle_filled(handle, 4.0, color);
                    }
                    let major_handle = radial_handles_geometry_screen(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *center,
                        *radius,
                        *rotation,
                    )[0];
                    let rotation_handle = radial_rotation_handle_geometry(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *center,
                        *radius,
                        *rotation,
                    );
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
                    let axis = linear_axis_geometry_screen_points(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *start,
                        *end,
                        48,
                    );
                    painter.add(Shape::line(axis, Stroke::new(2.0, color)));
                    let a = final_geometry_native_source_to_screen(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *start,
                    );
                    let b = final_geometry_native_source_to_screen(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *end,
                    );
                    painter.circle_filled(a, 5.0, color);
                    painter.circle_filled(b, 5.0, color);
                    let (middle, rotation_handle) = linear_rotation_handle_geometry(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *start,
                        *end,
                    );
                    painter.line_segment(
                        [middle, rotation_handle],
                        Stroke::new(1.0, color.gamma_multiply(0.72)),
                    );
                    painter.circle_stroke(rotation_handle, 6.0, Stroke::new(2.0, color));

                    let width_factor = feather.clamp(0.02, 1.0);
                    for t in [0.5 - 0.5 * width_factor, 0.5 + 0.5 * width_factor] {
                        let boundary = linear_isot_geometry_screen_points(
                            image_rect,
                            app.develop.geometry,
                            lens_geometry.as_deref(),
                            source_width,
                            source_height,
                            *start,
                            *end,
                            t,
                            64,
                        );
                        painter.add(Shape::line(
                            boundary,
                            Stroke::new(1.0, color.gamma_multiply(0.65)),
                        ));
                    }
                }
                _ => {}
            }
        }

        let refining_subject = app.masks.subject_refinement_active
            && app
                .masks
                .stack
                .selected_component()
                .is_some_and(|component| {
                    matches!(component.kind, MaskKind::Subject | MaskKind::Background)
                        && component.enabled
                });
        if refining_subject
            || app
                .masks
                .stack
                .selected_component()
                .is_some_and(|component| {
                    matches!(component.kind, MaskKind::Brush | MaskKind::Object)
                        && component.enabled
                })
        {
            if let Some(pointer) = ui
                .ctx()
                .pointer_hover_pos()
                .or_else(|| ui.ctx().pointer_interact_pos())
                .filter(|position| preview_rect.contains(*position))
            {
                let cursor_color = match app.masks.brush_mode {
                    BrushMode::Paint => Color32::WHITE,
                    BrushMode::Erase => subtract,
                };
                if refining_subject {
                    let source_uv = final_geometry_screen_to_native_source(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        pointer,
                    );
                    if let Some(uv) = editable_source_uv(source_uv) {
                        let brush_size = zoom_scaled_brush_size(
                            app.masks.stack.subject_refinement.size,
                            app.preview.zoom,
                            app.preferences.image_relative_brush_size,
                        );
                        let outline = brush_outline_geometry_screen_points(
                            image_rect,
                            app.develop.geometry,
                            lens_geometry.as_deref(),
                            source_width,
                            source_height,
                            uv,
                            brush_size,
                            64,
                        );
                        let cursor_painter = ui.painter_at(preview_rect.intersect(image_rect));
                        cursor_painter.add(Shape::line(outline, Stroke::new(1.5, cursor_color)));
                        let inner_size = brush_size
                            * (1.0 - app.masks.stack.subject_refinement.feather.clamp(0.0, 1.0));
                        if inner_size > brush_size * 0.04 {
                            let inner = brush_outline_geometry_screen_points(
                                image_rect,
                                app.develop.geometry,
                                lens_geometry.as_deref(),
                                source_width,
                                source_height,
                                uv,
                                inner_size,
                                64,
                            );
                            cursor_painter.add(Shape::line(
                                inner,
                                Stroke::new(1.0, cursor_color.gamma_multiply(0.65)),
                            ));
                        }
                    }
                } else if let Some(component) = app.masks.stack.selected_component() {
                    match &component.geometry {
                        MaskGeometry::Brush { size, .. } => {
                            let source_uv = final_geometry_screen_to_native_source(
                                image_rect,
                                app.develop.geometry,
                                lens_geometry.as_deref(),
                                source_width,
                                source_height,
                                pointer,
                            );
                            if let Some(uv) = editable_source_uv(source_uv) {
                                let outline = brush_outline_geometry_screen_points(
                                    image_rect,
                                    app.develop.geometry,
                                    lens_geometry.as_deref(),
                                    source_width,
                                    source_height,
                                    uv,
                                    zoom_scaled_brush_size(
                                        *size,
                                        app.preview.zoom,
                                        app.preferences.image_relative_brush_size,
                                    ),
                                    64,
                                );
                                painter.add(Shape::line(outline, Stroke::new(1.5, cursor_color)));
                            }
                        }
                        MaskGeometry::Object { brush_size, .. } => {
                            let source_uv = final_geometry_screen_to_native_source(
                                image_rect,
                                app.develop.geometry,
                                lens_geometry.as_deref(),
                                source_width,
                                source_height,
                                pointer,
                            );
                            if let Some(uv) = editable_source_uv(source_uv) {
                                let outline = brush_outline_geometry_screen_points(
                                    image_rect,
                                    app.develop.geometry,
                                    lens_geometry.as_deref(),
                                    source_width,
                                    source_height,
                                    uv,
                                    zoom_scaled_brush_size(
                                        *brush_size,
                                        app.preview.zoom,
                                        app.preferences.image_relative_brush_size,
                                    ),
                                    64,
                                );
                                painter.add(Shape::line(outline, Stroke::new(1.5, cursor_color)));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    pub(in crate::ui::preview) fn paint_coverage_texture(
        ui: &Ui,
        app: &mut CalibRawApp,
        image_rect: Rect,
        preview_rect: Rect,
        mask_index: usize,
        component_index: Option<usize>,
        source_width: u32,
        source_height: u32,
    ) {
        if app.preview.gpu_pipeline.is_none() {
            return;
        }
        let margin = app.masks.stack.raster_margin_pixels_for_layer(
            mask_index,
            component_index,
            source_width,
            source_height,
        );
        let region = overlay_raster_region(
            app.preview.visible_uv,
            source_width,
            source_height,
            preview_rect,
            physical_pixels_per_point(ui.ctx()),
            margin,
        );
        let key = (
            mask_index,
            component_index,
            app.masks.overlay_revision,
            region,
        );

        if app.masks.overlay_texture_key != Some(key) {
            let cropped_masks = app.masks.stack.cropped_for_region(
                region.source_x,
                region.source_y,
                region.source_width,
                region.source_height,
                source_width,
                source_height,
            );
            let rgba = if let Some(component_index) = component_index {
                let coverage = cropped_masks.rasterize_component_layer(
                    mask_index,
                    component_index,
                    region.texture_width,
                    region.texture_height,
                    region.source_width,
                    region.source_height,
                );
                coverage_rgba(coverage, mask_component_color(component_index))
            } else {
                group_coverage_rgba(
                    &cropped_masks,
                    mask_index,
                    region.texture_width,
                    region.texture_height,
                    region.source_width,
                    region.source_height,
                )
            };
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [
                    region.texture_width as usize,
                    region.texture_height as usize,
                ],
                &rgba,
            );
            if let Some(texture) = app.masks.overlay_texture.as_mut() {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                app.masks.overlay_texture = Some(ui.ctx().load_texture(
                    "selected-mask-coverage",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
            app.masks.overlay_texture_key = Some(key);
        }

        if let Some(texture) = &app.masks.overlay_texture {
            paint_final_geometry_overlay_texture(
                ui,
                texture.id(),
                image_rect,
                app.develop.geometry,
                app.develop
                    .loaded_raw
                    .as_ref()
                    .and_then(|raw| raw.lens_geometry.as_deref()),
                source_width,
                source_height,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                overlay_source_uv(region, source_width, source_height),
            );
        }
    }

    pub(in crate::ui::preview) fn paint_tool_hint(ui: &Ui, app: &CalibRawApp, preview_rect: Rect) {
        let Some(kind) = app.masks.active_tool else {
            return;
        };
        let text = match kind {
            MaskKind::Subject | MaskKind::Background if app.masks.subject_refinement_active => {
                match app.masks.brush_mode {
                    BrushMode::Paint => "Refine: paint subject",
                    BrushMode::Erase => "Refine: subtract subject / paint background",
                }
            }
            MaskKind::Brush => return,
            MaskKind::Object
                if app
                    .masks
                    .stack
                    .selected_component()
                    .is_some_and(|component| {
                        matches!(&component.geometry, MaskGeometry::Object { mask: None, .. })
                    }) =>
            {
                "Paint through the middle of the object part"
            }
            MaskKind::Radial
                if !app
                    .masks
                    .stack
                    .selected_component()
                    .is_some_and(|component| component.geometry.is_initialized()) =>
            {
                "Drag from the center to create a radial gradient"
            }
            MaskKind::Linear
                if !app
                    .masks
                    .stack
                    .selected_component()
                    .is_some_and(|component| component.geometry.is_initialized()) =>
            {
                "Drag across the image to create a linear gradient"
            }
            MaskKind::ColorRange
                if !app
                    .masks
                    .stack
                    .selected_component()
                    .is_some_and(|component| {
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
        let painter = ui.painter_at(preview_rect);
        let position = preview_rect.left_top() + egui::vec2(12.0, 12.0);
        painter.text(
            position,
            egui::Align2::LEFT_TOP,
            text,
            egui::FontId::proportional(13.0),
            Color32::WHITE,
        );
    }
}
