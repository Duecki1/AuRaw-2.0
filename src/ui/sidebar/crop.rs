use crate::pipeline::{CropAspectRatio, GeometryTransform};

impl Sidebar {
    fn show_crop(ui: &mut Ui, app: &mut AurawApp) {
        let source_dimensions = app
            .loaded_raw
            .as_ref()
            .map(|raw| (raw.width, raw.height))
            .unwrap_or((1, 1));

        // Match the compact action row used by the other Develop tabs. Touch
        // friendliness is handled by egui's platform spacing and by the larger
        // invisible crop handles in the preview, not by oversized visible widgets.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Reset").clicked() {
                    app.geometry = GeometryTransform::default();
                    app.crop_drag = None;
                    app.note_geometry_changed();
                }
            });
        });
        ui.separator();

        let before = app.geometry;
        let previous_aspect = app.geometry.aspect_ratio;
        egui::ComboBox::from_label("Aspect ratio")
            .selected_text(app.geometry.aspect_ratio.label())
            .show_ui(ui, |ui| {
                for aspect in [
                    CropAspectRatio::Free,
                    CropAspectRatio::Original,
                    CropAspectRatio::Square,
                    CropAspectRatio::FourThree,
                    CropAspectRatio::ThreeFour,
                    CropAspectRatio::ThreeTwo,
                    CropAspectRatio::TwoThree,
                    CropAspectRatio::SixteenNine,
                    CropAspectRatio::NineSixteen,
                ] {
                    ui.selectable_value(&mut app.geometry.aspect_ratio, aspect, aspect.label());
                }
            });
        if app.geometry.aspect_ratio != previous_aspect {
            Self::apply_crop_aspect(app, source_dimensions.0, source_dimensions.1);
        }

        ui.add_space(4.0);
        ui.strong("Rotation");
        ui.horizontal(|ui| {
            if ui.button("↶ 90°").clicked() {
                app.geometry.rotate_quarter_turn(false);
            }
            if ui.button("90° ↷").clicked() {
                app.geometry.rotate_quarter_turn(true);
            }
        });
        adjustment_slider(
            ui,
            "Straighten",
            &mut app.geometry.rotation_degrees,
            -45.0..=45.0,
            1,
            0.1,
            Some("Fine rotation for leveling the image."),
        );

        ui.separator();
        ui.strong("Transform");
        ui.horizontal(|ui| {
            ui.checkbox(&mut app.geometry.flip_horizontal, "Flip horizontal");
            ui.checkbox(&mut app.geometry.flip_vertical, "Flip vertical");
        });
        adjustment_slider(
            ui,
            "Horizontal",
            &mut app.geometry.horizontal_transform,
            -30.0..=30.0,
            1,
            0.1,
            Some("Correct horizontal perspective."),
        );
        adjustment_slider(
            ui,
            "Vertical",
            &mut app.geometry.vertical_transform,
            -30.0..=30.0,
            1,
            0.1,
            Some("Correct vertical perspective."),
        );

        let (crop_width, crop_height) = app
            .geometry
            .crop_pixel_dimensions(source_dimensions.0, source_dimensions.1);
        ui.separator();
        ui.label(
            egui::RichText::new(format!(
                "Crop: {crop_width} × {crop_height} px before export resize"
            ))
            .size(11.5)
            .color(ui.visuals().weak_text_color()),
        );

        app.geometry = app.geometry.sanitized();
        if app.geometry != before {
            app.note_geometry_changed();
        }
    }

    fn apply_crop_aspect(app: &mut AurawApp, source_width: u32, source_height: u32) {
        let Some(ratio) = app.geometry.aspect_ratio.value(source_width, source_height) else {
            return;
        };
        let crop = app.geometry.crop;
        let center_x = (crop[0] + crop[2]) * 0.5;
        let center_y = (crop[1] + crop[3]) * 0.5;
        let source_aspect = source_width.max(1) as f32 / source_height.max(1) as f32;
        let target_normalized_ratio = ratio / source_aspect;
        let mut width = crop[2] - crop[0];
        let mut height = crop[3] - crop[1];
        if width / height.max(f32::EPSILON) > target_normalized_ratio {
            width = height * target_normalized_ratio;
        } else {
            height = width / target_normalized_ratio.max(f32::EPSILON);
        }
        width = width.clamp(GeometryTransform::MIN_CROP_EXTENT, 1.0);
        height = height.clamp(GeometryTransform::MIN_CROP_EXTENT, 1.0);
        let left = (center_x - width * 0.5).clamp(0.0, 1.0 - width);
        let top = (center_y - height * 0.5).clamp(0.0, 1.0 - height);
        app.geometry.crop = [left, top, left + width, top + height];
    }
}
