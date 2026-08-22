use super::*;

pub(super) fn overlay_raster_region(
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

pub(super) fn overlay_source_uv(region: OverlayRasterKey, source_width: u32, source_height: u32) -> [f32; 4] {
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    [
        region.source_x as f32 / width,
        region.source_y as f32 / height,
        (region.source_x + region.source_width) as f32 / width,
        (region.source_y + region.source_height) as f32 / height,
    ]
}

pub(super) fn coverage_rgba(coverage: Vec<u8>, color: Color32) -> Vec<u8> {
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

pub(super) fn group_coverage_rgba(
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

                    let contribution = pixel[0];
                    pixel[1] += rgb[0] * contribution;
                    pixel[2] += rgb[1] * contribution;
                    pixel[3] += rgb[2] * contribution;
                    pixel[4] += contribution;
                }
            }
        }
    }

    let fallback = crate::ui::theme::MASK_ADD;
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

pub(super) fn zoom_scaled_brush_size(tool_size: f32, preview_zoom: f32, image_relative: bool) -> f32 {
    let tool_size = tool_size.max(0.0);
    if image_relative {
        tool_size
    } else {
        tool_size / preview_zoom.max(MIN_PREVIEW_ZOOM)
    }
}
