use super::*;

pub(super) fn shortest_angle_delta(from: f32, to: f32) -> f32 {
    let mut delta = to - from;
    while delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    while delta < -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    delta
}

pub(super) fn source_angle_from(
    center: [f32; 2],
    point: [f32; 2],
    source_width: u32,
    source_height: u32,
) -> f32 {
    let dx = (point[0] - center[0]) * source_width.max(1) as f32;
    let dy = (point[1] - center[1]) * source_height.max(1) as f32;
    dy.atan2(dx)
}

pub(super) fn linear_rotation_handle_geometry(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    start: [f32; 2],
    end: [f32; 2],
) -> (Pos2, Pos2) {
    let midpoint_uv = [(start[0] + end[0]) * 0.5, (start[1] + end[1]) * 0.5];
    let midpoint = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        midpoint_uv,
    );

    // Use the local tangent of the *warped* gradient axis. The old code used
    // the straight chord between transformed endpoints, which points in the
    // wrong direction under nonlinear lens correction.
    let tangent_a_uv = [
        start[0] + (end[0] - start[0]) * 0.48,
        start[1] + (end[1] - start[1]) * 0.48,
    ];
    let tangent_b_uv = [
        start[0] + (end[0] - start[0]) * 0.52,
        start[1] + (end[1] - start[1]) * 0.52,
    ];
    let tangent_a = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        tangent_a_uv,
    );
    let tangent_b = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        tangent_b_uv,
    );
    let tangent = tangent_b - tangent_a;
    let normal = if tangent.length_sq() > 1e-6 {
        egui::vec2(-tangent.y, tangent.x) / tangent.length()
    } else {
        egui::vec2(0.0, -1.0)
    };
    (midpoint, midpoint + normal * 34.0)
}

pub(super) fn linear_axis_geometry_screen_points(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    start: [f32; 2],
    end: [f32; 2],
    segments: usize,
) -> Vec<Pos2> {
    let segments = segments.max(2);
    (0..=segments)
        .map(|index| {
            let t = index as f32 / segments as f32;
            let uv = [
                start[0] + (end[0] - start[0]) * t,
                start[1] + (end[1] - start[1]) * t,
            ];
            final_geometry_native_source_to_screen(
                image_rect,
                geometry,
                lens_geometry,
                source_width,
                source_height,
                uv,
            )
        })
        .collect()
}

pub(super) fn clip_infinite_source_line(
    point: [f32; 2],
    direction: [f32; 2],
    source_width: u32,
    source_height: u32,
) -> Option<(f32, f32)> {
    let bounds = [source_width.max(1) as f32, source_height.max(1) as f32];
    let mut lo = f32::NEG_INFINITY;
    let mut hi = f32::INFINITY;
    for axis in 0..2 {
        let p = point[axis];
        let d = direction[axis];
        if d.abs() <= 1e-8 {
            if p < 0.0 || p > bounds[axis] {
                return None;
            }
            continue;
        }
        let a = (0.0 - p) / d;
        let b = (bounds[axis] - p) / d;
        lo = lo.max(a.min(b));
        hi = hi.min(a.max(b));
        if lo > hi {
            return None;
        }
    }
    (lo.is_finite() && hi.is_finite()).then_some((lo, hi))
}

pub(super) fn linear_isot_geometry_screen_points(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    start: [f32; 2],
    end: [f32; 2],
    t: f32,
    segments: usize,
) -> Vec<Pos2> {
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let start_px = [start[0] * width, start[1] * height];
    let delta = [(end[0] - start[0]) * width, (end[1] - start[1]) * height];
    let center = [start_px[0] + delta[0] * t, start_px[1] + delta[1] * t];
    let perpendicular = [-delta[1], delta[0]];
    if perpendicular[0].abs().max(perpendicular[1].abs()) <= 1e-6 {
        return vec![final_geometry_native_source_to_screen(
            image_rect,
            geometry,
            lens_geometry,
            source_width,
            source_height,
            start,
        )];
    }
    let Some((q0, q1)) =
        clip_infinite_source_line(center, perpendicular, source_width, source_height)
    else {
        return Vec::new();
    };
    let segments = segments.max(2);
    (0..=segments)
        .map(|index| {
            let fraction = index as f32 / segments as f32;
            let q = q0 + (q1 - q0) * fraction;
            let source_px = [
                center[0] + perpendicular[0] * q,
                center[1] + perpendicular[1] * q,
            ];
            let uv = [source_px[0] / width, source_px[1] / height];
            final_geometry_native_source_to_screen(
                image_rect,
                geometry,
                lens_geometry,
                source_width,
                source_height,
                uv,
            )
        })
        .collect()
}

pub(super) fn brush_outline_geometry_screen_points(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    size: f32,
    segments: usize,
) -> Vec<Pos2> {
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let radius = size.max(0.0) * source_width.min(source_height).max(1) as f32;
    let center_px = [center[0] * width, center[1] * height];
    let segments = segments.max(16);
    (0..=segments)
        .map(|index| {
            let angle = std::f32::consts::TAU * index as f32 / segments as f32;
            let uv = [
                (center_px[0] + radius * angle.cos()) / width,
                (center_px[1] + radius * angle.sin()) / height,
            ];
            final_geometry_native_source_to_screen(
                image_rect,
                geometry,
                lens_geometry,
                source_width,
                source_height,
                uv,
            )
        })
        .collect()
}

pub(super) fn radial_source_uv_at(
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
    angle: f32,
    source_width: u32,
    source_height: u32,
) -> [f32; 2] {
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let local_x = radius[0] * width * angle.cos();
    let local_y = radius[1] * height * angle.sin();
    let cos_r = rotation.cos();
    let sin_r = rotation.sin();
    let dx = cos_r * local_x - sin_r * local_y;
    let dy = sin_r * local_x + cos_r * local_y;
    [center[0] + dx / width, center[1] + dy / height]
}

pub(super) fn radial_handles_geometry_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
) -> [Pos2; 4] {
    [
        0.0,
        std::f32::consts::PI,
        std::f32::consts::FRAC_PI_2,
        -std::f32::consts::FRAC_PI_2,
    ]
    .map(|angle| {
        final_geometry_native_source_to_screen(
            image_rect,
            geometry,
            lens_geometry,
            source_width,
            source_height,
            radial_source_uv_at(center, radius, rotation, angle, source_width, source_height),
        )
    })
}

pub(super) fn radial_rotation_handle_geometry(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
) -> Pos2 {
    let center_screen = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        center,
    );
    let major_screen = radial_handles_geometry_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        center,
        radius,
        rotation,
    )[0];
    let direction = (major_screen - center_screen).normalized();
    major_screen + direction * 30.0
}

pub(super) fn radial_outline_geometry_screen_points(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
    segments: usize,
) -> Vec<Pos2> {
    let segments = segments.max(12);
    (0..=segments)
        .map(|index| {
            let angle = std::f32::consts::TAU * index as f32 / segments as f32;
            final_geometry_native_source_to_screen(
                image_rect,
                geometry,
                lens_geometry,
                source_width,
                source_height,
                radial_source_uv_at(center, radius, rotation, angle, source_width, source_height),
            )
        })
        .collect()
}

pub(super) fn distance_to_segment(point: Pos2, start: Pos2, end: Pos2) -> f32 {
    let segment = end - start;
    let length_sq = segment.length_sq();
    if length_sq <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(segment) / length_sq).clamp(0.0, 1.0);
    point.distance(start + segment * t)
}

pub(super) fn distance_to_polyline(point: Pos2, points: &[Pos2]) -> f32 {
    match points {
        [] => f32::INFINITY,
        [only] => point.distance(*only),
        _ => points
            .windows(2)
            .map(|pair| distance_to_segment(point, pair[0], pair[1]))
            .fold(f32::INFINITY, f32::min),
    }
}

pub(super) fn geometry_forward_affine(geometry: GeometryTransform, dx: f32, dy: f32) -> [f32; 2] {
    let geometry = geometry.sanitized();
    let fx = if geometry.flip_horizontal { -1.0 } else { 1.0 };
    let fy = if geometry.flip_vertical { -1.0 } else { 1.0 };
    let shx = geometry.horizontal_transform.to_radians().tan();
    let shy = geometry.vertical_transform.to_radians().tan();
    let angle = geometry.rotation_degrees.to_radians();
    let c = angle.cos();
    let s = angle.sin();

    let flipped_x = dx * fx;
    let flipped_y = dy * fy;
    let sheared_x = flipped_x + shx * flipped_y;
    let sheared_y = shy * flipped_x + flipped_y;
    [c * sheared_x - s * sheared_y, s * sheared_x + c * sheared_y]
}

pub(super) fn geometry_inverse_affine(geometry: GeometryTransform, dx: f32, dy: f32) -> [f32; 2] {
    let geometry = geometry.sanitized();
    let fx = if geometry.flip_horizontal { -1.0 } else { 1.0 };
    let fy = if geometry.flip_vertical { -1.0 } else { 1.0 };
    let shx = geometry.horizontal_transform.to_radians().tan();
    let shy = geometry.vertical_transform.to_radians().tan();
    let angle = geometry.rotation_degrees.to_radians();
    let c = angle.cos();
    let s = angle.sin();
    let a = c * fx - s * shy * fx;
    let b = c * shx * fy - s * fy;
    let c2 = s * fx + c * shy * fx;
    let d = s * shx * fy + c * fy;
    let determinant = a * d - b * c2;
    if determinant.abs() < 1e-6 {
        return [0.0, 0.0];
    }
    [
        (d * dx - b * dy) / determinant,
        (-c2 * dx + a * dy) / determinant,
    ]
}

pub(super) fn quarter_rotate_delta(quarter_turns: u8, dx: f32, dy: f32) -> [f32; 2] {
    match quarter_turns % 4 {
        0 => [dx, dy],
        1 => [-dy, dx],
        2 => [-dx, -dy],
        _ => [dy, -dx],
    }
}

pub(super) fn quarter_unrotate_delta(quarter_turns: u8, dx: f32, dy: f32) -> [f32; 2] {
    match quarter_turns % 4 {
        0 => [dx, dy],
        1 => [dy, -dx],
        2 => [-dx, -dy],
        _ => [-dy, dx],
    }
}

pub(super) fn geometry_forward_linear(geometry: GeometryTransform, dx: f32, dy: f32) -> [f32; 2] {
    let affine = geometry_forward_affine(geometry, dx, dy);
    quarter_rotate_delta(geometry.quarter_turns, affine[0], affine[1])
}

pub(super) fn geometry_inverse_linear(geometry: GeometryTransform, dx: f32, dy: f32) -> [f32; 2] {
    let affine = quarter_unrotate_delta(geometry.quarter_turns, dx, dy);
    geometry_inverse_affine(geometry, affine[0], affine[1])
}

pub(super) fn quarter_rotate_image_point(
    quarter_turns: u8,
    source_width: f32,
    source_height: f32,
    point: [f32; 2],
) -> [f32; 2] {
    match quarter_turns % 4 {
        0 => point,
        1 => [source_height - point[1], point[0]],
        2 => [source_width - point[0], source_height - point[1]],
        _ => [point[1], source_width - point[0]],
    }
}

pub(super) fn quarter_unrotate_image_point(
    quarter_turns: u8,
    source_width: f32,
    source_height: f32,
    point: [f32; 2],
) -> [f32; 2] {
    match quarter_turns % 4 {
        0 => point,
        1 => [point[1], source_height - point[0]],
        2 => [source_width - point[0], source_height - point[1]],
        _ => [source_width - point[1], point[0]],
    }
}

pub(super) fn geometry_crop_metrics(
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
) -> ([f32; 2], [f32; 2]) {
    let geometry = geometry.sanitized();
    let source_width = source_width.max(1) as f32;
    let source_height = source_height.max(1) as f32;
    let crop = geometry.crop;
    (
        [
            (crop[0] + crop[2]) * 0.5 * source_width,
            (crop[1] + crop[3]) * 0.5 * source_height,
        ],
        [
            (crop[2] - crop[0]) * source_width,
            (crop[3] - crop[1]) * source_height,
        ],
    )
}

pub(super) fn final_geometry_source_to_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    source_uv: [f32; 2],
) -> Pos2 {
    let geometry = geometry.sanitized();
    let ([center_x, center_y], [crop_width, crop_height]) =
        geometry_crop_metrics(geometry, source_width, source_height);
    let source_x = source_uv[0] * source_width.max(1) as f32;
    let source_y = source_uv[1] * source_height.max(1) as f32;
    let transformed = geometry_forward_linear(geometry, source_x - center_x, source_y - center_y);
    let (output_width, output_height) = if geometry.quarter_turns.is_multiple_of(2) {
        (crop_width, crop_height)
    } else {
        (crop_height, crop_width)
    };
    let output_uv = [
        0.5 + transformed[0] / output_width.max(f32::EPSILON),
        0.5 + transformed[1] / output_height.max(f32::EPSILON),
    ];
    normalized_to_screen(image_rect, output_uv)
}

pub(super) fn final_geometry_native_source_to_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    source_uv: [f32; 2],
) -> Pos2 {
    let corrected_uv = lens_geometry.map_or(source_uv, |lens_geometry| {
        native_source_to_corrected_uv(lens_geometry, source_width, source_height, source_uv)
    });
    final_geometry_source_to_screen(
        image_rect,
        geometry,
        source_width,
        source_height,
        corrected_uv,
    )
}

pub(super) fn final_geometry_screen_to_source(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    screen: Pos2,
) -> [f32; 2] {
    let geometry = geometry.sanitized();
    let ([center_x, center_y], [crop_width, crop_height]) =
        geometry_crop_metrics(geometry, source_width, source_height);
    let output_uv = screen_to_normalized_unclamped(image_rect, screen);
    let (output_width, output_height) = if geometry.quarter_turns.is_multiple_of(2) {
        (crop_width, crop_height)
    } else {
        (crop_height, crop_width)
    };
    let output_dx = (output_uv[0] - 0.5) * output_width;
    let output_dy = (output_uv[1] - 0.5) * output_height;
    let source_delta = geometry_inverse_linear(geometry, output_dx, output_dy);
    [
        (center_x + source_delta[0]) / source_width.max(1) as f32,
        (center_y + source_delta[1]) / source_height.max(1) as f32,
    ]
}

pub(super) fn editable_source_uv(uv: [f32; 2]) -> Option<[f32; 2]> {
    // Geometry export samples from the full source image after defining the crop
    // as an output frame. A rotated/sheared crop therefore often contains valid
    // pixels whose source coordinates lie outside `geometry.crop`. Treat only
    // coordinates outside the source image as pasteboard. The small tolerance
    // absorbs inverse-transform floating-point noise at the exact image border,
    // then clamps stored brush/color coordinates back into the canonical range.
    const EDGE_EPSILON: f32 = 1e-4;
    if !uv[0].is_finite()
        || !uv[1].is_finite()
        || uv[0] < -EDGE_EPSILON
        || uv[0] > 1.0 + EDGE_EPSILON
        || uv[1] < -EDGE_EPSILON
        || uv[1] > 1.0 + EDGE_EPSILON
    {
        return None;
    }
    Some([uv[0].clamp(0.0, 1.0), uv[1].clamp(0.0, 1.0)])
}

pub(super) fn geometry_brush_radius_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    center: [f32; 2],
    size: f32,
) -> f32 {
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let radius_source_pixels = size.max(0.0) * source_width.min(source_height).max(1) as f32;
    let center_screen = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        center,
    );
    let x_screen = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        [center[0] + radius_source_pixels / source_width_f, center[1]],
    );
    let y_screen = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        [
            center[0],
            center[1] + radius_source_pixels / source_height_f,
        ],
    );
    center_screen
        .distance(x_screen)
        .max(center_screen.distance(y_screen))
}

pub(super) fn crop_workspace_source_to_screen(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    source_uv: [f32; 2],
) -> Pos2 {
    let geometry = geometry.sanitized();
    let ([center_x, center_y], _) = geometry_crop_metrics(geometry, source_width, source_height);
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let source_x = source_uv[0] * source_width_f;
    let source_y = source_uv[1] * source_height_f;
    let transformed = geometry_forward_affine(geometry, source_x - center_x, source_y - center_y);
    let pre_quarter = [center_x + transformed[0], center_y + transformed[1]];
    let canvas_point = quarter_rotate_image_point(
        geometry.quarter_turns,
        source_width_f,
        source_height_f,
        pre_quarter,
    );
    let (canvas_width, canvas_height) = if geometry.quarter_turns.is_multiple_of(2) {
        (source_width_f, source_height_f)
    } else {
        (source_height_f, source_width_f)
    };
    normalized_to_screen(
        image_rect,
        [
            canvas_point[0] / canvas_width,
            canvas_point[1] / canvas_height,
        ],
    )
}

pub(super) fn crop_workspace_screen_to_source(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    screen: Pos2,
) -> [f32; 2] {
    let geometry = geometry.sanitized();
    let ([center_x, center_y], _) = geometry_crop_metrics(geometry, source_width, source_height);
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let (canvas_width, canvas_height) = if geometry.quarter_turns.is_multiple_of(2) {
        (source_width_f, source_height_f)
    } else {
        (source_height_f, source_width_f)
    };
    let canvas_uv = screen_to_normalized_unclamped(image_rect, screen);
    let canvas_point = [canvas_uv[0] * canvas_width, canvas_uv[1] * canvas_height];
    let pre_quarter = quarter_unrotate_image_point(
        geometry.quarter_turns,
        source_width_f,
        source_height_f,
        canvas_point,
    );
    let source_delta = geometry_inverse_affine(
        geometry,
        pre_quarter[0] - center_x,
        pre_quarter[1] - center_y,
    );
    [
        (center_x + source_delta[0]) / source_width_f,
        (center_y + source_delta[1]) / source_height_f,
    ]
}

pub(super) fn source_uv_bbox(points: impl IntoIterator<Item = [f32; 2]>) -> crate::app::PreviewUvRect {
    let mut min = [1.0_f32, 1.0_f32];
    let mut max = [0.0_f32, 0.0_f32];
    for point in points {
        min[0] = min[0].min(point[0]);
        min[1] = min[1].min(point[1]);
        max[0] = max[0].max(point[0]);
        max[1] = max[1].max(point[1]);
    }
    min[0] = min[0].clamp(0.0, 1.0);
    min[1] = min[1].clamp(0.0, 1.0);
    max[0] = max[0].clamp(0.0, 1.0);
    max[1] = max[1].clamp(0.0, 1.0);
    if max[0] <= min[0] {
        if min[0] >= 1.0 {
            min[0] = 1.0 - 1e-6;
            max[0] = 1.0;
        } else {
            max[0] = (min[0] + 1e-6).min(1.0);
        }
    }
    if max[1] <= min[1] {
        if min[1] >= 1.0 {
            min[1] = 1.0 - 1e-6;
            max[1] = 1.0;
        } else {
            max[1] = (min[1] + 1e-6).min(1.0);
        }
    }
    crate::app::PreviewUvRect { min, max }
}

pub(super) fn visible_rect_sample_points(rect: Rect, nonlinear: bool) -> Vec<Pos2> {
    let steps = if nonlinear { 10 } else { 1 };
    let mut points = Vec::with_capacity((steps + 1) * (steps + 1));
    for y in 0..=steps {
        let ty = y as f32 / steps as f32;
        for x in 0..=steps {
            let tx = x as f32 / steps as f32;
            points.push(Pos2::new(
                rect.left() + rect.width() * tx,
                rect.top() + rect.height() * ty,
            ));
        }
    }
    points
}

pub(super) fn final_geometry_visible_source_uv(
    image_rect: Rect,
    visible_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
) -> crate::app::PreviewUvRect {
    source_uv_bbox(
        visible_rect_sample_points(visible_rect, lens_geometry.is_some())
            .into_iter()
            .map(|point| {
                final_geometry_screen_to_native_source(
                    image_rect,
                    geometry,
                    lens_geometry,
                    source_width,
                    source_height,
                    point,
                )
            }),
    )
}

pub(super) fn crop_workspace_visible_source_uv(
    image_rect: Rect,
    visible_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
) -> crate::app::PreviewUvRect {
    source_uv_bbox(
        visible_rect_sample_points(visible_rect, lens_geometry.is_some())
            .into_iter()
            .map(|point| {
                crop_workspace_screen_to_native_source(
                    image_rect,
                    geometry,
                    lens_geometry,
                    source_width,
                    source_height,
                    point,
                )
            }),
    )
}

pub(super) fn native_source_to_corrected_uv(
    lens_geometry: &LensGeometryMap,
    source_width: u32,
    source_height: u32,
    source_uv: [f32; 2],
) -> [f32; 2] {
    if source_uv[0] < 0.0 || source_uv[0] > 1.0 || source_uv[1] < 0.0 || source_uv[1] > 1.0 {
        return source_uv;
    }
    let width = source_width.saturating_sub(1).max(1) as f32;
    let height = source_height.saturating_sub(1).max(1) as f32;
    let corrected = lens_geometry.corrected_position_for_raster(
        source_uv[0] * width,
        source_uv[1] * height,
        source_width,
        source_height,
    );
    [corrected[0] / width, corrected[1] / height]
}

pub(super) fn final_geometry_screen_to_native_source(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    screen: Pos2,
) -> [f32; 2] {
    let corrected_uv =
        final_geometry_screen_to_source(image_rect, geometry, source_width, source_height, screen);
    corrected_uv_to_native_source(corrected_uv, lens_geometry, source_width, source_height)
}

pub(super) fn crop_workspace_screen_to_native_source(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    screen: Pos2,
) -> [f32; 2] {
    let corrected_uv =
        crop_workspace_screen_to_source(image_rect, geometry, source_width, source_height, screen);
    corrected_uv_to_native_source(corrected_uv, lens_geometry, source_width, source_height)
}

pub(super) fn corrected_uv_to_native_source(
    corrected_uv: [f32; 2],
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
) -> [f32; 2] {
    let Some(lens_geometry) = lens_geometry else {
        return corrected_uv;
    };
    if corrected_uv[0] < 0.0
        || corrected_uv[0] > 1.0
        || corrected_uv[1] < 0.0
        || corrected_uv[1] > 1.0
    {
        return corrected_uv;
    }
    let width = source_width.saturating_sub(1).max(1) as f32;
    let height = source_height.saturating_sub(1).max(1) as f32;
    let source = lens_geometry.source_position_for_raster(
        corrected_uv[0] * width,
        corrected_uv[1] * height,
        source_width,
        source_height,
    );
    [source[0] / width, source[1] / height]
}

pub(super) fn crop_preview_screen_rect(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
) -> Rect {
    let geometry = geometry.sanitized();
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let crop = geometry.crop;
    let crop_center = [
        (crop[0] + crop[2]) * 0.5 * source_width_f,
        (crop[1] + crop[3]) * 0.5 * source_height_f,
    ];
    let display_center = quarter_rotate_image_point(
        geometry.quarter_turns,
        source_width_f,
        source_height_f,
        crop_center,
    );
    let crop_width = (crop[2] - crop[0]) * source_width_f;
    let crop_height = (crop[3] - crop[1]) * source_height_f;
    let (canvas_width, canvas_height, display_width, display_height) =
        if geometry.quarter_turns.is_multiple_of(2) {
            (source_width_f, source_height_f, crop_width, crop_height)
        } else {
            (source_height_f, source_width_f, crop_height, crop_width)
        };
    let center = normalized_to_screen(
        image_rect,
        [
            display_center[0] / canvas_width,
            display_center[1] / canvas_height,
        ],
    );
    Rect::from_center_size(
        center,
        egui::vec2(
            display_width / canvas_width * image_rect.width(),
            display_height / canvas_height * image_rect.height(),
        ),
    )
}

pub(super) fn crop_source_handle_for_display(handle: CropHandle, quarter_turns: u8) -> CropHandle {
    use CropHandle::*;
    match quarter_turns % 4 {
        0 => handle,
        1 => match handle {
            TopLeft => BottomLeft,
            TopRight => TopLeft,
            BottomRight => TopRight,
            BottomLeft => BottomRight,
            Top => Left,
            Right => Top,
            Bottom => Right,
            Left => Bottom,
            Move => Move,
        },
        2 => match handle {
            TopLeft => BottomRight,
            TopRight => BottomLeft,
            BottomRight => TopLeft,
            BottomLeft => TopRight,
            Top => Bottom,
            Right => Left,
            Bottom => Top,
            Left => Right,
            Move => Move,
        },
        _ => match handle {
            TopLeft => TopRight,
            TopRight => BottomRight,
            BottomRight => BottomLeft,
            BottomLeft => TopLeft,
            Top => Right,
            Right => Bottom,
            Bottom => Left,
            Left => Top,
            Move => Move,
        },
    }
}

pub(super) fn crop_preview_pointer_to_source_normalized(
    image_rect: Rect,
    quarter_turns: u8,
    source_width: u32,
    source_height: u32,
    pointer: Pos2,
) -> [f32; 2] {
    let source_width_f = source_width.max(1) as f32;
    let source_height_f = source_height.max(1) as f32;
    let (canvas_width, canvas_height) = if quarter_turns.is_multiple_of(2) {
        (source_width_f, source_height_f)
    } else {
        (source_height_f, source_width_f)
    };
    let canvas_uv = screen_to_normalized_unclamped(image_rect, pointer);
    let source_point = quarter_unrotate_image_point(
        quarter_turns,
        source_width_f,
        source_height_f,
        [canvas_uv[0] * canvas_width, canvas_uv[1] * canvas_height],
    );
    [
        source_point[0] / source_width_f,
        source_point[1] / source_height_f,
    ]
}

pub(super) fn source_uv_inside_image(uv: [f32; 2]) -> bool {
    const EPSILON: f32 = 1e-4;
    uv[0].is_finite()
        && uv[1].is_finite()
        && uv[0] >= -EPSILON
        && uv[0] <= 1.0 + EPSILON
        && uv[1] >= -EPSILON
        && uv[1] <= 1.0 + EPSILON
}

pub(super) fn normalize_degrees(mut degrees: f32) -> f32 {
    while degrees > 180.0 {
        degrees -= 360.0;
    }
    while degrees <= -180.0 {
        degrees += 360.0;
    }
    degrees
}

pub(super) fn nearest_straight_axis_degrees(angle: f32) -> f32 {
    // Pick the nearest horizontal or vertical axis. Drawing left-to-right or
    // right-to-left therefore produces the same correction, as does either
    // direction along a vertical edge.
    (angle / 90.0).round() * 90.0
}

pub(super) fn crop_workspace_image_polygon(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
) -> Vec<Pos2> {
    [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
        .into_iter()
        .map(|uv| {
            crop_workspace_source_to_screen(image_rect, geometry, source_width, source_height, uv)
        })
        .collect()
}

pub(super) fn clip_polygon_to_rect(polygon: &[Pos2], rect: Rect) -> Vec<Pos2> {
    fn clip_axis(
        input: &[Pos2],
        inside: impl Fn(Pos2) -> bool,
        intersect: impl Fn(Pos2, Pos2) -> Pos2,
    ) -> Vec<Pos2> {
        if input.is_empty() {
            return Vec::new();
        }
        let mut output = Vec::with_capacity(input.len() + 4);
        let mut previous = *input.last().unwrap();
        let mut previous_inside = inside(previous);
        for &current in input {
            let current_inside = inside(current);
            if current_inside != previous_inside {
                output.push(intersect(previous, current));
            }
            if current_inside {
                output.push(current);
            }
            previous = current;
            previous_inside = current_inside;
        }
        output
    }

    let mut output = polygon.to_vec();
    let left = rect.left();
    output = clip_axis(
        &output,
        |p| p.x >= left,
        |a, b| {
            let denom = b.x - a.x;
            let t = if denom.abs() <= f32::EPSILON {
                0.0
            } else {
                (left - a.x) / denom
            };
            Pos2::new(left, a.y + (b.y - a.y) * t)
        },
    );
    let right = rect.right();
    output = clip_axis(
        &output,
        |p| p.x <= right,
        |a, b| {
            let denom = b.x - a.x;
            let t = if denom.abs() <= f32::EPSILON {
                0.0
            } else {
                (right - a.x) / denom
            };
            Pos2::new(right, a.y + (b.y - a.y) * t)
        },
    );
    let top = rect.top();
    output = clip_axis(
        &output,
        |p| p.y >= top,
        |a, b| {
            let denom = b.y - a.y;
            let t = if denom.abs() <= f32::EPSILON {
                0.0
            } else {
                (top - a.y) / denom
            };
            Pos2::new(a.x + (b.x - a.x) * t, top)
        },
    );
    let bottom = rect.bottom();
    clip_axis(
        &output,
        |p| p.y <= bottom,
        |a, b| {
            let denom = b.y - a.y;
            let t = if denom.abs() <= f32::EPSILON {
                0.0
            } else {
                (bottom - a.y) / denom
            };
            Pos2::new(a.x + (b.x - a.x) * t, bottom)
        },
    )
}

pub(super) fn crop_rect_segments(rect: Rect) -> [(Pos2, Pos2); 4] {
    [
        (rect.left_top(), rect.right_top()),
        (rect.right_top(), rect.right_bottom()),
        (rect.right_bottom(), rect.left_bottom()),
        (rect.left_bottom(), rect.left_top()),
    ]
}

pub(super) fn liang_barsky_clip_test(p: f32, q: f32, t0: &mut f32, t1: &mut f32) -> bool {
    // Screen/source round-trips at crop boundaries can differ by a few ULPs as
    // zoom changes. Treat nearly parallel segments and boundary coordinates
    // with a normalized-source tolerance so an edge does not flicker between
    // accepted and rejected at isolated zoom levels.
    const CLIP_EPSILON: f32 = 1.0e-5;
    if p.abs() <= CLIP_EPSILON {
        return q >= -CLIP_EPSILON;
    }
    let r = q / p;
    if p < 0.0 {
        if r > *t1 + CLIP_EPSILON {
            return false;
        }
        if r > *t0 {
            *t0 = r;
        }
    } else {
        if r < *t0 - CLIP_EPSILON {
            return false;
        }
        if r < *t1 {
            *t1 = r;
        }
    }
    true
}

pub(super) fn clip_crop_workspace_segment_to_source_image(
    image_rect: Rect,
    geometry: GeometryTransform,
    source_width: u32,
    source_height: u32,
    a: Pos2,
    b: Pos2,
) -> Option<[Pos2; 2]> {
    let start =
        crop_workspace_screen_to_source(image_rect, geometry, source_width, source_height, a);
    let end = crop_workspace_screen_to_source(image_rect, geometry, source_width, source_height, b);
    let delta = [end[0] - start[0], end[1] - start[1]];
    let mut t0 = 0.0_f32;
    let mut t1 = 1.0_f32;
    if !liang_barsky_clip_test(-delta[0], start[0], &mut t0, &mut t1)
        || !liang_barsky_clip_test(delta[0], 1.0 - start[0], &mut t0, &mut t1)
        || !liang_barsky_clip_test(-delta[1], start[1], &mut t0, &mut t1)
        || !liang_barsky_clip_test(delta[1], 1.0 - start[1], &mut t0, &mut t1)
        || t1 + 1.0e-5 < t0
    {
        return None;
    }
    t0 = t0.clamp(0.0, 1.0);
    t1 = t1.clamp(t0, 1.0);
    let source_a = [start[0] + delta[0] * t0, start[1] + delta[1] * t0];
    let source_b = [start[0] + delta[0] * t1, start[1] + delta[1] * t1];
    let source_a = [source_a[0].clamp(0.0, 1.0), source_a[1].clamp(0.0, 1.0)];
    let source_b = [source_b[0].clamp(0.0, 1.0), source_b[1].clamp(0.0, 1.0)];
    Some([
        crop_workspace_source_to_screen(
            image_rect,
            geometry,
            source_width,
            source_height,
            source_a,
        ),
        crop_workspace_source_to_screen(
            image_rect,
            geometry,
            source_width,
            source_height,
            source_b,
        ),
    ])
}

pub(super) fn crop_handle_points(rect: Rect) -> [Pos2; 8] {
    [
        rect.left_top(),
        rect.right_top(),
        rect.left_bottom(),
        rect.right_bottom(),
        Pos2::new(rect.center().x, rect.top()),
        Pos2::new(rect.center().x, rect.bottom()),
        Pos2::new(rect.left(), rect.center().y),
        Pos2::new(rect.right(), rect.center().y),
    ]
}

pub(super) fn crop_handle_at(rect: Rect, pointer: Pos2, radius: f32) -> Option<CropHandle> {
    let candidates = [
        (CropHandle::TopLeft, rect.left_top()),
        (CropHandle::TopRight, rect.right_top()),
        (CropHandle::BottomLeft, rect.left_bottom()),
        (CropHandle::BottomRight, rect.right_bottom()),
        (CropHandle::Top, Pos2::new(rect.center().x, rect.top())),
        (
            CropHandle::Bottom,
            Pos2::new(rect.center().x, rect.bottom()),
        ),
        (CropHandle::Left, Pos2::new(rect.left(), rect.center().y)),
        (CropHandle::Right, Pos2::new(rect.right(), rect.center().y)),
    ];
    for (handle, point) in candidates {
        if point.distance(pointer) <= radius {
            return Some(handle);
        }
    }
    rect.contains(pointer).then_some(CropHandle::Move)
}

pub(super) fn sanitize_dragged_crop(mut crop: [f32; 4], handle: CropHandle) -> [f32; 4] {
    let min = GeometryTransform::MIN_CROP_EXTENT;
    match handle {
        CropHandle::Left | CropHandle::TopLeft | CropHandle::BottomLeft => {
            crop[0] = crop[0].clamp(0.0, crop[2] - min);
        }
        CropHandle::Right | CropHandle::TopRight | CropHandle::BottomRight => {
            crop[2] = crop[2].clamp(crop[0] + min, 1.0);
        }
        _ => {}
    }
    match handle {
        CropHandle::Top | CropHandle::TopLeft | CropHandle::TopRight => {
            crop[1] = crop[1].clamp(0.0, crop[3] - min);
        }
        CropHandle::Bottom | CropHandle::BottomLeft | CropHandle::BottomRight => {
            crop[3] = crop[3].clamp(crop[1] + min, 1.0);
        }
        _ => {}
    }
    crop
}

pub(super) fn is_crop_corner(handle: CropHandle) -> bool {
    matches!(
        handle,
        CropHandle::TopLeft
            | CropHandle::TopRight
            | CropHandle::BottomLeft
            | CropHandle::BottomRight
    )
}

/// Constrains a corner drag to the selected aspect ratio while keeping the
/// diagonally opposite corner fixed. The anchor comes from the crop at drag
/// start, so clamping at an image boundary can never make the opposite corner
/// wander under the pointer.
pub(super) fn constrain_crop_corner_aspect(
    app: &AurawApp,
    original_crop: [f32; 4],
    pointer: [f32; 2],
    handle: CropHandle,
) -> Option<[f32; 4]> {
    let raw = app.loaded_raw.as_ref()?;
    let ratio = app.geometry.aspect_ratio.value(raw.width, raw.height)?;
    let normalized_ratio = ratio / (raw.width.max(1) as f32 / raw.height.max(1) as f32);
    if !normalized_ratio.is_finite() || normalized_ratio <= f32::EPSILON {
        return None;
    }

    let (anchor_x, anchor_y, x_sign, y_sign) = match handle {
        CropHandle::TopLeft => (original_crop[2], original_crop[3], -1.0, -1.0),
        CropHandle::TopRight => (original_crop[0], original_crop[3], 1.0, -1.0),
        CropHandle::BottomLeft => (original_crop[2], original_crop[1], -1.0, 1.0),
        CropHandle::BottomRight => (original_crop[0], original_crop[1], 1.0, 1.0),
        _ => return None,
    };

    let desired_width = (pointer[0] - anchor_x).abs();
    let desired_height = (pointer[1] - anchor_y).abs();

    // Orthogonally project the pointer distance onto width/height pairs that
    // satisfy width / height == normalized_ratio. This makes diagonal, mostly
    // horizontal, and mostly vertical drags all feel continuous.
    let inv_ratio = 1.0 / normalized_ratio;
    let projected_width =
        (desired_width + desired_height * inv_ratio) / (1.0 + inv_ratio * inv_ratio);

    let max_width_from_x = if x_sign < 0.0 {
        anchor_x
    } else {
        1.0 - anchor_x
    };
    let max_height_from_y = if y_sign < 0.0 {
        anchor_y
    } else {
        1.0 - anchor_y
    };
    let max_width = max_width_from_x.min(max_height_from_y * normalized_ratio);

    let min_extent = crate::pipeline::GeometryTransform::MIN_CROP_EXTENT;
    let min_width = min_extent.max(min_extent * normalized_ratio);
    let width = projected_width.clamp(min_width.min(max_width), max_width);
    let height = width / normalized_ratio;

    let dragged_x = anchor_x + x_sign * width;
    let dragged_y = anchor_y + y_sign * height;
    Some(match handle {
        CropHandle::TopLeft => [dragged_x, dragged_y, anchor_x, anchor_y],
        CropHandle::TopRight => [anchor_x, dragged_y, dragged_x, anchor_y],
        CropHandle::BottomLeft => [dragged_x, anchor_y, anchor_x, dragged_y],
        CropHandle::BottomRight => [anchor_x, anchor_y, dragged_x, dragged_y],
        _ => return None,
    })
}

pub(super) fn constrain_crop_aspect(app: &AurawApp, mut crop: [f32; 4], handle: CropHandle) -> [f32; 4] {
    let Some(raw) = app.loaded_raw.as_ref() else {
        return crop;
    };
    let Some(ratio) = app.geometry.aspect_ratio.value(raw.width, raw.height) else {
        return crop;
    };
    let normalized_ratio = ratio / (raw.width.max(1) as f32 / raw.height.max(1) as f32);
    let width = crop[2] - crop[0];
    let height = crop[3] - crop[1];
    let target_height = width / normalized_ratio.max(f32::EPSILON);
    let target_width = height * normalized_ratio;

    let horizontal_edge = matches!(handle, CropHandle::Left | CropHandle::Right);
    if horizontal_edge
        || (target_height <= 1.0 && (target_height - height).abs() <= (target_width - width).abs())
    {
        let new_height =
            target_height.clamp(crate::pipeline::GeometryTransform::MIN_CROP_EXTENT, 1.0);
        let center = (crop[1] + crop[3]) * 0.5;
        crop[1] = (center - new_height * 0.5).clamp(0.0, 1.0 - new_height);
        crop[3] = crop[1] + new_height;
    } else {
        let new_width =
            target_width.clamp(crate::pipeline::GeometryTransform::MIN_CROP_EXTENT, 1.0);
        let center = (crop[0] + crop[2]) * 0.5;
        crop[0] = (center - new_width * 0.5).clamp(0.0, 1.0 - new_width);
        crop[2] = crop[0] + new_width;
    }
    crop
}

pub(super) fn fitted_image_size(available: egui::Vec2, image_aspect: f32) -> egui::Vec2 {
    let available_aspect = available.x / available.y.max(1.0);
    if available_aspect > image_aspect {
        egui::vec2(available.y * image_aspect, available.y)
    } else {
        egui::vec2(available.x, available.x / image_aspect.max(f32::EPSILON))
    }
}

pub(super) fn zoomed_image_rect(outer_rect: Rect, base_size: egui::Vec2, zoom: f32, center: [f32; 2]) -> Rect {
    let size = base_size * zoom;
    let min = Pos2::new(
        outer_rect.center().x - center[0] * size.x,
        outer_rect.center().y - center[1] * size.y,
    );
    Rect::from_min_size(min, size)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn transform_preview_about_screen_points(
    outer_rect: Rect,
    current_image_rect: Rect,
    base_size: egui::Vec2,
    zoom: &mut f32,
    center: &mut [f32; 2],
    anchor_screen: Pos2,
    target_screen: Pos2,
    zoom_factor: f32,
) -> bool {
    let previous_zoom = *zoom;
    let previous_center = *center;
    let anchor_uv = [
        (anchor_screen.x - current_image_rect.left()) / current_image_rect.width().max(1.0),
        (anchor_screen.y - current_image_rect.top()) / current_image_rect.height().max(1.0),
    ];

    *zoom = (previous_zoom * zoom_factor).clamp(MIN_PREVIEW_ZOOM, MAX_PREVIEW_ZOOM);
    let new_size = base_size * *zoom;
    let new_min = Pos2::new(
        target_screen.x - anchor_uv[0] * new_size.x,
        target_screen.y - anchor_uv[1] * new_size.y,
    );
    *center = [
        (outer_rect.center().x - new_min.x) / new_size.x.max(1.0),
        (outer_rect.center().y - new_min.y) / new_size.y.max(1.0),
    ];
    clamp_preview_center(center, outer_rect.size(), new_size);

    (*zoom - previous_zoom).abs() > f32::EPSILON
        || (center[0] - previous_center[0]).abs() > f32::EPSILON
        || (center[1] - previous_center[1]).abs() > f32::EPSILON
}

pub(super) fn clamp_preview_center(center: &mut [f32; 2], viewport: egui::Vec2, image: egui::Vec2) {
    for (axis, center_axis) in center.iter_mut().enumerate() {
        let viewport_axis = if axis == 0 { viewport.x } else { viewport.y };
        let image_axis = if axis == 0 { image.x } else { image.y };
        if image_axis <= viewport_axis + 0.5 {
            *center_axis = 0.5;
        } else {
            let half_visible = (viewport_axis / (2.0 * image_axis)).clamp(0.0, 0.5);
            *center_axis = center_axis.clamp(half_visible, 1.0 - half_visible);
        }
    }
}

pub(super) fn preview_uv_changed(left: crate::app::PreviewUvRect, right: crate::app::PreviewUvRect) -> bool {
    left.min
        .into_iter()
        .chain(left.max)
        .zip(right.min.into_iter().chain(right.max))
        .any(|(left, right)| (left - right).abs() > 0.0005)
}

pub(super) fn screen_to_normalized_unclamped(rect: Rect, point: Pos2) -> [f32; 2] {
    [
        (point.x - rect.left()) / rect.width().max(1.0),
        (point.y - rect.top()) / rect.height().max(1.0),
    ]
}

pub(super) fn normalized_to_screen(rect: Rect, point: [f32; 2]) -> Pos2 {
    Pos2::new(
        rect.left() + point[0] * rect.width(),
        rect.top() + point[1] * rect.height(),
    )
}

