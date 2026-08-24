use crate::app::{maximum_raw_cache_limit, AurawApp, PreviewQuality};
use crate::pipeline::{CameraProfileMode, HighlightReconstructionMethod};
use crate::ui::components::adjustment_slider::adjustment_slider;
use crate::ui::layout::ScreenLayout;
use crate::ui::library::maximum_thumbnail_worker_count;
use eframe::egui::{self, Ui};

pub struct Settings;

fn diagnostics_snapshot_with_ai_backends() -> String {
    let mut diagnostic_log = crate::diagnostics::snapshot();
    let providers = auraw_ai::active_execution_providers();
    if providers.is_empty() {
        return diagnostic_log;
    }

    if !diagnostic_log.ends_with('\n') && !diagnostic_log.is_empty() {
        diagnostic_log.push('\n');
    }
    diagnostic_log.push_str("AI execution providers (current):\n");
    for provider in providers {
        diagnostic_log.push_str("- ");
        diagnostic_log.push_str(&provider.model_name);
        diagnostic_log.push_str(": ");
        diagnostic_log.push_str(&provider.active_provider);
        if provider.degraded {
            diagnostic_log.push_str(" [degraded/fallback]");
        }
        diagnostic_log.push('\n');
    }
    diagnostic_log
}

impl Settings {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, layout: ScreenLayout) {
        let available_width = ui.available_width().max(1.0);
        let content_width = match layout {
            ScreenLayout::Horizontal => available_width.min(760.0),
            ScreenLayout::Vertical => available_width,
        }
        .max(1.0);
        let left_margin = ((available_width - content_width) * 0.5).max(0.0);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.add_space(left_margin);
            ui.vertical(|ui| {
                ui.set_width(content_width);
                ui.set_max_width(content_width);
                Self::show_content(ui, app, layout, content_width);
            });
        });
    }

    fn show_content(ui: &mut Ui, app: &mut AurawApp, layout: ScreenLayout, content_width: f32) {
        if layout == ScreenLayout::Vertical {
            ui.spacing_mut().item_spacing = egui::vec2(7.0, 6.0);
        }

        #[cfg(target_os = "android")]
        {
            if crate::ui::top_bar::TopBar::back_icon_button(
                ui,
                crate::ui::theme::toolbar_icon_size(),
            )
            .clicked()
            {
                app.activate_tab(crate::app::AppTab::Library);
            }
            ui.add_space(4.0);
        }

        ui.heading("Settings");
        ui.add_space(4.0);

        Self::group(ui, content_width, |ui| {
            ui.heading("Interface");
            ui.checkbox(&mut app.ui.expert_mode, "Expert mode")
                .on_hover_text(
                    "Show detailed creative-effect tuning, darktable-style rendering internals, and RAW reconstruction controls. Disabled by default.",
                );
            ui.add(
                egui::Label::new(
                    "The standard Develop view keeps only photographic controls visible.",
                )
                .wrap(),
            );

            ui.separator();
            let previous_quality = app.preview.quality;
            crate::ui::theme::form_combo(
                ui,
                "Preview quality",
                "settings-preview-quality",
                app.preview.quality.label(),
                160.0,
                |ui| {
                    ui.selectable_value(&mut app.preview.quality, PreviewQuality::Low, "Low");
                    ui.selectable_value(&mut app.preview.quality, PreviewQuality::Medium, "Medium");
                    ui.selectable_value(&mut app.preview.quality, PreviewQuality::High, "High");
                    ui.selectable_value(&mut app.preview.quality, PreviewQuality::Max, "Max");
                },
            );
            if app.preview.quality != previous_quality {
                app.preview_quality_changed();
            }
            ui.add(
                egui::Label::new(
                    "All levels follow the preview's physical screen size: Low 75%, Medium 100% (native density), High 125%, and Max 150%. Zoom detail uses the same density.",
                )
                .wrap(),
            );

            ui.separator();
            if ui
                .checkbox(
                    &mut app.preferences.image_relative_brush_size,
                    "Keep brush size fixed to the image",
                )
                .on_hover_text(
                    "When enabled, zooming changes the brush's on-screen size while it continues to cover the same area of the image. When disabled, the brush stays the same size on screen.",
                )
                .changed()
            {
                app.persist_performance_settings();
            }
            ui.add(
                egui::Label::new(if app.preferences.image_relative_brush_size {
                    "Brushes keep the same image footprint at every zoom level."
                } else {
                    "Brushes keep the same on-screen footprint at every zoom level."
                })
                .wrap(),
            );

            ui.separator();
            ui.strong("Library performance");
            #[cfg(not(target_os = "android"))]
            {
                let mut render_edited_thumbnails =
                    app.library.renders_edited_thumbnails_during_indexing();
                if ui
                    .checkbox(
                        &mut render_edited_thumbnails,
                        "Render edited thumbnails while indexing",
                    )
                    .on_hover_text(
                        "Apply saved edits to library thumbnails during indexing. Disabled by default to keep indexing fast and memory use low.",
                    )
                    .changed()
                {
                    app.set_render_edited_thumbnails_during_indexing(render_edited_thumbnails);
                }
                ui.add(
                    egui::Label::new(if render_edited_thumbnails {
                        "Edited RAWs are rendered in the background after their original previews appear."
                    } else {
                        "All RAWs use their original previews. An orange refresh badge marks previews that do not include saved edits."
                    })
                    .wrap(),
                );
            }

            let mut raw_cache_files = app.develop.raw_cache_limit;
            if adjustment_slider(
                ui,
                "Decoded RAW cache",
                &mut raw_cache_files,
                0..=maximum_raw_cache_limit(),
                0,
                1.0,
                Some(
                    "Number of fully decoded RAW files kept in memory for faster switching. Zero disables the cache. The default is 2 on desktop and 1 on Android.",
                ),
            ) {
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
            if adjustment_slider(
                ui,
                "Thumbnail workers",
                &mut thumbnail_workers,
                1..=maximum_thumbnail_worker_count(),
                0,
                1.0,
                Some(
                    "Concurrent background thumbnail jobs, including embedded previews, preview-less RAW fallback renders, and edited-thumbnail rendering when enabled above.",
                ),
            ) {
                app.set_thumbnail_worker_count(thumbnail_workers);
            }
            ui.add(
                egui::Label::new(
                    "Higher values fill the library faster, but preview-less jobs—and edited jobs when enabled—may unpack a full sensor and use substantial memory. Changing this restarts the current queue; the setting is saved across restarts.",
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
            crate::ui::theme::action_row(ui, |ui| {
                if ui.button("Clear thumbnail cache").clicked() {
                    app.clear_thumbnail_cache();
                }
                ui.label(egui::RichText::new(cache_size).color(ui.visuals().weak_text_color()));
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

                let mut enabled = app.preferences.display_color_management;
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
                    egui::Label::new(
                        egui::RichText::new(&app.preferences.display_profile_label).strong(),
                    )
                    .wrap(),
                );
                if let Some(source) = app.preferences.display_profile_source.as_deref() {
                    ui.add(egui::Label::new(egui::RichText::new(source).monospace()).wrap());
                } else if app.preferences.display_color_management {
                    ui.small("No OS monitor profile was found; sRGB is used as the safe fallback.");
                }

                ui.separator();
                ui.strong("Manual override");
                if let Some(path) = app.preferences.display_profile_override.as_deref() {
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
                crate::ui::theme::action_row(ui, |ui| {
                    if ui.button("Choose ICC…").clicked() {
                        app.choose_display_profile_override();
                    }
                    if ui
                        .add_enabled(
                            app.preferences.display_profile_override.is_some(),
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

            let mut settings = app.preferences.adjustment_copy_settings;
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

            let previous_mode = app.preferences.camera_profile_mode;
            let mut mode = previous_mode;
            crate::ui::theme::form_combo(
                ui,
                "Profile mode",
                "settings-camera-profile-mode",
                mode.label(),
                190.0,
                |ui| {
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
                },
            );
            if mode != previous_mode {
                app.set_camera_profile_mode(mode);
            }

            let description = match app.preferences.camera_profile_mode {
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
            if let Some(label) = &app.android.camera_profile_folder_importing_label {
                ui.add(
                    egui::Label::new(egui::RichText::new(format!("Importing {label}…")).strong())
                        .wrap(),
                );
                ui.small("Copying .dcp files into AuRaw's persistent private storage. Large Adobe CameraProfiles trees can take a moment.");
            } else if let Some(path) = &app.preferences.camera_profile_folder {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            app.preferences
                                .camera_profile_folder_label
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
            if let Some(path) = &app.preferences.camera_profile_folder {
                #[cfg(not(target_os = "android"))]
                {
                    if let Some(label) = &app.preferences.camera_profile_folder_label {
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
            crate::ui::theme::action_row(ui, |ui| {
                #[cfg(target_os = "android")]
                let choose_label = if app.android.camera_profile_folder_importing_label.is_some() {
                    "Importing…"
                } else {
                    "Choose folder…"
                };
                #[cfg(not(target_os = "android"))]
                let choose_label = "Choose folder…";

                #[cfg(target_os = "android")]
                let choose_enabled = !app.android.picker_pending;
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
                let can_clear = app.preferences.camera_profile_folder.is_some()
                    || app.preferences.camera_profile_auto_detect;
                #[cfg(target_os = "android")]
                let can_clear = can_clear && !app.android.picker_pending;
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
                ui.heading("AI models");
                let mut acceleration = app.ai.gpu_acceleration;
                if ui
                    .checkbox(
                        &mut acceleration,
                        "Use GPU acceleration when available",
                    )
                    .on_hover_text(
                        "Allow AI masks and AI denoise to use a supported GPU execution provider. CPU fallback remains automatic.",
                    )
                    .changed()
                {
                    app.set_ai_gpu_acceleration(acceleration);
                }
                ui.add(
                    egui::Label::new(
                        "Enabled by default. Turn this off if GPU-backed AI inference causes driver errors, instability, or incorrect results; AI tools will then run on CPU.",
                    )
                    .wrap(),
                );

                ui.separator();
                ui.strong("Subject masks");
                let previous_quality = app.ai.birefnet_quality;
                let mut quality = previous_quality;
                ui.add_enabled_ui(app.birefnet_quality_change_enabled(), |ui| {
                    crate::ui::theme::form_combo(
                        ui,
                        "Subject mask quality",
                        "settings-subject-mask-quality",
                        quality.label(),
                        180.0,
                        |ui| {
                            for option in crate::ai_masks::BiRefNetQuality::ALL {
                                ui.selectable_value(&mut quality, option, option.label());
                            }
                        },
                    );
                });
                if quality != previous_quality {
                    app.set_birefnet_quality(quality);
                }
                ui.add(egui::Label::new(quality.model().explanation).wrap());
                ui.small(
                    "This quality is used for newly generated and rerun Subject / Not Subject masks.",
                );

                ui.separator();
                ui.strong("ONNX Runtime");
                let runtime_help = if cfg!(target_os = "windows") {
                    "Choose a trusted ONNX Runtime 1.18 or newer onnxruntime.dll that matches this AuRaw build's CPU architecture. AuRaw validates the DLL in an isolated helper process before AI tools use it. GPU provider libraries and their dependencies must remain beside it."
                } else {
                    "Choose a trusted ONNX Runtime 1.18 or newer shared library built for your hardware. AuRaw never downloads or dynamically loads a native runtime without this explicit selection. GPU provider libraries and their dependencies must remain beside it."
                };
                ui.add(egui::Label::new(runtime_help).wrap());
                ui.add_space(4.0);
                if let Some(path) = &app.ai.runtime_path {
                    ui.label("Selected runtime:");
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(path.display().to_string()).monospace(),
                        )
                        .wrap(),
                    );
                    if let Some(sha256) = &app.ai.runtime_sha256 {
                        ui.small("Pinned SHA-256:");
                        ui.add(egui::Label::new(egui::RichText::new(sha256).monospace()).wrap());
                    }
                } else {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "No runtime selected. Subject and Not Subject masks cannot run yet.",
                    );
                }
                crate::ui::theme::action_row(ui, |ui| {
                    if ui.button("Choose ONNX Runtime…").clicked() {
                        app.choose_onnx_runtime();
                    }
                    if ui
                        .add_enabled(
                            app.ai.runtime_path.is_some(),
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

            let mut diagnostic_log = diagnostics_snapshot_with_ai_backends();
            crate::ui::theme::action_row(ui, |ui| {
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
                    diagnostic_log = diagnostics_snapshot_with_ai_backends();
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

        if !app.ui.expert_mode {
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

            crate::ui::theme::form_combo(
                ui,
                "Method",
                "settings-highlight-reconstruction-method",
                match app.develop.exposure.highlight_method {
                    HighlightReconstructionMethod::Off => "Off",
                    HighlightReconstructionMethod::Lch => "LCh (fast)",
                    HighlightReconstructionMethod::InpaintOpposed => "Inpaint opposed",
                },
                180.0,
                |ui| {
                    changed |= ui
                        .selectable_value(
                            &mut app.develop.exposure.highlight_method,
                            HighlightReconstructionMethod::Off,
                            "Off",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut app.develop.exposure.highlight_method,
                            HighlightReconstructionMethod::Lch,
                            "LCh (fast)",
                        )
                        .changed();
                    changed |= ui
                        .selectable_value(
                            &mut app.develop.exposure.highlight_method,
                            HighlightReconstructionMethod::InpaintOpposed,
                            "Inpaint opposed",
                        )
                        .changed();
                },
            );

            let enabled =
                app.develop.exposure.highlight_method != HighlightReconstructionMethod::Off;
            ui.add_enabled_ui(enabled, |ui| {
                changed |= adjustment_slider(
                    ui,
                    "Clip threshold",
                    &mut app.develop.exposure.highlight_clip,
                    0.5..=2.0,
                    2,
                    0.01,
                    Some("Sensor-relative level at which a channel is treated as clipped."),
                );
                if app.develop.exposure.highlight_method == HighlightReconstructionMethod::Lch {
                    changed |= adjustment_slider(
                        ui,
                        "Reconstruction strength",
                        &mut app.develop.exposure.highlight_reconstruction,
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
        let inner_width = (total_width - 26.0).max(1.0);
        crate::ui::theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(inner_width);
            ui.set_max_width(inner_width);
            contents(ui);
        });
    }
}
