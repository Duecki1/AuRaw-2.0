use super::*;

#[cfg(test)]
mod live_retouch_preview_tests {
    use super::*;

    #[test]
    fn clone_preview_samples_the_offset_source_while_dragging() {
        let mut rgba = Vec::new();
        for _y in 0..2 {
            for x in 0..4u8 {
                rgba.extend_from_slice(&[x * 10, 0, 0, 255]);
            }
        }
        let source = MaskRgbImage::new(4, 2, rgba).unwrap();
        let region = OverlayRasterKey {
            source_x: 0,
            source_y: 0,
            source_width: 4,
            source_height: 2,
            texture_width: 4,
            texture_height: 2,
        };
        let dabs = [BrushDab {
            center: [0.25, 0.5],
            opacity: 1.0,
            size: 0.5,
            feather: 0.0,
        }];
        let preview = live_retouch_rgba(
            &source,
            region,
            4,
            2,
            &dabs,
            &[255; 8],
            InpaintStrokeKind::Clone,
            [0.5, 0.0],
        )
        .unwrap();
        assert_eq!(&preview[0..4], &[20, 0, 0, 255]);
        assert_eq!(preview[15], 0);
    }

    #[test]
    fn aligned_source_marker_follows_the_idle_brush() {
        let offset = Some([-0.2, 0.1]);
        let first = aligned_retouch_source_uv([0.6, 0.3], [0.1, 0.1], offset);
        let second = aligned_retouch_source_uv([0.8, 0.6], [0.1, 0.1], offset);
        assert!((first[0] - 0.4).abs() < 1e-6 && (first[1] - 0.4).abs() < 1e-6);
        assert!((second[0] - 0.6).abs() < 1e-6 && (second[1] - 0.7).abs() < 1e-6);
    }

    #[test]
    fn clone_preview_samples_pixels_from_earlier_active_dabs() {
        let mut rgba = Vec::new();
        for x in 0..8u8 {
            rgba.extend_from_slice(&[x * 10, 0, 0, 255]);
        }
        let source = MaskRgbImage::new(8, 1, rgba).unwrap();
        let region = OverlayRasterKey {
            source_x: 0,
            source_y: 0,
            source_width: 8,
            source_height: 1,
            texture_width: 8,
            texture_height: 1,
        };
        let dabs = [
            BrushDab {
                center: [2.5 / 8.0, 0.5],
                opacity: 1.0,
                size: 0.49,
                feather: 0.0,
            },
            BrushDab {
                center: [3.5 / 8.0, 0.5],
                opacity: 1.0,
                size: 0.49,
                feather: 0.0,
            },
        ];
        let coverage = rasterize_brush_dabs(8, 1, 8, 1, &dabs);
        let preview = live_retouch_rgba(
            &source,
            region,
            8,
            1,
            &dabs,
            &coverage,
            InpaintStrokeKind::Clone,
            [-1.0 / 8.0, 0.0],
        )
        .unwrap();
        assert_eq!(&preview[2 * 4..2 * 4 + 4], &[10, 0, 0, 255]);
        assert_eq!(&preview[3 * 4..3 * 4 + 4], &[10, 0, 0, 255]);
    }

    #[test]
    fn live_overlay_raster_is_limited_to_the_active_stroke() {
        let dabs = [BrushDab {
            center: [0.5, 0.5],
            opacity: 1.0,
            size: 0.02,
            feather: 0.0,
        }];
        let region = inpaint_live_overlay_region(
            &dabs,
            crate::app::PreviewUvRect {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
            1000,
            1000,
            Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 1000.0)),
            1.0,
        );
        assert!(region.source_width < 64);
        assert!(region.source_height < 64);
        assert!(region.texture_width < 64);
        assert!(region.texture_height < 64);
    }
}

#[cfg(test)]
mod white_balance_picker_tests {
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
        // The visible crop contains only 600x800 native pixels, so zooming it
        // to a larger viewport must retain those native samples rather than
        // rasterizing the entire 6000px frame into a 512px overlay.
        assert_eq!(region.texture_width, 604);
        assert_eq!(region.texture_height, 804);
    }

    #[test]
    fn cropped_inpaint_dab_keeps_its_source_pixel_radius() {
        let region = OverlayRasterKey {
            source_x: 2500,
            source_y: 1500,
            source_width: 1000,
            source_height: 1000,
            texture_width: 1000,
            texture_height: 1000,
        };
        let dab = BrushDab {
            center: [0.5, 0.5],
            opacity: 1.0,
            size: 0.01,
            feather: 0.0,
        };
        let cropped = crop_overlay_dabs(&[dab], region, 6000, 4000);
        assert_eq!(cropped[0].center, [0.5, 0.5]);
        assert!((cropped[0].size - 0.04).abs() < 1e-6);
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
