#![allow(clippy::too_many_arguments)]

use crate::app::{
    AurawApp, CropDragState, CropHandle, MaskDragState, MaskOverlayBlink, OverlayRasterKey,
    SidebarTab, StraightenDragState,
};
use crate::pipeline::{
    BrushDab, BrushMode, GeometryTransform, LensGeometryMap, MaskCombineMode, MaskGeometry,
    MaskKind, ObjectStroke, RetouchStroke,
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

mod canvas;
mod interaction;
mod overlays;
mod tools;
mod transform;

use canvas::*;
use interaction::*;
use overlays::*;
use transform::*;

#[cfg(test)]
mod tests;

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
        let source_dimensions = app
            .develop
            .loaded_raw
            .as_ref()
            .map(|raw| (raw.width, raw.height))
            .unwrap_or((pipeline_width, pipeline_height));
        let lens_geometry = app
            .develop
            .loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());
        let crop_preview =
            app.ui.sidebar_tab == SidebarTab::Crop && !app.preview.original_visible();
        let final_geometry_preview =
            !crop_preview && (!app.develop.geometry.is_identity() || lens_geometry.is_some());
        let (geometry_width, geometry_height) = if final_geometry_preview {
            app.develop
                .geometry
                .crop_pixel_dimensions(source_dimensions.0, source_dimensions.1)
        } else if crop_preview && app.develop.geometry.quarter_turns % 2 == 1 {
            (source_dimensions.1, source_dimensions.0)
        } else {
            source_dimensions
        };
        let base_size = fitted_image_size(
            outer_rect.size(),
            geometry_width as f32 / geometry_height.max(1) as f32,
        );
        app.preview.zoom = app.preview.zoom.clamp(MIN_PREVIEW_ZOOM, MAX_PREVIEW_ZOOM);
        clamp_preview_center(
            &mut app.preview.center,
            outer_rect.size(),
            base_size * app.preview.zoom,
        );
        let mut image_rect =
            zoomed_image_rect(outer_rect, base_size, app.preview.zoom, app.preview.center);
        let visible_image_rect = outer_rect.intersect(image_rect);
        let mut interaction_rect =
            if matches!(app.ui.sidebar_tab, SidebarTab::Masks | SidebarTab::Crop) {
                outer_rect
            } else {
                visible_image_rect
            };
        if interaction_rect.width() <= 0.0 || interaction_rect.height() <= 0.0 {
            interaction_rect = outer_rect;
        }
        let white_balance_canvas = white_balance_picker_owns_canvas(
            app.ui.sidebar_tab,
            app.develop_ui.white_balance_picker_active,
        );
        if !white_balance_canvas {
            app.develop_ui.white_balance_picker_drag = None;
        }
        let brush_canvas = matches!(
            app.ui.sidebar_tab,
            SidebarTab::Masks | SidebarTab::Inpainting
        ) || white_balance_canvas;
        let interaction_id = match app.ui.sidebar_tab {
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
            ui.ctx().request_repaint();
        }
        if multi_touch.is_some() {
            app.preview.touch_navigation_active = true;
        } else if !any_touches {
            app.preview.touch_navigation_active = false;
        }
        let touch_navigation = app.preview.touch_navigation_active;

        if let Some(multi_touch) = multi_touch {
            let previous_touch_center = multi_touch.center_pos - multi_touch.translation_delta;
            moved |= transform_preview_about_screen_points(
                outer_rect,
                image_rect,
                base_size,
                &mut app.preview.zoom,
                &mut app.preview.center,
                previous_touch_center,
                multi_touch.center_pos,
                multi_touch.zoom_delta,
            );
            image_rect =
                zoomed_image_rect(outer_rect, base_size, app.preview.zoom, app.preview.center);
        }

        if multi_touch.is_some() {
            if app.ui.sidebar_tab == SidebarTab::Masks {
                app.cancel_mask_touch_gesture();
            } else if app.ui.sidebar_tab == SidebarTab::Crop {
                app.develop_ui.crop_drag = None;
                app.develop_ui.straighten_drag = None;
            } else if app.ui.sidebar_tab == SidebarTab::Inpainting {
                app.inpaint.active_points.clear();
                app.inpaint.last_brush_uv = None;
            } else if white_balance_canvas {
                app.develop_ui.white_balance_picker_drag = None;
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
                    &mut app.preview.zoom,
                    &mut app.preview.center,
                    pointer,
                    pointer,
                    (scroll_y * 0.0018).exp(),
                );
            }
        }

        let pan_with_primary = multi_touch.is_none()
            && !original_hold_tracking
            && !brush_canvas
            && app.ui.sidebar_tab != SidebarTab::Crop
            && response.dragged_by(egui::PointerButton::Primary);
        let pan_with_middle = !touch_navigation && response.dragged_by(egui::PointerButton::Middle);
        if pan_with_primary || pan_with_middle {
            let delta = ui.input(|input| input.pointer.delta());
            let image_size = base_size * app.preview.zoom;
            app.preview.center[0] -= delta.x / image_size.x.max(1.0);
            app.preview.center[1] -= delta.y / image_size.y.max(1.0);
            clamp_preview_center(&mut app.preview.center, outer_rect.size(), image_size);
            moved |= delta.length_sq() > 0.0;
        }

        let fit_gesture = !white_balance_canvas && !touch_navigation && response.double_clicked();
        if fit_gesture {
            app.preview.zoom = 1.0;
            app.preview.center = [0.5, 0.5];
            moved = true;
        }

        image_rect = zoomed_image_rect(outer_rect, base_size, app.preview.zoom, app.preview.center);
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
                app.develop.geometry,
                lens_geometry.as_deref(),
                source_dimensions.0,
                source_dimensions.1,
            )
        } else if final_geometry_preview {
            final_geometry_visible_source_uv(
                image_rect,
                visible_screen,
                app.develop.geometry,
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
        if preview_uv_changed(app.preview.visible_uv, visible_uv) {
            app.preview.visible_uv = visible_uv;
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
                app.develop.geometry,
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
                app.develop.geometry,
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
            .preview
            .detail
            .as_ref()
            .filter(|detail| detail.revision == app.preview.revision)
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
                        app.develop.geometry,
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
                        app.develop.geometry,
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

        if app.preview.original_visible() {
            painter.text(
                outer_rect.right_top() + egui::vec2(-12.0, 12.0),
                egui::Align2::RIGHT_TOP,
                "ORIGINAL",
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
        }

        if !app.preview.original_visible() {
            if app.ui.sidebar_tab == SidebarTab::Inpainting && !touch_navigation && !fit_gesture {
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

            if app.ui.sidebar_tab == SidebarTab::Masks {
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
}
