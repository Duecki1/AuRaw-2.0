#![allow(clippy::too_many_arguments)]

use crate::app::{
    AurawApp, CropDragState, CropHandle, MaskDragState, MaskOverlayBlink, OverlayRasterKey,
    SidebarTab, StraightenDragState,
};
use crate::pipeline::{
    rasterize_inpaint_dabs_binary, BrushDab, BrushMode, GeometryTransform, LensGeometryMap,
    MaskCombineMode, MaskGeometry, MaskKind, ObjectStroke,
};
use crate::ui::mask_component_color;
use eframe::egui::{self, Color32, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui};

const MIN_PREVIEW_ZOOM: f32 = 0.70;
const MAX_PREVIEW_ZOOM: f32 = 32.0;

fn physical_pixels_per_point(ctx: &egui::Context) -> f32 {
    let native = ctx.input(|input| input.viewport().native_pixels_per_point);
    native
        .unwrap_or_else(|| ctx.pixels_per_point())
        .max(ctx.pixels_per_point())
        .max(0.1)
}

fn white_balance_picker_owns_canvas(sidebar_tab: SidebarTab, picker_active: bool) -> bool {
    sidebar_tab == SidebarTab::Adjustments && picker_active
}

pub struct Preview;

impl Preview {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        let available = ui.available_size();
        if app.preview_base_pipeline().is_none() && app.preview_is_preparing() {
            app.refresh_develop_loading_thumbnail(ui.ctx());
        }
        let base_pipeline = app.preview_base_pipeline().and_then(|pipeline| {
            pipeline
                .egui_texture_id
                .map(|texture_id| (texture_id, pipeline.width, pipeline.height))
        });
        if base_pipeline.is_none() && available.x > 0.0 && available.y > 0.0 {
            let pixels_per_point = physical_pixels_per_point(ui.ctx());
            app.set_preview_viewport_pixels([
                (available.x * pixels_per_point).round().max(1.0) as u32,
                (available.y * pixels_per_point).round().max(1.0) as u32,
            ]);
        }

        let Some((texture_id, pipeline_width, pipeline_height)) = base_pipeline else {
            if app.preview_is_preparing() {
                if show_loading_thumbnail(ui, app, available) {
                    return;
                }
                ui.centered_and_justified(|ui| {
                    ui.spinner();
                    ui.label("Preparing preview…");
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("No image open");
                        ui.label("Open a RAW from the Library to start developing.");
                    });
                });
            }
            return;
        };

        if available.x <= 0.0 || available.y <= 0.0 || pipeline_height == 0 {
            return;
        }

        let (outer_rect, _) = ui.allocate_exact_size(available, Sense::hover());
        // Anchor zoom geometry to the full developed image, independent of proxy size.
        let source_dimensions = app
            .loaded_raw
            .as_ref()
            .map(|raw| (raw.width, raw.height))
            .unwrap_or((pipeline_width, pipeline_height));
        let lens_geometry = app
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());
        let crop_preview = app.sidebar_tab == SidebarTab::Crop && !app.original_preview_visible();
        // *start = screen_to_normalized_unclamped(image_rect, midpoint - half_vector);
        // *end = screen_to_normalized_unclamped(image_rect, midpoint + half_vector);
        // Every non-Crop Develop surface uses the same final geometry frame that
        // export writes. Mask and inpainting interactions are inverse-mapped back
        // into source coordinates below, so their tools remain accurate while the
        // pixels and overlays stay aligned with crop/rotation/flip/transform.
        let final_geometry_preview =
            !crop_preview && (!app.geometry.is_identity() || lens_geometry.is_some());
        let (geometry_width, geometry_height) = if final_geometry_preview {
            app.geometry
                .crop_pixel_dimensions(source_dimensions.0, source_dimensions.1)
        } else if crop_preview && app.geometry.quarter_turns % 2 == 1 {
            (source_dimensions.1, source_dimensions.0)
        } else {
            source_dimensions
        };
        let base_size = fitted_image_size(
            outer_rect.size(),
            geometry_width as f32 / geometry_height.max(1) as f32,
        );
        app.preview_zoom = app.preview_zoom.clamp(MIN_PREVIEW_ZOOM, MAX_PREVIEW_ZOOM);
        clamp_preview_center(
            &mut app.preview_center,
            outer_rect.size(),
            base_size * app.preview_zoom,
        );
        let mut image_rect =
            zoomed_image_rect(outer_rect, base_size, app.preview_zoom, app.preview_center);
        let visible_image_rect = outer_rect.intersect(image_rect);
        let mut interaction_rect = if app.sidebar_tab == SidebarTab::Masks {
            // Geometry handles for radial/linear masks are allowed to live in
            // the pasteboard around the image, so the mask canvas must receive
            // pointer input across the whole preview panel. Brush-like tools
            // still filter their pointer to the visible image below.
            outer_rect
        } else if app.sidebar_tab == SidebarTab::Crop {
            // Crop edge/corner hit targets deliberately extend into the pasteboard,
            // which is especially important for finger input near image boundaries.
            outer_rect
        } else {
            visible_image_rect
        };
        if interaction_rect.width() <= 0.0 || interaction_rect.height() <= 0.0 {
            interaction_rect = outer_rect;
        }
        // Desktop shows every adjustment category as an accordion, so its
        // `adjustment_section` retains the mobile navigation selection and is
        // not evidence that the Color accordion is closed. Once the eyedropper
        // is armed it owns the Adjustments preview regardless of that mobile-
        // only section state.
        let white_balance_canvas =
            white_balance_picker_owns_canvas(app.sidebar_tab, app.white_balance_picker_active);
        if !white_balance_canvas {
            app.white_balance_picker_drag = None;
        }
        let brush_canvas = matches!(app.sidebar_tab, SidebarTab::Masks | SidebarTab::Inpainting)
            || white_balance_canvas;
        let interaction_id = match app.sidebar_tab {
            SidebarTab::Masks => ui.id().with("develop-preview-mask-interaction"),
            SidebarTab::Inpainting => ui.id().with("develop-preview-inpaint-interaction"),
            SidebarTab::Adjustments if white_balance_canvas => {
                ui.id().with("develop-preview-white-balance-interaction")
            }
            _ => ui.id().with("develop-preview-interaction"),
        };
        let interaction_sense = if brush_canvas {
            Sense::drag()
        } else {
            Sense::click_and_drag()
        };
        let response = ui.interact(interaction_rect, interaction_id, interaction_sense);

        let mut moved = false;
        let (multi_touch, any_touches) = ui.input(|input| {
            (
                input.multi_touch().filter(|multi_touch| {
                    outer_rect.contains(multi_touch.start_pos)
                        || outer_rect.contains(multi_touch.center_pos)
                }),
                input.any_touches(),
            )
        });
        #[cfg(target_os = "android")]
        if any_touches {
            // NativeActivity is event-driven, while Android may batch motion
            // samples. Keep navigation repainting at the surface cadence until
            // the last finger lifts so intermediate pinch/pan states stay fluid.
            ui.ctx().request_repaint();
        }
        if multi_touch.is_some() {
            app.preview_touch_navigation_active = true;
        } else if !any_touches {
            app.preview_touch_navigation_active = false;
        }
        let touch_navigation = app.preview_touch_navigation_active;

        if let Some(multi_touch) = multi_touch {
            // Keep the image point that was under the previous gesture center under
            // the current center. This combines pinch zooming and two-finger panning
            // without accumulating a separate touch-only camera state.
            let previous_touch_center = multi_touch.center_pos - multi_touch.translation_delta;
            moved |= transform_preview_about_screen_points(
                outer_rect,
                image_rect,
                base_size,
                &mut app.preview_zoom,
                &mut app.preview_center,
                previous_touch_center,
                multi_touch.center_pos,
                multi_touch.zoom_delta,
            );
            image_rect =
                zoomed_image_rect(outer_rect, base_size, app.preview_zoom, app.preview_center);
        }

        if multi_touch.is_some() {
            // A second finger switches a mask gesture into viewport navigation.
            // Roll back any pending mask stroke and prevent this frame from painting.
            if app.sidebar_tab == SidebarTab::Masks {
                app.cancel_mask_touch_gesture();
            } else if app.sidebar_tab == SidebarTab::Crop {
                app.crop_drag = None;
                app.straighten_drag = None;
            } else if app.sidebar_tab == SidebarTab::Inpainting {
                app.inpaint_stroke.clear();
                app.last_inpaint_brush_point = None;
                app.inpaint_stroke_texture = None;
                app.inpaint_stroke_texture_key = None;
            } else if white_balance_canvas {
                app.white_balance_picker_drag = None;
            }
        }

        #[cfg(target_os = "android")]
        let original_hold_tracking =
            Self::handle_android_original_hold(ui, app, interaction_rect, touch_navigation);
        #[cfg(not(target_os = "android"))]
        let original_hold_tracking = false;

        if !touch_navigation && response.hovered() {
            let scroll_y = ui.input(|input| input.smooth_scroll_delta.y);
            if scroll_y.abs() > 0.01 {
                let pointer = ui
                    .input(|input| input.pointer.hover_pos())
                    .unwrap_or(outer_rect.center());
                moved |= transform_preview_about_screen_points(
                    outer_rect,
                    image_rect,
                    base_size,
                    &mut app.preview_zoom,
                    &mut app.preview_center,
                    pointer,
                    pointer,
                    (scroll_y * 0.0018).exp(),
                );
            }
        }

        // Once a pinch drops back to one finger, resume ordinary panning
        // immediately. `touch_navigation` deliberately stays latched until all
        // fingers lift so brush/crop gestures cannot restart halfway through a
        // pinch, but that latch must not freeze the viewport itself.
        let pan_with_primary = multi_touch.is_none()
            && !original_hold_tracking
            && !brush_canvas
            && app.sidebar_tab != SidebarTab::Crop
            && response.dragged_by(egui::PointerButton::Primary);
        let pan_with_middle = !touch_navigation && response.dragged_by(egui::PointerButton::Middle);
        if pan_with_primary || pan_with_middle {
            let delta = ui.input(|input| input.pointer.delta());
            let image_size = base_size * app.preview_zoom;
            app.preview_center[0] -= delta.x / image_size.x.max(1.0);
            app.preview_center[1] -= delta.y / image_size.y.max(1.0);
            clamp_preview_center(&mut app.preview_center, outer_rect.size(), image_size);
            moved |= delta.length_sq() > 0.0;
        }

        let fit_gesture = !white_balance_canvas && !touch_navigation && response.double_clicked();
        if fit_gesture {
            app.preview_zoom = 1.0;
            app.preview_center = [0.5, 0.5];
            moved = true;
        }

        image_rect = zoomed_image_rect(outer_rect, base_size, app.preview_zoom, app.preview_center);
        let visible_screen = outer_rect.intersect(image_rect);
        let pixels_per_point = physical_pixels_per_point(ui.ctx());
        let viewport_pixels = [
            (visible_screen.width() * pixels_per_point).round().max(1.0) as u32,
            (visible_screen.height() * pixels_per_point)
                .round()
                .max(1.0) as u32,
        ];
        app.set_preview_viewport_pixels(viewport_pixels);
        let visible_uv = if crop_preview {
            crop_workspace_visible_source_uv(
                image_rect,
                visible_screen,
                app.geometry,
                lens_geometry.as_deref(),
                source_dimensions.0,
                source_dimensions.1,
            )
        } else if final_geometry_preview {
            final_geometry_visible_source_uv(
                image_rect,
                visible_screen,
                app.geometry,
                lens_geometry.as_deref(),
                source_dimensions.0,
                source_dimensions.1,
            )
        } else {
            crate::app::PreviewUvRect {
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
            }
        };
        if preview_uv_changed(app.preview_visible_uv, visible_uv) {
            app.preview_visible_uv = visible_uv;
            app.preview_source_region_changed();
        }
        if moved {
            app.note_preview_motion();
        }
        let painter = ui.painter_at(outer_rect);
        if crop_preview {
            paint_crop_workspace_texture(
                ui,
                texture_id,
                image_rect,
                app.geometry,
                lens_geometry.as_deref(),
                source_dimensions.0,
                source_dimensions.1,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                [0.0, 0.0, 1.0, 1.0],
            );
        } else if final_geometry_preview {
            paint_final_geometry_texture(
                ui,
                texture_id,
                image_rect,
                app.geometry,
                lens_geometry.as_deref(),
                source_dimensions.0,
                source_dimensions.1,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                [0.0, 0.0, 1.0, 1.0],
            );
        } else {
            painter.image(
                texture_id,
                image_rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        if let Some(detail) = app
            .preview_detail
            .as_ref()
            .filter(|detail| detail.revision == app.preview_revision)
        {
            if let Some(detail_texture_id) = detail.pipeline.egui_texture_id {
                let detail_texture_uv = Rect::from_min_max(
                    Pos2::new(detail.texture_uv_rect.min[0], detail.texture_uv_rect.min[1]),
                    Pos2::new(detail.texture_uv_rect.max[0], detail.texture_uv_rect.max[1]),
                );
                let detail_source_uv = [
                    detail.uv_rect.min[0],
                    detail.uv_rect.min[1],
                    detail.uv_rect.max[0],
                    detail.uv_rect.max[1],
                ];
                if crop_preview {
                    paint_crop_workspace_texture(
                        ui,
                        detail_texture_id,
                        image_rect,
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_dimensions.0,
                        source_dimensions.1,
                        detail_texture_uv,
                        detail_source_uv,
                    );
                } else if final_geometry_preview {
                    paint_final_geometry_texture(
                        ui,
                        detail_texture_id,
                        image_rect,
                        app.geometry,
                        lens_geometry.as_deref(),
                        source_dimensions.0,
                        source_dimensions.1,
                        detail_texture_uv,
                        detail_source_uv,
                    );
                } else {
                    let detail_rect = Rect::from_min_max(
                        normalized_to_screen(image_rect, detail.uv_rect.min),
                        normalized_to_screen(image_rect, detail.uv_rect.max),
                    );
                    painter.image(
                        detail_texture_id,
                        detail_rect,
                        detail_texture_uv,
                        Color32::WHITE,
                    );
                }
            }
        }

        if crop_preview {
            if !touch_navigation && !fit_gesture {
                Self::handle_crop_interaction(
                    ui,
                    app,
                    image_rect,
                    source_dimensions.0,
                    source_dimensions.1,
                );
            }
            Self::paint_crop_overlay(
                ui,
                app,
                image_rect,
                visible_screen,
                outer_rect,
                source_dimensions.0,
                source_dimensions.1,
            );
        }

        #[cfg(not(target_os = "android"))]
        painter.text(
            outer_rect.left_top() + egui::vec2(10.0, 10.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{:.1}% · pinch/scroll zoom · drag pan · double-tap/click fit",
                ((image_rect.width() * physical_pixels_per_point(ui.ctx())
                    / geometry_width.max(1) as f32)
                    .min(
                        image_rect.height() * physical_pixels_per_point(ui.ctx())
                            / geometry_height.max(1) as f32,
                    )
                    * 100.0)
            ),
            egui::FontId::proportional(11.0),
            Color32::from_white_alpha(180),
        );

        if app.original_preview_visible() {
            painter.text(
                outer_rect.right_top() + egui::vec2(-12.0, 12.0),
                egui::Align2::RIGHT_TOP,
                "ORIGINAL",
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
        }

        if !app.original_preview_visible() {
            if app.sidebar_tab == SidebarTab::Inpainting && !touch_navigation && !fit_gesture {
                Self::handle_inpaint_interaction(
                    ui,
                    app,
                    frame,
                    image_rect,
                    visible_screen,
                    source_dimensions.0,
                    source_dimensions.1,
                    &response,
                );
            }
            // Completed inpainting is part of the developed image and remains
            // visible while switching between Develop tabs. The live stroke
            // and cursor are shown only while the Inpainting tab is active.
            Self::paint_inpaint_overlay(
                ui,
                app,
                image_rect,
                visible_screen,
                source_dimensions.0,
                source_dimensions.1,
            );

            if white_balance_canvas {
                if !touch_navigation {
                    Self::handle_white_balance_picker(
                        ui,
                        app,
                        image_rect,
                        visible_screen,
                        source_dimensions.0,
                        source_dimensions.1,
                        &response,
                    );
                }
                Self::paint_white_balance_picker(
                    ui,
                    app,
                    image_rect,
                    visible_screen,
                    source_dimensions.0,
                    source_dimensions.1,
                );
            }

            if app.sidebar_tab == SidebarTab::Masks {
                if !touch_navigation && !fit_gesture {
                    Self::handle_mask_interaction(
                        ui,
                        app,
                        image_rect,
                        visible_screen,
                        outer_rect,
                        source_dimensions.0,
                        source_dimensions.1,
                        &response,
                    );
                }
                // Coverage stays clipped to the image, while geometry/transform
                // handles may extend into the surrounding preview pasteboard.
                Self::paint_mask_overlay(
                    ui,
                    app,
                    image_rect,
                    visible_screen,
                    outer_rect,
                    source_dimensions.0,
                    source_dimensions.1,
                );
                Self::paint_tool_hint(ui, app, visible_screen);
            }
        }
    }

    fn handle_white_balance_picker(
        ui: &Ui,
        app: &mut AurawApp,
        image_rect: Rect,
        preview_rect: Rect,
        source_width: u32,
        source_height: u32,
        response: &egui::Response,
    ) {
        let lens_geometry = app
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
                app.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
                position,
            ))
        });

        if pressed {
            if let Some(uv) = pointer_uv {
                app.white_balance_picker_drag = Some([uv, uv]);
            }
        } else if down {
            if let (Some(area), Some(uv)) = (app.white_balance_picker_drag.as_mut(), pointer_uv) {
                area[1] = uv;
                ui.ctx().request_repaint();
            }
        }

        if released {
            if let Some(mut area) = app.white_balance_picker_drag.take() {
                if let Some(uv) = pointer_uv {
                    area[1] = uv;
                }
                app.apply_white_balance_area(area);
            }
        }
    }

    fn paint_white_balance_picker(
        ui: &Ui,
        app: &AurawApp,
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
        let Some(area) = app.white_balance_picker_drag else {
            return;
        };
        let lens_geometry = app
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.as_deref());
        let start = final_geometry_native_source_to_screen(
            image_rect,
            app.geometry,
            lens_geometry,
            source_width,
            source_height,
            area[0],
        );
        let current = final_geometry_native_source_to_screen(
            image_rect,
            app.geometry,
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

    fn handle_crop_interaction(
        ui: &mut Ui,
        app: &mut AurawApp,
        image_rect: Rect,
        source_width: u32,
        source_height: u32,
    ) {
        if image_rect.width() <= 1.0 || image_rect.height() <= 1.0 {
            return;
        }
        let pointer = ui.input(|input| input.pointer.interact_pos());
        let primary_pressed = ui.input(|input| input.pointer.primary_pressed());
        let primary_down = ui.input(|input| input.pointer.primary_down());
        let primary_released = ui.input(|input| input.pointer.primary_released());
        let quarter_turns = app.geometry.quarter_turns % 4;

        if app.straighten_tool_active {
            if primary_pressed {
                if let Some(pointer) = pointer.filter(|point| image_rect.contains(*point)) {
                    let uv = crop_workspace_screen_to_source(
                        image_rect,
                        app.geometry,
                        source_width,
                        source_height,
                        pointer,
                    );
                    if source_uv_inside_image(uv) {
                        app.straighten_drag = Some(StraightenDragState {
                            start: pointer,
                            current: pointer,
                        });
                        app.crop_drag = None;
                    }
                }
            }
            if primary_down {
                if let (Some(pointer), Some(mut drag)) = (pointer, app.straighten_drag) {
                    let uv = crop_workspace_screen_to_source(
                        image_rect,
                        app.geometry,
                        source_width,
                        source_height,
                        pointer,
                    );
                    if source_uv_inside_image(uv) {
                        drag.current = pointer;
                        app.straighten_drag = Some(drag);
                    }
                }
            }
            if primary_released {
                if let Some(drag) = app.straighten_drag.take() {
                    let delta = drag.current - drag.start;
                    if delta.length() >= 12.0 {
                        let angle = delta.y.atan2(delta.x).to_degrees();
                        let target = nearest_straight_axis_degrees(angle);
                        let correction = normalize_degrees(target - angle);
                        let previous = app.geometry.rotation_degrees;
                        app.geometry.rotation_degrees = (previous + correction).clamp(-45.0, 45.0);
                        if (app.geometry.rotation_degrees - previous).abs() > 1e-4 {
                            let reference = if let Some(reference) = app.crop_constraint_reference {
                                reference
                            } else {
                                let reference = app.geometry.crop;
                                app.crop_constraint_reference = Some(reference);
                                reference
                            };
                            app.geometry.crop = reference;
                            app.geometry
                                .fit_crop_inside_transformed_source(source_width, source_height);
                            app.note_geometry_changed();
                        }
                    }
                }
            } else if !primary_down {
                app.straighten_drag = None;
            }
            return;
        }

        if primary_pressed {
            if let Some(pointer) = pointer.filter(|point| image_rect.expand(28.0).contains(*point))
            {
                let display_crop_rect =
                    crop_preview_screen_rect(image_rect, app.geometry, source_width, source_height);
                if let Some(display_handle) = crop_handle_at(display_crop_rect, pointer, 28.0) {
                    let handle = crop_source_handle_for_display(display_handle, quarter_turns);
                    let start = crop_preview_pointer_to_source_normalized(
                        image_rect,
                        quarter_turns,
                        source_width,
                        source_height,
                        pointer,
                    );
                    app.crop_drag = Some(CropDragState {
                        handle,
                        start,
                        crop: app.geometry.crop,
                    });
                }
            }
        }

        if primary_down {
            if let (Some(pointer), Some(drag)) = (pointer, app.crop_drag) {
                let current = crop_preview_pointer_to_source_normalized(
                    image_rect,
                    quarter_turns,
                    source_width,
                    source_height,
                    pointer,
                );
                let delta = [current[0] - drag.start[0], current[1] - drag.start[1]];
                let mut crop = drag.crop;
                match drag.handle {
                    CropHandle::Move => {
                        let width = crop[2] - crop[0];
                        let height = crop[3] - crop[1];
                        let left = (crop[0] + delta[0]).clamp(0.0, 1.0 - width);
                        let top = (crop[1] + delta[1]).clamp(0.0, 1.0 - height);
                        crop = [left, top, left + width, top + height];
                    }
                    CropHandle::Left => crop[0] += delta[0],
                    CropHandle::Right => crop[2] += delta[0],
                    CropHandle::Top => crop[1] += delta[1],
                    CropHandle::Bottom => crop[3] += delta[1],
                    CropHandle::TopLeft => {
                        crop[0] += delta[0];
                        crop[1] += delta[1];
                    }
                    CropHandle::TopRight => {
                        crop[2] += delta[0];
                        crop[1] += delta[1];
                    }
                    CropHandle::BottomLeft => {
                        crop[0] += delta[0];
                        crop[3] += delta[1];
                    }
                    CropHandle::BottomRight => {
                        crop[2] += delta[0];
                        crop[3] += delta[1];
                    }
                }
                crop = sanitize_dragged_crop(crop, drag.handle);
                if drag.handle != CropHandle::Move {
                    crop = if is_crop_corner(drag.handle) {
                        constrain_crop_corner_aspect(app, drag.crop, current, drag.handle)
                            .unwrap_or(crop)
                    } else {
                        constrain_crop_aspect(app, crop, drag.handle)
                    };
                }
                // Do not merely clip the crop overlay against the rotated
                // image. Clamp the actual crop rectangle to the last valid drag
                // position so all four white crop corners remain over source
                // pixels and the exported frame contains no pasteboard.
                crop = app.geometry.constrain_crop_drag_to_transformed_source(
                    drag.crop,
                    crop,
                    source_width,
                    source_height,
                );
                if crop != app.geometry.crop {
                    app.geometry.crop = crop;
                    // A manual crop becomes the new user intent. Future
                    // straighten changes may auto-fit from this rectangle, but
                    // must never expand beyond it.
                    app.crop_constraint_reference = Some(crop);
                    app.note_geometry_changed();
                }
            }
        }

        if primary_released || !primary_down {
            app.crop_drag = None;
        }
    }

    fn paint_crop_overlay(
        ui: &mut Ui,
        app: &AurawApp,
        image_rect: Rect,
        visible_rect: Rect,
        overlay_clip_rect: Rect,
        source_width: u32,
        source_height: u32,
    ) {
        // Shade only real image pixels below, but allow the crop border stroke
        // and handles to paint into the surrounding pasteboard. Clipping the
        // painter to visible_rect cut off half of every edge handle whenever
        // the crop touched an image boundary.
        let painter = ui.painter_at(overlay_clip_rect);
        let crop_rect =
            crop_preview_screen_rect(image_rect, app.geometry, source_width, source_height);
        let visible_crop = crop_rect.intersect(visible_rect);
        if visible_crop.width() <= 0.0 || visible_crop.height() <= 0.0 {
            return;
        }
        // Keep the crop mask on top of real image pixels only. Fine rotation
        // and keystone can expose pasteboard inside the Crop workspace; shading
        // that pasteboard makes the overlay look as if it extends beyond the
        // photograph. Clip each outside-crop band against the transformed full-
        // image quadrilateral instead.
        let image_polygon =
            crop_workspace_image_polygon(image_rect, app.geometry, source_width, source_height);
        let shade = Color32::from_black_alpha(150);
        for rect in [
            Rect::from_min_max(
                visible_rect.min,
                Pos2::new(visible_rect.right(), visible_crop.top()),
            ),
            Rect::from_min_max(
                Pos2::new(visible_rect.left(), visible_crop.bottom()),
                visible_rect.max,
            ),
            Rect::from_min_max(
                Pos2::new(visible_rect.left(), visible_crop.top()),
                Pos2::new(visible_crop.left(), visible_crop.bottom()),
            ),
            Rect::from_min_max(
                Pos2::new(visible_crop.right(), visible_crop.top()),
                Pos2::new(visible_rect.right(), visible_crop.bottom()),
            ),
        ] {
            let rect = rect.intersect(visible_rect);
            if rect.width() <= 0.0 || rect.height() <= 0.0 {
                continue;
            }
            let clipped = clip_polygon_to_rect(&image_polygon, rect);
            if clipped.len() >= 3 {
                painter.add(Shape::convex_polygon(
                    clipped,
                    shade,
                    Stroke::new(0.0, Color32::TRANSPARENT),
                ));
            }
        }

        let border_stroke = Stroke::new(2.0, Color32::WHITE);
        for (a, b) in crop_rect_segments(crop_rect) {
            if let Some([a, b]) = clip_crop_workspace_segment_to_source_image(
                image_rect,
                app.geometry,
                source_width,
                source_height,
                a,
                b,
            ) {
                painter.line_segment([a, b], border_stroke);
            }
        }
        for fraction in [1.0 / 3.0, 2.0 / 3.0] {
            let x = egui::lerp(crop_rect.left()..=crop_rect.right(), fraction);
            let y = egui::lerp(crop_rect.top()..=crop_rect.bottom(), fraction);
            for (a, b) in [
                (
                    Pos2::new(x, crop_rect.top()),
                    Pos2::new(x, crop_rect.bottom()),
                ),
                (
                    Pos2::new(crop_rect.left(), y),
                    Pos2::new(crop_rect.right(), y),
                ),
            ] {
                if let Some([a, b]) = clip_crop_workspace_segment_to_source_image(
                    image_rect,
                    app.geometry,
                    source_width,
                    source_height,
                    a,
                    b,
                ) {
                    painter.line_segment([a, b], Stroke::new(1.0, Color32::from_white_alpha(115)));
                }
            }
        }

        for point in crop_handle_points(crop_rect) {
            let uv = crop_workspace_screen_to_source(
                image_rect,
                app.geometry,
                source_width,
                source_height,
                point,
            );
            if source_uv_inside_image(uv) {
                painter.circle_filled(point, 5.5, Color32::WHITE);
                painter.circle_stroke(point, 7.5, Stroke::new(1.5, Color32::BLACK));
            }
        }

        if let Some(line) = app.straighten_drag {
            let stroke = Stroke::new(2.0, Color32::WHITE);
            painter.line_segment([line.start, line.current], stroke);
            painter.circle_filled(line.start, 4.0, Color32::WHITE);
            painter.circle_filled(line.current, 4.0, Color32::WHITE);
        }
    }

    #[cfg(target_os = "android")]
    fn handle_android_original_hold(
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

    fn handle_inpaint_interaction(
        ui: &Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
        image_rect: Rect,
        preview_rect: Rect,
        source_width: u32,
        source_height: u32,
        response: &egui::Response,
    ) {
        let lens_geometry = app
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
        if !primary_down {
            if primary_released {
                // `last_inpaint_brush_point` is intentionally cleared when a
                // stroke crosses transformed pasteboard. The accumulated dabs,
                // not the last pointer position, are the reliable indication
                // that this gesture has real work to submit.
                app.last_inpaint_brush_point = None;
                if !app.inpaint_stroke.is_empty() {
                    app.request_inpaint(frame);
                }
            } else if primary_is_down {
                // The pointer can leave the clipped preview while the button is
                // still held. Break interpolation until it re-enters so a stroke
                // never jumps across hidden/pasteboard space.
                app.last_inpaint_brush_point = None;
            }
            return;
        }
        if app.inpaint_busy() {
            return;
        }
        let Some(pointer) = pointer else {
            return;
        };
        let source_uv = final_geometry_screen_to_native_source(
            image_rect,
            app.geometry,
            lens_geometry.as_deref(),
            source_width,
            source_height,
            pointer,
        );
        // The crop rectangle defines the destination frame; after straighten or
        // keystone correction, valid destination pixels can legitimately sample
        // source pixels outside that crop rectangle. Only reject true pasteboard
        // where the inverse transform lands outside the source image itself.
        let Some(uv) = editable_source_uv(source_uv) else {
            // Break the stroke while crossing pasteboard so re-entering the image
            // cannot bridge an inpaint line across an empty transformed corner.
            app.last_inpaint_brush_point = None;
            return;
        };

        let first_dab = app.last_inpaint_brush_point.is_none();
        let previous = app.last_inpaint_brush_point.unwrap_or(uv);
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
            app.inpaint_brush_size,
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
            if app.inpaint_stroke.len() < 8192 {
                app.inpaint_stroke.push(BrushDab {
                    center: uv,
                    opacity: 1.0,
                    size: dab_size,
                    feather: 0.0,
                });
                changed = true;
            }
        } else if distance_px >= spacing_px * 0.80 {
            let steps = (distance_px / spacing_px).ceil().max(1.0) as usize;
            for step in 1..=steps {
                if app.inpaint_stroke.len() >= 8192 {
                    break;
                }
                let t = step as f32 / steps as f32;
                app.inpaint_stroke.push(BrushDab {
                    center: [
                        previous[0] + (uv[0] - previous[0]) * t,
                        previous[1] + (uv[1] - previous[1]) * t,
                    ],
                    opacity: 1.0,
                    size: dab_size,
                    feather: 0.0,
                });
                changed = true;
            }
        }
        if changed {
            app.last_inpaint_brush_point = Some(uv);
            app.inpaint_stroke_texture_key = None;
            ui.ctx().request_repaint();
        }
    }

    fn paint_inpaint_overlay(
        ui: &Ui,
        app: &mut AurawApp,
        image_rect: Rect,
        preview_rect: Rect,
        source_width: u32,
        source_height: u32,
    ) {
        let painter = ui.painter_at(preview_rect);
        if app.sidebar_tab != SidebarTab::Inpainting {
            return;
        }
        let lens_geometry = app
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());

        let focused_stroke = app
            .inpaint_hovered_stroke
            .or(app.inpaint_selected_stroke)
            .filter(|index| *index < app.inpaint_strokes.len());
        if let Some(index) = focused_stroke {
            if app.gpu_pipeline.is_none() {
                return;
            }
            let hovered = app.inpaint_hovered_stroke == Some(index);
            let region = overlay_raster_region(
                app.preview_visible_uv,
                source_width,
                source_height,
                preview_rect,
                physical_pixels_per_point(ui.ctx()),
                2,
            );
            let key = (index, app.inpaint_texture_revision, region, hovered);
            if app.inpaint_focus_texture_key != Some(key) {
                let dabs = crop_overlay_dabs(
                    &app.inpaint_strokes[index].dabs,
                    region,
                    source_width,
                    source_height,
                );
                let coverage = rasterize_inpaint_dabs_binary(
                    region.texture_width,
                    region.texture_height,
                    region.source_width,
                    region.source_height,
                    &dabs,
                );
                let color = if hovered {
                    Color32::from_rgb(255, 190, 70)
                } else {
                    Color32::from_rgb(77, 196, 255)
                };
                let rgba = coverage_rgba(coverage, color);
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [
                        region.texture_width as usize,
                        region.texture_height as usize,
                    ],
                    &rgba,
                );
                if let Some(texture) = app.inpaint_focus_texture.as_mut() {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    app.inpaint_focus_texture = Some(ui.ctx().load_texture(
                        "auraw-inpaint-focused-stroke",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                app.inpaint_focus_texture_key = Some(key);
            }
            if let Some(texture) = &app.inpaint_focus_texture {
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

            if let Some(bounds) = inpaint_stroke_geometry_screen_bounds(
                &app.inpaint_strokes[index].dabs,
                image_rect,
                app.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
            ) {
                let color = if hovered {
                    Color32::from_rgb(255, 210, 105)
                } else {
                    Color32::from_rgb(115, 210, 255)
                };
                let bounds = bounds.intersect(preview_rect);
                if bounds.is_positive() {
                    painter.rect_stroke(
                        bounds.expand(3.0),
                        4.0,
                        Stroke::new(2.0, color),
                        egui::StrokeKind::Outside,
                    );
                    painter.text(
                        bounds.left_top() + egui::vec2(4.0, -6.0),
                        egui::Align2::LEFT_BOTTOM,
                        format!("Stroke {}", index + 1),
                        egui::FontId::proportional(11.0),
                        color,
                    );
                }
            }
        }

        if !app.inpaint_stroke.is_empty() {
            if app.gpu_pipeline.is_none() {
                return;
            }
            let region = overlay_raster_region(
                app.preview_visible_uv,
                source_width,
                source_height,
                preview_rect,
                physical_pixels_per_point(ui.ctx()),
                2,
            );
            let key = (app.inpaint_stroke.len(), region);
            if app.inpaint_stroke_texture_key != Some(key) {
                let dabs =
                    crop_overlay_dabs(&app.inpaint_stroke, region, source_width, source_height);
                let coverage = rasterize_inpaint_dabs_binary(
                    region.texture_width,
                    region.texture_height,
                    region.source_width,
                    region.source_height,
                    &dabs,
                );
                let rgba = coverage_rgba(coverage, Color32::from_rgb(255, 94, 94));
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [
                        region.texture_width as usize,
                        region.texture_height as usize,
                    ],
                    &rgba,
                );
                if let Some(texture) = app.inpaint_stroke_texture.as_mut() {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    app.inpaint_stroke_texture = Some(ui.ctx().load_texture(
                        "auraw-inpaint-stroke",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                app.inpaint_stroke_texture_key = Some(key);
            }
            if let Some(texture) = &app.inpaint_stroke_texture {
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

        if let Some(pointer) = ui
            .ctx()
            .pointer_hover_pos()
            .filter(|position| preview_rect.contains(*position))
        {
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
                        app.inpaint_brush_size,
                        app.preview_zoom,
                        app.image_relative_brush_size,
                    ),
                    64,
                );
                painter.add(Shape::line(outline, Stroke::new(1.5, Color32::WHITE)));
            }
        }
    }

    fn handle_mask_interaction(
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

    fn paint_mask_overlay(
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
        let neutral = mask.adjustments.is_neutral();
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

    fn paint_coverage_texture(
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

    fn paint_tool_hint(ui: &Ui, app: &AurawApp, preview_rect: Rect) {
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

fn show_loading_thumbnail(ui: &mut Ui, app: &AurawApp, available: egui::Vec2) -> bool {
    let (Some(texture), Some([width, height])) = (
        app.develop_loading_thumbnail.texture.as_ref(),
        app.develop_loading_thumbnail.texture_size,
    ) else {
        return false;
    };
    if available.x <= 0.0 || available.y <= 0.0 || width == 0 || height == 0 {
        return false;
    }

    let (outer_rect, _) = ui.allocate_exact_size(available, Sense::hover());
    let image_size = fitted_image_size(outer_rect.size(), width as f32 / height as f32);
    let image_rect = Rect::from_center_size(outer_rect.center(), image_size);
    ui.painter().image(
        texture.id(),
        image_rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );

    if outer_rect.width() >= 104.0 && outer_rect.height() >= 48.0 {
        let badge_width = 132.0_f32.min(outer_rect.width() - 16.0);
        let badge_rect = Rect::from_center_size(outer_rect.center(), egui::vec2(badge_width, 32.0));
        ui.painter()
            .rect_filled(badge_rect, 16.0, Color32::from_black_alpha(190));
        ui.painter().text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            "Loading RAW…",
            egui::FontId::proportional(13.0),
            Color32::WHITE,
        );
    }
    true
}

fn begin_mask_drag(
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

fn source_angle_from(
    center: [f32; 2],
    point: [f32; 2],
    source_width: u32,
    source_height: u32,
) -> f32 {
    let dx = (point[0] - center[0]) * source_width.max(1) as f32;
    let dy = (point[1] - center[1]) * source_height.max(1) as f32;
    dy.atan2(dx)
}

fn linear_rotation_handle_geometry(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    start: [f32; 2],
    end: [f32; 2],
) -> (Pos2, Pos2) {
    let midpoint_uv = [(start[0] + end[0]) * 0.5, (start[1] + end[1]) * 0.5];
    let midpoint = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        midpoint_uv,
    );

    // Use the local tangent of the *warped* gradient axis. The old code used
    // the straight chord between transformed endpoints, which points in the
    // wrong direction under nonlinear lens correction.
    let tangent_a_uv = [
        start[0] + (end[0] - start[0]) * 0.48,
        start[1] + (end[1] - start[1]) * 0.48,
    ];
    let tangent_b_uv = [
        start[0] + (end[0] - start[0]) * 0.52,
        start[1] + (end[1] - start[1]) * 0.52,
    ];
    let tangent_a = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        tangent_a_uv,
    );
    let tangent_b = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        tangent_b_uv,
    );
    let tangent = tangent_b - tangent_a;
    let normal = if tangent.length_sq() > 1e-6 {
        egui::vec2(-tangent.y, tangent.x) / tangent.length()
    } else {
        egui::vec2(0.0, -1.0)
    };
    (midpoint, midpoint + normal * 34.0)
}

fn linear_axis_geometry_screen_points(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    start: [f32; 2],
    end: [f32; 2],
    segments: usize,
) -> Vec<Pos2> {
    let segments = segments.max(2);
    (0..=segments)
        .map(|index| {
            let t = index as f32 / segments as f32;
            let uv = [
                start[0] + (end[0] - start[0]) * t,
                start[1] + (end[1] - start[1]) * t,
            ];
            final_geometry_native_source_to_screen(
                image_rect,
                geometry,
                lens_geometry,
                source_width,
                source_height,
                uv,
            )
        })
        .collect()
}

fn clip_infinite_source_line(
    point: [f32; 2],
    direction: [f32; 2],
    source_width: u32,
    source_height: u32,
) -> Option<(f32, f32)> {
    let bounds = [source_width.max(1) as f32, source_height.max(1) as f32];
    let mut lo = f32::NEG_INFINITY;
    let mut hi = f32::INFINITY;
    for axis in 0..2 {
        let p = point[axis];
        let d = direction[axis];
        if d.abs() <= 1e-8 {
            if p < 0.0 || p > bounds[axis] {
                return None;
            }
            continue;
        }
        let a = (0.0 - p) / d;
        let b = (bounds[axis] - p) / d;
        lo = lo.max(a.min(b));
        hi = hi.min(a.max(b));
        if lo > hi {
            return None;
        }
    }
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

fn linear_isot_geometry_screen_points(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    start: [f32; 2],
    end: [f32; 2],
    t: f32,
    segments: usize,
) -> Vec<Pos2> {
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let start_px = [start[0] * width, start[1] * height];
    let delta = [(end[0] - start[0]) * width, (end[1] - start[1]) * height];
    let center = [start_px[0] + delta[0] * t, start_px[1] + delta[1] * t];
    let perpendicular = [-delta[1], delta[0]];
    if perpendicular[0].abs().max(perpendicular[1].abs()) <= 1e-6 {
        return vec![final_geometry_native_source_to_screen(
            image_rect,
            geometry,
            lens_geometry,
            source_width,
            source_height,
            start,
        )];
    }
    let Some((q0, q1)) =
        clip_infinite_source_line(center, perpendicular, source_width, source_height)
    else {
        return Vec::new();
    };
    let segments = segments.max(2);
    (0..=segments)
        .map(|index| {
            let fraction = index as f32 / segments as f32;
            let q = q0 + (q1 - q0) * fraction;
            let source_px = [
                center[0] + perpendicular[0] * q,
                center[1] + perpendicular[1] * q,
            ];
            let uv = [source_px[0] / width, source_px[1] / height];
            final_geometry_native_source_to_screen(
                image_rect,
                geometry,
                lens_geometry,
                source_width,
                source_height,
                uv,
            )
        })
        .collect()
}

fn brush_outline_geometry_screen_points(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    size: f32,
    segments: usize,
) -> Vec<Pos2> {
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let radius = size.max(0.0) * source_width.min(source_height).max(1) as f32;
    let center_px = [center[0] * width, center[1] * height];
    let segments = segments.max(16);
    (0..=segments)
        .map(|index| {
            let angle = std::f32::consts::TAU * index as f32 / segments as f32;
            let uv = [
                (center_px[0] + radius * angle.cos()) / width,
                (center_px[1] + radius * angle.sin()) / height,
            ];
            final_geometry_native_source_to_screen(
                image_rect,
                geometry,
                lens_geometry,
                source_width,
                source_height,
                uv,
            )
        })
        .collect()
}

fn radial_source_uv_at(
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
    angle: f32,
    source_width: u32,
    source_height: u32,
) -> [f32; 2] {
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let local_x = radius[0] * width * angle.cos();
    let local_y = radius[1] * height * angle.sin();
    let cos_r = rotation.cos();
    let sin_r = rotation.sin();
    let dx = cos_r * local_x - sin_r * local_y;
    let dy = sin_r * local_x + cos_r * local_y;
    [center[0] + dx / width, center[1] + dy / height]
}

fn radial_handles_geometry_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
) -> [Pos2; 4] {
    [
        0.0,
        std::f32::consts::PI,
        std::f32::consts::FRAC_PI_2,
        -std::f32::consts::FRAC_PI_2,
    ]
    .map(|angle| {
        final_geometry_native_source_to_screen(
            image_rect,
            geometry,
            lens_geometry,
            source_width,
            source_height,
            radial_source_uv_at(center, radius, rotation, angle, source_width, source_height),
        )
    })
}

fn radial_rotation_handle_geometry(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
) -> Pos2 {
    let center_screen = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        center,
    );
    let major_screen = radial_handles_geometry_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        center,
        radius,
        rotation,
    )[0];
    let direction = (major_screen - center_screen).normalized();
    major_screen + direction * 30.0
}

fn radial_outline_geometry_screen_points(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
    segments: usize,
) -> Vec<Pos2> {
    let segments = segments.max(12);
    (0..=segments)
        .map(|index| {
            let angle = std::f32::consts::TAU * index as f32 / segments as f32;
            final_geometry_native_source_to_screen(
                image_rect,
                geometry,
                lens_geometry,
                source_width,
                source_height,
                radial_source_uv_at(center, radius, rotation, angle, source_width, source_height),
            )
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

fn distance_to_polyline(point: Pos2, points: &[Pos2]) -> f32 {
    match points {
        [] => f32::INFINITY,
        [only] => point.distance(*only),
        _ => points
            .windows(2)
            .map(|pair| distance_to_segment(point, pair[0], pair[1]))
            .fold(f32::INFINITY, f32::min),
    }
}

fn geometry_forward_affine(geometry: GeometryTransform, dx: f32, dy: f32) -> [f32; 2] {
    let geometry = geometry.sanitized();
    let fx = if geometry.flip_horizontal { -1.0 } else { 1.0 };
    let fy = if geometry.flip_vertical { -1.0 } else { 1.0 };
    let shx = geometry.horizontal_transform.to_radians().tan();
    let shy = geometry.vertical_transform.to_radians().tan();
    let angle = geometry.rotation_degrees.to_radians();
    let c = angle.cos();
    let s = angle.sin();

    let flipped_x = dx * fx;
    let flipped_y = dy * fy;
    let sheared_x = flipped_x + shx * flipped_y;
    let sheared_y = shy * flipped_x + flipped_y;
    [c * sheared_x - s * sheared_y, s * sheared_x + c * sheared_y]
}

fn geometry_inverse_affine(geometry: GeometryTransform, dx: f32, dy: f32) -> [f32; 2] {
    let geometry = geometry.sanitized();
    let fx = if geometry.flip_horizontal { -1.0 } else { 1.0 };
    let fy = if geometry.flip_vertical { -1.0 } else { 1.0 };
    let shx = geometry.horizontal_transform.to_radians().tan();
    let shy = geometry.vertical_transform.to_radians().tan();
    let angle = geometry.rotation_degrees.to_radians();
    let c = angle.cos();
    let s = angle.sin();
    let a = c * fx - s * shy * fx;
    let b = c * shx * fy - s * fy;
    let c2 = s * fx + c * shy * fx;
    let d = s * shx * fy + c * fy;
    let determinant = a * d - b * c2;
    if determinant.abs() < 1e-6 {
        return [0.0, 0.0];
    }
    [
        (d * dx - b * dy) / determinant,
        (-c2 * dx + a * dy) / determinant,
    ]
}

fn quarter_rotate_delta(quarter_turns: u8, dx: f32, dy: f32) -> [f32; 2] {
    match quarter_turns % 4 {
        0 => [dx, dy],
        1 => [-dy, dx],
        2 => [-dx, -dy],
        _ => [dy, -dx],
    }
}

fn quarter_unrotate_delta(quarter_turns: u8, dx: f32, dy: f32) -> [f32; 2] {
    match quarter_turns % 4 {
        0 => [dx, dy],
        1 => [dy, -dx],
        2 => [-dx, -dy],
        _ => [-dy, dx],
    }
}

fn geometry_forward_linear(geometry: GeometryTransform, dx: f32, dy: f32) -> [f32; 2] {
    let affine = geometry_forward_affine(geometry, dx, dy);
    quarter_rotate_delta(geometry.quarter_turns, affine[0], affine[1])
}

fn geometry_inverse_linear(geometry: GeometryTransform, dx: f32, dy: f32) -> [f32; 2] {
    let affine = quarter_unrotate_delta(geometry.quarter_turns, dx, dy);
    geometry_inverse_affine(geometry, affine[0], affine[1])
}

fn quarter_rotate_image_point(
    quarter_turns: u8,
    source_width: f32,
    source_height: f32,
    point: [f32; 2],
) -> [f32; 2] {
    match quarter_turns % 4 {
        0 => point,
        1 => [source_height - point[1], point[0]],
        2 => [source_width - point[0], source_height - point[1]],
        _ => [point[1], source_width - point[0]],
    }
}

fn quarter_unrotate_image_point(
    quarter_turns: u8,
    source_width: f32,
    source_height: f32,
    point: [f32; 2],
) -> [f32; 2] {
    match quarter_turns % 4 {
        0 => point,
        1 => [point[1], source_height - point[0]],
        2 => [source_width - point[0], source_height - point[1]],
        _ => [source_width - point[1], point[0]],
    }
}

fn geometry_crop_metrics(
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
) -> ([f32; 2], [f32; 2]) {
    let geometry = geometry.sanitized();
    let source_width = source_width.max(1) as f32;
    let source_height = source_height.max(1) as f32;
    let crop = geometry.crop;
    (
        [
            (crop[0] + crop[2]) * 0.5 * source_width,
            (crop[1] + crop[3]) * 0.5 * source_height,
        ],
        [
            (crop[2] - crop[0]) * source_width,
            (crop[3] - crop[1]) * source_height,
        ],
    )
}

fn final_geometry_source_to_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    source_uv: [f32; 2],
) -> Pos2 {
    let geometry = geometry.sanitized();
    let ([center_x, center_y], [crop_width, crop_height]) =
        geometry_crop_metrics(geometry, source_width, source_height);
    let source_x = source_uv[0] * source_width.max(1) as f32;
    let source_y = source_uv[1] * source_height.max(1) as f32;
    let transformed = geometry_forward_linear(geometry, source_x - center_x, source_y - center_y);
    let (output_width, output_height) = if geometry.quarter_turns.is_multiple_of(2) {
        (crop_width, crop_height)
    } else {
        (crop_height, crop_width)
    };
    let output_uv = [
        0.5 + transformed[0] / output_width.max(f32::EPSILON),
        0.5 + transformed[1] / output_height.max(f32::EPSILON),
    ];
    normalized_to_screen(image_rect, output_uv)
}

fn final_geometry_native_source_to_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    source_uv: [f32; 2],
) -> Pos2 {
    let corrected_uv = lens_geometry.map_or(source_uv, |lens_geometry| {
        native_source_to_corrected_uv(lens_geometry, source_width, source_height, source_uv)
    });
    final_geometry_source_to_screen(
        image_rect,
        geometry,
        source_width,
        source_height,
        corrected_uv,
    )
}

fn final_geometry_screen_to_source(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    screen: Pos2,
) -> [f32; 2] {
    let geometry = geometry.sanitized();
    let ([center_x, center_y], [crop_width, crop_height]) =
        geometry_crop_metrics(geometry, source_width, source_height);
    let output_uv = screen_to_normalized_unclamped(image_rect, screen);
    let (output_width, output_height) = if geometry.quarter_turns.is_multiple_of(2) {
        (crop_width, crop_height)
    } else {
        (crop_height, crop_width)
    };
    let output_dx = (output_uv[0] - 0.5) * output_width;
    let output_dy = (output_uv[1] - 0.5) * output_height;
    let source_delta = geometry_inverse_linear(geometry, output_dx, output_dy);
    [
        (center_x + source_delta[0]) / source_width.max(1) as f32,
        (center_y + source_delta[1]) / source_height.max(1) as f32,
    ]
}

fn editable_source_uv(uv: [f32; 2]) -> Option<[f32; 2]> {
    // Geometry export samples from the full source image after defining the crop
    // as an output frame. A rotated/sheared crop therefore often contains valid
    // pixels whose source coordinates lie outside `geometry.crop`. Treat only
    // coordinates outside the source image as pasteboard. The small tolerance
    // absorbs inverse-transform floating-point noise at the exact image border,
    // then clamps stored brush/color coordinates back into the canonical range.
    const EDGE_EPSILON: f32 = 1e-4;
    if !uv[0].is_finite()
        || !uv[1].is_finite()
        || uv[0] < -EDGE_EPSILON
        || uv[0] > 1.0 + EDGE_EPSILON
        || uv[1] < -EDGE_EPSILON
        || uv[1] > 1.0 + EDGE_EPSILON
    {
        return None;
    }
    Some([uv[0].clamp(0.0, 1.0), uv[1].clamp(0.0, 1.0)])
}

fn geometry_brush_radius_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    size: f32,
) -> f32 {
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let radius_source_pixels = size.max(0.0) * source_width.min(source_height).max(1) as f32;
    let center_screen = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        center,
    );
    let x_screen = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        [center[0] + radius_source_pixels / source_width_f, center[1]],
    );
    let y_screen = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        [
            center[0],
            center[1] + radius_source_pixels / source_height_f,
        ],
    );
    center_screen
        .distance(x_screen)
        .max(center_screen.distance(y_screen))
}

fn overlay_raster_region(
    visible: crate::app::PreviewUvRect,
    source_width: u32,
    source_height: u32,
    preview_rect: Rect,
    pixels_per_point: f32,
    margin_pixels: u32,
) -> OverlayRasterKey {
    let source_width = source_width.max(1);
    let source_height = source_height.max(1);
    let source_x0 = (visible.min[0].clamp(0.0, 1.0) * source_width as f32).floor() as u32;
    let source_y0 = (visible.min[1].clamp(0.0, 1.0) * source_height as f32).floor() as u32;
    let source_x1 = (visible.max[0].clamp(0.0, 1.0) * source_width as f32).ceil() as u32;
    let source_y1 = (visible.max[1].clamp(0.0, 1.0) * source_height as f32).ceil() as u32;
    let source_x = source_x0.saturating_sub(margin_pixels);
    let source_y = source_y0.saturating_sub(margin_pixels);
    let source_end_x = source_x1
        .saturating_add(margin_pixels)
        .clamp(source_x.saturating_add(1), source_width);
    let source_end_y = source_y1
        .saturating_add(margin_pixels)
        .clamp(source_y.saturating_add(1), source_height);
    let region_width = source_end_x - source_x;
    let region_height = source_end_y - source_y;

    // Match the physical viewport density while zoomed, but never invent more
    // samples than the source region contains. Unlike the old fixed 512 px
    // full-frame overlay, this gives a small visible crop its native source
    // resolution and keeps a narrow brush stroke on the correct row/column.
    let visible_width = source_x1.saturating_sub(source_x0).max(1) as f32;
    let visible_height = source_y1.saturating_sub(source_y0).max(1) as f32;
    let scale_x = (preview_rect.width().max(1.0) * pixels_per_point / visible_width).min(1.0);
    let scale_y = (preview_rect.height().max(1.0) * pixels_per_point / visible_height).min(1.0);
    let mut texture_width = (region_width as f32 * scale_x).ceil().max(1.0) as u32;
    let mut texture_height = (region_height as f32 * scale_y).ceil().max(1.0) as u32;
    let edge_limit = if cfg!(target_os = "android") {
        2048
    } else {
        4096
    };
    let limit_scale = (edge_limit as f32 / texture_width.max(texture_height) as f32).min(1.0);
    texture_width = (texture_width as f32 * limit_scale).floor().max(1.0) as u32;
    texture_height = (texture_height as f32 * limit_scale).floor().max(1.0) as u32;

    OverlayRasterKey {
        source_x,
        source_y,
        source_width: region_width,
        source_height: region_height,
        texture_width,
        texture_height,
    }
}

fn overlay_source_uv(region: OverlayRasterKey, source_width: u32, source_height: u32) -> [f32; 4] {
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    [
        region.source_x as f32 / width,
        region.source_y as f32 / height,
        (region.source_x + region.source_width) as f32 / width,
        (region.source_y + region.source_height) as f32 / height,
    ]
}

fn crop_overlay_dabs(
    dabs: &[BrushDab],
    region: OverlayRasterKey,
    source_width: u32,
    source_height: u32,
) -> Vec<BrushDab> {
    let full_width = source_width.max(1) as f32;
    let full_height = source_height.max(1) as f32;
    let region_width = region.source_width.max(1) as f32;
    let region_height = region.source_height.max(1) as f32;
    let image_scale = source_width.min(source_height).max(1) as f32
        / region.source_width.min(region.source_height).max(1) as f32;
    dabs.iter()
        .map(|dab| BrushDab {
            center: [
                (dab.center[0] * full_width - region.source_x as f32) / region_width,
                (dab.center[1] * full_height - region.source_y as f32) / region_height,
            ],
            size: dab.size * image_scale,
            ..*dab
        })
        .collect()
}

fn crop_workspace_source_to_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    source_uv: [f32; 2],
) -> Pos2 {
    let geometry = geometry.sanitized();
    let ([center_x, center_y], _) = geometry_crop_metrics(geometry, source_width, source_height);
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let source_x = source_uv[0] * source_width_f;
    let source_y = source_uv[1] * source_height_f;
    let transformed = geometry_forward_affine(geometry, source_x - center_x, source_y - center_y);
    let pre_quarter = [center_x + transformed[0], center_y + transformed[1]];
    let canvas_point = quarter_rotate_image_point(
        geometry.quarter_turns,
        source_width_f,
        source_height_f,
        pre_quarter,
    );
    let (canvas_width, canvas_height) = if geometry.quarter_turns.is_multiple_of(2) {
        (source_width_f, source_height_f)
    } else {
        (source_height_f, source_width_f)
    };
    normalized_to_screen(
        image_rect,
        [
            canvas_point[0] / canvas_width,
            canvas_point[1] / canvas_height,
        ],
    )
}

fn crop_workspace_screen_to_source(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    screen: Pos2,
) -> [f32; 2] {
    let geometry = geometry.sanitized();
    let ([center_x, center_y], _) = geometry_crop_metrics(geometry, source_width, source_height);
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let (canvas_width, canvas_height) = if geometry.quarter_turns.is_multiple_of(2) {
        (source_width_f, source_height_f)
    } else {
        (source_height_f, source_width_f)
    };
    let canvas_uv = screen_to_normalized_unclamped(image_rect, screen);
    let canvas_point = [canvas_uv[0] * canvas_width, canvas_uv[1] * canvas_height];
    let pre_quarter = quarter_unrotate_image_point(
        geometry.quarter_turns,
        source_width_f,
        source_height_f,
        canvas_point,
    );
    let source_delta = geometry_inverse_affine(
        geometry,
        pre_quarter[0] - center_x,
        pre_quarter[1] - center_y,
    );
    [
        (center_x + source_delta[0]) / source_width_f,
        (center_y + source_delta[1]) / source_height_f,
    ]
}

fn source_uv_bbox(points: impl IntoIterator<Item = [f32; 2]>) -> crate::app::PreviewUvRect {
    let mut min = [1.0_f32, 1.0_f32];
    let mut max = [0.0_f32, 0.0_f32];
    for point in points {
        min[0] = min[0].min(point[0]);
        min[1] = min[1].min(point[1]);
        max[0] = max[0].max(point[0]);
        max[1] = max[1].max(point[1]);
    }
    min[0] = min[0].clamp(0.0, 1.0);
    min[1] = min[1].clamp(0.0, 1.0);
    max[0] = max[0].clamp(0.0, 1.0);
    max[1] = max[1].clamp(0.0, 1.0);
    if max[0] <= min[0] {
        if min[0] >= 1.0 {
            min[0] = 1.0 - 1e-6;
            max[0] = 1.0;
        } else {
            max[0] = (min[0] + 1e-6).min(1.0);
        }
    }
    if max[1] <= min[1] {
        if min[1] >= 1.0 {
            min[1] = 1.0 - 1e-6;
            max[1] = 1.0;
        } else {
            max[1] = (min[1] + 1e-6).min(1.0);
        }
    }
    crate::app::PreviewUvRect { min, max }
}

fn visible_rect_sample_points(rect: Rect, nonlinear: bool) -> Vec<Pos2> {
    let steps = if nonlinear { 10 } else { 1 };
    let mut points = Vec::with_capacity((steps + 1) * (steps + 1));
    for y in 0..=steps {
        let ty = y as f32 / steps as f32;
        for x in 0..=steps {
            let tx = x as f32 / steps as f32;
            points.push(Pos2::new(
                rect.left() + rect.width() * tx,
                rect.top() + rect.height() * ty,
            ));
        }
    }
    points
}

fn final_geometry_visible_source_uv(
    image_rect: Rect,
    visible_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
) -> crate::app::PreviewUvRect {
    source_uv_bbox(
        visible_rect_sample_points(visible_rect, lens_geometry.is_some())
            .into_iter()
            .map(|point| {
                final_geometry_screen_to_native_source(
                    image_rect,
                    geometry,
                    lens_geometry,
                    source_width,
                    source_height,
                    point,
                )
            }),
    )
}

fn crop_workspace_visible_source_uv(
    image_rect: Rect,
    visible_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
) -> crate::app::PreviewUvRect {
    source_uv_bbox(
        visible_rect_sample_points(visible_rect, lens_geometry.is_some())
            .into_iter()
            .map(|point| {
                crop_workspace_screen_to_native_source(
                    image_rect,
                    geometry,
                    lens_geometry,
                    source_width,
                    source_height,
                    point,
                )
            }),
    )
}

fn paint_textured_geometry_quad(
    ui: &Ui,
    texture_id: egui::TextureId,
    clip_rect: Rect,
    positions: [Pos2; 4],
    texture_uv: Rect,
) {
    let mut mesh = Mesh::with_texture(texture_id);
    let uvs = [
        texture_uv.left_top(),
        texture_uv.right_top(),
        texture_uv.right_bottom(),
        texture_uv.left_bottom(),
    ];
    for (pos, uv) in positions.into_iter().zip(uvs) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos,
            uv,
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    ui.painter_at(clip_rect).add(Shape::mesh(mesh));
}

fn paint_textured_combined_geometry_mesh(
    ui: &Ui,
    texture_id: egui::TextureId,
    clip_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    texture_uv: Rect,
    source_uv: [f32; 4],
    crop_workspace: bool,
) {
    if lens_geometry.is_none() {
        let positions = source_uv_corners(source_uv).map(|point| {
            if crop_workspace {
                crop_workspace_source_to_screen(
                    clip_rect,
                    geometry,
                    source_width,
                    source_height,
                    point,
                )
            } else {
                final_geometry_source_to_screen(
                    clip_rect,
                    geometry,
                    source_width,
                    source_height,
                    point,
                )
            }
        });
        paint_textured_geometry_quad(ui, texture_id, clip_rect, positions, texture_uv);
        return;
    }

    // Egui meshes linearly interpolate UVs inside each triangle. A modest grid
    // therefore turns Lensfun's smooth nonlinear map into a GPU texture warp
    // without first resampling the preview pixels on the CPU/CFA.
    // Keep the display warp close to the exact map used by vector overlays and
    // inverse pointer mapping. A fixed 32x32 mesh can span ~200 source pixels
    // per cell on high-resolution RAWs, visibly separating mask handles from
    // their coverage near strongly distorted edges. Bound each cell to roughly
    // 96 source pixels while keeping vertex count predictable.
    let span_u = (source_uv[2] - source_uv[0]).abs().max(1e-6);
    let span_v = (source_uv[3] - source_uv[1]).abs().max(1e-6);
    let grid_x = ((source_width.max(1) as f32 * span_u / 96.0).ceil() as usize).clamp(16, 96);
    let grid_y = ((source_height.max(1) as f32 * span_v / 96.0).ceil() as usize).clamp(16, 96);
    let lens_geometry = lens_geometry.expect("lens geometry checked above");
    let mut mesh = Mesh::with_texture(texture_id);
    mesh.vertices.reserve((grid_x + 1) * (grid_y + 1));
    mesh.indices.reserve(grid_x * grid_y * 6);
    for gy in 0..=grid_y {
        let ty = gy as f32 / grid_y as f32;
        let raw_v = source_uv[1] + (source_uv[3] - source_uv[1]) * ty;
        let texture_v = texture_uv.top() + (texture_uv.bottom() - texture_uv.top()) * ty;
        for gx in 0..=grid_x {
            let tx = gx as f32 / grid_x as f32;
            let raw_u = source_uv[0] + (source_uv[2] - source_uv[0]) * tx;
            let texture_u = texture_uv.left() + (texture_uv.right() - texture_uv.left()) * tx;
            let corrected_uv = native_source_to_corrected_uv(
                lens_geometry,
                source_width,
                source_height,
                [raw_u, raw_v],
            );
            let pos = if crop_workspace {
                crop_workspace_source_to_screen(
                    clip_rect,
                    geometry,
                    source_width,
                    source_height,
                    corrected_uv,
                )
            } else {
                final_geometry_source_to_screen(
                    clip_rect,
                    geometry,
                    source_width,
                    source_height,
                    corrected_uv,
                )
            };
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv: Pos2::new(texture_u, texture_v),
                color: Color32::WHITE,
            });
        }
    }
    let stride = grid_x + 1;
    for gy in 0..grid_y {
        for gx in 0..grid_x {
            let a = (gy * stride + gx) as u32;
            let b = a + 1;
            let c = a + stride as u32;
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, b, d, a, d, c]);
        }
    }
    ui.painter_at(clip_rect).add(Shape::mesh(mesh));
}

fn native_source_to_corrected_uv(
    lens_geometry: &LensGeometryMap,
    source_width: u32,
    source_height: u32,
    source_uv: [f32; 2],
) -> [f32; 2] {
    if source_uv[0] < 0.0 || source_uv[0] > 1.0 || source_uv[1] < 0.0 || source_uv[1] > 1.0 {
        return source_uv;
    }
    let width = source_width.saturating_sub(1).max(1) as f32;
    let height = source_height.saturating_sub(1).max(1) as f32;
    let corrected = lens_geometry.corrected_position_for_raster(
        source_uv[0] * width,
        source_uv[1] * height,
        source_width,
        source_height,
    );
    [corrected[0] / width, corrected[1] / height]
}

fn final_geometry_screen_to_native_source(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    screen: Pos2,
) -> [f32; 2] {
    let corrected_uv =
        final_geometry_screen_to_source(image_rect, geometry, source_width, source_height, screen);
    corrected_uv_to_native_source(corrected_uv, lens_geometry, source_width, source_height)
}

fn crop_workspace_screen_to_native_source(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    screen: Pos2,
) -> [f32; 2] {
    let corrected_uv =
        crop_workspace_screen_to_source(image_rect, geometry, source_width, source_height, screen);
    corrected_uv_to_native_source(corrected_uv, lens_geometry, source_width, source_height)
}

fn corrected_uv_to_native_source(
    corrected_uv: [f32; 2],
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
) -> [f32; 2] {
    let Some(lens_geometry) = lens_geometry else {
        return corrected_uv;
    };
    if corrected_uv[0] < 0.0
        || corrected_uv[0] > 1.0
        || corrected_uv[1] < 0.0
        || corrected_uv[1] > 1.0
    {
        return corrected_uv;
    }
    let width = source_width.saturating_sub(1).max(1) as f32;
    let height = source_height.saturating_sub(1).max(1) as f32;
    let source = lens_geometry.source_position_for_raster(
        corrected_uv[0] * width,
        corrected_uv[1] * height,
        source_width,
        source_height,
    );
    [source[0] / width, source[1] / height]
}

fn source_uv_corners(source_uv: [f32; 4]) -> [[f32; 2]; 4] {
    [
        [source_uv[0], source_uv[1]],
        [source_uv[2], source_uv[1]],
        [source_uv[2], source_uv[3]],
        [source_uv[0], source_uv[3]],
    ]
}

fn paint_final_geometry_texture(
    ui: &Ui,
    texture_id: egui::TextureId,
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    texture_uv: Rect,
    source_uv: [f32; 4],
) {
    if source_uv == [0.0, 0.0, 1.0, 1.0] {
        ui.painter_at(image_rect)
            .rect_filled(image_rect, 0.0, Color32::BLACK);
    }
    paint_textured_combined_geometry_mesh(
        ui,
        texture_id,
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        texture_uv,
        source_uv,
        false,
    );
}

fn paint_final_geometry_overlay_texture(
    ui: &Ui,
    texture_id: egui::TextureId,
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    texture_uv: Rect,
    source_uv: [f32; 4],
) {
    if lens_geometry.is_none() {
        let positions = source_uv_corners(source_uv).map(|point| {
            final_geometry_source_to_screen(
                image_rect,
                geometry,
                source_width,
                source_height,
                point,
            )
        });
        paint_textured_geometry_quad(ui, texture_id, image_rect, positions, texture_uv);
        return;
    }
    paint_textured_combined_geometry_mesh(
        ui,
        texture_id,
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        texture_uv,
        source_uv,
        false,
    );
}

fn paint_crop_workspace_texture(
    ui: &Ui,
    texture_id: egui::TextureId,
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    texture_uv: Rect,
    source_uv: [f32; 4],
) {
    if source_uv == [0.0, 0.0, 1.0, 1.0] {
        ui.painter_at(image_rect)
            .rect_filled(image_rect, 0.0, ui.visuals().panel_fill);
    }
    paint_textured_combined_geometry_mesh(
        ui,
        texture_id,
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        texture_uv,
        source_uv,
        true,
    );
}

fn crop_preview_screen_rect(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
) -> Rect {
    let geometry = geometry.sanitized();
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let crop = geometry.crop;
    let crop_center = [
        (crop[0] + crop[2]) * 0.5 * source_width_f,
        (crop[1] + crop[3]) * 0.5 * source_height_f,
    ];
    let display_center = quarter_rotate_image_point(
        geometry.quarter_turns,
        source_width_f,
        source_height_f,
        crop_center,
    );
    let crop_width = (crop[2] - crop[0]) * source_width_f;
    let crop_height = (crop[3] - crop[1]) * source_height_f;
    let (canvas_width, canvas_height, display_width, display_height) =
        if geometry.quarter_turns.is_multiple_of(2) {
            (source_width_f, source_height_f, crop_width, crop_height)
        } else {
            (source_height_f, source_width_f, crop_height, crop_width)
        };
    let center = normalized_to_screen(
        image_rect,
        [
            display_center[0] / canvas_width,
            display_center[1] / canvas_height,
        ],
    );
    Rect::from_center_size(
        center,
        egui::vec2(
            display_width / canvas_width * image_rect.width(),
            display_height / canvas_height * image_rect.height(),
        ),
    )
}

fn crop_source_handle_for_display(handle: CropHandle, quarter_turns: u8) -> CropHandle {
    use CropHandle::*;
    match quarter_turns % 4 {
        0 => handle,
        1 => match handle {
            TopLeft => BottomLeft,
            TopRight => TopLeft,
            BottomRight => TopRight,
            BottomLeft => BottomRight,
            Top => Left,
            Right => Top,
            Bottom => Right,
            Left => Bottom,
            Move => Move,
        },
        2 => match handle {
            TopLeft => BottomRight,
            TopRight => BottomLeft,
            BottomRight => TopLeft,
            BottomLeft => TopRight,
            Top => Bottom,
            Right => Left,
            Bottom => Top,
            Left => Right,
            Move => Move,
        },
        _ => match handle {
            TopLeft => TopRight,
            TopRight => BottomRight,
            BottomRight => BottomLeft,
            BottomLeft => TopLeft,
            Top => Right,
            Right => Bottom,
            Bottom => Left,
            Left => Top,
            Move => Move,
        },
    }
}

fn crop_preview_pointer_to_source_normalized(
    image_rect: Rect,
    quarter_turns: u8,
    source_width: u32,
    source_height: u32,
    pointer: Pos2,
) -> [f32; 2] {
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let (canvas_width, canvas_height) = if quarter_turns.is_multiple_of(2) {
        (source_width_f, source_height_f)
    } else {
        (source_height_f, source_width_f)
    };
    let canvas_uv = screen_to_normalized_unclamped(image_rect, pointer);
    let source_point = quarter_unrotate_image_point(
        quarter_turns,
        source_width_f,
        source_height_f,
        [canvas_uv[0] * canvas_width, canvas_uv[1] * canvas_height],
    );
    [
        source_point[0] / source_width_f,
        source_point[1] / source_height_f,
    ]
}

fn source_uv_inside_image(uv: [f32; 2]) -> bool {
    const EPSILON: f32 = 1e-4;
    uv[0].is_finite()
        && uv[1].is_finite()
        && uv[0] >= -EPSILON
        && uv[0] <= 1.0 + EPSILON
        && uv[1] >= -EPSILON
        && uv[1] <= 1.0 + EPSILON
}

fn normalize_degrees(mut degrees: f32) -> f32 {
    while degrees > 180.0 {
        degrees -= 360.0;
    }
    while degrees <= -180.0 {
        degrees += 360.0;
    }
    degrees
}

fn nearest_straight_axis_degrees(angle: f32) -> f32 {
    // Pick the nearest horizontal or vertical axis. Drawing left-to-right or
    // right-to-left therefore produces the same correction, as does either
    // direction along a vertical edge.
    (angle / 90.0).round() * 90.0
}

fn crop_workspace_image_polygon(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
) -> Vec<Pos2> {
    [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
        .into_iter()
        .map(|uv| {
            crop_workspace_source_to_screen(image_rect, geometry, source_width, source_height, uv)
        })
        .collect()
}

fn clip_polygon_to_rect(polygon: &[Pos2], rect: Rect) -> Vec<Pos2> {
    fn clip_axis(
        input: &[Pos2],
        inside: impl Fn(Pos2) -> bool,
        intersect: impl Fn(Pos2, Pos2) -> Pos2,
    ) -> Vec<Pos2> {
        if input.is_empty() {
            return Vec::new();
        }
        let mut output = Vec::with_capacity(input.len() + 4);
        let mut previous = *input.last().unwrap();
        let mut previous_inside = inside(previous);
        for &current in input {
            let current_inside = inside(current);
            if current_inside != previous_inside {
                output.push(intersect(previous, current));
            }
            if current_inside {
                output.push(current);
            }
            previous = current;
            previous_inside = current_inside;
        }
        output
    }

    let mut output = polygon.to_vec();
    let left = rect.left();
    output = clip_axis(
        &output,
        |p| p.x >= left,
        |a, b| {
            let denom = b.x - a.x;
            let t = if denom.abs() <= f32::EPSILON {
                0.0
            } else {
                (left - a.x) / denom
            };
            Pos2::new(left, a.y + (b.y - a.y) * t)
        },
    );
    let right = rect.right();
    output = clip_axis(
        &output,
        |p| p.x <= right,
        |a, b| {
            let denom = b.x - a.x;
            let t = if denom.abs() <= f32::EPSILON {
                0.0
            } else {
                (right - a.x) / denom
            };
            Pos2::new(right, a.y + (b.y - a.y) * t)
        },
    );
    let top = rect.top();
    output = clip_axis(
        &output,
        |p| p.y >= top,
        |a, b| {
            let denom = b.y - a.y;
            let t = if denom.abs() <= f32::EPSILON {
                0.0
            } else {
                (top - a.y) / denom
            };
            Pos2::new(a.x + (b.x - a.x) * t, top)
        },
    );
    let bottom = rect.bottom();
    clip_axis(
        &output,
        |p| p.y <= bottom,
        |a, b| {
            let denom = b.y - a.y;
            let t = if denom.abs() <= f32::EPSILON {
                0.0
            } else {
                (bottom - a.y) / denom
            };
            Pos2::new(a.x + (b.x - a.x) * t, bottom)
        },
    )
}

fn crop_rect_segments(rect: Rect) -> [(Pos2, Pos2); 4] {
    [
        (rect.left_top(), rect.right_top()),
        (rect.right_top(), rect.right_bottom()),
        (rect.right_bottom(), rect.left_bottom()),
        (rect.left_bottom(), rect.left_top()),
    ]
}

fn liang_barsky_clip_test(p: f32, q: f32, t0: &mut f32, t1: &mut f32) -> bool {
    // Screen/source round-trips at crop boundaries can differ by a few ULPs as
    // zoom changes. Treat nearly parallel segments and boundary coordinates
    // with a normalized-source tolerance so an edge does not flicker between
    // accepted and rejected at isolated zoom levels.
    const CLIP_EPSILON: f32 = 1.0e-5;
    if p.abs() <= CLIP_EPSILON {
        return q >= -CLIP_EPSILON;
    }
    let r = q / p;
    if p < 0.0 {
        if r > *t1 + CLIP_EPSILON {
            return false;
        }
        if r > *t0 {
            *t0 = r;
        }
    } else {
        if r < *t0 - CLIP_EPSILON {
            return false;
        }
        if r < *t1 {
            *t1 = r;
        }
    }
    true
}

fn clip_crop_workspace_segment_to_source_image(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    a: Pos2,
    b: Pos2,
) -> Option<[Pos2; 2]> {
    let start =
        crop_workspace_screen_to_source(image_rect, geometry, source_width, source_height, a);
    let end = crop_workspace_screen_to_source(image_rect, geometry, source_width, source_height, b);
    let delta = [end[0] - start[0], end[1] - start[1]];
    let mut t0 = 0.0_f32;
    let mut t1 = 1.0_f32;
    if !liang_barsky_clip_test(-delta[0], start[0], &mut t0, &mut t1)
        || !liang_barsky_clip_test(delta[0], 1.0 - start[0], &mut t0, &mut t1)
        || !liang_barsky_clip_test(-delta[1], start[1], &mut t0, &mut t1)
        || !liang_barsky_clip_test(delta[1], 1.0 - start[1], &mut t0, &mut t1)
        || t1 + 1.0e-5 < t0
    {
        return None;
    }
    t0 = t0.clamp(0.0, 1.0);
    t1 = t1.clamp(t0, 1.0);
    let source_a = [start[0] + delta[0] * t0, start[1] + delta[1] * t0];
    let source_b = [start[0] + delta[0] * t1, start[1] + delta[1] * t1];
    let source_a = [source_a[0].clamp(0.0, 1.0), source_a[1].clamp(0.0, 1.0)];
    let source_b = [source_b[0].clamp(0.0, 1.0), source_b[1].clamp(0.0, 1.0)];
    Some([
        crop_workspace_source_to_screen(
            image_rect,
            geometry,
            source_width,
            source_height,
            source_a,
        ),
        crop_workspace_source_to_screen(
            image_rect,
            geometry,
            source_width,
            source_height,
            source_b,
        ),
    ])
}

fn crop_handle_points(rect: Rect) -> [Pos2; 8] {
    [
        rect.left_top(),
        rect.right_top(),
        rect.left_bottom(),
        rect.right_bottom(),
        Pos2::new(rect.center().x, rect.top()),
        Pos2::new(rect.center().x, rect.bottom()),
        Pos2::new(rect.left(), rect.center().y),
        Pos2::new(rect.right(), rect.center().y),
    ]
}

fn crop_handle_at(rect: Rect, pointer: Pos2, radius: f32) -> Option<CropHandle> {
    let candidates = [
        (CropHandle::TopLeft, rect.left_top()),
        (CropHandle::TopRight, rect.right_top()),
        (CropHandle::BottomLeft, rect.left_bottom()),
        (CropHandle::BottomRight, rect.right_bottom()),
        (CropHandle::Top, Pos2::new(rect.center().x, rect.top())),
        (
            CropHandle::Bottom,
            Pos2::new(rect.center().x, rect.bottom()),
        ),
        (CropHandle::Left, Pos2::new(rect.left(), rect.center().y)),
        (CropHandle::Right, Pos2::new(rect.right(), rect.center().y)),
    ];
    for (handle, point) in candidates {
        if point.distance(pointer) <= radius {
            return Some(handle);
        }
    }
    rect.contains(pointer).then_some(CropHandle::Move)
}

fn sanitize_dragged_crop(mut crop: [f32; 4], handle: CropHandle) -> [f32; 4] {
    let min = GeometryTransform::MIN_CROP_EXTENT;
    match handle {
        CropHandle::Left | CropHandle::TopLeft | CropHandle::BottomLeft => {
            crop[0] = crop[0].clamp(0.0, crop[2] - min);
        }
        CropHandle::Right | CropHandle::TopRight | CropHandle::BottomRight => {
            crop[2] = crop[2].clamp(crop[0] + min, 1.0);
        }
        _ => {}
    }
    match handle {
        CropHandle::Top | CropHandle::TopLeft | CropHandle::TopRight => {
            crop[1] = crop[1].clamp(0.0, crop[3] - min);
        }
        CropHandle::Bottom | CropHandle::BottomLeft | CropHandle::BottomRight => {
            crop[3] = crop[3].clamp(crop[1] + min, 1.0);
        }
        _ => {}
    }
    crop
}

fn is_crop_corner(handle: CropHandle) -> bool {
    matches!(
        handle,
        CropHandle::TopLeft
            | CropHandle::TopRight
            | CropHandle::BottomLeft
            | CropHandle::BottomRight
    )
}

/// Constrains a corner drag to the selected aspect ratio while keeping the
/// diagonally opposite corner fixed. The anchor comes from the crop at drag
/// start, so clamping at an image boundary can never make the opposite corner
/// wander under the pointer.
fn constrain_crop_corner_aspect(
    app: &AurawApp,
    original_crop: [f32; 4],
    pointer: [f32; 2],
    handle: CropHandle,
) -> Option<[f32; 4]> {
    let raw = app.loaded_raw.as_ref()?;
    let ratio = app.geometry.aspect_ratio.value(raw.width, raw.height)?;
    let normalized_ratio = ratio / (raw.width.max(1) as f32 / raw.height.max(1) as f32);
    if !normalized_ratio.is_finite() || normalized_ratio <= f32::EPSILON {
        return None;
    }

    let (anchor_x, anchor_y, x_sign, y_sign) = match handle {
        CropHandle::TopLeft => (original_crop[2], original_crop[3], -1.0, -1.0),
        CropHandle::TopRight => (original_crop[0], original_crop[3], 1.0, -1.0),
        CropHandle::BottomLeft => (original_crop[2], original_crop[1], -1.0, 1.0),
        CropHandle::BottomRight => (original_crop[0], original_crop[1], 1.0, 1.0),
        _ => return None,
    };

    let desired_width = (pointer[0] - anchor_x).abs();
    let desired_height = (pointer[1] - anchor_y).abs();

    // Orthogonally project the pointer distance onto width/height pairs that
    // satisfy width / height == normalized_ratio. This makes diagonal, mostly
    // horizontal, and mostly vertical drags all feel continuous.
    let inv_ratio = 1.0 / normalized_ratio;
    let projected_width =
        (desired_width + desired_height * inv_ratio) / (1.0 + inv_ratio * inv_ratio);

    let max_width_from_x = if x_sign < 0.0 {
        anchor_x
    } else {
        1.0 - anchor_x
    };
    let max_height_from_y = if y_sign < 0.0 {
        anchor_y
    } else {
        1.0 - anchor_y
    };
    let max_width = max_width_from_x.min(max_height_from_y * normalized_ratio);

    let min_extent = crate::pipeline::GeometryTransform::MIN_CROP_EXTENT;
    let min_width = min_extent.max(min_extent * normalized_ratio);
    let width = projected_width.clamp(min_width.min(max_width), max_width);
    let height = width / normalized_ratio;

    let dragged_x = anchor_x + x_sign * width;
    let dragged_y = anchor_y + y_sign * height;
    Some(match handle {
        CropHandle::TopLeft => [dragged_x, dragged_y, anchor_x, anchor_y],
        CropHandle::TopRight => [anchor_x, dragged_y, dragged_x, anchor_y],
        CropHandle::BottomLeft => [dragged_x, anchor_y, anchor_x, dragged_y],
        CropHandle::BottomRight => [anchor_x, anchor_y, dragged_x, dragged_y],
        _ => return None,
    })
}

fn constrain_crop_aspect(app: &AurawApp, mut crop: [f32; 4], handle: CropHandle) -> [f32; 4] {
    let Some(raw) = app.loaded_raw.as_ref() else {
        return crop;
    };
    let Some(ratio) = app.geometry.aspect_ratio.value(raw.width, raw.height) else {
        return crop;
    };
    let normalized_ratio = ratio / (raw.width.max(1) as f32 / raw.height.max(1) as f32);
    let width = crop[2] - crop[0];
    let height = crop[3] - crop[1];
    let target_height = width / normalized_ratio.max(f32::EPSILON);
    let target_width = height * normalized_ratio;

    let horizontal_edge = matches!(handle, CropHandle::Left | CropHandle::Right);
    if horizontal_edge
        || (target_height <= 1.0 && (target_height - height).abs() <= (target_width - width).abs())
    {
        let new_height =
            target_height.clamp(crate::pipeline::GeometryTransform::MIN_CROP_EXTENT, 1.0);
        let center = (crop[1] + crop[3]) * 0.5;
        crop[1] = (center - new_height * 0.5).clamp(0.0, 1.0 - new_height);
        crop[3] = crop[1] + new_height;
    } else {
        let new_width =
            target_width.clamp(crate::pipeline::GeometryTransform::MIN_CROP_EXTENT, 1.0);
        let center = (crop[0] + crop[2]) * 0.5;
        crop[0] = (center - new_width * 0.5).clamp(0.0, 1.0 - new_width);
        crop[2] = crop[0] + new_width;
    }
    crop
}

fn fitted_image_size(available: egui::Vec2, image_aspect: f32) -> egui::Vec2 {
    let available_aspect = available.x / available.y.max(1.0);
    if available_aspect > image_aspect {
        egui::vec2(available.y * image_aspect, available.y)
    } else {
        egui::vec2(available.x, available.x / image_aspect.max(f32::EPSILON))
    }
}

fn zoomed_image_rect(outer_rect: Rect, base_size: egui::Vec2, zoom: f32, center: [f32; 2]) -> Rect {
    let size = base_size * zoom;
    let min = Pos2::new(
        outer_rect.center().x - center[0] * size.x,
        outer_rect.center().y - center[1] * size.y,
    );
    Rect::from_min_size(min, size)
}

#[allow(clippy::too_many_arguments)]
fn transform_preview_about_screen_points(
    outer_rect: Rect,
    current_image_rect: Rect,
    base_size: egui::Vec2,
    zoom: &mut f32,
    center: &mut [f32; 2],
    anchor_screen: Pos2,
    target_screen: Pos2,
    zoom_factor: f32,
) -> bool {
    let previous_zoom = *zoom;
    let previous_center = *center;
    let anchor_uv = [
        (anchor_screen.x - current_image_rect.left()) / current_image_rect.width().max(1.0),
        (anchor_screen.y - current_image_rect.top()) / current_image_rect.height().max(1.0),
    ];

    *zoom = (previous_zoom * zoom_factor).clamp(MIN_PREVIEW_ZOOM, MAX_PREVIEW_ZOOM);
    let new_size = base_size * *zoom;
    let new_min = Pos2::new(
        target_screen.x - anchor_uv[0] * new_size.x,
        target_screen.y - anchor_uv[1] * new_size.y,
    );
    *center = [
        (outer_rect.center().x - new_min.x) / new_size.x.max(1.0),
        (outer_rect.center().y - new_min.y) / new_size.y.max(1.0),
    ];
    clamp_preview_center(center, outer_rect.size(), new_size);

    (*zoom - previous_zoom).abs() > f32::EPSILON
        || (center[0] - previous_center[0]).abs() > f32::EPSILON
        || (center[1] - previous_center[1]).abs() > f32::EPSILON
}

fn clamp_preview_center(center: &mut [f32; 2], viewport: egui::Vec2, image: egui::Vec2) {
    for (axis, center_axis) in center.iter_mut().enumerate() {
        let viewport_axis = if axis == 0 { viewport.x } else { viewport.y };
        let image_axis = if axis == 0 { image.x } else { image.y };
        if image_axis <= viewport_axis + 0.5 {
            *center_axis = 0.5;
        } else {
            let half_visible = (viewport_axis / (2.0 * image_axis)).clamp(0.0, 0.5);
            *center_axis = center_axis.clamp(half_visible, 1.0 - half_visible);
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
    masks: &crate::pipeline::MaskStack,
    mask_index: usize,
    width: u32,
    height: u32,
    image_width: u32,
    image_height: u32,
) -> Vec<u8> {
    let final_coverage =
        masks.rasterize_layer(mask_index, width, height, image_width, image_height);
    let component_count = masks
        .masks
        .get(mask_index)
        .map_or(0, |mask| mask.components.len());

    // combined coverage, weighted red, green, blue, and total color weight.
    // Keeping these together avoids allocating one full image per component.
    let mut composite = vec![[0.0_f32; 5]; final_coverage.len()];
    let mut has_component = false;

    for component_index in 0..component_count {
        let Some((combine, enabled, initialized)) = masks
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

        let coverage = masks.rasterize_component_layer(
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

/// Converts a brush tool size into the image-relative radius captured by a dab.
///
/// Tool sizes are defined at fit zoom (1x). Screen-relative mode compensates
/// for preview zoom, while image-relative mode preserves the source footprint.
fn zoom_scaled_brush_size(tool_size: f32, preview_zoom: f32, image_relative: bool) -> f32 {
    let tool_size = tool_size.max(0.0);
    if image_relative {
        tool_size
    } else {
        tool_size / preview_zoom.max(MIN_PREVIEW_ZOOM)
    }
}

fn inpaint_stroke_geometry_screen_bounds(
    dabs: &[BrushDab],
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    for dab in dabs {
        // A source-space circular dab is generally not bounded by the four
        // transformed corners of its source-space square after perspective or
        // nonlinear lens distortion: an arc can bulge beyond every transformed
        // corner. Sample the actual dab circumference through the same final
        // mapping used by the mask texture/cursor so the focused-stroke box
        // cannot visibly cut through a warped mask footprint.
        for screen in brush_outline_geometry_screen_points(
            image_rect,
            geometry,
            lens_geometry,
            source_width,
            source_height,
            dab.center,
            dab.size,
            48,
        ) {
            if !screen.x.is_finite() || !screen.y.is_finite() {
                continue;
            }
            let point_rect = Rect::from_min_max(screen, screen);
            bounds = Some(match bounds {
                Some(existing) => existing.union(point_rect),
                None => point_rect,
            });
        }
    }
    bounds
}

fn screen_to_normalized_unclamped(rect: Rect, point: Pos2) -> [f32; 2] {
    [
        (point.x - rect.left()) / rect.width().max(1.0),
        (point.y - rect.top()) / rect.height().max(1.0),
    ]
}

fn normalized_to_screen(rect: Rect, point: [f32; 2]) -> Pos2 {
    Pos2::new(
        rect.left() + point[0] * rect.width(),
        rect.top() + point[1] * rect.height(),
    )
}

#[cfg(test)]
mod white_balance_picker_tests {
    use super::*;

    #[test]
    fn armed_picker_owns_the_adjustments_canvas_without_mobile_section_state() {
        assert!(white_balance_picker_owns_canvas(
            SidebarTab::Adjustments,
            true
        ));
        assert!(!white_balance_picker_owns_canvas(
            SidebarTab::Adjustments,
            false
        ));
        assert!(!white_balance_picker_owns_canvas(SidebarTab::Crop, true));
    }

    #[test]
    fn zoom_overlay_uses_a_native_density_source_crop() {
        let region = overlay_raster_region(
            crate::app::PreviewUvRect {
                min: [0.45, 0.40],
                max: [0.55, 0.60],
            },
            6000,
            4000,
            Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0)),
            1.0,
            2,
        );

        assert_eq!(region.source_x, 2698);
        assert_eq!(region.source_y, 1598);
        assert_eq!(region.source_width, 604);
        assert_eq!(region.source_height, 804);
        // The visible crop contains only 600x800 native pixels, so zooming it
        // to a larger viewport must retain those native samples rather than
        // rasterizing the entire 6000px frame into a 512px overlay.
        assert_eq!(region.texture_width, 604);
        assert_eq!(region.texture_height, 804);
    }

    #[test]
    fn cropped_inpaint_dab_keeps_its_source_pixel_radius() {
        let region = OverlayRasterKey {
            source_x: 2500,
            source_y: 1500,
            source_width: 1000,
            source_height: 1000,
            texture_width: 1000,
            texture_height: 1000,
        };
        let dab = BrushDab {
            center: [0.5, 0.5],
            opacity: 1.0,
            size: 0.01,
            feather: 0.0,
        };
        let cropped = crop_overlay_dabs(&[dab], region, 6000, 4000);
        assert_eq!(cropped[0].center, [0.5, 0.5]);
        assert!((cropped[0].size - 0.04).abs() < 1e-6);
    }

    #[test]
    fn screen_relative_brush_compensates_for_zoom() {
        assert!((zoom_scaled_brush_size(0.08, 4.0, false) - 0.02).abs() < 1e-6);
    }

    #[test]
    fn image_relative_brush_ignores_zoom() {
        assert!((zoom_scaled_brush_size(0.08, 4.0, true) - 0.08).abs() < 1e-6);
    }
}
