use super::super::*;

const DAB_SPACING_RADIUS_FRACTION: f32 = 0.22;
const MIN_DAB_SPACING_PX: f32 = 0.85;
const MAX_DAB_SPACING_PX: f32 = 24.0;
pub(super) const STANDARD_BRUSH_MINIMUM_SPACING_FRACTION: f32 = 0.80;
pub(super) const OBJECT_BRUSH_MINIMUM_SPACING_FRACTION: f32 = 0.75;

pub(super) struct BrushStrokeSamples {
    pub uv: [f32; 2],
    pub dab_size: f32,
    pub first: bool,
    pub samples: Vec<[f32; 2]>,
}

pub(super) fn sample_brush_stroke(
    image_rect: Rect,
    geometry: GeometryTransform,
    lens_geometry: Option<&LensGeometryMap>,
    source_width: u32,
    source_height: u32,
    pointer: Pos2,
    tool_size: f32,
    preview_zoom: f32,
    image_relative_size: bool,
    previous: &mut Option<[f32; 2]>,
    minimum_spacing_fraction: f32,
) -> Option<BrushStrokeSamples> {
    let Some(uv) = editable_source_uv(final_geometry_screen_to_native_source(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        pointer,
    )) else {
        *previous = None;
        return None;
    };
    let dab_size = zoom_scaled_brush_size(tool_size, preview_zoom, image_relative_size);
    let first = previous.is_none();
    if first {
        return Some(BrushStrokeSamples {
            uv,
            dab_size,
            first: true,
            samples: vec![uv],
        });
    }

    let previous = (*previous).unwrap_or(uv);
    let previous_screen = final_geometry_native_source_to_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        previous,
    );
    let distance_px = pointer.distance(previous_screen);
    let radius_px = geometry_brush_radius_screen(
        image_rect,
        geometry,
        lens_geometry,
        source_width,
        source_height,
        uv,
        dab_size,
    );
    let samples = interpolated_brush_samples(
        previous,
        uv,
        distance_px,
        radius_px,
        minimum_spacing_fraction,
    );
    Some(BrushStrokeSamples {
        uv,
        dab_size,
        first: false,
        samples,
    })
}

fn interpolated_brush_samples(
    previous: [f32; 2],
    current: [f32; 2],
    distance_px: f32,
    radius_px: f32,
    minimum_spacing_fraction: f32,
) -> Vec<[f32; 2]> {
    let spacing_px = (radius_px * DAB_SPACING_RADIUS_FRACTION)
        .clamp(MIN_DAB_SPACING_PX, MAX_DAB_SPACING_PX);
    if distance_px < spacing_px * minimum_spacing_fraction {
        return Vec::new();
    }
    let steps = (distance_px / spacing_px).ceil().max(1.0) as usize;
    let dx = current[0] - previous[0];
    let dy = current[1] - previous[1];
    (1..=steps)
        .map(|step| {
            let t = step as f32 / steps as f32;
            [previous[0] + dx * t, previous[1] + dy * t]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stationary_pointer_emits_no_duplicate_dab() {
        assert!(interpolated_brush_samples(
            [0.4, 0.5],
            [0.4, 0.5],
            0.0,
            20.0,
            STANDARD_BRUSH_MINIMUM_SPACING_FRACTION,
        )
        .is_empty());
    }

    #[test]
    fn shared_spacing_interpolates_to_the_current_native_point() {
        let samples = interpolated_brush_samples(
            [0.1, 0.2],
            [0.5, 0.6],
            44.0,
            20.0,
            STANDARD_BRUSH_MINIMUM_SPACING_FRACTION,
        );
        assert_eq!(samples.len(), 10);
        assert_eq!(samples.last().copied(), Some([0.5, 0.6]));
    }
}
