use super::*;

/// Normal justified galleries allow modest row-height variation so discrete
/// aspect-ratio combinations can still reach both edges of the viewport. Rows
/// requiring more growth than this are genuinely sparse and retain their
/// natural width instead of turning a handful of thumbnails into huge tiles.
#[cfg(test)]
pub(super) const MAX_JUSTIFIED_ROW_HEIGHT_SCALE: f32 = 4.0 / 3.0;

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

#[cfg(test)]
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

/// A predictable library grid is easier to scan than a collage of justified
/// rows. Every tile reserves the same image and metadata space; source aspect
/// ratios are handled by cover cropping inside that stable frame.
pub(super) fn uniform_thumbnail_layout(
    item_count: usize,
    available_width: f32,
    preferred_tile_width: f32,
    gap: f32,
) -> (Vec<egui::Rect>, f32) {
    if item_count == 0 {
        return (Vec::new(), 0.0);
    }

    const CARD_HEIGHT_RATIO: f32 = 0.72;
    let available_width = available_width.max(1.0);
    let preferred_tile_width = preferred_tile_width.max(1.0);
    let gap = gap.max(0.0);
    let columns = ((available_width + gap) / (preferred_tile_width + gap))
        .floor()
        .max(1.0) as usize;
    let columns = columns.min(item_count).max(1);
    let tile_width =
        ((available_width - gap * columns.saturating_sub(1) as f32) / columns as f32).max(1.0);
    let tile_height = (tile_width * CARD_HEIGHT_RATIO).round().max(1.0);
    let rows = item_count.div_ceil(columns);
    let mut placements = Vec::with_capacity(item_count);

    for index in 0..item_count {
        let column = index % columns;
        let row = index / columns;
        placements.push(egui::Rect::from_min_size(
            egui::pos2(
                column as f32 * (tile_width + gap),
                row as f32 * (tile_height + gap),
            ),
            egui::vec2(tile_width, tile_height),
        ));
    }

    let total_height = rows as f32 * tile_height + rows.saturating_sub(1) as f32 * gap;
    (placements, total_height)
}

#[cfg(test)]
pub(super) fn balanced_justified_row_ranges(
    aspects: &[f32],
    available_width: f32,
    target_height: f32,
    gap: f32,
) -> Vec<(usize, usize)> {
    if aspects.is_empty() {
        return Vec::new();
    }

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

#[cfg(test)]
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

#[cfg(test)]
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

#[cfg(test)]
fn justified_thumbnail_layout_from_aspects(
    aspects: Vec<f32>,
    available_width: f32,
    target_height: f32,
    gap: f32,
) -> (Vec<egui::Rect>, f32) {
    let available_width = available_width.max(1.0);
    let target_height = target_height.max(1.0);
    let gap = gap.max(0.0);

    let row_ranges = balanced_justified_row_ranges(&aspects, available_width, target_height, gap);
    let mut placements = Vec::with_capacity(aspects.len());
    let mut y = 0.0;

    for (row_start, row_end) in row_ranges {
        let row_aspects = &aspects[row_start..row_end];
        let item_count = row_aspects.len();
        let aspect_sum = row_aspects.iter().sum::<f32>();
        let gaps_width = gap * (item_count.saturating_sub(1) as f32);
        let justified_height =
            ((available_width - gaps_width).max(1.0) / aspect_sum.max(f32::EPSILON)).max(1.0);
        let row_is_justified = justified_height <= target_height * MAX_JUSTIFIED_ROW_HEIGHT_SCALE;
        let row_height = if row_is_justified {
            justified_height
        } else {
            target_height
        };
        let mut x = 0.0;

        for (row_offset, aspect) in row_aspects.iter().copied().enumerate() {
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
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("library-thumbnail-tile", entry.asset.display_path.as_str())),
        Sense::click(),
    );
    let visuals = ui.visuals();

    let tile_rect = rect;
    let image_rect = egui::Rect::from_min_max(
        tile_rect.min + egui::vec2(3.0, 3.0),
        egui::pos2(tile_rect.right() - 3.0, tile_rect.bottom() - 26.0),
    );
    ui.painter()
        .rect_filled(tile_rect, 6.0, crate::ui::theme::THUMBNAIL_BACKDROP);
    if let Some(texture) = &entry.texture {
        let uv = thumbnail_cover_uv(entry.thumbnail_size, image_rect.size());
        ui.painter()
            .image(texture.id(), image_rect, uv, Color32::WHITE);
    } else {
        ui.painter().text(
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
            visuals.weak_text_color(),
        );
    }

    if entry.developed_thumbnail_pending {
        let badge_edge = 25.0_f32.min(rect.width() * 0.32).min(rect.height() * 0.32);
        let center = egui::pos2(
            rect.right() - badge_edge * 0.5 - 6.0,
            rect.top() + badge_edge * 0.5 + 6.0,
        );
        crate::ui::components::pending_indicator(
            ui.painter(),
            center,
            badge_edge * 0.5,
            (badge_edge * 0.72).max(12.0),
        );
    }

    if response.hovered() {
        ui.painter()
            .rect_filled(tile_rect, 6.0, Color32::from_white_alpha(10));
    }
    if selected {
        ui.painter().rect_stroke(
            tile_rect,
            6.0,
            Stroke::new(2.0, visuals.selection.bg_fill),
            StrokeKind::Inside,
        );
    } else {
        ui.painter().rect_stroke(
            tile_rect,
            6.0,
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
            StrokeKind::Inside,
        );
    }

    let label_rect = egui::Rect::from_min_max(
        egui::pos2(tile_rect.left() + 8.0, tile_rect.bottom() - 20.0),
        egui::pos2(tile_rect.right() - 8.0, tile_rect.bottom() - 4.0),
    );
    let max_chars = ((label_rect.width() - 2.0) / 6.5).floor().max(8.0) as usize;
    ui.painter().text(
        label_rect.left_center(),
        Align2::LEFT_CENTER,
        elide_middle(&entry.asset.display_name, max_chars),
        FontId::proportional(11.0),
        visuals.weak_text_color(),
    );

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
