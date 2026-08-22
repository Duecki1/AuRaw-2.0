use super::*;

pub(in crate::app) fn aligned_detail_axis(
    min_uv: f32,
    max_uv: f32,
    extent: u32,
    cfa_period: u32,
    viewport_pixels: u32,
    detail_pixel_scale: f32,
) -> (u32, u32) {
    let extent = extent.max(1);
    let period = cfa_period.max(1);
    let visible_start =
        ((min_uv.clamp(0.0, 1.0) * extent as f32).floor() as u32).min(extent.saturating_sub(1));
    let visible_end =
        ((max_uv.clamp(0.0, 1.0) * extent as f32).ceil() as u32).clamp(visible_start + 1, extent);
    let visible_len = visible_end - visible_start;

    // Preserve spatial context around detail crops to prevent visible edge seams.
    let visible_detail_pixels =
        (viewport_pixels.max(1) as f32 * detail_pixel_scale.max(0.1)).max(1.0);
    let support_padding =
        (visible_len as f32 * EXPORT_TILE_HALO as f32 / visible_detail_pixels).ceil() as u32;
    let padding = ((visible_len as f32 * 0.06).ceil() as u32)
        .max(EXPORT_TILE_HALO)
        .max(support_padding);
    let padded_start = visible_start.saturating_sub(padding);
    let padded_end = visible_end.saturating_add(padding).min(extent);
    let aligned_start = (padded_start / period) * period;
    let aligned_end = padded_end
        .div_ceil(period)
        .saturating_mul(period)
        .min(extent)
        .max(aligned_start + 1);
    (aligned_start, aligned_end)
}

pub(in crate::app) fn detail_texture_uv(visible: PreviewUvRect, crop: PreviewUvRect) -> PreviewUvRect {
    let crop_width = (crop.max[0] - crop.min[0]).max(f32::EPSILON);
    let crop_height = (crop.max[1] - crop.min[1]).max(f32::EPSILON);
    PreviewUvRect {
        min: [
            ((visible.min[0] - crop.min[0]) / crop_width).clamp(0.0, 1.0),
            ((visible.min[1] - crop.min[1]) / crop_height).clamp(0.0, 1.0),
        ],
        max: [
            ((visible.max[0] - crop.min[0]) / crop_width).clamp(0.0, 1.0),
            ((visible.max[1] - crop.min[1]) / crop_height).clamp(0.0, 1.0),
        ],
    }
}

pub(in crate::app) fn requested_detail_edge(
    quality: PreviewQuality,
    viewport_pixels: [u32; 2],
    visible: PreviewUvRect,
    crop_width: u32,
    crop_height: u32,
    full_width: u32,
    full_height: u32,
) -> u32 {
    let visible_source_width =
        ((visible.max[0] - visible.min[0]).max(1.0 / full_width.max(1) as f32) * full_width as f32)
            .max(1.0);
    let visible_source_height = ((visible.max[1] - visible.min[1])
        .max(1.0 / full_height.max(1) as f32)
        * full_height as f32)
        .max(1.0);
    let padded_width_pixels =
        viewport_pixels[0].max(1) as f32 * crop_width as f32 / visible_source_width;
    let padded_height_pixels =
        viewport_pixels[1].max(1) as f32 * crop_height as f32 / visible_source_height;
    (padded_width_pixels.max(padded_height_pixels) * quality.detail_pixel_scale())
        .ceil()
        .clamp(
            256.0,
            quality.detail_edge_for_viewport(viewport_pixels) as f32,
        ) as u32
}

pub(in crate::app) fn navigation_proxy_edge() -> u32 {
    if cfg!(target_os = "android") { 384 } else { 512 }
}

pub(in crate::app) fn navigation_mask_edge() -> u32 {
    if cfg!(target_os = "android") { 256 } else { 384 }
}

pub(in crate::app) fn detail_mask_edge() -> u32 {
    // This atlas covers only the zoomed source region (plus the exact shaping
    // halo), so it can match the viewport at much higher density without the
    // enormous 32-layer allocation a full-frame 4K atlas would require.
    if cfg!(target_os = "android") { 1024 } else { 2048 }
}

pub(in crate::app) fn detail_mask_source_region(
    masks: &MaskStack,
    source_origin: [u32; 2],
    source_size: [u32; 2],
    full_width: u32,
    full_height: u32,
) -> [u32; 4] {
    let full_width = full_width.max(1);
    let full_height = full_height.max(1);
    let margin = masks.raster_margin_pixels(full_width, full_height);
    let x0 = source_origin[0].min(full_width - 1).saturating_sub(margin);
    let y0 = source_origin[1].min(full_height - 1).saturating_sub(margin);
    let x1 = source_origin[0]
        .saturating_add(source_size[0])
        .saturating_add(margin)
        .clamp(x0 + 1, full_width);
    let y1 = source_origin[1]
        .saturating_add(source_size[1])
        .saturating_add(margin)
        .clamp(y0 + 1, full_height);
    [x0, y0, x1 - x0, y1 - y0]
}

pub(in crate::app) fn mask_source_region_uv(region: [u32; 4], full_width: u32, full_height: u32) -> [f32; 4] {
    let width = full_width.max(1) as f32;
    let height = full_height.max(1) as f32;
    [
        region[0] as f32 / width,
        region[1] as f32 / height,
        region[0].saturating_add(region[2]) as f32 / width,
        region[1].saturating_add(region[3]) as f32 / height,
    ]
}

pub(in crate::app) fn mask_region_texture_extent(region: [u32; 4], max_edge: u32) -> [u32; 2] {
    let width = region[2].max(1);
    let height = region[3].max(1);
    let longest = width.max(height);
    if longest <= max_edge {
        return [width, height];
    }
    let scale = max_edge.max(1) as f64 / longest as f64;
    [
        ((width as f64 * scale).round() as u32).clamp(1, max_edge.max(1)),
        ((height as f64 * scale).round() as u32).clamp(1, max_edge.max(1)),
    ]
}

/// Start a detailed crop for every real zoom level above fit. The previous
/// 1.01 cutoff excluded an exact 101% zoom and, together with the former
/// proxy-texel shortcut, kept the tiny navigation image visible until much deeper
/// zoom levels.
pub(super) const DETAIL_ZOOM_START: f32 = 1.0005;

pub(in crate::app) fn zoom_detail_idle_delay() -> Duration {
    // Wait only long enough to coalesce wheel/pinch events. A full second made
    // the navigation proxy look like the final preview after zooming stopped.
    Duration::from_millis(if cfg!(target_os = "android") { 220 } else { 140 })
}

mod detail;
mod navigation;
mod processing;
mod rebuild;
mod state;

#[cfg(test)]
mod detail_resolution_tests {
    use super::*;

    #[test]
    fn medium_zoom_detail_matches_the_physical_viewport_density() {
        let visible = PreviewUvRect {
            min: [0.25, 0.25],
            max: [0.75, 0.75],
        };
        let medium = requested_detail_edge(
            PreviewQuality::Medium,
            [3_000, 2_000],
            visible,
            3_500,
            2_344,
            7_000,
            4_688,
        );
        let low = requested_detail_edge(
            PreviewQuality::Low,
            [3_000, 2_000],
            visible,
            3_500,
            2_344,
            7_000,
            4_688,
        );

        assert_eq!(medium, 3_000);
        assert!(low < medium);
    }
}
