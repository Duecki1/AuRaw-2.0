use crate::pipeline::{PointCurve, MAX_POINT_CURVE_POINTS};
use eframe::egui::{self, Color32, Pos2, Sense, Stroke, StrokeKind, Ui};

const CURVE_HEIGHT: f32 = 210.0;
const POINT_RADIUS: f32 = 5.0;
const PICK_RADIUS: f32 = 16.0;
const MIN_POINT_X_GAP: f32 = 0.005;

pub fn tone_curve_editor(ui: &mut Ui, curve: &mut PointCurve, curve_color: Color32) -> bool {
    curve.sanitize();
    // Stay inside the scroll area's reserved content column even when the
    // sidebar is resized narrower than the preferred curve width.
    let width = ui.available_width().max(1.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, CURVE_HEIGHT), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    // The graph background and grid belong inside the plot, but the curve stroke
    // and control points need the surrounding UI's clip rect. Otherwise points
    // on the 0/1 boundaries are cut in half by `painter_at(rect)`.
    let overlay_painter = ui.painter().with_clip_rect(ui.clip_rect());
    let visuals = ui.visuals();

    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
    painter.rect_stroke(
        rect,
        4.0,
        Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color),
        StrokeKind::Inside,
    );

    for step in 1..4 {
        let t = step as f32 / 4.0;
        let x = egui::lerp(rect.left()..=rect.right(), t);
        let y = egui::lerp(rect.bottom()..=rect.top(), t);
        let grid = Stroke::new(1.0, visuals.faint_bg_color);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            grid,
        );
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            grid,
        );
    }

    painter.line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.top()),
        ],
        Stroke::new(1.0, visuals.weak_text_color()),
    );

    let mut previous = curve_to_screen(rect, [0.0, sample_curve(curve, 0.0)]);
    for sample in 1..=128 {
        let x = sample as f32 / 128.0;
        let next = curve_to_screen(rect, [x, sample_curve(curve, x)]);
        overlay_painter.line_segment([previous, next], Stroke::new(2.0, curve_color));
        previous = next;
    }

    for point in curve.points.iter().take(curve.len as usize) {
        let center = curve_to_screen(rect, *point);
        overlay_painter.circle_filled(center, POINT_RADIUS, Color32::WHITE);
        overlay_painter.circle_stroke(center, POINT_RADIUS, Stroke::new(1.5, curve_color));
    }

    let mut changed = false;
    if response.dragged() {
        if let Some(pointer) = response.interact_pointer_pos() {
            if let Some(index) = nearest_point(curve, rect, pointer, PICK_RADIUS * 2.0) {
                let mut normalized = screen_to_curve(rect, pointer);
                let len = curve.len as usize;
                if index == 0 {
                    normalized[0] = normalized[0].clamp(0.0, curve.points[1][0] - MIN_POINT_X_GAP);
                } else if index + 1 == len {
                    normalized[0] =
                        normalized[0].clamp(curve.points[index - 1][0] + MIN_POINT_X_GAP, 1.0);
                } else {
                    normalized[0] = normalized[0].clamp(
                        curve.points[index - 1][0] + MIN_POINT_X_GAP,
                        curve.points[index + 1][0] - MIN_POINT_X_GAP,
                    );
                }
                normalized[1] = normalized[1].clamp(0.0, 1.0);
                if curve.points[index] != normalized {
                    curve.points[index] = normalized;
                    changed = true;
                }
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    if response.clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let point = screen_to_curve(rect, pointer);
            changed |= insert_point(curve, point);
        }
    }

    #[cfg(target_os = "android")]
    if response.double_clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            let point = screen_to_curve(rect, pointer);
            changed |= insert_point(curve, point);
        }
    }

    if response.secondary_clicked() {
        if let Some(pointer) = response.interact_pointer_pos() {
            if let Some(index) = nearest_point(curve, rect, pointer, PICK_RADIUS) {
                changed |= remove_point(curve, index);
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    response.on_hover_text(
        "Click to add a point. Drag points to shape the curve; right-click an interior point to remove it.",
    );
    #[cfg(target_os = "android")]
    response.on_hover_text("Drag points to shape the curve. Double-tap to add a point.");
    curve.sanitize();
    changed
}

fn curve_to_screen(rect: egui::Rect, point: [f32; 2]) -> Pos2 {
    Pos2::new(
        egui::lerp(rect.left()..=rect.right(), point[0]),
        egui::lerp(rect.bottom()..=rect.top(), point[1]),
    )
}

fn screen_to_curve(rect: egui::Rect, point: Pos2) -> [f32; 2] {
    [
        ((point.x - rect.left()) / rect.width()).clamp(0.0, 1.0),
        ((rect.bottom() - point.y) / rect.height()).clamp(0.0, 1.0),
    ]
}

fn nearest_point(
    curve: &PointCurve,
    rect: egui::Rect,
    pointer: Pos2,
    radius: f32,
) -> Option<usize> {
    curve
        .points
        .iter()
        .take(curve.len as usize)
        .enumerate()
        .map(|(index, point)| (index, curve_to_screen(rect, *point).distance(pointer)))
        .filter(|(_, distance)| *distance <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

fn insert_point(curve: &mut PointCurve, point: [f32; 2]) -> bool {
    let len = curve.len as usize;
    if len >= MAX_POINT_CURVE_POINTS
        || point[0] <= curve.points[0][0] + MIN_POINT_X_GAP
        || point[0] >= curve.points[len - 1][0] - MIN_POINT_X_GAP
    {
        return false;
    }
    if curve.points[..len]
        .iter()
        .any(|existing| (existing[0] - point[0]).abs() < 0.015)
    {
        return false;
    }

    let insert_at = curve.points[..len]
        .iter()
        .position(|existing| existing[0] > point[0])
        .unwrap_or(len - 1);
    for index in (insert_at..len).rev() {
        curve.points[index + 1] = curve.points[index];
    }
    curve.points[insert_at] = [point[0], point[1].clamp(0.0, 1.0)];
    curve.len += 1;
    true
}

fn remove_point(curve: &mut PointCurve, index: usize) -> bool {
    let len = curve.len as usize;
    if index == 0 || index + 1 == len || len <= 2 {
        return false;
    }
    for current in index..len - 1 {
        curve.points[current] = curve.points[current + 1];
    }
    curve.points[len - 1] = [1.0, 1.0];
    curve.len -= 1;
    true
}

fn sample_curve(curve: &PointCurve, input: f32) -> f32 {
    let len = curve.len.clamp(2, MAX_POINT_CURVE_POINTS as u32) as usize;
    let x = input.clamp(0.0, 1.0);
    let mut segment = len - 2;
    for index in 0..len - 1 {
        if x <= curve.points[index + 1][0] {
            segment = index;
            break;
        }
    }

    let p0 = curve.points[segment];
    let p1 = curve.points[segment + 1];
    let width = (p1[0] - p0[0]).max(1e-5);
    let t = ((x - p0[0]) / width).clamp(0.0, 1.0);
    let t2 = t * t;
    let t3 = t2 * t;
    let m0 = tangent(curve, segment, len) * width;
    let m1 = tangent(curve, segment + 1, len) * width;
    let y = (2.0 * t3 - 3.0 * t2 + 1.0) * p0[1]
        + (t3 - 2.0 * t2 + t) * m0
        + (-2.0 * t3 + 3.0 * t2) * p1[1]
        + (t3 - t2) * m1;
    y.clamp(p0[1].min(p1[1]), p0[1].max(p1[1]))
}

fn tangent(curve: &PointCurve, index: usize, len: usize) -> f32 {
    if index == 0 {
        return secant(curve.points[0], curve.points[1]);
    }
    if index + 1 >= len {
        return secant(curve.points[len - 2], curve.points[len - 1]);
    }
    let previous = secant(curve.points[index - 1], curve.points[index]);
    let next = secant(curve.points[index], curve.points[index + 1]);
    if previous * next <= 0.0 {
        0.0
    } else {
        2.0 * previous * next / (previous + next)
    }
}

fn secant(a: [f32; 2], b: [f32; 2]) -> f32 {
    (b[1] - a[1]) / (b[0] - a[0]).max(1e-5)
}
