use crate::app::{maximum_raw_cache_limit, AurawApp, PreviewQuality};
use crate::ui::library::maximum_thumbnail_worker_count;
use crate::pipeline::HighlightReconstructionMethod;
use crate::ui::components::adjustment_slider::adjustment_slider;
use crate::ui::layout::ScreenLayout;
use eframe::egui::{self, ComboBox, Ui};

pub struct Settings;

impl Settings {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, layout: ScreenLayout) {
        let content_width = match layout {
            ScreenLayout::Horizontal => ui.available_width().min(720.0),
            ScreenLayout::Vertical => ui.available_width(),
        }
        .max(1.0);
        ui.set_width(content_width);
        ui.set_max_width(content_width);
        if layout == ScreenLayout::Vertical {
            ui.spacing_mut().item_spacing = egui::vec2(7.0, 6.0);
        }

        ui.heading("Settings");
        ui.add_space(4.0);

        Self::group(ui, content_width, |ui| {
            ui.heading("Interface");
            ui.checkbox(&mut app.expert_mode, "Expert mode")
                .on_hover_text(
                    "Show detailed creative-effect tuning, darktable-style rendering internals, and RAW reconstruction controls. Disabled by default.",
                );
            ui.add(
                egui::Label::new(
                    "The standard Develop view keeps only Lightroom-style photographic controls visible.",
                )
                .wrap(),
            );

            ui.separator();
            let previous_quality = app.preview_quality;
            ComboBox::from_label("Preview quality")
                .selected_text(app.preview_quality.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut app.preview_quality, PreviewQuality::Fast, "Fast");
                    ui.selectable_value(
                        &mut app.preview_quality,
                        PreviewQuality::Balanced,
                        "Balanced",
                    );
                    ui.selectable_value(&mut app.preview_quality, PreviewQuality::High, "High");
                });
            if app.preview_quality != previous_quality {
                app.preview_quality_changed();
            }
            ui.add(
                egui::Label::new(
                    "Controls the normal proxy and the detailed visible-region render created about one second after zooming stops.",
                )
                .wrap(),
            );

            ui.separator();
            ui.strong("Library performance");
            let mut raw_cache_files = app.raw_cache_limit();
            if ui
                .add(
                    egui::Slider::new(
                        &mut raw_cache_files,
                        0..=maximum_raw_cache_limit(),
                    )
                    .integer()
                    .text("Decoded RAW cache"),
                )
                .on_hover_text(
                    "Number of fully decoded RAW files kept in memory for faster switching. Zero disables the cache. The default is 2 on desktop and 1 on Android.",
                )
                .changed()
            {
                app.set_raw_cache_limit(raw_cache_files);
            }
            let raw_cache_description = if raw_cache_files == 0 {
                "Decoded RAW reuse is disabled; only the image currently being edited stays loaded."
                    .to_owned()
            } else {
                format!(
                    "Keeps up to {raw_cache_files} decoded RAW {} in memory, including the current image, for faster switching back to recent files.",
                    if raw_cache_files == 1 { "file" } else { "files" }
                )
            };
            ui.add(egui::Label::new(raw_cache_description).wrap());

            let mut thumbnail_workers = app.thumbnail_worker_count();
            if ui
                .add(
                    egui::Slider::new(
                        &mut thumbnail_workers,
                        1..=maximum_thumbnail_worker_count(),
                    )
                    .integer()
                    .text("Thumbnail workers"),
                )
                .on_hover_text(
                    "Parallel background thumbnail decoders. More workers fill the library faster but use more CPU and memory. Full RAW loading remains exclusive.",
                )
                .changed()
            {
                app.set_thumbnail_worker_count(thumbnail_workers);
            }
            ui.add(
                egui::Label::new(
                    "Changing this restarts the current library thumbnail queue. The setting is saved across restarts.",
                )
                .wrap(),
            );
        });

        #[cfg(not(target_os = "android"))]
        {
            ui.add_space(8.0);
            Self::group(ui, content_width, |ui| {
                ui.heading("Subject selection runtime");
                ui.add(
                    egui::Label::new(
                        "Choose a trusted ONNX Runtime 1.18 or newer shared library built for your hardware. AuRaw never downloads or dynamically loads a native runtime without this explicit selection. GPU provider libraries and their dependencies must remain beside it.",
                    )
                    .wrap(),
                );
                ui.add_space(4.0);
                if let Some(path) = &app.onnx_runtime_path {
                    ui.label("Selected runtime:");
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(path.display().to_string()).monospace(),
                        )
                        .wrap(),
                    );
                    if let Some(sha256) = &app.onnx_runtime_sha256 {
                        ui.small("Pinned SHA-256:");
                        ui.add(egui::Label::new(egui::RichText::new(sha256).monospace()).wrap());
                    }
                } else {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "No runtime selected. Subject and Not Subject masks cannot run yet.",
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Choose ONNX Runtime…").clicked() {
                        app.choose_onnx_runtime();
                    }
                    if ui
                        .add_enabled(
                            app.onnx_runtime_path.is_some(),
                            eframe::egui::Button::new("Clear"),
                        )
                        .clicked()
                    {
                        app.clear_onnx_runtime();
                    }
                });
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "Restart AuRaw after changing this library. The runtime is loaded once per process.",
                        )
                        .small(),
                    )
                    .wrap(),
                );
            });
        }

        ui.add_space(8.0);
        Self::group(ui, content_width, |ui| {
            ui.heading("Diagnostics");
            ui.add(
                egui::Label::new(
                    "Open the same RAW and run an export on each device, then use Copy log. The report includes Android, GPU/backend, RAW calibration, fingerprints, and timing information.",
                )
                .wrap(),
            );
            ui.add_space(4.0);

            let mut diagnostic_log = crate::diagnostics::snapshot();
            ui.horizontal_wrapped(|ui| {
                if ui.button("Copy log").clicked() {
                    #[cfg(target_os = "android")]
                    match app.copy_text_to_clipboard("AuRaw diagnostics", &diagnostic_log) {
                        Ok(()) => {
                            crate::diagnostics::record("Diagnostic log copied to Android clipboard")
                        }
                        Err(error) => crate::diagnostics::record(format!(
                            "Android clipboard copy failed: {error}"
                        )),
                    }
                    #[cfg(not(target_os = "android"))]
                    ui.ctx().copy_text(diagnostic_log.clone());
                }
                if ui.button("Clear events").clicked() {
                    crate::diagnostics::clear();
                    diagnostic_log = crate::diagnostics::snapshot();
                }
            });

            let rows = match layout {
                ScreenLayout::Horizontal => 16,
                ScreenLayout::Vertical => 12,
            };
            let mut diagnostic_view = diagnostic_log.as_str();
            ui.add(
                egui::TextEdit::multiline(&mut diagnostic_view)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(rows)
                    .desired_width(f32::INFINITY),
            );
        });

        if !app.expert_mode {
            return;
        }

        ui.add_space(8.0);
        let mut changed = false;

        Self::group(ui, content_width, |ui| {
            ui.heading("Highlight reconstruction");
            ui.add(
                egui::Label::new(
                    "Reconstruct clipped sensor channels before demosaicing to avoid pink or grey highlights.",
                )
                .wrap(),
            );
            ui.add_space(4.0);

            ComboBox::from_label("Method")
                .selected_text(match app.exposure.highlight_method {
                    HighlightReconstructionMethod::Off => "Off",
                    HighlightReconstructionMethod::Lch => "LCh (fast)",
                    HighlightReconstructionMethod::Guided => "Guided (best quality)",
                })
                .show_ui(ui, |ui| {
                    changed |= ui
                        .selectable_value(
                            &mut app.exposure.highlight_method,
                            HighlightReconstructionMethod::Off,
                            "Off",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut app.exposure.highlight_method,
                            HighlightReconstructionMethod::Lch,
                            "LCh (fast)",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut app.exposure.highlight_method,
                            HighlightReconstructionMethod::Guided,
                            "Guided (best quality)",
                        )
                        .changed();
                });

            let enabled = app.exposure.highlight_method != HighlightReconstructionMethod::Off;
            ui.add_enabled_ui(enabled, |ui| {
                changed |= adjustment_slider(
                    ui,
                    "Clip threshold",
                    &mut app.exposure.highlight_clip,
                    0.5..=2.0,
                    2,
                    0.01,
                    Some("Sensor-relative level at which a channel is treated as clipped."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Reconstruction strength",
                    &mut app.exposure.highlight_reconstruction,
                    0.0..=1.0,
                    2,
                    0.01,
                    Some("Blend between the original and reconstructed highlight."),
                );

                if app.exposure.highlight_method == HighlightReconstructionMethod::Guided {
                    ui.separator();
                    ui.strong("Guided reconstruction");
                    changed |= adjustment_slider(
                        ui,
                        "Iterations",
                        &mut app.exposure.highlight_iterations,
                        1..=4,
                        0,
                        1.0,
                        Some(
                            "More iterations propagate surrounding color farther into clipped regions.",
                        ),
                    );
                    changed |= adjustment_slider(
                        ui,
                        "Color adaptation",
                        &mut app.exposure.highlight_color_adaptation,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Controls how strongly reconstructed highlights follow nearby color."),
                    );
                }
            });

            ui.add_space(4.0);
            if ui.button("Restore reconstruction defaults").clicked() {
                app.reset_highlight_reconstruction_settings();
            }
        });

        if changed {
            app.mark_pipeline_dirty();
        }
    }

    fn group(ui: &mut Ui, total_width: f32, contents: impl FnOnce(&mut Ui)) {
        let inner_width = (total_width - 16.0).max(1.0);
        ui.group(|ui| {
            ui.set_width(inner_width);
            ui.set_max_width(inner_width);
            contents(ui);
        });
    }
}
