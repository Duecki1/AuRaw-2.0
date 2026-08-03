use crate::app::{maximum_raw_cache_limit, AurawApp, PreviewQuality};
use crate::pipeline::{CameraProfileMode, HighlightReconstructionMethod};
use crate::ui::components::adjustment_slider::adjustment_slider;
use crate::ui::layout::ScreenLayout;
use crate::ui::library::maximum_thumbnail_worker_count;
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

        #[cfg(target_os = "android")]
        {
            if crate::ui::top_bar::TopBar::back_icon_button(ui, egui::vec2(42.0, 36.0)).clicked() {
                app.activate_tab(crate::app::AppTab::Library);
            }
            ui.add_space(4.0);
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
                    ui.selectable_value(&mut app.preview_quality, PreviewQuality::Low, "Low");
                    ui.selectable_value(&mut app.preview_quality, PreviewQuality::Medium, "Medium");
                    ui.selectable_value(&mut app.preview_quality, PreviewQuality::High, "High");
                    ui.selectable_value(&mut app.preview_quality, PreviewQuality::Max, "Max");
                });
            if app.preview_quality != previous_quality {
                app.preview_quality_changed();
            }
            ui.add(
                egui::Label::new(
                    "All levels follow the preview's physical screen size: Low 50%, Medium 67%, High 84%, and Max one rendered pixel per display pixel. Zoom detail keeps the same density.",
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
                    "Concurrent background thumbnail jobs, including embedded previews, preview-less RAW fallback renders, and edited-thumbnail rebuilding.",
                )
                .changed()
            {
                app.set_thumbnail_worker_count(thumbnail_workers);
            }
            ui.add(
                egui::Label::new(
                    "Higher values fill the library faster, but preview-less and edited jobs may unpack a full sensor and use substantial memory. Changing this restarts the current queue; the setting is saved across restarts.",
                )
                .wrap(),
            );

            ui.separator();
            ui.strong("Thumbnail cache");
            ui.add(
                egui::Label::new(
                    "Delete generated library previews and rebuild them from the RAW files and saved edits.",
                )
                .wrap(),
            );
            let cache_size = app.thumbnail_cache_size_label();
            ui.horizontal(|ui| {
                if ui.button("Clear thumbnail cache").clicked() {
                    app.clear_thumbnail_cache();
                }
                ui.label(
                    egui::RichText::new(cache_size)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            });
        });

        #[cfg(not(target_os = "android"))]
        {
            ui.add_space(8.0);
            Self::group(ui, content_width, |ui| {
                ui.heading("Display color management");
                ui.add(
                    egui::Label::new(
                        "Use the active monitor ICC profile for the live preview. Matrix and LUT/CLUT profiles are converted once through LCMS2 into the GPU display LUT.",
                    )
                    .wrap(),
                );
                ui.add_space(4.0);

                let mut enabled = app.display_color_management;
                if ui
                    .checkbox(&mut enabled, "Use monitor color profile")
                    .on_hover_text(
                        "Automatically follows the display containing the app window. Disable to render the preview as plain sRGB.",
                    )
                    .changed()
                {
                    app.set_display_color_management(enabled);
                }

                ui.separator();
                ui.strong("Active display profile");
                ui.add(
                    egui::Label::new(egui::RichText::new(&app.display_profile_label).strong())
                        .wrap(),
                );
                if let Some(source) = app.display_profile_source() {
                    ui.add(egui::Label::new(egui::RichText::new(source).monospace()).wrap());
                } else if app.display_color_management {
                    ui.small("No OS monitor profile was found; sRGB is used as the safe fallback.");
                }

                ui.separator();
                ui.strong("Manual override");
                if let Some(path) = app.display_profile_override.as_deref() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(path.display().to_string()).monospace(),
                        )
                        .wrap(),
                    );
                } else {
                    ui.small("Automatic per-monitor discovery is enabled.");
                    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
                    ui.small(
                        "Linux automatic discovery uses X11 _ICC_PROFILE properties; Wayland-only sessions can use a manual ICC override.",
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Choose ICC…").clicked() {
                        app.choose_display_profile_override();
                    }
                    if ui
                        .add_enabled(
                            app.display_profile_override.is_some(),
                            egui::Button::new("Use automatic"),
                        )
                        .clicked()
                    {
                        app.clear_display_profile_override();
                    }
                });
                ui.small(
                    "The profile is rebuilt only when the selected monitor/profile changes; normal rendering is a single 3D-LUT lookup.",
                );
            });
        }

        ui.add_space(8.0);
        Self::group(ui, content_width, |ui| {
            ui.heading("Copy & paste adjustments");
            ui.add(
                egui::Label::new(
                    "Choose which edit categories Library > Copy Adjustments stores for later pasting. The selection is remembered across restarts.",
                )
                .wrap(),
            );
            ui.add_space(4.0);

            let mut settings = app.adjustment_copy_settings;
            let mut changed = false;
            changed |= ui
                .checkbox(&mut settings.adjustments, "Adjustments")
                .on_hover_text("Global light, color, white-balance temperature/tint, tone curve, effects, color mixer, and RAW adjustment values.")
                .changed();
            changed |= ui
                .checkbox(&mut settings.geometry, "Geometry")
                .on_hover_text("Crop, rotation, straighten, perspective, flips, and geometry transforms. Disabled by default.")
                .changed();
            changed |= ui
                .checkbox(&mut settings.camera_profile, "Camera profile")
                .on_hover_text("The per-image camera/DCP profile selection. Enabled by default.")
                .changed();
            changed |= ui
                .checkbox(&mut settings.masks, "Normal masks")
                .on_hover_text("Brush, radial-gradient, and linear-gradient mask components, including their local adjustments. Mixed mask groups are split so disabling this never copies their manual components.")
                .changed();
            changed |= ui
                .checkbox(&mut settings.ai_masks, "AI masks")
                .on_hover_text("Subject, background, object, landscape, luminance-range, and color-range components. Generated/source-dependent results are marked for regeneration on the destination image.")
                .changed();
            changed |= ui
                .checkbox(&mut settings.inpainting, "Inpainting")
                .on_hover_text("Inpainting strokes and generated patch data.")
                .changed();
            changed |= ui
                .checkbox(&mut settings.lens_correction, "Lens correction")
                .on_hover_text("Lens correction enabled state and selected lens profile.")
                .changed();
            if changed {
                app.set_adjustment_copy_settings(settings);
            }
        });

        ui.add_space(8.0);
        Self::group(ui, content_width, |ui| {
            ui.heading("RAW color profiles");
            ui.add(
                    egui::Label::new(
                        "Choose how AuRaw builds the camera-to-working color transform and whether it applies DCP color rendering stages."
                    )
                    .wrap(),
                );
            ui.add_space(4.0);

            let previous_mode = app.camera_profile_mode;
            let mut mode = previous_mode;
            ComboBox::from_label("Profile mode")
                .selected_text(mode.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut mode, CameraProfileMode::Automatic, "Automatic");
                    ui.selectable_value(
                        &mut mode,
                        CameraProfileMode::DcpProfiles,
                        "Use DCP profiles",
                    );
                    ui.selectable_value(
                        &mut mode,
                        CameraProfileMode::MatrixOnly,
                        "Embedded matrix only",
                    );
                });
            if mode != previous_mode {
                app.set_camera_profile_mode(mode);
            }

            let description = match app.camera_profile_mode {
                    CameraProfileMode::Automatic => {
                        "Automatic uses the RAW's embedded camera matrix by default. A DCP chosen for an individual image remains explicit."
                    }
                    CameraProfileMode::DcpProfiles => {
                        "Use DCP profiles searches the folder below for this camera. If no safe match exists, AuRaw falls back to the camera matrix."
                    }
                    CameraProfileMode::MatrixOnly => {
                        "Embedded matrix only ignores DCP look tables, hue/saturation maps, and profile tone curves and uses the camera/DNG/LibRaw matrix path."
                    }
                };
            ui.add(egui::Label::new(description).wrap());

            ui.separator();
            ui.strong("Camera profile folder");
            #[cfg(target_os = "android")]
            if let Some(label) = &app.camera_profile_folder_importing_label {
                ui.add(
                    egui::Label::new(egui::RichText::new(format!("Importing {label}…")).strong())
                        .wrap(),
                );
                ui.small("Copying .dcp files into AuRaw's persistent private storage. Large Adobe CameraProfiles trees can take a moment.");
            } else if let Some(path) = &app.camera_profile_folder {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            app.camera_profile_folder_label
                                .as_deref()
                                .unwrap_or("CameraProfiles"),
                        )
                        .strong(),
                    )
                    .wrap(),
                );
                ui.small("Android keeps a private persistent copy of the selected .dcp files so profiles remain available after restart.");
                let _ = path;
            } else {
                ui.small("No external DCP folder selected.");
            }
            #[cfg(not(target_os = "android"))]
            if let Some(path) = &app.camera_profile_folder {
                #[cfg(not(target_os = "android"))]
                {
                    if let Some(label) = &app.camera_profile_folder_label {
                        ui.small(label);
                    }
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(path.display().to_string()).monospace(),
                        )
                        .wrap(),
                    );
                }
            } else {
                ui.small("No external DCP folder selected.");
            }
            ui.horizontal_wrapped(|ui| {
                #[cfg(target_os = "android")]
                let choose_label = if app.camera_profile_folder_importing_label.is_some() {
                    "Importing…"
                } else {
                    "Choose folder…"
                };
                #[cfg(not(target_os = "android"))]
                let choose_label = "Choose folder…";

                #[cfg(target_os = "android")]
                let choose_enabled = !app.picker_pending;
                #[cfg(not(target_os = "android"))]
                let choose_enabled = true;

                if ui
                    .add_enabled(choose_enabled, egui::Button::new(choose_label))
                    .clicked()
                {
                    app.choose_camera_profile_folder();
                }
                #[cfg(not(target_os = "android"))]
                if ui.button("Auto-detect Adobe").clicked() {
                    app.auto_detect_camera_profile_folder();
                }
                let can_clear =
                    app.camera_profile_folder.is_some() || app.camera_profile_auto_detect;
                #[cfg(target_os = "android")]
                let can_clear = can_clear && !app.picker_pending;
                if ui
                    .add_enabled(can_clear, egui::Button::new("Clear"))
                    .clicked()
                {
                    app.clear_camera_profile_folder();
                }
            });
            #[cfg(not(target_os = "android"))]
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "Choose the top-level profile root (for example CameraProfiles). AuRaw searches every subfolder recursively. Auto-detect checks Adobe Camera Raw's standard CameraProfiles install location on Windows and macOS."
                        )
                        .small(),
                    )
                    .wrap(),
                );
            #[cfg(target_os = "android")]
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "Choose the top-level CameraProfiles folder with Android's system folder picker. AuRaw recursively imports only .dcp files, then groups all matches by camera and exposes multiple profiles in Develop."
                        )
                        .small(),
                    )
                    .wrap(),
                );
        });

        #[cfg(not(target_os = "android"))]
        {
            ui.add_space(8.0);
            Self::group(ui, content_width, |ui| {
                ui.heading("Subject selection runtime");
                let runtime_help = if cfg!(target_os = "windows") {
                    "Choose a trusted ONNX Runtime 1.18 or newer onnxruntime.dll that matches this AuRaw build's CPU architecture. AuRaw validates the DLL in an isolated helper process before AI tools use it. Windows AI masks currently use the core CPU execution provider for stability with user-selected runtimes."
                } else {
                    "Choose a trusted ONNX Runtime 1.18 or newer shared library built for your hardware. AuRaw never downloads or dynamically loads a native runtime without this explicit selection. GPU provider libraries and their dependencies must remain beside it."
                };
                ui.add(egui::Label::new(runtime_help).wrap());
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
                    HighlightReconstructionMethod::InpaintOpposed => "Inpaint opposed",
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
                            HighlightReconstructionMethod::InpaintOpposed,
                            "Inpaint opposed",
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
                if app.exposure.highlight_method == HighlightReconstructionMethod::Lch {
                    changed |= adjustment_slider(
                        ui,
                        "Reconstruction strength",
                        &mut app.exposure.highlight_reconstruction,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Blend between the original and LCh-reconstructed highlight."),
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
