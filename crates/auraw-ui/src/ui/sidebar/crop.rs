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
        crate::ui::theme::toolbar_row(ui, |ui| {
            ui.strong("Crop geometry");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::ui::icons::phosphor_icon_button(
                    ui,
                    egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                    crate::ui::theme::toolbar_icon_size(),
                    "Reset crop and geometry",
                )
                .clicked()
                {
                    app.geometry = GeometryTransform::default();
                    app.crop_constraint_reference = Some(app.geometry.crop);
                    app.crop_drag = None;
                    app.straighten_tool_active = false;
                    app.straighten_drag = None;
                    app.note_geometry_changed();
                }
            });
        });
        ui.add_space(4.0);

        let before = app.geometry;
        if app.crop_constraint_reference.is_none() {
            app.crop_constraint_reference = Some(app.geometry.crop);
        }
        crate::ui::theme::section_card(ui, "Crop", |ui| {
            let previous_aspect = app.geometry.aspect_ratio;
            crate::ui::theme::form_combo(
                ui,
                "Aspect ratio",
                "crop-aspect-ratio",
                app.geometry.aspect_ratio.label(),
                150.0,
                |ui| {
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
                },
            );
            if app.geometry.aspect_ratio != previous_aspect {
                Self::apply_crop_aspect(app, source_dimensions.0, source_dimensions.1);
                app.crop_constraint_reference = Some(app.geometry.crop);
            }
        });

        ui.add_space(crate::ui::theme::CARD_GAP);
        crate::ui::theme::section_card(ui, "Rotation", |ui| {
            ui.horizontal(|ui| {
                if crate::ui::icons::icon_toggle_button(
                    ui,
                    crate::ui::icons::UiIcon::RotateLeft,
                    false,
                    crate::ui::theme::toolbar_icon_size(),
                    "Rotate 90° counter-clockwise",
                )
                .clicked()
                {
                    app.geometry.rotate_quarter_turn(false);
                }
                if crate::ui::icons::icon_toggle_button(
                    ui,
                    crate::ui::icons::UiIcon::RotateRight,
                    false,
                    crate::ui::theme::toolbar_icon_size(),
                    "Rotate 90° clockwise",
                )
                .clicked()
                {
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
            let straighten_label = if app.straighten_tool_active {
                "Drawing straighten line…"
            } else {
                "Draw straighten line"
            };
            if ui
                .selectable_label(app.straighten_tool_active, straighten_label)
                .on_hover_text("Drag along a horizon or vertical edge in the Crop preview. AuRaw rotates the image so that line becomes level.")
                .clicked()
            {
                app.straighten_tool_active = !app.straighten_tool_active;
                app.straighten_drag = None;
                app.crop_drag = None;
            }
        });

        ui.add_space(crate::ui::theme::CARD_GAP);
        crate::ui::theme::section_card(ui, "Transform", |ui| {
            ui.horizontal_wrapped(|ui| {
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
        });

        app.geometry = app.geometry.sanitized();
        let containment_transform_changed =
            (app.geometry.rotation_degrees - before.rotation_degrees).abs() > 1e-6
                || (app.geometry.horizontal_transform - before.horizontal_transform).abs() > 1e-6
                || (app.geometry.vertical_transform - before.vertical_transform).abs() > 1e-6;
        if containment_transform_changed {
            // Re-fit from the user's unconstrained crop rather than repeatedly
            // shrinking the already-fitted result. This makes the white crop
            // rectangle expand again when straighten/keystone is reduced.
            if let Some(reference) = app.crop_constraint_reference {
                app.geometry.crop = reference;
            }
        }
        // Keep the crop rectangle itself inside the usable transformed image.
        // Fine rotation and keystone otherwise expose pasteboard at the crop
        // corners even though the overlay can be visually clipped there.
        app.geometry
            .fit_crop_inside_transformed_source(source_dimensions.0, source_dimensions.1);
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
