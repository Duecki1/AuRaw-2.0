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

pub(super) fn inpaint_live_overlay_region(
    dabs: &[BrushDab],
    visible: crate::app::PreviewUvRect,
    source_width: u32,
    source_height: u32,
    preview_rect: Rect,
    pixels_per_point: f32,
) -> OverlayRasterKey {
    let viewport = overlay_raster_region(
        visible,
        source_width,
        source_height,
        preview_rect,
        pixels_per_point,
        0,
    );
    let image_min = source_width.min(source_height).max(1) as f32;
    let mut bounds = crate::app::PreviewUvRect {
        min: [1.0, 1.0],
        max: [0.0, 0.0],
    };
    for dab in dabs {
        let radius = dab.size.max(0.0) * image_min + 4.0;
        bounds.min[0] = bounds.min[0].min(dab.center[0] - radius / source_width.max(1) as f32);
        bounds.min[1] = bounds.min[1].min(dab.center[1] - radius / source_height.max(1) as f32);
        bounds.max[0] = bounds.max[0].max(dab.center[0] + radius / source_width.max(1) as f32);
        bounds.max[1] = bounds.max[1].max(dab.center[1] + radius / source_height.max(1) as f32);
    }
    bounds.min[0] = bounds.min[0].max(visible.min[0]);
    bounds.min[1] = bounds.min[1].max(visible.min[1]);
    bounds.max[0] = bounds.max[0].min(visible.max[0]);
    bounds.max[1] = bounds.max[1].min(visible.max[1]);
    if bounds.max[0] <= bounds.min[0] || bounds.max[1] <= bounds.min[1] {
        return viewport;
    }
    let mut region = overlay_raster_region(
        bounds,
        source_width,
        source_height,
        preview_rect,
        pixels_per_point,
        2,
    );
    let scale_x = viewport.texture_width as f32 / viewport.source_width.max(1) as f32;
    let scale_y = viewport.texture_height as f32 / viewport.source_height.max(1) as f32;
    region.texture_width = (region.source_width as f32 * scale_x).ceil().max(1.0) as u32;
    region.texture_height = (region.source_height as f32 * scale_y).ceil().max(1.0) as u32;
    region
}

pub(super) fn live_retouch_rgba(
    source: &MaskRgbImage,
    region: OverlayRasterKey,
    full_width: u32,
    full_height: u32,
    dabs: &[BrushDab],
    coverage: &[u8],
    kind: InpaintStrokeKind,
    source_offset: [f32; 2],
) -> Option<Vec<u8>> {
    if !kind.requires_source() || source.width == 0 || source.height == 0 {
        return None;
    }
    let first = dabs.first()?;
    let expected = region.texture_width as usize * region.texture_height as usize;
    if coverage.len() != expected {
        return None;
    }
    let source_anchor = [
        first.center[0] + source_offset[0],
        first.center[1] + source_offset[1],
    ];
    let color_delta = if kind == InpaintStrokeKind::Heal {
        let source_average =
            preview_brush_average(source, source_anchor, first.size, full_width, full_height)?;
        let destination_average =
            preview_brush_average(source, first.center, first.size, full_width, full_height)?;
        std::array::from_fn(|channel| destination_average[channel] - source_average[channel])
    } else {
        [0.0; 3]
    };

    // Keep a small working canvas for the active destination region. Dabs are
    // applied in pointer order, and source samples that land in this canvas see
    // the result of all earlier dabs. This is what makes an aligned source that
    // crosses the active stroke follow the pixels being painted right now,
    // rather than the snapshot captured at pointer-down.
    let mut canvas = Vec::with_capacity(expected);
    for y in 0..region.texture_height {
        let destination_v = (region.source_y as f32
            + (y as f32 + 0.5) * region.source_height as f32 / region.texture_height as f32)
            / full_height.max(1) as f32;
        for x in 0..region.texture_width {
            let destination_u = (region.source_x as f32
                + (x as f32 + 0.5) * region.source_width as f32 / region.texture_width as f32)
                / full_width.max(1) as f32;
            canvas.push(sample_preview_rgb(source, [destination_u, destination_v])?);
        }
    }
    let mut painted = vec![false; expected];
    for dab in dabs {
        let Some(bounds) = live_dab_texture_bounds(*dab, region, full_width, full_height) else {
            continue;
        };
        let mut updates = Vec::new();
        for y in bounds[1]..bounds[3] {
            for x in bounds[0]..bounds[2] {
                let dab_coverage = live_dab_coverage(x, y, *dab, region, full_width, full_height);
                if dab_coverage <= 0.0 {
                    continue;
                }
                let destination_uv =
                    overlay_texture_pixel_uv(x, y, region, full_width, full_height);
                let source_uv = [
                    destination_uv[0] + source_offset[0],
                    destination_uv[1] + source_offset[1],
                ];
                let sample = sample_live_retouch_rgb(
                    source,
                    &canvas,
                    region,
                    full_width,
                    full_height,
                    source_uv,
                )?;
                updates.push((
                    (y * region.texture_width + x) as usize,
                    std::array::from_fn(|channel| sample[channel] + color_delta[channel]),
                    dab_coverage,
                ));
            }
        }
        for (index, mut sample, dab_coverage) in updates {
            for channel in 0..3 {
                sample[channel] = (canvas[index][channel]
                    + (sample[channel] - canvas[index][channel]) * dab_coverage)
                    .clamp(0.0, 255.0);
            }
            canvas[index] = sample;
            painted[index] = true;
        }
    }

    let mut rgba = Vec::with_capacity(expected * 4);
    for (index, sample) in canvas.into_iter().enumerate() {
        rgba.extend_from_slice(&[
            sample[0].round().clamp(0.0, 255.0) as u8,
            sample[1].round().clamp(0.0, 255.0) as u8,
            sample[2].round().clamp(0.0, 255.0) as u8,
            if painted[index] && coverage[index] > 0 {
                255
            } else {
                0
            },
        ]);
    }
    Some(rgba)
}

pub(super) fn overlay_texture_pixel_uv(
    x: u32,
    y: u32,
    region: OverlayRasterKey,
    full_width: u32,
    full_height: u32,
) -> [f32; 2] {
    [
        (region.source_x as f32
            + (x as f32 + 0.5) * region.source_width as f32 / region.texture_width.max(1) as f32)
            / full_width.max(1) as f32,
        (region.source_y as f32
            + (y as f32 + 0.5) * region.source_height as f32 / region.texture_height.max(1) as f32)
            / full_height.max(1) as f32,
    ]
}

pub(super) fn live_dab_texture_bounds(
    dab: BrushDab,
    region: OverlayRasterKey,
    full_width: u32,
    full_height: u32,
) -> Option<[u32; 4]> {
    let image_min = full_width.min(full_height).max(1) as f32;
    let radius = dab.size.max(0.0) * image_min;
    let center_x = (dab.center[0] * full_width as f32 - region.source_x as f32)
        * region.texture_width as f32
        / region.source_width.max(1) as f32;
    let center_y = (dab.center[1] * full_height as f32 - region.source_y as f32)
        * region.texture_height as f32
        / region.source_height.max(1) as f32;
    let radius_x = radius * region.texture_width as f32 / region.source_width.max(1) as f32;
    let radius_y = radius * region.texture_height as f32 / region.source_height.max(1) as f32;
    if ![center_x, center_y, radius_x, radius_y]
        .iter()
        .all(|value| value.is_finite())
    {
        return None;
    }
    let x0 = (center_x - radius_x - 2.0)
        .floor()
        .clamp(0.0, region.texture_width as f32) as u32;
    let y0 = (center_y - radius_y - 2.0)
        .floor()
        .clamp(0.0, region.texture_height as f32) as u32;
    let x1 = (center_x + radius_x + 2.0)
        .ceil()
        .clamp(0.0, region.texture_width as f32) as u32;
    let y1 = (center_y + radius_y + 2.0)
        .ceil()
        .clamp(0.0, region.texture_height as f32) as u32;
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1, y1])
}

pub(super) fn live_dab_coverage(
    x: u32,
    y: u32,
    dab: BrushDab,
    region: OverlayRasterKey,
    full_width: u32,
    full_height: u32,
) -> f32 {
    let image_min = full_width.min(full_height).max(1) as f32;
    let radius = dab.size.clamp(f32::EPSILON, 0.5) * image_min;
    let center_x = (dab.center[0] * full_width as f32 - region.source_x as f32)
        * region.texture_width as f32
        / region.source_width.max(1) as f32;
    let center_y = (dab.center[1] * full_height as f32 - region.source_y as f32)
        * region.texture_height as f32
        / region.source_height.max(1) as f32;
    let radius_x = radius * region.texture_width as f32 / region.source_width.max(1) as f32;
    let radius_y = radius * region.texture_height as f32 / region.source_height.max(1) as f32;
    let dx = (x as f32 + 0.5 - center_x) / radius_x.max(0.5);
    let dy = (y as f32 + 0.5 - center_y) / radius_y.max(0.5);
    let distance = (dx * dx + dy * dy).sqrt();
    let antialias = (1.0 / radius_x.max(radius_y).max(1.0)).clamp(0.002, 0.25);
    let inner = 1.0 - antialias;
    1.0 - smoothstep_preview(inner, 1.0 + antialias, distance)
}

pub(super) fn smoothstep_preview(edge0: f32, edge1: f32, value: f32) -> f32 {
    let t = ((value - edge0) / (edge1 - edge0).max(f32::EPSILON)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub(super) fn sample_live_retouch_rgb(
    source: &MaskRgbImage,
    canvas: &[[f32; 3]],
    region: OverlayRasterKey,
    full_width: u32,
    full_height: u32,
    uv: [f32; 2],
) -> Option<[f32; 3]> {
    let x = (uv[0] * full_width.max(1) as f32 - region.source_x as f32)
        * region.texture_width as f32
        / region.source_width.max(1) as f32
        - 0.5;
    let y = (uv[1] * full_height.max(1) as f32 - region.source_y as f32)
        * region.texture_height as f32
        / region.source_height.max(1) as f32
        - 0.5;
    sample_live_canvas_rgb(canvas, region.texture_width, region.texture_height, x, y)
        .or_else(|| sample_preview_rgb(source, uv))
}

pub(super) fn sample_live_canvas_rgb(
    canvas: &[[f32; 3]],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
) -> Option<[f32; 3]> {
    if width == 0
        || height == 0
        || canvas.len() != width as usize * height as usize
        || x < -0.5
        || y < -0.5
        || x > width as f32 - 0.5
        || y > height as f32 - 0.5
    {
        return None;
    }
    let x = x.clamp(0.0, width.saturating_sub(1) as f32);
    let y = y.clamp(0.0, height.saturating_sub(1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let sample = |sample_x: u32, sample_y: u32| canvas[(sample_y * width + sample_x) as usize];
    let top_left = sample(x0, y0);
    let top_right = sample(x1, y0);
    let bottom_left = sample(x0, y1);
    let bottom_right = sample(x1, y1);
    Some(std::array::from_fn(|channel| {
        let top = top_left[channel] + (top_right[channel] - top_left[channel]) * tx;
        let bottom = bottom_left[channel] + (bottom_right[channel] - bottom_left[channel]) * tx;
        top + (bottom - top) * ty
    }))
}

pub(super) fn aligned_retouch_source_uv(
    destination: [f32; 2],
    source_anchor: [f32; 2],
    source_offset: Option<[f32; 2]>,
) -> [f32; 2] {
    source_offset.map_or(source_anchor, |offset| {
        [destination[0] + offset[0], destination[1] + offset[1]]
    })
}

pub(super) fn preview_brush_average(
    source: &MaskRgbImage,
    center: [f32; 2],
    brush_size: f32,
    full_width: u32,
    full_height: u32,
) -> Option<[f32; 3]> {
    let image_min = full_width.min(full_height).max(1) as f32;
    let radius = brush_size.max(0.0) * image_min;
    let radius_uv = [
        radius / full_width.max(1) as f32,
        radius / full_height.max(1) as f32,
    ];
    let mut sum = [0.0f32; 3];
    let mut count = 0.0f32;
    for grid_y in -3i32..=3 {
        for grid_x in -3i32..=3 {
            if grid_x * grid_x + grid_y * grid_y > 9 {
                continue;
            }
            let uv = [
                center[0] + grid_x as f32 / 3.0 * radius_uv[0] * 0.75,
                center[1] + grid_y as f32 / 3.0 * radius_uv[1] * 0.75,
            ];
            if let Some(sample) = sample_preview_rgb(source, uv) {
                for channel in 0..3 {
                    sum[channel] += sample[channel];
                }
                count += 1.0;
            }
        }
    }
    (count > 0.0).then(|| sum.map(|value| value / count))
}

pub(super) fn sample_preview_rgb(source: &MaskRgbImage, uv: [f32; 2]) -> Option<[f32; 3]> {
    if source.width == 0
        || source.height == 0
        || source.rgba.len() != source.width as usize * source.height as usize * 4
        || !uv.iter().all(|value| value.is_finite())
        || uv[0] < 0.0
        || uv[0] > 1.0
        || uv[1] < 0.0
        || uv[1] > 1.0
    {
        return None;
    }
    let x = (uv[0] * source.width as f32 - 0.5).clamp(0.0, source.width.saturating_sub(1) as f32);
    let y = (uv[1] * source.height as f32 - 0.5).clamp(0.0, source.height.saturating_sub(1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(source.width - 1);
    let y1 = (y0 + 1).min(source.height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let sample = |sample_x: u32, sample_y: u32, channel: usize| {
        source.rgba[((sample_y * source.width + sample_x) * 4) as usize + channel] as f32
    };
    Some(std::array::from_fn(|channel| {
        let top =
            sample(x0, y0, channel) + (sample(x1, y0, channel) - sample(x0, y0, channel)) * tx;
        let bottom =
            sample(x0, y1, channel) + (sample(x1, y1, channel) - sample(x0, y1, channel)) * tx;
        top + (bottom - top) * ty
    }))
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

pub(super) fn crop_overlay_dabs(
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
pub(super) fn zoom_scaled_brush_size(tool_size: f32, preview_zoom: f32, image_relative: bool) -> f32 {
    let tool_size = tool_size.max(0.0);
    if image_relative {
        tool_size
    } else {
        tool_size / preview_zoom.max(MIN_PREVIEW_ZOOM)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn paint_retouch_source_marker(
    painter: &egui::Painter,
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    source_uv: [f32; 2],
    brush_size: f32,
    label: &str,
) {
    if !source_uv.iter().all(|value| value.is_finite()) {
        return;
    }
    let outline = brush_outline_geometry_screen_points(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        source_uv,
        brush_size,
        64,
    );
    let color = Color32::from_rgb(95, 225, 155);
    painter.add(Shape::line(outline, Stroke::new(1.5, color)));
    let center = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        source_uv,
    );
    painter.line_segment(
        [center - egui::vec2(5.0, 0.0), center + egui::vec2(5.0, 0.0)],
        Stroke::new(1.2, color),
    );
    painter.line_segment(
        [center - egui::vec2(0.0, 5.0), center + egui::vec2(0.0, 5.0)],
        Stroke::new(1.2, color),
    );
    painter.text(
        center + egui::vec2(7.0, -7.0),
        egui::Align2::LEFT_BOTTOM,
        label,
        egui::FontId::proportional(10.0),
        color,
    );
}

pub(super) fn inpaint_stroke_geometry_screen_bounds(
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

