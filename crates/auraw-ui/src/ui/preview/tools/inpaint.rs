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
        let lens_geometry = app.develop.loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());
        let pointer = response
            .interact_pointer_pos()
            .filter(|position| preview_rect.contains(*position));
        let (primary_is_down, primary_released, alt_down) = ui.input(|input| {
            (
                input.pointer.primary_down(),
                input.pointer.primary_released(),
                input.modifiers.alt,
            )
        });
        let primary_down =
            pointer.is_some() && response.is_pointer_button_down_on() && primary_is_down;
        if app.inpaint_busy() {
            return;
        }

        if app.inpaint.tool.requires_source()
            && (app.inpaint.source_pick_active || (alt_down && primary_down))
        {
            if alt_down && primary_down {
                app.inpaint.source_pick_active = true;
            }
            app.inpaint.stroke.clear();
            app.inpaint.last_brush_point = None;
            app.inpaint.stroke_texture = None;
            app.inpaint.stroke_texture_key = None;
            if primary_down {
                if let Some(pointer) = pointer {
                    let source_uv = final_geometry_screen_to_native_source(
                        image_rect,
                        app.develop.geometry,
                        lens_geometry.as_deref(),
                        source_width,
                        source_height,
                        pointer,
                    );
                    if let Some(uv) = editable_source_uv(source_uv) {
                        app.inpaint.source_anchor = Some(uv);
                        app.inpaint.source_offset = None;
                        ui.ctx().request_repaint();
                    }
                }
            }
            if primary_released {
                app.inpaint.source_pick_active = false;
                ui.ctx().request_repaint();
            }
            return;
        }
        if app.inpaint.tool.requires_source() && app.inpaint.source_anchor.is_none() {
            app.inpaint.source_pick_active = true;
            return;
        }
        if !primary_down {
            if primary_released {
                // `last_inpaint_brush_point` is intentionally cleared when a
                // stroke crosses transformed pasteboard. The accumulated dabs,
                // not the last pointer position, are the reliable indication
                // that this gesture has real work to submit.
                app.inpaint.last_brush_point = None;
                if !app.inpaint.stroke.is_empty() {
                    app.request_inpaint(frame);
                }
            } else if primary_is_down {
                // The pointer can leave the clipped preview while the button is
                // still held. Break interpolation until it re-enters so a stroke
                // never jumps across hidden/pasteboard space.
                app.inpaint.last_brush_point = None;
            }
            return;
        }
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
        // The crop rectangle defines the destination frame; after straighten or
        // keystone correction, valid destination pixels can legitimately sample
        // source pixels outside that crop rectangle. Only reject true pasteboard
        // where the inverse transform lands outside the source image itself.
        let Some(uv) = editable_source_uv(source_uv) else {
            // Break the stroke while crossing pasteboard so re-entering the image
            // cannot bridge an inpaint line across an empty transformed corner.
            app.inpaint.last_brush_point = None;
            return;
        };

        let starting_stroke = app.inpaint.stroke.is_empty();
        let first_dab = app.inpaint.last_brush_point.is_none();
        let previous = app.inpaint.last_brush_point.unwrap_or(uv);
        let previous_screen = final_geometry_native_source_to_screen(
            image_rect,
            app.develop.geometry,
            lens_geometry.as_deref(),
            source_width,
            source_height,
            previous,
        );
        let distance_px = pointer.distance(previous_screen);
        let dab_size = zoom_scaled_brush_size(
            app.inpaint.brush_size,
            app.preview.zoom,
            app.preferences.image_relative_brush_size,
        );
        let radius_px = geometry_brush_radius_screen(
            image_rect,
            app.develop.geometry,
            lens_geometry.as_deref(),
            source_width,
            source_height,
            uv,
            dab_size,
        );
        let spacing_px = (radius_px * 0.22).clamp(0.85, 24.0);
        let mut changed = false;
        if first_dab {
            if app.inpaint.stroke.len() < 8192 {
                if starting_stroke && app.inpaint.tool.requires_source() {
                    if app.inpaint.source_offset.is_none() {
                        app.inpaint.source_offset = app.inpaint.source_anchor
                            .map(|source| [source[0] - uv[0], source[1] - uv[1]]);
                    }
                    app.prepare_live_retouch_preview(frame);
                }
                app.inpaint.stroke.push(BrushDab {
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
                if app.inpaint.stroke.len() >= 8192 {
                    break;
                }
                let t = step as f32 / steps as f32;
                app.inpaint.stroke.push(BrushDab {
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
            app.inpaint.last_brush_point = Some(uv);
            app.inpaint.stroke_texture_key = None;
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
        let painter = ui.painter_at(preview_rect);
        if app.ui.sidebar_tab != SidebarTab::Inpainting {
            return;
        }
        let lens_geometry = app.develop.loaded_raw
            .as_ref()
            .and_then(|raw| raw.lens_geometry.clone());

        let focused_stroke = app.inpaint.hovered_stroke
            .or(app.inpaint.selected_stroke)
            .filter(|index| *index < app.inpaint.strokes.len());
        if let Some(index) = focused_stroke {
            if app.preview.gpu_pipeline.is_none() {
                return;
            }
            let hovered = app.inpaint.hovered_stroke == Some(index);
            let region = overlay_raster_region(
                app.preview.visible_uv,
                source_width,
                source_height,
                preview_rect,
                physical_pixels_per_point(ui.ctx()),
                2,
            );
            let key = (index, app.inpaint.texture_revision, region, hovered);
            if app.inpaint.focus_texture_key != Some(key) {
                let dabs = crop_overlay_dabs(
                    &app.inpaint.strokes[index].dabs,
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
                if let Some(texture) = app.inpaint.focus_texture.as_mut() {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    app.inpaint.focus_texture = Some(ui.ctx().load_texture(
                        "auraw-inpaint-focused-stroke",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                app.inpaint.focus_texture_key = Some(key);
            }
            if let Some(texture) = &app.inpaint.focus_texture {
                paint_final_geometry_overlay_texture(
                    ui,
                    texture.id(),
                    image_rect,
                    app.develop.geometry,
                    app.develop.loaded_raw
                        .as_ref()
                        .and_then(|raw| raw.lens_geometry.as_deref()),
                    source_width,
                    source_height,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    overlay_source_uv(region, source_width, source_height),
                );
            }

            if let Some(bounds) = inpaint_stroke_geometry_screen_bounds(
                &app.inpaint.strokes[index].dabs,
                image_rect,
                app.develop.geometry,
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
                        format!("{} {}", app.inpaint.strokes[index].kind.label(), index + 1),
                        egui::FontId::proportional(11.0),
                        color,
                    );
                }
            }
            let stroke = &app.inpaint.strokes[index];
            if let (Some(first_dab), Some(offset)) = (stroke.dabs.first(), stroke.source_offset) {
                let source_uv = [
                    first_dab.center[0] + offset[0],
                    first_dab.center[1] + offset[1],
                ];
                let destination_screen = final_geometry_native_source_to_screen(
                    image_rect,
                    app.develop.geometry,
                    lens_geometry.as_deref(),
                    source_width,
                    source_height,
                    first_dab.center,
                );
                let source_screen = final_geometry_native_source_to_screen(
                    image_rect,
                    app.develop.geometry,
                    lens_geometry.as_deref(),
                    source_width,
                    source_height,
                    source_uv,
                );
                painter.line_segment(
                    [destination_screen, source_screen],
                    Stroke::new(1.0, Color32::from_rgb(95, 225, 155)),
                );
                paint_retouch_source_marker(
                    &painter,
                    image_rect,
                    app.develop.geometry,
                    lens_geometry.as_deref(),
                    source_width,
                    source_height,
                    source_uv,
                    first_dab.size,
                    "Source",
                );
            }
        }

        if !app.inpaint.stroke.is_empty() {
            if app.preview.gpu_pipeline.is_none() {
                return;
            }
            let region = inpaint_live_overlay_region(
                &app.inpaint.stroke,
                app.preview.visible_uv,
                source_width,
                source_height,
                preview_rect,
                physical_pixels_per_point(ui.ctx()),
            );
            let key = (app.inpaint.stroke.len(), region);
            if app.inpaint.stroke_texture_key != Some(key) {
                let dabs =
                    crop_overlay_dabs(&app.inpaint.stroke, region, source_width, source_height);
                let rgba = if app.inpaint.tool.requires_source() {
                    let coverage = rasterize_brush_dabs(
                        region.texture_width,
                        region.texture_height,
                        region.source_width,
                        region.source_height,
                        &dabs,
                    );
                    app.inpaint.live_retouch_preview()
                        .and_then(|source| {
                            live_retouch_rgba(
                                source,
                                region,
                                source_width,
                                source_height,
                                &app.inpaint.stroke,
                                &coverage,
                                app.inpaint.tool,
                                app.inpaint.source_offset?,
                            )
                        })
                        .unwrap_or_else(|| {
                            let color = match app.inpaint.tool {
                                InpaintStrokeKind::Heal => Color32::from_rgb(75, 205, 145),
                                InpaintStrokeKind::Clone => Color32::from_rgb(185, 120, 255),
                                InpaintStrokeKind::Remove => Color32::from_rgb(255, 94, 94),
                            };
                            coverage_rgba(coverage, color)
                        })
                } else {
                    let coverage = rasterize_inpaint_dabs_binary(
                        region.texture_width,
                        region.texture_height,
                        region.source_width,
                        region.source_height,
                        &dabs,
                    );
                    coverage_rgba(coverage, Color32::from_rgb(255, 94, 94))
                };
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [
                        region.texture_width as usize,
                        region.texture_height as usize,
                    ],
                    &rgba,
                );
                if let Some(texture) = app.inpaint.stroke_texture.as_mut() {
                    texture.set(image, egui::TextureOptions::LINEAR);
                } else {
                    app.inpaint.stroke_texture = Some(ui.ctx().load_texture(
                        "auraw-inpaint-stroke",
                        image,
                        egui::TextureOptions::LINEAR,
                    ));
                }
                app.inpaint.stroke_texture_key = Some(key);
            }
            if let Some(texture) = &app.inpaint.stroke_texture {
                paint_final_geometry_overlay_texture(
                    ui,
                    texture.id(),
                    image_rect,
                    app.develop.geometry,
                    app.develop.loaded_raw
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
                app.develop.geometry,
                lens_geometry.as_deref(),
                source_width,
                source_height,
                pointer,
            );
            if let Some(uv) = editable_source_uv(source_uv) {
                let dab_size = zoom_scaled_brush_size(
                    app.inpaint.brush_size,
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
                    dab_size,
                    64,
                );
                let cursor_color = if app.inpaint.source_pick_active {
                    Color32::from_rgb(95, 225, 155)
                } else {
                    Color32::WHITE
                };
                painter.add(Shape::line(outline, Stroke::new(1.5, cursor_color)));
                if app.inpaint.source_pick_active {
                    painter.text(
                        pointer + egui::vec2(10.0, 10.0),
                        egui::Align2::LEFT_TOP,
                        "Set source",
                        egui::FontId::proportional(11.0),
                        cursor_color,
                    );
                } else if app.inpaint.tool.requires_source() {
                    if let Some(anchor) = app.inpaint.source_anchor {
                        let source_cursor =
                            aligned_retouch_source_uv(uv, anchor, app.inpaint.source_offset);
                        let source_screen = final_geometry_native_source_to_screen(
                            image_rect,
                            app.develop.geometry,
                            lens_geometry.as_deref(),
                            source_width,
                            source_height,
                            source_cursor,
                        );
                        painter.line_segment(
                            [pointer, source_screen],
                            Stroke::new(1.0, Color32::from_rgb(95, 225, 155)),
                        );
                        paint_retouch_source_marker(
                            &painter,
                            image_rect,
                            app.develop.geometry,
                            lens_geometry.as_deref(),
                            source_width,
                            source_height,
                            source_cursor,
                            dab_size,
                            "Source",
                        );
                    }
                }
            }
        }
    }

}
