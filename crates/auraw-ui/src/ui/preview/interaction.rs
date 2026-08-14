use super::*;

impl Preview {
    #[cfg(target_os = "android")]
    pub(super) fn handle_android_original_hold(
        ui: &Ui,
        app: &mut AurawApp,
        preview_rect: Rect,
        touch_navigation: bool,
    ) -> bool {
        const HOLD_TIME: std::time::Duration = std::time::Duration::from_millis(350);
        const MAX_STATIONARY_DISTANCE: f32 = 12.0;

        let (pressed, down, released, pointer, any_touches, multi_touch) = ui.input(|input| {
            (
                input.pointer.primary_pressed(),
                input.pointer.primary_down(),
                input.pointer.primary_released(),
                input.pointer.interact_pos(),
                input.any_touches(),
                input.multi_touch().is_some(),
            )
        });

        let allowed = !matches!(
            app.sidebar_tab,
            SidebarTab::Crop | SidebarTab::Masks | SidebarTab::Inpainting
        ) && !touch_navigation
            && !multi_touch
            && any_touches;
        if !allowed {
            app.android_original_hold = None;
            app.set_original_preview_requested(false);
            return false;
        }

        if pressed {
            if let Some(position) = pointer.filter(|position| preview_rect.contains(*position)) {
                app.android_original_hold = Some(crate::app::AndroidOriginalHold {
                    start: position,
                    started_at: std::time::Instant::now(),
                    showing_original: false,
                });
            }
        }

        let Some(hold) = app.android_original_hold else {
            return false;
        };

        let moved_too_far = pointer
            .map(|position| position.distance(hold.start) > MAX_STATIONARY_DISTANCE)
            .unwrap_or(false);
        if moved_too_far || released || !down {
            app.android_original_hold = None;
            app.set_original_preview_requested(false);
            return false;
        }

        if !hold.showing_original {
            let elapsed = hold.started_at.elapsed();
            if elapsed >= HOLD_TIME {
                if let Some(active_hold) = app.android_original_hold.as_mut() {
                    active_hold.showing_original = true;
                }
                app.set_original_preview_requested(true);
            } else {
                ui.ctx().request_repaint_after(HOLD_TIME - elapsed);
            }
        }

        true
    }

}

pub(super) fn begin_mask_drag(
    geometry: &MaskGeometry,
    uv: [f32; 2],
    pointer: Pos2,
    image_rect: Rect,
    display_geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
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
            let rotation_handle = radial_rotation_handle_geometry(
                image_rect,
                display_geometry,
                lens_geometry,
                source_width,
                source_height,
                *center,
                *radius,
                *rotation,
            );
            if rotation_handle.distance(pointer) <= 24.0 {
                return Some(MaskDragState::RotateRadial {
                    pointer_angle: source_angle_from(*center, uv, source_width, source_height),
                    rotation: *rotation,
                });
            }
            for (index, handle) in radial_handles_geometry_screen(
                image_rect,
                display_geometry,
                lens_geometry,
                source_width,
                source_height,
                *center,
                *radius,
                *rotation,
            )
            .into_iter()
            .enumerate()
            {
                if handle.distance(pointer) <= 22.0 {
                    return Some(MaskDragState::ResizeRadial { axis: index / 2 });
                }
            }

            let dx = (uv[0] - center[0]) * source_width.max(1) as f32;
            let dy = (uv[1] - center[1]) * source_height.max(1) as f32;
            let cos_r = rotation.cos();
            let sin_r = rotation.sin();
            let local_x = (cos_r * dx + sin_r * dy)
                / (radius[0].abs().max(0.005) * source_width.max(1) as f32);
            let local_y = (-sin_r * dx + cos_r * dy)
                / (radius[1].abs().max(0.005) * source_height.max(1) as f32);
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
            let a = final_geometry_native_source_to_screen(
                image_rect,
                display_geometry,
                lens_geometry,
                source_width,
                source_height,
                *start,
            );
            let b = final_geometry_native_source_to_screen(
                image_rect,
                display_geometry,
                lens_geometry,
                source_width,
                source_height,
                *end,
            );
            let (_, rotation_handle) = linear_rotation_handle_geometry(
                image_rect,
                display_geometry,
                lens_geometry,
                source_width,
                source_height,
                *start,
                *end,
            );
            if rotation_handle.distance(pointer) <= 24.0 {
                let midpoint = [(start[0] + end[0]) * 0.5, (start[1] + end[1]) * 0.5];
                Some(MaskDragState::RotateLinear {
                    pointer_angle: source_angle_from(midpoint, uv, source_width, source_height),
                    start: *start,
                    end: *end,
                })
            } else if a.distance(pointer) <= 22.0 {
                Some(MaskDragState::LinearStart)
            } else if b.distance(pointer) <= 22.0 {
                Some(MaskDragState::LinearEnd)
            } else if distance_to_polyline(
                pointer,
                &linear_axis_geometry_screen_points(
                    image_rect,
                    display_geometry,
                    lens_geometry,
                    source_width,
                    source_height,
                    *start,
                    *end,
                    32,
                ),
            ) <= 18.0
            {
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

