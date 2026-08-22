use super::*;

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
        // Android's logical-point coordinate system already follows device density.
        // Keep touch targets predictable instead of making them balloon on very dense phones.
        return 120.0;
    }

    // egui sizes are already expressed in DPI-aware logical points, so using the raw
    // pixels-per-point value as a direct multiplier would double-apply OS display scaling.
    // Scale primarily with usable window area: a 4K/full-screen workspace should show
    // substantially larger rows than a small laptop window, while preserving similar
    // gallery density as the app is resized.
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

    // A restrained density adjustment helps very high-DPI desktop displays without
    // fighting egui/OS scaling. sqrt keeps 150–200% scaling from becoming excessive.
    let density_scale = if pixels_per_point.is_finite() {
        pixels_per_point.max(1.0).sqrt().clamp(1.0, 1.20)
    } else {
        1.0
    };

    (BASE_HEIGHT * viewport_scale * density_scale).clamp(126.0, 270.0)
}

pub(super) fn balanced_justified_row_ranges(
    aspects: &[f32],
    available_width: f32,
    target_height: f32,
    gap: f32,
) -> Vec<(usize, usize)> {
    if aspects.is_empty() {
        return Vec::new();
    }

    // Treat every item as its aspect-ratio width plus the gap that follows it.
    // This lets us estimate the ideal number of rows for the whole gallery
    // before choosing any breaks, instead of greedily leaving a tiny orphan
    // row at the end.
    let gap_weight = gap / target_height.max(1.0);
    let weights = aspects
        .iter()
        .map(|aspect| aspect.max(f32::EPSILON) + gap_weight)
        .collect::<Vec<_>>();
    let total_weight = weights.iter().sum::<f32>();
    let target_row_weight = (available_width + gap) / target_height.max(1.0);
    let row_count = (total_weight / target_row_weight.max(f32::EPSILON))
        .round()
        .clamp(1.0, aspects.len() as f32) as usize;

    let mut ranges = Vec::with_capacity(row_count);
    let mut start = 0usize;
    let mut remaining_weight = total_weight;

    for row_index in 0..row_count {
        let rows_left = row_count - row_index;
        if rows_left == 1 {
            ranges.push((start, aspects.len()));
            break;
        }

        let max_end = aspects.len() - (rows_left - 1);
        let desired_weight = remaining_weight / rows_left as f32;
        let mut end = start + 1;
        let mut row_weight = weights[start];

        // Pick the break closest to an equal share of the gallery's total
        // visual width. Because every future row is reserved at least one
        // image, the final row cannot collapse into a few oversized leftovers.
        while end < max_end {
            let with_next = row_weight + weights[end];
            if (row_weight - desired_weight).abs() <= (with_next - desired_weight).abs() {
                break;
            }
            row_weight = with_next;
            end += 1;
        }

        ranges.push((start, end));
        remaining_weight = (remaining_weight - row_weight).max(0.0);
        start = end;
    }

    ranges
}

pub(super) fn justified_thumbnail_layout(
    entries: &[LibraryEntry],
    available_width: f32,
    target_height: f32,
    gap: f32,
) -> (Vec<egui::Rect>, f32) {
    let available_width = available_width.max(1.0);
    let target_height = target_height.max(1.0);
    let gap = gap.max(0.0);
    let aspects: Vec<f32> = entries
        .iter()
        .map(|entry| {
            entry
                .layout_size
                .or(entry.thumbnail_size)
                .and_then(|[width, source_height]| {
                    (width > 0 && source_height > 0).then_some(width as f32 / source_height as f32)
                })
                .filter(|aspect| aspect.is_finite() && *aspect > 0.0)
                .unwrap_or(1.5)
        })
        .collect();

    let row_ranges = balanced_justified_row_ranges(&aspects, available_width, target_height, gap);
    let mut placements = Vec::with_capacity(entries.len());
    let mut y = 0.0;

    for (row_start, row_end) in row_ranges {
        let row_aspects = &aspects[row_start..row_end];
        let item_count = row_aspects.len();
        let aspect_sum = row_aspects.iter().sum::<f32>();
        let gaps_width = gap * (item_count.saturating_sub(1) as f32);
        let justified_height =
            ((available_width - gaps_width).max(1.0) / aspect_sum.max(f32::EPSILON)).max(1.0);
        // A sparse row must not inflate a handful of thumbnails to fill the
        // entire viewport. Keep such rows at the same responsive target height
        // as a full gallery row and leave the unused space on the right. Rows
        // that need to shrink still justify normally so they never overflow a
        // narrow phone or window.
        let row_is_justified = justified_height <= target_height;
        let row_height = justified_height.min(target_height);
        let mut x = 0.0;

        for (row_offset, aspect) in row_aspects.iter().copied().enumerate() {
            // Give the final item the exact remaining width to absorb floating-
            // point rounding when this is a justified row. Sparse rows retain
            // every thumbnail's natural aspect width instead of stretching the
            // final item across all remaining space.
            let width = if row_is_justified && row_offset + 1 == item_count {
                (available_width - x).max(1.0)
            } else {
                row_height * aspect
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

pub(super) fn thumbnail_cover_uv(source_size: Option<[u32; 2]>, target_size: egui::Vec2) -> egui::Rect {
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
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("library-thumbnail-tile", entry.asset.display_path.as_str())),
        Sense::click(),
    );
    let visuals = ui.visuals();

    ui.painter()
        .rect_filled(rect, 0.0, Color32::from_rgb(17, 18, 20));
    if let Some(texture) = &entry.texture {
        let uv = thumbnail_cover_uv(entry.thumbnail_size, rect.size());
        ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
    } else {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            if entry.thumbnail_error.is_some() {
                "Retrying preview…"
            } else if entry.thumbnail_queued {
                "Loading preview…"
            } else {
                "RAW"
            },
            FontId::proportional(11.0),
            visuals.weak_text_color(),
        );
    }

    if entry.developed_thumbnail_pending {
        let badge_edge = 25.0_f32.min(rect.width() * 0.32).min(rect.height() * 0.32);
        let center = egui::pos2(
            rect.right() - badge_edge * 0.5 - 6.0,
            rect.top() + badge_edge * 0.5 + 6.0,
        );
        ui.painter()
            .circle_filled(center, badge_edge * 0.5, Color32::from_black_alpha(190));
        ui.painter().text(
            center,
            Align2::CENTER_CENTER,
            egui_phosphor::regular::ARROW_CLOCKWISE,
            FontId::proportional((badge_edge * 0.72).max(12.0)),
            Color32::from_rgb(244, 142, 48),
        );
    }

    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_white_alpha(14));
    }
    if selected {
        ui.painter().rect_stroke(
            rect,
            0.0,
            Stroke::new(2.0, visuals.selection.bg_fill),
            StrokeKind::Inside,
        );
    }

    let overlay_height = 32.0_f32.min(rect.height());
    let overlay = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.bottom() - overlay_height),
        rect.right_bottom(),
    );
    ui.painter()
        .rect_filled(overlay, 0.0, Color32::from_black_alpha(116));
    let max_chars = ((rect.width() - 16.0) / 7.0).floor().max(8.0) as usize;
    ui.painter().text(
        egui::pos2(rect.left() + 8.0, rect.bottom() - 7.0),
        Align2::LEFT_BOTTOM,
        elide_middle(&entry.asset.display_name, max_chars),
        FontId::proportional(12.5),
        Color32::WHITE,
    );

    let mut tooltip = entry.asset.display_path.clone();
    if let Some(error) = &entry.thumbnail_error {
        tooltip.push_str("\nPreview: ");
        tooltip.push_str(error);
    }
    if entry.developed_thumbnail_pending {
        tooltip.push_str("\nRendering edits in the background.");
    }
    response.on_hover_text(tooltip)
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
