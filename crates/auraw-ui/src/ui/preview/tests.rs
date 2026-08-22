use super::*;

#[cfg(test)]
mod preview_overlay_tests {
    use super::*;

    #[test]
    fn armed_picker_owns_the_adjustments_canvas_without_mobile_section_state() {
        assert!(white_balance_picker_owns_canvas(
            SidebarTab::Adjustments,
            true
        ));
        assert!(!white_balance_picker_owns_canvas(
            SidebarTab::Adjustments,
            false
        ));
        assert!(!white_balance_picker_owns_canvas(SidebarTab::Crop, true));
    }

    #[test]
    fn zoom_overlay_uses_a_native_density_source_crop() {
        let region = overlay_raster_region(
            crate::app::PreviewUvRect {
                min: [0.45, 0.40],
                max: [0.55, 0.60],
            },
            6000,
            4000,
            Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0)),
            1.0,
            2,
        );

        assert_eq!(region.source_x, 2698);
        assert_eq!(region.source_y, 1598);
        assert_eq!(region.source_width, 604);
        assert_eq!(region.source_height, 804);
        assert_eq!(region.texture_width, 604);
        assert_eq!(region.texture_height, 804);
    }

    #[test]
    fn screen_relative_brush_compensates_for_zoom() {
        assert!((zoom_scaled_brush_size(0.08, 4.0, false) - 0.02).abs() < 1e-6);
    }

    #[test]
    fn image_relative_brush_ignores_zoom() {
        assert!((zoom_scaled_brush_size(0.08, 4.0, true) - 0.08).abs() < 1e-6);
    }
}
