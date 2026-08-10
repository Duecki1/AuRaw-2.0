pub(crate) fn export_settings_controls(
    ui: &mut Ui,
    settings: &mut crate::pipeline::ExportSettings,
    source_dimensions: Option<(u32, u32)>,
    show_dimension_summary: bool,
    fallback_picker_directory: Option<&std::path::Path>,
) {
    #[cfg(target_os = "android")]
    let _ = fallback_picker_directory;
    crate::ui::theme::section_card(ui, "Image sizing", |ui| {
        crate::ui::theme::form_combo(
            ui,
            "Resize to fit",
            "export-resize-mode",
            settings.resize_mode.label(),
            150.0,
            |ui| {
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
            },
        );

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

    ui.add_space(crate::ui::theme::CARD_GAP);
    crate::ui::theme::section_card(ui, "Precision", |ui| {
        crate::ui::theme::form_combo(
            ui,
            "Bit depth",
            "export-bit-depth",
            settings.bit_depth.label(),
            150.0,
            |ui| {
                for depth in [
                    ExportBitDepth::Eight,
                    ExportBitDepth::Sixteen,
                    ExportBitDepth::Float32Linear,
                ] {
                    ui.selectable_value(&mut settings.bit_depth, depth, depth.label());
                }
            },
        );
    });

    ui.add_space(crate::ui::theme::CARD_GAP);
    crate::ui::theme::section_card(ui, "Color profile", |ui| {
        crate::ui::theme::form_combo(
            ui,
            "Output profile",
            "export-output-profile",
            if settings.bit_depth.is_float() {
                "Linear Rec.2020"
            } else {
                settings.color_profile.label()
            },
            150.0,
            |ui| {
                ui.add_enabled_ui(!settings.bit_depth.is_float(), |ui| {
                    ui.selectable_value(
                        &mut settings.color_profile,
                        ExportColorProfile::Srgb,
                        "sRGB",
                    );
                    #[cfg(not(target_os = "android"))]
                    ui.selectable_value(
                        &mut settings.color_profile,
                        ExportColorProfile::CustomIcc,
                        "Custom ICC",
                    );
                });
            },
        );

        if settings.bit_depth.is_float() {
            ui.label(
                egui::RichText::new("Float masters embed a linear Rec.2020 ICC profile.")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        } else if settings.color_profile == ExportColorProfile::CustomIcc {
            #[cfg(not(target_os = "android"))]
            {
                if ui.button("Choose ICC profile…").clicked() {
                    let mut dialog =
                        rfd::FileDialog::new().add_filter("ICC profiles", &["icc", "icm"]);
                    let selected_directory = settings
                        .custom_icc_path
                        .as_deref()
                        .and_then(|path| path.parent())
                        .filter(|parent| !parent.as_os_str().is_empty())
                        .or(fallback_picker_directory);
                    if let Some(directory) = selected_directory {
                        dialog = dialog.set_directory(directory);
                    }
                    if let Some(path) = dialog.pick_file() {
                        settings.custom_icc_path = Some(path);
                    }
                }
            }
            if let Some(path) = settings.custom_icc_path.as_deref() {
                ui.label(
                    egui::RichText::new(
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Selected ICC profile"),
                    )
                    .small(),
                );
            } else {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Choose an ICC profile before export.",
                );
            }
        }
    });

    ui.add_space(crate::ui::theme::CARD_GAP);
    crate::ui::theme::section_card(ui, "Metadata", |ui| {
        ui.checkbox(&mut settings.keep_metadata, "Keep metadata")
            .on_hover_text(
                "Embeds available source, camera, lens, exposure, creator, original-size, software, and normalized-orientation metadata in the exported image.",
            );
    });

    ui.add_space(crate::ui::theme::CARD_GAP);
    crate::ui::theme::section_card(ui, "JPEG", |ui| {
        adjustment_slider(
            ui,
            "Quality",
            &mut settings.jpeg_quality,
            1..=100,
            0,
            1.0,
            Some("Higher quality keeps more detail and produces a larger JPEG file."),
        );
    });
}

impl Sidebar {
    fn show_export(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        // Sidebar::show already reserves the scrollbar gutter. Use the actual
        // remaining width directly so this column follows sidebar resizing
        // without another inset or a fixed pixel width.
        let content_width = ui.available_width().max(1.0);
        let column_width = content_width;

        // Export sizing is defined after non-destructive crop/orientation.
        // Using the full RAW dimensions here made the sidebar disagree with the
        // exporter (and could label a crop as a resize). Keep UI validation and
        // displayed dimensions on the same geometry contract as export.rs.
        let source_dimensions = app
            .loaded_raw
            .as_ref()
            .map(|raw| app.geometry.crop_pixel_dimensions(raw.width, raw.height));
        #[cfg(not(target_os = "android"))]
        let export_picker_directory = app
            .current_path
            .as_deref()
            .and_then(|path| path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf);
        #[cfg(target_os = "android")]
        let export_picker_directory: Option<std::path::PathBuf> = None;
        ui.allocate_ui_with_layout(
            egui::vec2(column_width, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                // This child UI is the single source of truth for horizontal
                // sizing. Cards, progress, and actions all receive precisely
                // the same available width.
                ui.set_min_width(column_width);
                ui.set_max_width(column_width);
                export_settings_controls(
                    ui,
                    &mut app.export_settings,
                    source_dimensions,
                    true,
                    export_picker_directory.as_deref(),
                );

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
                    ui.add_sized(
                        [ui.available_width(), 18.0],
                        egui::ProgressBar::new(fraction).text(text),
                    );
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
                let profile_ready = app.export_settings.color_profile
                    != ExportColorProfile::CustomIcc
                    || app.export_settings.custom_icc_path.is_some()
                    || app.export_settings.bit_depth.is_float();
                let png_enabled = export_enabled
                    && profile_ready
                    && app.export_settings.bit_depth != ExportBitDepth::Float32Linear;
                let action_width = ui.available_width();
                let png_response = ui
                    .add_enabled_ui(png_enabled, |ui| {
                        ui.add_sized(
                            [action_width, crate::ui::theme::CONTROL_HEIGHT],
                            egui::Button::new("Export PNG…"),
                        )
                    })
                    .inner;
                if png_response.clicked() {
                    app.export_png(frame);
                }
                ui.add_space(4.0);
                let tiff_response = ui
                    .add_enabled_ui(export_enabled && profile_ready, |ui| {
                        ui.add_sized(
                            [action_width, crate::ui::theme::CONTROL_HEIGHT],
                            egui::Button::new("Export TIFF…"),
                        )
                    })
                    .inner;
                if tiff_response.clicked() {
                    app.export_tiff(frame);
                }
                ui.add_space(4.0);
                let jpeg_enabled = export_enabled
                    && profile_ready
                    && app.export_settings.bit_depth != ExportBitDepth::Float32Linear;
                let jpeg_response = ui
                    .add_enabled_ui(jpeg_enabled, |ui| {
                        ui.add_sized(
                            [action_width, crate::ui::theme::CONTROL_HEIGHT],
                            egui::Button::new("Export JPEG…"),
                        )
                    })
                    .inner;
                if jpeg_response.clicked() {
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
            },
        );
    }
}
