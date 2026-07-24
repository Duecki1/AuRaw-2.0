use crate::app::{AurawApp, CropDragState, CropHandle, MaskDragState, MaskOverlayBlink, SidebarTab};
use crate::pipeline::{
    rasterize_inpaint_dabs_binary, BrushDab, BrushMode, MaskCombineMode, MaskGeometry, MaskKind,
    ObjectStroke,
};
use crate::ui::mask_component_color;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Ui};

const MIN_PREVIEW_ZOOM: f32 = 0.70;
const MAX_PREVIEW_ZOOM: f32 = 32.0;

pub struct Preview;

impl Preview {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        let Some((texture_id, pipeline_width, pipeline_height)) =
            app.preview_base_pipeline().and_then(|pipeline| {
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
                        ui.label("Open a RAW from the Library to start developing.");
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
        // While zoomed, the displayed backing texture may switch between the
        // normal proxy and the tiny full-frame navigation proxy while an
        // adjustment is being dragged. Fit view deliberately never makes that
        // swap, avoiding a packaged-build pixelation flash. The independently
        // downscaled proxies can differ by a pixel
        // after integer rounding, which gives them a slightly different aspect
        // ratio. Deriving zoom geometry from whichever texture happens to be
        // active makes that texture swap look like camera motion: visible UVs
        // change, `note_preview_motion` invalidates the detail crop, and the
        // low-resolution backing briefly flashes through. Anchor all preview
        // geometry to the full developed image instead; texture swaps then only
        // change pixels, never the zoom/crop coordinate system.
        let (geometry_width, geometry_height) = app
            .loaded_raw
            .as_ref()
            .map(|raw| (raw.width, raw.height))
            .unwrap_or((pipeline_width, pipeline_height));
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
        let brush_canvas = matches!(app.sidebar_tab, SidebarTab::Masks | SidebarTab::Inpainting);
        let interaction_id = match app.sidebar_tab {
            SidebarTab::Masks => ui.id().with("develop-preview-mask-interaction"),
            SidebarTab::Inpainting => ui.id().with("develop-preview-inpaint-interaction"),
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

            // A second finger switches a mask gesture into viewport navigation.
            // Roll back any pending mask stroke and prevent this frame from painting.
            if app.sidebar_tab == SidebarTab::Masks {
                app.cancel_mask_touch_gesture();
            } else if app.sidebar_tab == SidebarTab::Crop {
                app.crop_drag = None;
            } else if app.sidebar_tab == SidebarTab::Inpainting {
                app.inpaint_stroke.clear();
                app.last_inpaint_brush_point = None;
                app.inpaint_stroke_texture = None;
                app.inpaint_stroke_texture_key = None;
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

        let pan_with_primary = !touch_navigation
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

        let fit_gesture = !touch_navigation && response.double_clicked();
        if fit_gesture {
            app.preview_zoom = 1.0;
            app.preview_center = [0.5, 0.5];
            moved = true;
        }

        image_rect = zoomed_image_rect(outer_rect, base_size, app.preview_zoom, app.preview_center);
        let visible_screen = outer_rect.intersect(image_rect);
        let pixels_per_point = ui.ctx().pixels_per_point();
        let viewport_pixels = [
            (visible_screen.width() * pixels_per_point).round().max(1.0) as u32,
            (visible_screen.height() * pixels_per_point)
                .round()
                .max(1.0) as u32,
        ];
        if app.preview_viewport_pixels != viewport_pixels {
            app.preview_viewport_pixels = viewport_pixels;
            moved = true;
        }
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
                    Rect::from_min_max(
                        Pos2::new(detail.texture_uv_rect.min[0], detail.texture_uv_rect.min[1]),
                        Pos2::new(detail.texture_uv_rect.max[0], detail.texture_uv_rect.max[1]),
                    ),
                    Color32::WHITE,
                );
            }
        }

        if app.sidebar_tab == SidebarTab::Crop && !app.original_preview_visible() {
            if !touch_navigation && !fit_gesture {
                Self::handle_crop_interaction(ui, app, image_rect);
            }
            Self::paint_crop_overlay(ui, app, image_rect, visible_screen);
        }

        painter.text(
            outer_rect.left_top() + egui::vec2(10.0, 10.0),
            egui::Align2::LEFT_TOP,
            format!(
                "{:.0}% · pinch/scroll zoom · drag pan · double-tap/click fit",
                app.preview_zoom * 100.0
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
                    &response,
                );
            }
            // Completed inpainting is part of the developed image and remains
            // visible while switching between Develop tabs. The live stroke
            // and cursor are shown only while the Inpainting tab is active.
            Self::paint_inpaint_overlay(ui, app, image_rect, visible_screen);

            if app.sidebar_tab == SidebarTab::Masks {
                if !touch_navigation && !fit_gesture {
                    Self::handle_mask_interaction(
                        ui,
                        app,
                        image_rect,
                        visible_screen,
                        outer_rect,
                        &response,
                    );
                }
                // Coverage stays clipped to the image, while geometry/transform
                // handles may extend into the surrounding preview pasteboard.
                Self::paint_mask_overlay(ui, app, image_rect, visible_screen, outer_rect);
                Self::paint_tool_hint(ui, app, visible_screen);
            }
        }
    }

    fn handle_crop_interaction(ui: &mut Ui, app: &mut AurawApp, image_rect: Rect) {
        if image_rect.width() <= 1.0 || image_rect.height() <= 1.0 {
            return;
        }
        let pointer = ui.input(|input| input.pointer.interact_pos());
        let primary_pressed = ui.input(|input| input.pointer.primary_pressed());
        let primary_down = ui.input(|input| input.pointer.primary_down());
        let primary_released = ui.input(|input| input.pointer.primary_released());

        let to_normalized = |position: Pos2| -> [f32; 2] {
            [
                ((position.x - image_rect.left()) / image_rect.width()).clamp(0.0, 1.0),
                ((position.y - image_rect.top()) / image_rect.height()).clamp(0.0, 1.0),
            ]
        };

        if primary_pressed {
            if let Some(pointer) = pointer.filter(|point| image_rect.expand(28.0).contains(*point)) {
                let crop_rect = crop_screen_rect(image_rect, app.geometry.crop);
                if let Some(handle) = crop_handle_at(crop_rect, pointer, 28.0) {
                    app.crop_drag = Some(CropDragState {
                        handle,
                        start: to_normalized(pointer),
                        crop: app.geometry.crop,
                    });
                }
            }
        }

        if primary_down {
            if let (Some(pointer), Some(drag)) = (pointer, app.crop_drag) {
                let current = to_normalized(pointer);
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
                if crop != app.geometry.crop {
                    app.geometry.crop = crop;
                    app.note_geometry_changed();
                }
            }
        }

        if primary_released || !primary_down {
            app.crop_drag = None;
        }
    }

    fn paint_crop_overlay(ui: &mut Ui, app: &AurawApp, image_rect: Rect, visible_rect: Rect) {
        let painter = ui.painter_at(visible_rect);
        let crop_rect = crop_screen_rect(image_rect, app.geometry.crop).intersect(visible_rect);
        if crop_rect.width() <= 0.0 || crop_rect.height() <= 0.0 {
            return;
        }
        let shade = Color32::from_black_alpha(150);
        for rect in [
            Rect::from_min_max(visible_rect.min, Pos2::new(visible_rect.right(), crop_rect.top())),
            Rect::from_min_max(Pos2::new(visible_rect.left(), crop_rect.bottom()), visible_rect.max),
            Rect::from_min_max(Pos2::new(visible_rect.left(), crop_rect.top()), Pos2::new(crop_rect.left(), crop_rect.bottom())),
            Rect::from_min_max(Pos2::new(crop_rect.right(), crop_rect.top()), Pos2::new(visible_rect.right(), crop_rect.bottom())),
        ] {
            if rect.width() > 0.0 && rect.height() > 0.0 {
                painter.rect_filled(rect, 0.0, shade);
            }
        }

        painter.rect_stroke(crop_rect, 0.0, Stroke::new(2.0, Color32::WHITE), egui::StrokeKind::Inside);
        for fraction in [1.0 / 3.0, 2.0 / 3.0] {
            let x = egui::lerp(crop_rect.left()..=crop_rect.right(), fraction);
            let y = egui::lerp(crop_rect.top()..=crop_rect.bottom(), fraction);
            painter.line_segment(
                [Pos2::new(x, crop_rect.top()), Pos2::new(x, crop_rect.bottom())],
                Stroke::new(1.0, Color32::from_white_alpha(115)),
            );
            painter.line_segment(
                [Pos2::new(crop_rect.left(), y), Pos2::new(crop_rect.right(), y)],
                Stroke::new(1.0, Color32::from_white_alpha(115)),
            );
        }

        for point in crop_handle_points(crop_rect) {
            painter.circle_filled(point, 5.5, Color32::WHITE);
            painter.circle_stroke(point, 7.5, Stroke::new(1.5, Color32::BLACK));
        }

        let angle = (app.geometry.rotation_degrees + f32::from(app.geometry.quarter_turns) * 90.0).to_radians();
        if angle.abs() > 0.001 {
            let center = crop_rect.center();
            let direction = egui::vec2(angle.cos(), angle.sin());
            let half = crop_rect.width().min(crop_rect.height()) * 0.36;
            painter.line_segment(
                [center - direction * half, center + direction * half],
                Stroke::new(1.5, Color32::from_rgb(255, 196, 64)),
            );
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

        let allowed = !matches!(app.sidebar_tab, SidebarTab::Crop | SidebarTab::Masks | SidebarTab::Inpainting)
            && !touch_navigation
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
        response: &egui::Response,
    ) {
        let pointer = response
            .interact_pointer_pos()
            .filter(|position| preview_rect.contains(*position));
        let primary_down = pointer.is_some()
            && response.is_pointer_button_down_on()
            && ui.input(|input| input.pointer.primary_down());
        if !primary_down {
            if ui.input(|input| input.pointer.primary_released()) {
                let stroke_finished =
                    app.last_inpaint_brush_point.take().is_some() && !app.inpaint_stroke.is_empty();
                if stroke_finished {
                    app.request_inpaint(frame);
                }
            }
            return;
        }
        if app.inpaint_busy() {
            return;
        }
        let Some(pointer) = pointer else {
            return;
        };
        let uv = screen_to_normalized(image_rect, pointer);
        let first_dab = app.last_inpaint_brush_point.is_none();
        let previous = app.last_inpaint_brush_point.unwrap_or(uv);
        let dx = uv[0] - previous[0];
        let dy = uv[1] - previous[1];
        let distance_px =
            ((dx * image_rect.width()).powi(2) + (dy * image_rect.height()).powi(2)).sqrt();
        let dab_size = zoom_scaled_brush_size(app.inpaint_brush_size, app.preview_zoom);
        let radius_px = dab_size * image_rect.width().min(image_rect.height());
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
                    center: [previous[0] + dx * t, previous[1] + dy * t],
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

    fn paint_inpaint_overlay(ui: &Ui, app: &mut AurawApp, image_rect: Rect, preview_rect: Rect) {
        let painter = ui.painter_at(preview_rect);
        if app.sidebar_tab != SidebarTab::Inpainting {
            return;
        }

        let focused_stroke = app
            .inpaint_hovered_stroke
            .or(app.inpaint_selected_stroke)
            .filter(|index| *index < app.inpaint_strokes.len());
        if let Some(index) = focused_stroke {
            let Some(pipeline) = app.gpu_pipeline.as_ref() else {
                return;
            };
            let hovered = app.inpaint_hovered_stroke == Some(index);
            let max_edge = if cfg!(target_os = "android") {
                384.0
            } else {
                512.0
            };
            let scale = (max_edge / image_rect.width().max(image_rect.height())).min(1.0);
            let width = (image_rect.width() * scale).round().max(1.0) as u32;
            let height = (image_rect.height() * scale).round().max(1.0) as u32;
            let key = (
                index,
                app.inpaint_texture_revision,
                width,
                height,
                hovered,
            );
            if app.inpaint_focus_texture_key != Some(key) {
                let coverage = rasterize_inpaint_dabs_binary(
                    width,
                    height,
                    pipeline.width,
                    pipeline.height,
                    &app.inpaint_strokes[index].dabs,
                );
                let color = if hovered {
                    Color32::from_rgb(255, 190, 70)
                } else {
                    Color32::from_rgb(77, 196, 255)
                };
                let rgba = coverage_rgba(coverage, color);
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
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
                painter_image_clipped(ui, texture.id(), image_rect, preview_rect);
            }

            if let Some(bounds) = inpaint_stroke_screen_bounds(
                &app.inpaint_strokes[index].dabs,
                image_rect,
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
            let key = (app.inpaint_stroke.len(), width, height);
            if app.inpaint_stroke_texture_key != Some(key) {
                let coverage = rasterize_inpaint_dabs_binary(
                    width,
                    height,
                    pipeline.width,
                    pipeline.height,
                    &app.inpaint_stroke,
                );
                let rgba = coverage_rgba(coverage, Color32::from_rgb(255, 94, 94));
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [width as usize, height as usize],
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
                painter_image_clipped(ui, texture.id(), image_rect, preview_rect);
            }
        }

        if let Some(pointer) = ui
            .ctx()
            .pointer_hover_pos()
            .filter(|position| preview_rect.contains(*position))
        {
            let radius = zoom_scaled_brush_size(app.inpaint_brush_size, app.preview_zoom)
                * image_rect.width().min(image_rect.height());
            painter.circle_stroke(pointer, radius.max(1.5), Stroke::new(1.5, Color32::WHITE));
        }
    }

    fn handle_mask_interaction(
        ui: &Ui,
        app: &mut AurawApp,
        image_rect: Rect,
        preview_rect: Rect,
        overlay_rect: Rect,
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
        let primary_down = pointer.is_some()
            && response.is_pointer_button_down_on()
            && ui.input(|input| input.pointer.primary_down());
        if !primary_down {
            let object_stroke_finished = kind == MaskKind::Object && app.last_brush_point.is_some();
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
        let uv = if matches!(kind, MaskKind::Radial | MaskKind::Linear) {
            screen_to_normalized_unclamped(image_rect, pointer)
        } else {
            screen_to_normalized(image_rect, pointer)
        };
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
            app.mask_drag = begin_mask_drag(geometry, uv, pointer, image_rect);
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
                        dabs,
                    },
                    MaskKind::Brush,
                ) => {
                    let opacity = match app.brush_mode {
                        BrushMode::Paint => 1.0,
                        BrushMode::Erase => -1.0,
                    };
                    let first_dab = app.last_brush_point.is_none();
                    let previous = app.last_brush_point.unwrap_or(uv);
                    let dx = uv[0] - previous[0];
                    let dy = uv[1] - previous[1];
                    // Space dabs in screen/image pixels rather than raw UV
                    // units so strokes remain continuous on wide and tall
                    // images on both mouse and touch input.
                    let distance_px = ((dx * image_rect.width()).powi(2)
                        + (dy * image_rect.height()).powi(2))
                    .sqrt();
                    let dab_size = zoom_scaled_brush_size(*size, app.preview_zoom);
                    let radius_px = dab_size * image_rect.width().min(image_rect.height());
                    let spacing_px = (radius_px * 0.22).clamp(0.85, 24.0);

                    // Pointer-down frames with no movement used to append a
                    // duplicate dab indefinitely. That made long touch holds
                    // and slow strokes progressively more expensive without
                    // changing a single mask pixel.
                    if first_dab {
                        if dabs.len() < 8192 {
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
                        *start = screen_to_normalized_unclamped(image_rect, midpoint - half_vector);
                        *end = screen_to_normalized_unclamped(image_rect, midpoint + half_vector);
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
                    let distance_px = ((dx * image_rect.width()).powi(2)
                        + (dy * image_rect.height()).powi(2))
                    .sqrt();
                    let stroke_brush_size = zoom_scaled_brush_size(*brush_size, app.preview_zoom);
                    let radius_px = stroke_brush_size * image_rect.width().min(image_rect.height());
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
    ) {
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

        if app.masks.selected_component().is_some_and(|component| {
            matches!(component.kind, MaskKind::Brush | MaskKind::Object) && component.enabled
        }) {
            if let Some(pointer) = ui
                .ctx()
                .pointer_hover_pos()
                .filter(|position| preview_rect.contains(*position))
            {
                if let Some(component) = app.masks.selected_component() {
                    let cursor_color = match app.brush_mode {
                        BrushMode::Paint => Color32::WHITE,
                        BrushMode::Erase => subtract,
                    };
                    match &component.geometry {
                        MaskGeometry::Brush { size, .. } => {
                            let radius = zoom_scaled_brush_size(*size, app.preview_zoom)
                                * image_rect.width().min(image_rect.height());
                            painter.circle_stroke(pointer, radius.max(1.5), Stroke::new(1.5, cursor_color));
                        }
                        MaskGeometry::Object { brush_size, .. } => {
                            let radius = zoom_scaled_brush_size(*brush_size, app.preview_zoom)
                                * image_rect.width().min(image_rect.height());
                            painter.circle_stroke(
                                pointer,
                                radius.max(1.5),
                                Stroke::new(1.5, cursor_color),
                            );
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
            painter_image_clipped(ui, texture.id(), image_rect, preview_rect);
        }
    }

    fn paint_tool_hint(ui: &Ui, app: &AurawApp, preview_rect: Rect) {
        let Some(kind) = app.active_mask_tool else {
            return;
        };
        let text = match kind {
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

fn crop_screen_rect(image_rect: Rect, crop: [f32; 4]) -> Rect {
    Rect::from_min_max(
        Pos2::new(
            egui::lerp(image_rect.left()..=image_rect.right(), crop[0]),
            egui::lerp(image_rect.top()..=image_rect.bottom(), crop[1]),
        ),
        Pos2::new(
            egui::lerp(image_rect.left()..=image_rect.right(), crop[2]),
            egui::lerp(image_rect.top()..=image_rect.bottom(), crop[3]),
        ),
    )
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
        (CropHandle::Bottom, Pos2::new(rect.center().x, rect.bottom())),
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
    use crate::pipeline::GeometryTransform;
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
    let ratio = app
        .geometry
        .aspect_ratio
        .value(raw.width, raw.height)?;
    let normalized_ratio =
        ratio / (raw.width.max(1) as f32 / raw.height.max(1) as f32);
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
    let projected_width = (desired_width + desired_height * inv_ratio)
        / (1.0 + inv_ratio * inv_ratio);

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
    if horizontal_edge || (target_height <= 1.0 && (target_height - height).abs() <= (target_width - width).abs()) {
        let new_height = target_height.clamp(crate::pipeline::GeometryTransform::MIN_CROP_EXTENT, 1.0);
        let center = (crop[1] + crop[3]) * 0.5;
        crop[1] = (center - new_height * 0.5).clamp(0.0, 1.0 - new_height);
        crop[3] = crop[1] + new_height;
    } else {
        let new_width = target_width.clamp(crate::pipeline::GeometryTransform::MIN_CROP_EXTENT, 1.0);
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

fn painter_image_clipped(ui: &Ui, texture_id: egui::TextureId, rect: Rect, clip_rect: Rect) {
    ui.painter_at(clip_rect).image(
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

/// Converts a brush tool size into the image-relative radius captured by a dab.
///
/// Tool sizes are defined at fit zoom (1x). Dividing by the current preview
/// zoom keeps the brush footprint constant in screen space: zooming in paints
/// fewer source pixels for detail work, while zooming out covers more.
fn zoom_scaled_brush_size(tool_size: f32, preview_zoom: f32) -> f32 {
    tool_size.max(0.0) / preview_zoom.max(MIN_PREVIEW_ZOOM)
}

fn inpaint_stroke_screen_bounds(dabs: &[BrushDab], image_rect: Rect) -> Option<Rect> {
    let mut bounds: Option<Rect> = None;
    let short_edge = image_rect.width().min(image_rect.height());
    for dab in dabs {
        let center = normalized_to_screen(image_rect, dab.center);
        let radius = (dab.size * short_edge).max(1.0);
        let dab_rect = Rect::from_center_size(center, egui::vec2(radius * 2.0, radius * 2.0));
        bounds = Some(match bounds {
            Some(existing) => existing.union(dab_rect),
            None => dab_rect,
        });
    }
    bounds
}

fn screen_to_normalized(rect: Rect, point: Pos2) -> [f32; 2] {
    let uv = screen_to_normalized_unclamped(rect, point);
    [uv[0].clamp(0.0, 1.0), uv[1].clamp(0.0, 1.0)]
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
