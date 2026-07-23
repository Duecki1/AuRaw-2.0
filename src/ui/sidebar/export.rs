pub(crate) fn export_settings_controls(
    ui: &mut Ui,
    settings: &mut crate::pipeline::ExportSettings,
    source_dimensions: Option<(u32, u32)>,
    show_dimension_summary: bool,
) {
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.strong("Image sizing");
        egui::ComboBox::from_label("Resize to fit")
            .selected_text(settings.resize_mode.label())
            .show_ui(ui, |ui| {
                for mode in [
                    ExportResizeMode::Original,
                    ExportResizeMode::LongEdge,
                    ExportResizeMode::ShortEdge,
                    ExportResizeMode::Width,
                    ExportResizeMode::Height,
                    ExportResizeMode::Percentage,
                ] {
                    ui.selectable_value(&mut settings.resize_mode, mode, mode.label());
                }
            });

        match settings.resize_mode {
            ExportResizeMode::Original => {
                ui.label("Exports the complete processed image.");
            }
            ExportResizeMode::Percentage => {
                ui.horizontal(|ui| {
                    ui.label("Scale");
                    ui.add(
                        egui::DragValue::new(&mut settings.percentage)
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
                        egui::DragValue::new(&mut settings.edge_or_dimension)
                            .range(64..=MAX_EXPORT_EDGE)
                            .speed(10.0)
                            .suffix(" px"),
                    );
                });
            }
        }

        if settings.resize_mode != ExportResizeMode::Original {
            ui.checkbox(&mut settings.allow_upscale, "Allow upscaling")
                .on_hover_text(
                    "Disabled by default to avoid enlarging beyond the source dimensions.",
                );
        }

        if show_dimension_summary {
            if let Some((width, height)) = source_dimensions {
                match settings.checked_output_dimensions(width, height) {
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
        } else {
            ui.label(
                egui::RichText::new("Sizing is applied independently to each selected image.")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }
    });

    ui.add_space(6.0);
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.strong("Metadata");
        ui.checkbox(&mut settings.keep_metadata, "Keep metadata")
            .on_hover_text(
                "Embeds available camera, source-file, original-size, software, and orientation metadata in the exported image.",
            );
    });

    ui.add_space(6.0);
    ui.group(|ui| {
        ui.set_width(ui.available_width());
        ui.strong("JPEG");
        ui.add(egui::Slider::new(&mut settings.jpeg_quality, 1..=100).text("Quality"))
            .on_hover_text("Higher quality keeps more detail and produces a larger JPEG file.");
    });
}

impl Sidebar {
    fn show_export(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        // A vertical ScrollArea reports the scrollbar lane as available child
        // width. Constrain the complete Export tab to the same content column
        // used by the other Develop controls so the JPEG card and full-width
        // buttons do not extend underneath that lane.
        let content_width = (ui.available_width() - Self::SCROLLBAR_GUTTER).max(1.0);
        ui.set_width(content_width);
        ui.set_max_width(content_width);

        let source_dimensions = app.loaded_raw.as_ref().map(|raw| (raw.width, raw.height));
        export_settings_controls(ui, &mut app.export_settings, source_dimensions, true);

        if let Some((completed, total)) = app.export_progress_state() {
            ui.add_space(8.0);
            let (fraction, text) = if total == 0 {
                (0.0, "Preparing export…".to_owned())
            } else {
                (
                    (completed as f32 / total as f32).clamp(0.0, 1.0),
                    format!("Exporting — {completed}/{total} tiles"),
                )
            };
            ui.add(egui::ProgressBar::new(fraction).text(text));
            if let Some((done, batch_total)) = app.library_batch_export_progress() {
                ui.label(
                    egui::RichText::new(format!(
                        "Batch: {done}/{batch_total} images completed"
                    ))
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
            }
        }

        ui.add_space(10.0);
        let dimensions_valid = source_dimensions.is_some_and(|(width, height)| {
            app.export_settings
                .checked_output_dimensions(width, height)
                .is_ok()
        });
        let export_enabled = app.can_export() && dimensions_valid;
        let png_button =
            egui::Button::new("Export PNG…").min_size(egui::vec2(ui.available_width(), 30.0));
        if ui.add_enabled(export_enabled, png_button).clicked() {
            app.export_png(frame);
        }
        ui.add_space(4.0);
        let jpeg_button =
            egui::Button::new("Export JPEG…").min_size(egui::vec2(ui.available_width(), 30.0));
        if ui.add_enabled(export_enabled, jpeg_button).clicked() {
            app.export_jpeg(frame);
        }
        if !app.can_export() && app.export_progress_state().is_none() {
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
