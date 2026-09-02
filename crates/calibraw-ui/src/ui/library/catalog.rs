use super::*;

/// Rows with enough images shrink to fit the viewport. Sparse rows retain the
/// selected thumbnail height rather than enlarging their images to fill space.
const THUMBNAIL_IMAGE_HORIZONTAL_INSET: f32 = 0.0;
const THUMBNAIL_IMAGE_VERTICAL_CHROME: f32 = 0.0;
const THUMBNAIL_CARD_RADIUS: f32 = crate::ui::theme::CARD_RADIUS;
#[cfg(not(target_os = "android"))]
const THUMBNAIL_HOVER_OVERLAY_ALPHA: u8 = 156;
#[cfg(not(target_os = "android"))]
const THUMBNAIL_HOVER_ANIMATION_SECONDS: f32 = 0.18;

pub(super) fn send_scan_failure(
    sender: &mpsc::SyncSender<ScanEvent>,
    generation: u64,
    error: String,
    repaint: &egui::Context,
) {
    let _ = sender.send(ScanEvent::Failed { generation, error });
    repaint.request_repaint();
}

pub(super) fn catalog_status(warning_count: usize, truncated: bool) -> String {
    let mut notices = Vec::new();
    if truncated {
        notices.push(format!("Newest {MAX_LIBRARY_FILES} RAW files shown"));
    }
    if warning_count > 0 {
        notices.push(format!(
            "{warning_count} unreadable {}",
            if warning_count == 1 { "item" } else { "items" }
        ));
    }
    notices.join(" · ")
}

pub(super) fn responsive_thumbnail_target_height(
    available_width: f32,
    available_height: f32,
    pixels_per_point: f32,
    android: bool,
) -> f32 {
    if android {
        return 120.0;
    }

    const REFERENCE_WIDTH: f32 = 1280.0;
    const REFERENCE_HEIGHT: f32 = 720.0;
    const BASE_HEIGHT: f32 = 140.0;

    let width = if available_width.is_finite() {
        available_width.max(1.0)
    } else {
        REFERENCE_WIDTH
    };
    let height = if available_height.is_finite() {
        available_height.max(1.0)
    } else {
        REFERENCE_HEIGHT
    };
    let viewport_scale = ((width * height) / (REFERENCE_WIDTH * REFERENCE_HEIGHT))
        .sqrt()
        .clamp(0.90, 1.70);

    let density_scale = if pixels_per_point.is_finite() {
        pixels_per_point.max(1.0).sqrt().clamp(1.0, 1.20)
    } else {
        1.0
    };

    (BASE_HEIGHT * viewport_scale * density_scale).clamp(126.0, 270.0)
}

/// Groups images in their display order. A row becomes complete only once its
/// natural width at the selected thumbnail height reaches the viewport width.
/// This leaves a short final row unscaled instead of spreading it across the
/// viewport.
pub(super) fn justified_thumbnail_row_ranges(
    aspects: &[f32],
    available_width: f32,
    target_image_height: f32,
    gap: f32,
) -> Vec<(usize, usize)> {
    if aspects.is_empty() {
        return Vec::new();
    }

    let available_width = available_width.max(1.0);
    let target_image_height = target_image_height.max(1.0);
    let gap = gap.max(0.0);
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut row_width = 0.0;

    for (index, aspect) in aspects.iter().copied().enumerate() {
        if index > start {
            row_width += gap;
        }
        row_width +=
            aspect.max(f32::EPSILON) * target_image_height + THUMBNAIL_IMAGE_HORIZONTAL_INSET;
        if row_width >= available_width {
            ranges.push((start, index + 1));
            start = index + 1;
            row_width = 0.0;
        }
    }

    if start < aspects.len() {
        ranges.push((start, aspects.len()));
    }

    ranges
}

pub(super) fn justified_thumbnail_layout(
    entries: &[LibraryEntry],
    available_width: f32,
    target_height: f32,
    gap: f32,
) -> (Vec<egui::Rect>, f32) {
    justified_thumbnail_layout_from_aspects(
        entries.iter().map(library_entry_aspect).collect(),
        available_width,
        target_height,
        gap,
    )
}

pub(super) fn justified_thumbnail_layout_for_indices(
    entries: &[LibraryEntry],
    indices: &[usize],
    available_width: f32,
    target_height: f32,
    gap: f32,
) -> (Vec<egui::Rect>, f32) {
    justified_thumbnail_layout_from_aspects(
        indices
            .iter()
            .map(|index| library_entry_aspect(&entries[*index]))
            .collect(),
        available_width,
        target_height,
        gap,
    )
}

fn library_entry_aspect(entry: &LibraryEntry) -> f32 {
    entry
        .layout_size
        .or(entry.thumbnail_size)
        .and_then(|[width, source_height]| {
            (width > 0 && source_height > 0).then_some(width as f32 / source_height as f32)
        })
        .filter(|aspect| aspect.is_finite() && *aspect > 0.0)
        .unwrap_or(1.5)
}

fn justified_thumbnail_layout_from_aspects(
    aspects: Vec<f32>,
    available_width: f32,
    target_height: f32,
    gap: f32,
) -> (Vec<egui::Rect>, f32) {
    let available_width = available_width.max(1.0);
    let target_height = target_height.max(1.0);
    let gap = gap.max(0.0);

    let target_image_height = (target_height - THUMBNAIL_IMAGE_VERTICAL_CHROME).max(1.0);
    let row_ranges =
        justified_thumbnail_row_ranges(&aspects, available_width, target_image_height, gap);
    let mut placements = Vec::with_capacity(aspects.len());
    let mut y = 0.0;

    for (row_start, row_end) in row_ranges {
        let row_aspects = &aspects[row_start..row_end];
        let item_count = row_aspects.len();
        let aspect_sum = row_aspects.iter().sum::<f32>();
        let gaps_width = gap * item_count.saturating_sub(1) as f32;
        let image_chrome_width = THUMBNAIL_IMAGE_HORIZONTAL_INSET * item_count as f32;
        let justified_image_height = ((available_width - gaps_width - image_chrome_width).max(1.0)
            / aspect_sum.max(f32::EPSILON))
        .max(1.0);
        let row_is_justified = justified_image_height <= target_image_height;
        let image_height = if row_is_justified {
            justified_image_height
        } else {
            target_image_height
        };
        let row_height = image_height + THUMBNAIL_IMAGE_VERTICAL_CHROME;
        let mut x = 0.0;

        for (row_offset, aspect) in row_aspects.iter().copied().enumerate() {
            let width = if row_is_justified && row_offset + 1 == item_count {
                (available_width - x).max(1.0)
            } else {
                image_height * aspect + THUMBNAIL_IMAGE_HORIZONTAL_INSET
            };
            placements.push(egui::Rect::from_min_size(
                egui::pos2(x, y),
                egui::vec2(width, row_height),
            ));
            x += width + gap;
        }

        y += row_height + gap;
    }

    let total_height = if placements.is_empty() {
        0.0
    } else {
        (y - gap).max(0.0)
    };
    (placements, total_height)
}

pub(super) fn thumbnail_cover_uv(
    source_size: Option<[u32; 2]>,
    target_size: egui::Vec2,
) -> egui::Rect {
    let Some([width, height]) = source_size.filter(|[width, height]| *width > 0 && *height > 0)
    else {
        return egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    };
    if target_size.x <= 0.0 || target_size.y <= 0.0 {
        return egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    }

    let source_aspect = width as f32 / height as f32;
    let target_aspect = target_size.x / target_size.y;
    if !source_aspect.is_finite() || !target_aspect.is_finite() {
        return egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    }

    if source_aspect > target_aspect {
        let visible = (target_aspect / source_aspect).clamp(0.0, 1.0);
        let inset = (1.0 - visible) * 0.5;
        egui::Rect::from_min_max(egui::pos2(inset, 0.0), egui::pos2(1.0 - inset, 1.0))
    } else if source_aspect < target_aspect {
        let visible = (source_aspect / target_aspect).clamp(0.0, 1.0);
        let inset = (1.0 - visible) * 0.5;
        egui::Rect::from_min_max(egui::pos2(0.0, inset), egui::pos2(1.0, 1.0 - inset))
    } else {
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0))
    }
}

pub(super) fn thumbnail_tile(
    ui: &mut Ui,
    entry: &LibraryEntry,
    rect: egui::Rect,
    selected: bool,
) -> egui::Response {
    #[cfg(target_os = "android")]
    let _ = selected;
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("library-thumbnail-tile", entry.asset.display_path.as_str())),
        Sense::click(),
    );
    let tile_rect = rect;
    let image_rect = tile_rect;
    let painter = ui.painter_at(tile_rect);
    if let Some(texture) = &entry.texture {
        let uv = thumbnail_cover_uv(entry.thumbnail_size, image_rect.size());
        painter.add(
            egui::epaint::RectShape::filled(image_rect, THUMBNAIL_CARD_RADIUS, Color32::WHITE)
                .with_texture(texture.id(), uv),
        );
    } else {
        painter.rect_filled(
            image_rect,
            THUMBNAIL_CARD_RADIUS,
            ui.visuals().extreme_bg_color,
        );
        painter.text(
            image_rect.center(),
            Align2::CENTER_CENTER,
            if entry.thumbnail_error.is_some() {
                "Retrying preview…"
            } else if entry.thumbnail_queued {
                "Loading preview…"
            } else {
                "RAW"
            },
            FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
    }

    #[cfg(not(target_os = "android"))]
    {
        thumbnail_hover_overlay(ui, &response, tile_rect, &entry.asset);
    }

    if entry.developed_thumbnail_pending {
        let badge_edge = 25.0_f32.min(rect.width() * 0.32).min(rect.height() * 0.32);
        let center = egui::pos2(
            rect.right() - badge_edge * 0.5 - 6.0,
            rect.top() + badge_edge * 0.5 + 6.0,
        );
        crate::ui::components::pending_indicator(
            &painter,
            center,
            badge_edge * 0.5,
            (badge_edge * 0.72).max(12.0),
        );
    }

    #[cfg(not(target_os = "android"))]
    if selected {
        let badge_edge = 24.0_f32.min(tile_rect.width() * 0.28);
        let badge_rect = egui::Rect::from_min_size(
            tile_rect.min + egui::vec2(7.0, 7.0),
            egui::Vec2::splat(badge_edge),
        );
        painter.rect_filled(badge_rect, badge_edge * 0.5, ui.visuals().selection.bg_fill);
        painter.text(
            badge_rect.center(),
            Align2::CENTER_CENTER,
            egui_phosphor::regular::CHECK,
            FontId::proportional((badge_edge * 0.58).max(10.0)),
            Color32::WHITE,
        );
    }

    #[cfg(not(target_os = "android"))]
    {
        let mut tooltip = entry.asset.display_path.clone();
        if let Some(error) = &entry.thumbnail_error {
            tooltip.push_str("\nPreview: ");
            tooltip.push_str(error);
        }
        if entry.developed_thumbnail_pending {
            tooltip.push_str(if entry.thumbnail_queued {
                "\nRendering edits in the background."
            } else {
                "\nThis original preview does not include saved edits."
            });
        }
        response.on_hover_text(tooltip)
    }
    #[cfg(target_os = "android")]
    response
}

#[cfg(not(target_os = "android"))]
fn thumbnail_hover_overlay(
    ui: &Ui,
    response: &egui::Response,
    tile_rect: egui::Rect,
    asset: &LibraryAsset,
) {
    let hover_progress = ui.ctx().animate_bool_with_time_and_easing(
        response.id.with("overlay"),
        response.hovered(),
        THUMBNAIL_HOVER_ANIMATION_SECONDS,
        egui::emath::easing::cubic_out,
    );
    if hover_progress <= 0.0 {
        return;
    }

    let painter = ui.painter_at(tile_rect);
    painter.rect_filled(
        tile_rect,
        THUMBNAIL_CARD_RADIUS,
        Color32::from_black_alpha(
            (f32::from(THUMBNAIL_HOVER_OVERLAY_ALPHA) * hover_progress).round() as u8,
        ),
    );
    let title_font_size = 14.5;
    let capture_font_size = 11.5;
    let details_font_size = 10.5;
    let text_width = (tile_rect.width() - 24.0).max(1.0);
    let title_chars = (text_width / (title_font_size * 0.55)).floor().max(6.0) as usize;
    let detail_chars = (text_width / (capture_font_size * 0.52)).floor().max(8.0) as usize;
    let title = elide_middle(&asset.display_name, title_chars);
    let capture = elide_middle(&thumbnail_capture_details(asset), detail_chars);
    let details = elide_middle(&thumbnail_hover_details(asset), detail_chars);
    let slide = egui::vec2(0.0, 7.0 * (1.0 - hover_progress));
    let title_center = tile_rect.center() - egui::vec2(0.0, 20.0) + slide;
    let title_color = Color32::from_white_alpha((255.0 * hover_progress).round() as u8);
    let detail_color = Color32::from_white_alpha((205.0 * hover_progress).round() as u8);
    painter.text(
        title_center,
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(title_font_size),
        title_color,
    );
    painter.text(
        title_center + egui::vec2(0.0, 21.0),
        Align2::CENTER_CENTER,
        capture,
        FontId::proportional(capture_font_size),
        detail_color,
    );
    painter.text(
        title_center + egui::vec2(0.0, 40.0),
        Align2::CENTER_CENTER,
        details,
        FontId::proportional(details_font_size),
        Color32::from_white_alpha((175.0 * hover_progress).round() as u8),
    );
}

#[cfg(any(not(target_os = "android"), test))]
pub(super) fn thumbnail_hover_details(asset: &LibraryAsset) -> String {
    let format = Path::new(&asset.display_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
        .map(str::to_uppercase)
        .unwrap_or_else(|| "RAW".to_owned());
    asset
        .metadata
        .dimensions_hint
        .map_or(format.clone(), |[width, height]| {
            format!("{format}  ·  {width} × {height}")
        })
}

#[cfg(any(not(target_os = "android"), test))]
pub(super) fn thumbnail_capture_details(asset: &LibraryAsset) -> String {
    let metadata = &asset.metadata;
    format!(
        "{}  ·  {}  ·  {}",
        format_thumbnail_iso(metadata.iso_speed),
        format_thumbnail_shutter(metadata.shutter_seconds),
        format_thumbnail_focal_length(metadata.focal_length),
    )
}

#[cfg(any(not(target_os = "android"), test))]
fn format_thumbnail_iso(value: f32) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "ISO —".to_owned();
    }
    if (value - value.round()).abs() < 0.05 {
        format!("ISO {value:.0}")
    } else {
        format!("ISO {value:.1}")
    }
}

#[cfg(any(not(target_os = "android"), test))]
fn format_thumbnail_shutter(seconds: f32) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "— s".to_owned();
    }
    if seconds < 0.5 {
        let denominator = (1.0 / seconds).round().max(1.0);
        format!("1/{denominator:.0} s")
    } else if (seconds - seconds.round()).abs() < 0.05 {
        format!("{seconds:.0} s")
    } else {
        format!("{seconds:.1} s")
    }
}

#[cfg(any(not(target_os = "android"), test))]
fn format_thumbnail_focal_length(value: f32) -> String {
    if !value.is_finite() || value <= 0.0 {
        return "— mm".to_owned();
    }
    if (value - value.round()).abs() < 0.05 {
        format!("{value:.0} mm")
    } else {
        format!("{value:.1} mm")
    }
}

#[cfg(target_os = "android")]
pub(super) fn thumbnail_selection_checkbox(
    ui: &mut Ui,
    entry: &LibraryEntry,
    thumbnail_rect: egui::Rect,
    selected: bool,
) -> egui::Response {
    const HIT_EDGE: f32 = 42.0;
    const BOX_EDGE: f32 = 23.0;
    const BOX_INSET: f32 = 7.0;

    let hit_rect = egui::Rect::from_min_size(
        thumbnail_rect.min,
        egui::vec2(
            HIT_EDGE.min(thumbnail_rect.width()),
            HIT_EDGE.min(thumbnail_rect.height()),
        ),
    );
    let response = ui
        .interact(
            hit_rect,
            ui.make_persistent_id((
                "library-thumbnail-selection-checkbox",
                entry.asset.display_path.as_str(),
            )),
            Sense::click(),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    let box_rect = egui::Rect::from_min_size(
        thumbnail_rect.min + egui::vec2(BOX_INSET, BOX_INSET),
        egui::Vec2::splat(BOX_EDGE),
    );
    let visuals = ui.visuals();
    let fill = if selected {
        visuals.selection.bg_fill
    } else if response.hovered() {
        Color32::from_black_alpha(220)
    } else {
        Color32::from_black_alpha(180)
    };
    ui.painter().rect_filled(box_rect, 4.0, fill);
    ui.painter().rect_stroke(
        box_rect,
        4.0,
        Stroke::new(1.5, Color32::WHITE),
        StrokeKind::Inside,
    );
    if selected {
        let left = egui::pos2(box_rect.left() + 5.0, box_rect.center().y);
        let middle = egui::pos2(box_rect.left() + 9.5, box_rect.bottom() - 5.5);
        let right = egui::pos2(box_rect.right() - 4.5, box_rect.top() + 5.5);
        ui.painter()
            .line_segment([left, middle], Stroke::new(2.2, Color32::WHITE));
        ui.painter()
            .line_segment([middle, right], Stroke::new(2.2, Color32::WHITE));
    }

    response.on_hover_text(if selected {
        "Deselect RAW"
    } else {
        "Select RAW"
    })
}

#[cfg(any(not(target_os = "android"), test))]
pub(super) fn elide_middle(value: &str, maximum_chars: usize) -> String {
    let count = value.chars().count();
    if count <= maximum_chars || maximum_chars < 5 {
        return value.to_owned();
    }
    let left = (maximum_chars - 1) / 2;
    let right = maximum_chars - 1 - left;
    let prefix = value.chars().take(left).collect::<String>();
    let suffix = value.chars().skip(count - right).collect::<String>();
    format!("{prefix}…{suffix}")
}
