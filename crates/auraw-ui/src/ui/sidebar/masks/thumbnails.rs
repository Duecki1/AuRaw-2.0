use super::*;

impl Sidebar {
    pub(super) fn refresh_mask_thumbnails(ui: &mut Ui, app: &mut AurawApp) {
        let selected_mask = app.masks.stack.selected_mask;
        let group_cache_valid = app.masks.thumbnail_revision == app.masks.overlay_revision
            && app.masks.thumbnail_group_textures.len() == app.masks.stack.masks.len();
        let component_len = selected_mask
            .and_then(|index| app.masks.stack.masks.get(index))
            .map_or(0, |mask| mask.components.len());
        let component_cache_valid = group_cache_valid
            && app.masks.thumbnail_component_mask == selected_mask
            && app.masks.thumbnail_component_textures.len() == component_len;
        if group_cache_valid && component_cache_valid {
            return;
        }

        let (image_width, image_height) = app
            .develop
            .preview_raw
            .as_ref()
            .map(|raw| (raw.width, raw.height))
            .unwrap_or((1, 1));
        let edge = Self::MASK_THUMBNAIL_EDGE;
        let (thumbnail_width, thumbnail_height) =
            Self::thumbnail_fit_size(image_width, image_height, edge);

        if !group_cache_valid {
            let images: Vec<_> = (0..app.masks.stack.masks.len())
                .map(|index| {
                    let gray = app.masks.stack.rasterize_layer(
                        index,
                        thumbnail_width,
                        thumbnail_height,
                        image_width,
                        image_height,
                    );
                    Self::gray_thumbnail_image(gray, thumbnail_width, thumbnail_height, edge)
                })
                .collect();
            Self::update_thumbnail_textures(
                ui,
                &mut app.masks.thumbnail_group_textures,
                images,
                "mask-group-thumbnail",
            );
        }

        if !component_cache_valid {
            let images: Vec<_> = selected_mask
                .and_then(|mask_index| {
                    app.masks
                        .stack
                        .masks
                        .get(mask_index)
                        .map(|mask| (mask_index, mask))
                })
                .map(|(mask_index, mask)| {
                    (0..mask.components.len())
                        .map(|component_index| {
                            let gray = app.masks.stack.rasterize_component_layer(
                                mask_index,
                                component_index,
                                thumbnail_width,
                                thumbnail_height,
                                image_width,
                                image_height,
                            );
                            Self::gray_thumbnail_image(
                                gray,
                                thumbnail_width,
                                thumbnail_height,
                                edge,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            Self::update_thumbnail_textures(
                ui,
                &mut app.masks.thumbnail_component_textures,
                images,
                "mask-component-thumbnail",
            );
        }

        app.masks.thumbnail_revision = app.masks.overlay_revision;
        app.masks.thumbnail_component_mask = selected_mask;
    }

    fn thumbnail_fit_size(image_width: u32, image_height: u32, edge: u32) -> (u32, u32) {
        let image_width = image_width.max(1);
        let image_height = image_height.max(1);
        if image_width >= image_height {
            let height = ((edge as f64 * image_height as f64 / image_width as f64).round() as u32)
                .clamp(1, edge);
            (edge, height)
        } else {
            let width = ((edge as f64 * image_width as f64 / image_height as f64).round() as u32)
                .clamp(1, edge);
            (width, edge)
        }
    }

    fn gray_thumbnail_image(gray: Vec<u8>, width: u32, height: u32, edge: u32) -> egui::ColorImage {
        let width = width.min(edge) as usize;
        let height = height.min(edge) as usize;
        let edge = edge as usize;
        let mut square = vec![0_u8; edge * edge];
        let offset_x = (edge - width) / 2;
        let offset_y = (edge - height) / 2;

        for row in 0..height {
            let source_start = row * width;
            let source_end = (source_start + width).min(gray.len());
            let copied = source_end.saturating_sub(source_start);
            if copied == 0 {
                break;
            }
            let destination_start = (offset_y + row) * edge + offset_x;
            square[destination_start..destination_start + copied]
                .copy_from_slice(&gray[source_start..source_end]);
        }

        egui::ColorImage::from_gray([edge, edge], &square)
    }

    fn update_thumbnail_textures(
        ui: &mut Ui,
        textures: &mut Vec<egui::TextureHandle>,
        images: Vec<egui::ColorImage>,
        prefix: &str,
    ) {
        let desired_len = images.len();
        for (index, image) in images.into_iter().enumerate() {
            if let Some(texture) = textures.get_mut(index) {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                textures.push(ui.ctx().load_texture(
                    format!("{prefix}-{index}"),
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        textures.truncate(desired_len);
    }

    pub(super) fn mask_thumbnail_card(
        ui: &mut Ui,
        texture: Option<&egui::TextureHandle>,
        label: &str,
        selected: bool,
        badge: Option<&str>,
        enabled: bool,
        card_size: MaskCardSize,
    ) -> egui::Response {
        use eframe::egui::{Align2, Color32, FontId, Stroke, StrokeKind};

        let size = card_size.card_size();
        let image_edge = card_size.image_edge();
        let thumbnail_sense = if cfg!(target_os = "android") {
            egui::Sense::click()
        } else {
            egui::Sense::click_and_drag()
        };
        let (rect, response) = ui.allocate_exact_size(size, thumbnail_sense);
        let visuals = ui.visuals();
        let fill = if selected {
            visuals.selection.bg_fill.gamma_multiply(0.18)
        } else if response.hovered() {
            visuals.widgets.hovered.bg_fill
        } else {
            visuals.faint_bg_color
        };
        let stroke = if selected {
            Stroke::new(1.5, visuals.selection.bg_fill)
        } else {
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, fill);
        painter.rect_stroke(rect, 6.0, stroke, StrokeKind::Inside);

        let image_rect = egui::Rect::from_min_size(
            egui::pos2(rect.center().x - image_edge * 0.5, rect.min.y + 5.0),
            egui::vec2(image_edge, image_edge),
        );
        painter.rect_filled(image_rect, 4.0, visuals.extreme_bg_color);
        painter.rect_stroke(
            image_rect,
            4.0,
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
            StrokeKind::Inside,
        );
        if let Some(texture) = texture {
            let tint = if enabled {
                Color32::WHITE
            } else {
                Color32::from_white_alpha(80)
            };
            painter.image(
                texture.id(),
                image_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                tint,
            );
        }

        if let Some(badge) = badge {
            let (font_size, badge_height, horizontal_padding) = match card_size {
                MaskCardSize::Group => (10.5, 18.0, 10.0),
                MaskCardSize::Submask => (9.0, 16.0, 8.0),
            };
            let badge_size = egui::vec2(
                (badge.chars().count() as f32 * font_size * 0.62 + horizontal_padding)
                    .max(badge_height + 2.0),
                badge_height,
            );
            let badge_rect =
                egui::Rect::from_min_size(image_rect.right_bottom() - badge_size, badge_size);
            painter.rect_filled(
                badge_rect,
                4.0,
                visuals.widgets.active.bg_fill.gamma_multiply(0.92),
            );
            painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                badge,
                FontId::proportional(font_size),
                visuals.widgets.active.fg_stroke.color,
            );
        }

        let max_label_chars = match card_size {
            MaskCardSize::Group => 13,
            MaskCardSize::Submask => 10,
        };
        let display_label: String = label.chars().take(max_label_chars).collect();
        let label_center_y = (image_rect.bottom() + rect.bottom()) * 0.5;
        painter.text(
            egui::pos2(rect.center().x, label_center_y),
            Align2::CENTER_CENTER,
            display_label,
            FontId::proportional(card_size.label_font_size()),
            if enabled {
                visuals.text_color()
            } else {
                visuals.weak_text_color()
            },
        );
        response.on_hover_text(label)
    }
}
