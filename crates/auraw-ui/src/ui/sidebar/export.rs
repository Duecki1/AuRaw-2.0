pub(crate) fn export_settings_controls(
    ui: &mut Ui,
    settings: &mut crate::pipeline::ExportSettings,
    _fallback_picker_directory: Option<&std::path::Path>,
) {
    settings.resize_mode = ExportResizeMode::Original;

    crate::ui::theme::section_card_with_help(
        ui,
        "Precision",
        "Choose the channel precision written to the exported file. Higher precision preserves more editing latitude but produces larger files.",
        |ui| {
        crate::ui::theme::form_combo_with_help(
            ui,
            "Bit depth",
            "export-bit-depth",
            settings.bit_depth.label(),
            150.0,
            "8-bit is broadly compatible; 16-bit retains more tonal precision; 32-bit float writes a scene-linear master.",
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

    crate::ui::theme::card_gap(ui);
    crate::ui::theme::section_card_with_help(
        ui,
        "Color space",
        "Integer exports use sRGB. Float TIFF masters use linear Rec.2020.",
        |ui| {
        ui.label(if settings.bit_depth.is_float() {
            "Linear Rec.2020"
        } else {
            "sRGB"
        });
    });

    crate::ui::theme::card_gap(ui);
    crate::ui::theme::section_card(ui, "Metadata", |ui| {
        crate::ui::theme::checkbox_with_help(
            ui,
            &mut settings.keep_metadata,
            "Keep metadata",
            "Embeds available source, camera, lens, exposure, creator, original-size, software, and normalized-orientation metadata in the exported image.",
        );
    });

    crate::ui::theme::card_gap(ui);
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
        let content_width = ui.available_width().max(1.0);
        let column_width = content_width;

        #[cfg(not(target_os = "android"))]
        let export_picker_directory = app.develop.current_path
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
                ui.set_min_width(column_width);
                ui.set_max_width(column_width);
                export_settings_controls(
                    ui,
                    &mut app.export.settings,
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
                let dimensions_valid = app.develop.loaded_raw.as_ref().is_some_and(|raw| {
                    let (width, height) = app
                        .develop
                        .geometry
                        .crop_pixel_dimensions(raw.width, raw.height);
                    app.export.settings
                        .checked_output_dimensions(width, height)
                        .is_ok()
                });
                let export_enabled = app.can_export() && dimensions_valid;
                let png_enabled = export_enabled
                    && app.export.settings.bit_depth != ExportBitDepth::Float32Linear;
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
                    .add_enabled_ui(export_enabled, |ui| {
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
                    && app.export.settings.bit_depth != ExportBitDepth::Float32Linear;
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
                if app.export_task_active() {
                    ui.label(
                        egui::RichText::new(
                            "An export is already running. Minimize its progress window to keep editing.",
                        )
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                } else if !app.can_export() && app.export_progress_state().is_none() {
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
