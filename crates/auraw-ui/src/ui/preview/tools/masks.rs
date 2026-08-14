use super::super::*;

impl Preview {
    pub(in crate::ui::preview) fn handle_mask_interaction(
        ui: &Ui,
        app: &mut AurawApp,
        image_rect: Rect,
        preview_rect: Rect,
        overlay_rect: Rect,
        source_width: u32,
        source_height: u32,
        response: &egui::Response,
    ) {
        let lens_geometry = app
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());
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
        if kind == MaskKind::Fullscreen {
            app.finish_mask_geometry_interaction();
            app.active_mask_tool = None;
            return;
        }
        let subject_refining = app.subject_refinement_active
            && matches!(kind, MaskKind::Subject | MaskKind::Background);
        app.active_mask_tool = Some(kind);
        let geometry_can_leave_image = matches!(kind, MaskKind::Radial | MaskKind::Linear)
            && (app.mask_drag.is_some()
                || app
                    .masks
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
                // Leaving the preview while still dragging must not connect the
                // last valid dab to a later re-entry point through hidden space.
                if subject_refining || matches!(kind, MaskKind::Brush | MaskKind::Object) {
                    app.last_brush_point = None;
                }
                return;
            }

            // A transformed object stroke may legitimately cross pasteboard,
            // which clears `last_brush_point` to prevent a shortcut chord when
            // the pointer re-enters. On the actual release frame, detect
            // completion from the unrefined prompt strokes themselves.
            let object_stroke_finished = primary_released
                && !subject_refining
                && kind == MaskKind::Object
                && app
                    .masks
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
            app.last_brush_point = None;
            app.mask_drag = None;
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
        let source_uv = final_geometry_screen_to_native_source(
            image_rect,
            app.geometry,
            lens_geometry.as_deref(),
            source_width,
            source_height,
            pointer,
        );
        let uv = if geometry_can_leave_image {
            source_uv
        } else if let Some(uv) = editable_source_uv(source_uv) {
            uv
        } else {
            // Brush/object strokes must be discontinuous across transformed
            // pasteboard. Otherwise a stroke that leaves and re-enters the image
            // gets interpolated through an area the user could not actually draw.
            if subject_refining || matches!(kind, MaskKind::Brush | MaskKind::Object) {
                app.last_brush_point = None;
            }
            return;
        };

        if subject_refining {
            let refinement = &mut app.masks.subject_refinement;
            let opacity = app.brush_mode.dab_opacity(true, refinement.flow);
            let first_dab = app.last_brush_point.is_none();
            let previous = app.last_brush_point.unwrap_or(uv);
            let dx = uv[0] - previous[0];
            let dy = uv[1] - previous[1];
            let previous_screen = final_geometry_native_source_to_screen(
                image_rect,
                app.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
                previous,
            );
            let distance_px = pointer.distance(previous_screen);
            let dab_size = zoom_scaled_brush_size(
                refinement.size,
                app.preview_zoom,
                app.image_relative_brush_size,
            );
            let radius_px = geometry_brush_radius_screen(
                image_rect,
                app.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
                uv,
                dab_size,
            );
            let spacing_px = (radius_px * 0.22).clamp(0.85, 24.0);
            let mut changed = false;
            if first_dab {
                if refinement.dabs.len() < 65_536 {
                    refinement.stroke_starts.push(refinement.dabs.len());
                    refinement.dabs.push(BrushDab {
                        center: uv,
                        opacity,
                        size: dab_size,
                        feather: refinement.feather,
                    });
                    changed = true;
                }
            } else if distance_px >= spacing_px * 0.80 {
                let steps = (distance_px / spacing_px).ceil().max(1.0) as usize;
                for step in 1..=steps {
                    if refinement.dabs.len() >= 65_536 {
                        break;
                    }
                    let t = step as f32 / steps as f32;
                    refinement.dabs.push(BrushDab {
                        center: [previous[0] + dx * t, previous[1] + dy * t],
                        opacity,
                        size: dab_size,
                        feather: refinement.feather,
                    });
                    changed = true;
                }
            }
            if changed {
                app.last_brush_point = Some(uv);
                app.note_subject_refinement_interaction();
                ui.ctx().request_repaint();
            }
            return;
        }
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

        if app.mask_drag.is_none() && kind != MaskKind::Brush && kind != MaskKind::Object {
            let geometry = &app.masks.masks[mask_index].components[component_index].geometry;
            app.mask_drag = begin_mask_drag(
                geometry,
                uv,
                pointer,
                image_rect,
                app.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
            );
        }

        let mut changed = false;

        if kind == MaskKind::Object && app.last_brush_point.is_none() {
            changed |= app.restart_refined_object_mask_for_stroke(mask_index, component_index);
        }

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
                        opacity_enabled,
                        opacity: brush_opacity,
                        stroke_starts,
                        dabs,
                        ..
                    },
                    MaskKind::Brush,
                ) => {
                    let opacity = app.brush_mode.dab_opacity(*opacity_enabled, *brush_opacity);
                    let first_dab = app.last_brush_point.is_none();
                    let previous = app.last_brush_point.unwrap_or(uv);
                    let dx = uv[0] - previous[0];
                    let dy = uv[1] - previous[1];
                    // Measure spacing in the transformed preview so brush density
                    // remains stable after crop, rotate, flip, and perspective.
                    let previous_screen = final_geometry_native_source_to_screen(
                        image_rect,
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        previous,
                    );
                    let distance_px = pointer.distance(previous_screen);
                    let dab_size = zoom_scaled_brush_size(
                        *size,
                        app.preview_zoom,
                        app.image_relative_brush_size,
                    );
                    let radius_px = geometry_brush_radius_screen(
                        image_rect,
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        uv,
                        dab_size,
                    );
                    let spacing_px = (radius_px * 0.22).clamp(0.85, 24.0);

                    // Pointer-down frames with no movement used to append a
                    // duplicate dab indefinitely. That made long touch holds
                    // and slow strokes progressively more expensive without
                    // changing a single mask pixel.
                    if first_dab {
                        if dabs.len() < 8192 {
                            stroke_starts.push(dabs.len());
                            dabs.push(BrushDab {
                                center: uv,
                                opacity,
                                size: dab_size,
                                feather: *feather,
                            });
                            changed = true;
                        }
                    } else if distance_px >= spacing_px * 0.80 {
                        let steps = (distance_px / spacing_px).ceil().max(1.0) as usize;
                        for step in 1..=steps {
                            if dabs.len() >= 8192 {
                                break;
                            }
                            let t = step as f32 / steps as f32;
                            dabs.push(BrushDab {
                                center: [previous[0] + dx * t, previous[1] + dy * t],
                                opacity,
                                size: dab_size,
                                feather: *feather,
                            });
                            changed = true;
                        }
                    }
                    if changed {
                        // Keep the last emitted point, not merely the last
                        // pointer sample, so sub-spacing motion accumulates
                        // instead of disappearing between frames.
                        app.last_brush_point = Some(uv);
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
                (
                    MaskGeometry::Object {
                        brush_size,
                        strokes,
                        ..
                    },
                    MaskKind::Object,
                ) => {
                    // Object strokes always start a new positive selection. A
                    // refined mask was cleared above on the first pointer-down.
                    let positive = true;
                    let first_point = app.last_brush_point.is_none();
                    let previous = app.last_brush_point.unwrap_or(uv);
                    let dx = uv[0] - previous[0];
                    let dy = uv[1] - previous[1];
                    let previous_screen = final_geometry_native_source_to_screen(
                        image_rect,
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        previous,
                    );
                    let distance_px = pointer.distance(previous_screen);
                    let stroke_brush_size = zoom_scaled_brush_size(
                        *brush_size,
                        app.preview_zoom,
                        app.image_relative_brush_size,
                    );
                    let radius_px = geometry_brush_radius_screen(
                        image_rect,
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        uv,
                        stroke_brush_size,
                    );
                    let spacing_px = (radius_px * 0.22).clamp(0.85, 24.0);
                    if first_point {
                        strokes.push(ObjectStroke {
                            points: vec![uv],
                            positive,
                            brush_size: stroke_brush_size,
                        });
                        changed = true;
                    } else if distance_px >= spacing_px * 0.75 {
                        if let Some(stroke) = strokes.last_mut() {
                            let steps = (distance_px / spacing_px).ceil().max(1.0) as usize;
                            for step in 1..=steps {
                                if stroke.points.len() >= 8192 {
                                    break;
                                }
                                let t = step as f32 / steps as f32;
                                stroke
                                    .points
                                    .push([previous[0] + dx * t, previous[1] + dy * t]);
                            }
                            changed = true;
                        }
                    }
                    if changed {
                        app.last_brush_point = Some(uv);
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
        app: &mut AurawApp,
        image_rect: Rect,
        preview_rect: Rect,
        overlay_rect: Rect,
        source_width: u32,
        source_height: u32,
    ) {
        let lens_geometry = app
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());
        let Some(mask_index) = app.masks.selected_mask else {
            return;
        };
        let Some(mask) = app.masks.masks.get(mask_index) else {
            return;
        };
        let selected_component = app.masks.selected_component;
        // Keep coverage visible when the selected type has no active rendered
        // result. Effect settings are independent from retained Adjustment
        // values, so switching types remains fully reversible.
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
            _ => true,
        };
        let accent = selected_component
            .map(mask_component_color)
            .unwrap_or(Color32::from_rgb(78, 163, 255));
        let subtract = Color32::from_rgb(255, 105, 105);
        let painter = ui.painter_at(overlay_rect);

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
                .is_some_and(|position| preview_rect.contains(position));
        if pointer_editing {
            let editing_live_mask = selected_component.is_some_and(|index| {
                app.masks.masks[mask_index]
                    .components
                    .get(index)
                    .is_some_and(|component| {
                        // Object-mask prompt strokes must remain visible while
                        // drawing even when the group already has adjustments: the
                        // painted prompt is exactly what the AI model will see.
                        component.kind == MaskKind::Object
                            || (app.subject_refinement_active
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
                        app.geometry,
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
                        app.geometry,
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
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *center,
                    );
                    painter.circle_filled(center_screen, 5.0, color);
                    for handle in radial_handles_geometry_screen(
                        image_rect,
                        app.geometry,
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
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *center,
                        *radius,
                        *rotation,
                    )[0];
                    let rotation_handle = radial_rotation_handle_geometry(
                        image_rect,
                        app.geometry,
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
                    // Linear masks are defined in native source-pixel space.
                    // Drawing their guides as straight *screen-space* lines is
                    // only valid for a similarity transform. Perspective shear
                    // and especially nonlinear Lensfun distortion turn both the
                    // axis and equal-t transition lines into warped curves. Keep
                    // the overlay derived from the exact rasterizer geometry.
                    let axis = linear_axis_geometry_screen_points(
                        image_rect,
                        app.geometry,
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
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *start,
                    );
                    let b = final_geometry_native_source_to_screen(
                        image_rect,
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        *end,
                    );
                    painter.circle_filled(a, 5.0, color);
                    painter.circle_filled(b, 5.0, color);
                    let (middle, rotation_handle) = linear_rotation_handle_geometry(
                        image_rect,
                        app.geometry,
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
                            app.geometry,
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

        let refining_subject = app.subject_refinement_active
            && app.masks.selected_component().is_some_and(|component| {
                matches!(component.kind, MaskKind::Subject | MaskKind::Background)
                    && component.enabled
            });
        if refining_subject
            || app.masks.selected_component().is_some_and(|component| {
                matches!(component.kind, MaskKind::Brush | MaskKind::Object) && component.enabled
            })
        {
            if let Some(pointer) = ui
                .ctx()
                .pointer_hover_pos()
                .or_else(|| ui.ctx().pointer_interact_pos())
                .filter(|position| preview_rect.contains(*position))
            {
                let cursor_color = match app.brush_mode {
                    BrushMode::Paint => Color32::WHITE,
                    BrushMode::Erase => subtract,
                };
                if refining_subject {
                    let source_uv = final_geometry_screen_to_native_source(
                        image_rect,
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        pointer,
                    );
                    if let Some(uv) = editable_source_uv(source_uv) {
                        let brush_size = zoom_scaled_brush_size(
                            app.masks.subject_refinement.size,
                            app.preview_zoom,
                            app.image_relative_brush_size,
                        );
                        let outline = brush_outline_geometry_screen_points(
                            image_rect,
                            app.geometry,
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
                            * (1.0 - app.masks.subject_refinement.feather.clamp(0.0, 1.0));
                        if inner_size > brush_size * 0.04 {
                            let inner = brush_outline_geometry_screen_points(
                                image_rect,
                                app.geometry,
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
                } else if let Some(component) = app.masks.selected_component() {
                    match &component.geometry {
                        MaskGeometry::Brush { size, .. } => {
                            let source_uv = final_geometry_screen_to_native_source(
                                image_rect,
                                app.geometry,
                                lens_geometry.as_deref(),
                                source_width,
                                source_height,
                                pointer,
                            );
                            if let Some(uv) = editable_source_uv(source_uv) {
                                let outline = brush_outline_geometry_screen_points(
                                    image_rect,
                                    app.geometry,
                                    lens_geometry.as_deref(),
                                    source_width,
                                    source_height,
                                    uv,
                                    zoom_scaled_brush_size(
                                        *size,
                                        app.preview_zoom,
                                        app.image_relative_brush_size,
                                    ),
                                    64,
                                );
                                painter.add(Shape::line(outline, Stroke::new(1.5, cursor_color)));
                            }
                        }
                        MaskGeometry::Object { brush_size, .. } => {
                            let source_uv = final_geometry_screen_to_native_source(
                                image_rect,
                                app.geometry,
                                lens_geometry.as_deref(),
                                source_width,
                                source_height,
                                pointer,
                            );
                            if let Some(uv) = editable_source_uv(source_uv) {
                                let outline = brush_outline_geometry_screen_points(
                                    image_rect,
                                    app.geometry,
                                    lens_geometry.as_deref(),
                                    source_width,
                                    source_height,
                                    uv,
                                    zoom_scaled_brush_size(
                                        *brush_size,
                                        app.preview_zoom,
                                        app.image_relative_brush_size,
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
        app: &mut AurawApp,
        image_rect: Rect,
        preview_rect: Rect,
        mask_index: usize,
        component_index: Option<usize>,
        source_width: u32,
        source_height: u32,
    ) {
        if app.gpu_pipeline.is_none() {
            return;
        }
        let margin = app.masks.raster_margin_pixels_for_layer(
            mask_index,
            component_index,
            source_width,
            source_height,
        );
        let region = overlay_raster_region(
            app.preview_visible_uv,
            source_width,
            source_height,
            preview_rect,
            physical_pixels_per_point(ui.ctx()),
            margin,
        );
        let key = (
            mask_index,
            component_index,
            app.mask_overlay_revision,
            region,
        );

        if app.mask_overlay_texture_key != Some(key) {
            let cropped_masks = app.masks.cropped_for_region(
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
            let _ = preview_rect;
            paint_final_geometry_overlay_texture(
                ui,
                texture.id(),
                image_rect,
                app.geometry,
                app.loaded_raw
                    .as_ref()
                    .and_then(|raw| raw.lens_geometry.as_deref()),
                source_width,
                source_height,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                overlay_source_uv(region, source_width, source_height),
            );
        }
    }

    pub(in crate::ui::preview) fn paint_tool_hint(ui: &Ui, app: &AurawApp, preview_rect: Rect) {
        let Some(kind) = app.active_mask_tool else {
            return;
        };
        let text = match kind {
            MaskKind::Subject | MaskKind::Background if app.subject_refinement_active => {
                match app.brush_mode {
                    BrushMode::Paint => "Refine: paint subject",
                    BrushMode::Erase => "Refine: subtract subject / paint background",
                }
            }
            MaskKind::Brush => return,
            MaskKind::Object
                if app.masks.selected_component().is_some_and(|component| {
                    matches!(&component.geometry, MaskGeometry::Object { mask: None, .. })
                }) =>
            {
                "Paint through the middle of the object part"
            }
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
