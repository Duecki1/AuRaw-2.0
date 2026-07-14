impl Sidebar {
    fn show_export(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        ui.heading("Export");
        ui.label(
            egui::RichText::new("PNG · sRGB · high-quality processing")
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(6.0);

        let source_dimensions = app.loaded_raw.as_ref().map(|raw| (raw.width, raw.height));
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.strong("Image sizing");
            egui::ComboBox::from_label("Resize to fit")
                .selected_text(app.export_settings.resize_mode.label())
                .show_ui(ui, |ui| {
                    for mode in [
                        ExportResizeMode::Original,
                        ExportResizeMode::LongEdge,
                        ExportResizeMode::ShortEdge,
                        ExportResizeMode::Width,
                        ExportResizeMode::Height,
                        ExportResizeMode::Percentage,
                    ] {
                        ui.selectable_value(
                            &mut app.export_settings.resize_mode,
                            mode,
                            mode.label(),
                        );
                    }
                });

            match app.export_settings.resize_mode {
                ExportResizeMode::Original => {
                    ui.label("Exports the complete processed image.");
                }
                ExportResizeMode::Percentage => {
                    ui.horizontal(|ui| {
                        ui.label("Scale");
                        ui.add(
                            egui::DragValue::new(&mut app.export_settings.percentage)
                                .range(1.0..=400.0)
                                .speed(1.0)
                                .suffix("%"),
                        );
                    });
                }
                mode => {
                    ui.horizontal(|ui| {
                        ui.label(mode.label());
                        ui.add(
                            egui::DragValue::new(&mut app.export_settings.edge_or_dimension)
                                .range(64..=MAX_EXPORT_EDGE)
                                .speed(10.0)
                                .suffix(" px"),
                        );
                    });
                }
            }

            if app.export_settings.resize_mode != ExportResizeMode::Original {
                ui.checkbox(&mut app.export_settings.allow_upscale, "Allow upscaling")
                    .on_hover_text(
                        "Disabled by default to avoid enlarging beyond the source dimensions.",
                    );
            }

            if let Some((width, height)) = source_dimensions {
                match app.export_settings.checked_output_dimensions(width, height) {
                    Ok((output_width, output_height)) => {
                        ui.label(format!(
                            "Source: {width}×{height}  →  Export: {output_width}×{output_height}"
                        ));
                    }
                    Err(error) => {
                        ui.colored_label(egui::Color32::RED, error.to_string());
                    }
                }
            } else {
                ui.label("Open a RAW file to calculate export dimensions.");
            }
        });

        ui.add_space(6.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.strong("Metadata");
            ui.checkbox(&mut app.export_settings.keep_metadata, "Keep metadata")
                .on_hover_text(
                    "Embeds available camera, source-file, original-size, software, and orientation metadata in the PNG.",
                );
        });

        ui.add_space(10.0);
        let dimensions_valid = source_dimensions.is_some_and(|(width, height)| {
            app.export_settings
                .checked_output_dimensions(width, height)
                .is_ok()
        });
        let button =
            egui::Button::new("Export PNG…").min_size(egui::vec2(ui.available_width(), 30.0));
        if ui
            .add_enabled(app.can_export() && dimensions_valid, button)
            .clicked()
        {
            app.export_png(frame);
        }
        if !app.can_export() {
            ui.label(
                egui::RichText::new(
                    "Export becomes available after a RAW image has finished loading.",
                )
                .small()
                .color(ui.visuals().weak_text_color()),
            );
        }
    }
}
