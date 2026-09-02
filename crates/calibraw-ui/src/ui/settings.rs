#[cfg(not(target_os = "android"))]
use crate::app::OnnxRuntimeMode;
use crate::app::{maximum_raw_cache_limit, CalibRawApp, PreviewQuality};
use crate::pipeline::CameraProfileMode;
use crate::ui::components::adjustment_slider::adjustment_slider;
use crate::ui::layout::ScreenLayout;
use crate::ui::library::maximum_thumbnail_worker_count;
use eframe::egui::{self, Ui};

const PROJECT_NOTICE: &str = include_str!("../../../../NOTICE.md");
const PROJECT_LICENSE: &str = include_str!("../../../../COPYING");
const PROJECT_LICENSE_ID: &str = env!("CARGO_PKG_LICENSE");
const PROJECT_REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const THIRD_PARTY_NOTICES: &str = include_str!("../../../../THIRD_PARTY_NOTICES.md");
const RUST_DEPENDENCY_LICENSES: &str = include_str!("../../../../THIRD_PARTY_LICENSES.md");

pub(crate) struct Settings;

fn diagnostics_snapshot_with_ai_backends() -> String {
    let mut diagnostic_log = crate::diagnostics::snapshot();
    let providers = calibraw_ai::active_execution_providers();
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
    pub(crate) fn show(ui: &mut Ui, app: &mut CalibRawApp, layout: ScreenLayout) {
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

    fn show_content(ui: &mut Ui, app: &mut CalibRawApp, layout: ScreenLayout, content_width: f32) {
        #[cfg(target_os = "android")]
        crate::ui::theme::toolbar_row(ui, |ui| {
            if crate::ui::top_bar::TopBar::back_icon_button(
                ui,
                crate::ui::theme::toolbar_icon_size(),
            )
            .clicked()
            {
                app.activate_tab(crate::app::AppTab::Library);
            }
            ui.heading("Settings");
        });
        #[cfg(not(target_os = "android"))]
        ui.heading("Settings");
        crate::ui::theme::card_gap(ui);

        Self::group(ui, content_width, |ui| {
            crate::ui::theme::heading_with_help(
                ui,
                "Appearance",
                "Choose the design used across every screen and the canvas color shown around the photo. Changes are saved and applied immediately.",
            );

            ui.separator();
            let mut design = app.preferences.ui_design;
            crate::ui::theme::form_combo_with_help(
                ui,
                "Design",
                "settings-ui-design",
                design.label(),
                220.0,
                design.description(),
                |ui| {
                    for option in crate::ui::theme::UiDesign::ALL {
                        ui.selectable_value(&mut design, option, option.label())
                            .on_hover_text(option.description());
                    }
                },
            );
            if design != app.preferences.ui_design {
                app.set_ui_design(design);
            }

            ui.separator();
            let mut backdrop = app.preferences.preview_backdrop;
            let backdrop_help = match backdrop {
                crate::ui::theme::PreviewBackdrop::MatchPhoto => {
                    "Derives a quiet, low-contrast canvas color from each image without competing with the edit. Toolbars, sidebars, and panels keep the selected design."
                }
                _ => {
                    "Changes only the canvas around the photo. Toolbars, sidebars, and panels keep the selected design."
                }
            };
            crate::ui::theme::form_combo_with_help(
                ui,
                "Preview background",
                "settings-preview-backdrop",
                backdrop.label(),
                220.0,
                backdrop_help,
                |ui| {
                    for option in crate::ui::theme::PreviewBackdrop::ALL {
                        ui.selectable_value(&mut backdrop, option, option.label());
                    }
                },
            );
            if backdrop != app.preferences.preview_backdrop {
                app.set_preview_backdrop(backdrop);
            }
        });

        crate::ui::theme::card_gap(ui);

        Self::group(ui, content_width, |ui| {
            crate::ui::theme::heading_with_help(
                ui,
                "Interface",
                "Configure preview resolution, brush behavior, and library resource use.",
            );

            ui.separator();
            if crate::ui::theme::checkbox_with_help(
                ui,
                &mut app.preferences.show_develop_navigation_labels,
                "Show Develop navigation labels",
                "Shows a short name below each icon in the Develop navigation bars. Leave this disabled for a quieter, more compact interface.",
            )
            .changed()
            {
                app.persist_performance_settings();
            }

            #[cfg(not(target_os = "android"))]
            {
                ui.separator();
                let mut enabled = app.preferences.discord_rich_presence;
                if crate::ui::theme::checkbox_with_help(
                    ui,
                    &mut enabled,
                    "Discord Rich Presence",
                    "Shares only whether you are browsing the Library or editing, plus the elapsed edit time. Photo names and paths are never sent. Discord's desktop client must be running.",
                )
                .changed()
                {
                    app.set_discord_rich_presence(enabled);
                }
                if !app.discord_rich_presence_configured() {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Unavailable in this build: CALIBRAW_DISCORD_APPLICATION_ID is not configured.",
                    );
                }
            }

            ui.separator();
            let previous_quality = app.preview.quality;
            crate::ui::theme::form_combo_with_help(
                ui,
                "Preview quality",
                "settings-preview-quality",
                app.preview.quality.label(),
                160.0,
                "Controls preview render density relative to its physical screen size: Low 75%, Medium 100%, High 125%, and Max 150%. Zoom detail uses the same density.",
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

            ui.separator();
            if crate::ui::theme::checkbox_with_help(
                ui,
                &mut app.preferences.image_relative_brush_size,
                "Keep brush size fixed to the image",
                "When enabled, the brush keeps the same image footprint at every zoom level. When disabled, it keeps the same on-screen footprint.",
            )
                .changed()
            {
                app.persist_performance_settings();
            }

            ui.separator();
            crate::ui::theme::strong_with_help(
                ui,
                "Library performance",
                "Tune memory use and background preview generation. Conservative values are recommended on phones.",
            );
            #[cfg(not(target_os = "android"))]
            {
                let mut render_edited_thumbnails =
                    app.library.renders_edited_thumbnails_during_indexing();
                if crate::ui::theme::checkbox_with_help(
                    ui,
                    &mut render_edited_thumbnails,
                    "Render edited thumbnails while indexing",
                    "Applies saved edits to library thumbnails during indexing. Turning it off keeps indexing faster and memory use lower; existing edited previews are still reused.",
                )
                    .changed()
                {
                    app.set_render_edited_thumbnails_during_indexing(render_edited_thumbnails);
                }
            }

            let mut raw_cache_files = app.develop.raw_cache_limit;
            let raw_cache_help = if raw_cache_files == 0 {
                "Decoded RAW reuse is disabled; only the current edit remains loaded. Zero disables the cache. The default is 2 on desktop and 1 on Android.".to_owned()
            } else {
                format!(
                    "Keeps up to {raw_cache_files} decoded RAW {} in memory, including the current image, for faster switching. The default is 2 on desktop and 1 on Android.",
                    if raw_cache_files == 1 { "file" } else { "files" }
                )
            };
            if adjustment_slider(
                ui,
                "Decoded RAW cache",
                &mut raw_cache_files,
                0..=maximum_raw_cache_limit(),
                0,
                1.0,
                Some(&raw_cache_help),
            ) {
                app.set_raw_cache_limit(raw_cache_files);
            }

            let mut thumbnail_workers = app.thumbnail_worker_count();
            if adjustment_slider(
                ui,
                "Thumbnail workers",
                &mut thumbnail_workers,
                1..=maximum_thumbnail_worker_count(),
                0,
                1.0,
                Some(
                    "Concurrent thumbnail jobs. Higher values fill the library faster but preview-less and edited jobs may unpack a full sensor and use substantial memory. Changing this restarts the queue.",
                ),
            ) {
                app.set_thumbnail_worker_count(thumbnail_workers);
            }

            ui.separator();
            crate::ui::theme::strong_with_help(
                ui,
                "Thumbnail cache",
                "Delete generated library previews. CalibRaw rebuilds them from the RAW files and saved edits when needed.",
            );
            let cache_size = app.thumbnail_cache_size_label();
            crate::ui::theme::action_row(ui, |ui| {
                if ui
                    .button("Clear thumbnail cache")
                    .on_hover_text("Delete generated previews and rebuild them when needed.")
                    .clicked()
                {
                    app.clear_thumbnail_cache();
                }
                ui.label(egui::RichText::new(cache_size).color(ui.visuals().weak_text_color()));
            });
        });

        crate::ui::theme::card_gap(ui);
        Self::group(ui, content_width, |ui| {
            crate::ui::theme::heading_with_help(
                ui,
                "Copy & paste adjustments",
                "Choose which categories Library > Copy Adjustments stores for later pasting. The selection is remembered across restarts.",
            );

            let mut settings = app.preferences.adjustment_copy_settings;
            let mut changed = false;
            changed |= crate::ui::theme::checkbox_with_help(
                ui,
                &mut settings.adjustments,
                "Adjustments",
                "Global light, color, white-balance temperature/tint, tone curve, effects, color mixer, and RAW adjustment values.",
            )
                .changed();
            changed |= crate::ui::theme::checkbox_with_help(
                ui,
                &mut settings.geometry,
                "Geometry",
                "Crop, rotation, straighten, perspective, flips, and geometry transforms. Disabled by default.",
            )
                .changed();
            changed |= crate::ui::theme::checkbox_with_help(
                ui,
                &mut settings.camera_profile,
                "Camera profile",
                "The per-image camera/DCP profile selection. Enabled by default.",
            )
            .changed();
            changed |= crate::ui::theme::checkbox_with_help(
                ui,
                &mut settings.masks,
                "Normal masks",
                "Brush, radial-gradient, and linear-gradient mask components, including local adjustments. Mixed groups are split so disabling this never copies their manual components.",
            )
                .changed();
            changed |= crate::ui::theme::checkbox_with_help(
                ui,
                &mut settings.ai_masks,
                "AI masks",
                "Subject, background, object, luminance-range, and color-range components. Source-dependent results are marked for regeneration on the destination image.",
            )
                .changed();
            changed |= crate::ui::theme::checkbox_with_help(
                ui,
                &mut settings.lens_correction,
                "Lens correction",
                "Lens correction enabled state and selected lens profile.",
            )
            .changed();
            if changed {
                app.set_adjustment_copy_settings(settings);
            }
        });

        crate::ui::theme::card_gap(ui);
        Self::group(ui, content_width, |ui| {
            crate::ui::theme::heading_with_help(
                ui,
                "RAW color profiles",
                "Choose how CalibRaw builds the camera-to-working color transform and whether it applies DCP color-rendering stages.",
            );

            let previous_mode = app.preferences.camera_profile_mode;
            let mut mode = previous_mode;
            let profile_mode_help = match mode {
                CameraProfileMode::Automatic => {
                    "Uses the RAW's embedded camera matrix by default. A DCP selected for an individual image remains explicit."
                }
                CameraProfileMode::DcpProfiles => {
                    "Searches the configured folder for this camera. If no safe DCP match exists, CalibRaw falls back to the embedded camera matrix."
                }
                CameraProfileMode::MatrixOnly => {
                    "Ignores DCP look tables, hue/saturation maps, and profile tone curves and uses the camera/DNG/LibRaw matrix path."
                }
            };
            crate::ui::theme::form_combo_with_help(
                ui,
                "Profile mode",
                "settings-camera-profile-mode",
                mode.label(),
                190.0,
                profile_mode_help,
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

            ui.separator();
            #[cfg(target_os = "android")]
            let camera_folder_help = "Choose a top-level CameraProfiles folder with Android's system picker. CalibRaw recursively imports only .dcp files into private persistent storage, groups matches by camera, and exposes them in Develop.";
            #[cfg(not(target_os = "android"))]
            let camera_folder_help = "Choose a top-level CameraProfiles folder. CalibRaw searches subfolders recursively. Auto-detect checks Adobe Camera Raw's standard install location on Windows and macOS.";
            crate::ui::theme::strong_with_help(ui, "Camera profile folder", camera_folder_help);
            #[cfg(target_os = "android")]
            if let Some(label) = &app.android.camera_profile_folder_importing_label {
                ui.add(
                    egui::Label::new(egui::RichText::new(format!("Importing {label}…")).strong())
                        .wrap(),
                );
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
                    .on_hover_text(camera_folder_help)
                    .clicked()
                {
                    app.choose_camera_profile_folder();
                }
                #[cfg(not(target_os = "android"))]
                if ui
                    .button("Auto-detect Adobe")
                    .on_hover_text("Check Adobe Camera Raw's standard CameraProfiles location.")
                    .clicked()
                {
                    app.auto_detect_camera_profile_folder();
                }
                let can_clear = app.preferences.camera_profile_folder.is_some()
                    || app.preferences.camera_profile_auto_detect;
                #[cfg(target_os = "android")]
                let can_clear = can_clear && !app.android.picker_pending;
                if ui
                    .add_enabled(can_clear, egui::Button::new("Clear"))
                    .on_hover_text("Stop using the configured external DCP profile folder.")
                    .clicked()
                {
                    app.clear_camera_profile_folder();
                }
            });
        });

        #[cfg(not(target_os = "android"))]
        {
            crate::ui::theme::card_gap(ui);
            Self::group(ui, content_width, |ui| {
                crate::ui::theme::heading_with_help(
                    ui,
                    "AI models",
                    "Configure local inference acceleration, Subject mask quality, and the trusted ONNX Runtime library used by AI tools.",
                );
                let mut acceleration = app.ai.gpu_acceleration;
                if crate::ui::theme::checkbox_with_help(
                    ui,
                    &mut acceleration,
                    "Use GPU acceleration when available",
                    "Allows AI masks and AI denoise to use a supported GPU execution provider. CPU fallback remains automatic. Turn this off if GPU inference causes driver errors, instability, or incorrect results.",
                )
                    .changed()
                {
                    app.set_ai_gpu_acceleration(acceleration);
                }

                ui.separator();
                crate::ui::theme::strong_with_help(
                    ui,
                    "Subject masks",
                    "Quality applies to newly generated Subject and Not Subject masks.",
                );
                let previous_quality = app.ai.birefnet_quality;
                let mut quality = previous_quality;
                let quality_help = format!(
                    "{} This quality applies to newly generated Subject and Not Subject masks.",
                    quality.model().explanation
                );
                ui.add_enabled_ui(app.birefnet_quality_change_enabled(), |ui| {
                    crate::ui::theme::form_combo_with_help(
                        ui,
                        "Subject mask quality",
                        "settings-subject-mask-quality",
                        quality.label(),
                        180.0,
                        &quality_help,
                        |ui| {
                            for option in crate::ai_masks::BiRefNetQuality::ALL {
                                ui.selectable_value(&mut quality, option, option.label())
                                    .on_hover_text(option.model().explanation);
                            }
                        },
                    );
                });
                if quality != previous_quality {
                    app.set_birefnet_quality(quality);
                }

                let mut crop_refinement = app.ai.subject_crop_refinement;
                ui.add_enabled_ui(app.birefnet_quality_change_enabled(), |ui| {
                    if crate::ui::theme::checkbox_with_help(
                        ui,
                        &mut crop_refinement,
                        "Refine subject edges with a cropped pass",
                        "Runs one additional BiRefNet pass on the cropped subject region. This may improve edge quality, but it can be worse at recognizing the entire subject and takes longer.",
                    )
                    .changed()
                    {
                        app.set_subject_crop_refinement(crop_refinement);
                    }
                });

                ui.separator();
                let runtime_help = "Automatic downloads the verified ONNX Runtime package matching this operating system and CPU architecture when an AI tool first needs it. Manual uses a local shared-library override.";
                crate::ui::theme::strong_with_help(ui, "ONNX Runtime", runtime_help);
                let previous_mode = app.ai.runtime_mode;
                let mut runtime_mode = previous_mode;
                ui.horizontal(|ui| {
                    ui.selectable_value(
                        &mut runtime_mode,
                        OnnxRuntimeMode::Automatic,
                        "Automatic",
                    )
                    .on_hover_text("Recommended. Download and use CalibRaw's verified runtime for this platform when AI is first used.");
                    ui.selectable_value(
                        &mut runtime_mode,
                        OnnxRuntimeMode::Manual,
                        "Manual",
                    )
                    .on_hover_text("Override automatic selection with a trusted local ONNX Runtime shared library.");
                });
                if runtime_mode != previous_mode {
                    app.set_onnx_runtime_mode(runtime_mode);
                }

                match app.ai.runtime_mode {
                    OnnxRuntimeMode::Automatic => {
                        ui.label(format!(
                            "CalibRaw will select the {} / {} runtime and download it from CalibRaw Artifacts when an AI tool first needs it.",
                            std::env::consts::OS,
                            std::env::consts::ARCH
                        ));
                        ui.small("The archive and extracted runtime are cached locally and verified with pinned SHA-256 values.");
                        ui.hyperlink_to(
                            "View CalibRaw runtime artifacts",
                            "https://huggingface.co/Duecki/CalibRaw-Artifacts/tree/main/onnxruntime",
                        );
                    }
                    OnnxRuntimeMode::Manual => {
                        let manual_help = if cfg!(target_os = "windows") {
                            "Choose a trusted ONNX Runtime 1.18 or newer onnxruntime.dll matching this CalibRaw build's CPU architecture. CalibRaw validates it in an isolated helper process. Provider libraries and dependencies must remain beside it."
                        } else {
                            "Choose a trusted ONNX Runtime 1.18 or newer shared library built for this hardware. CalibRaw validates it in an isolated helper process. Provider libraries and dependencies must remain beside it."
                        };
                        if let Some(path) = &app.ai.runtime_path {
                            ui.label("Selected manual runtime:");
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(path.display().to_string()).monospace(),
                                )
                                .wrap(),
                            );
                            if let Some(sha256) = &app.ai.runtime_sha256 {
                                ui.small("Pinned SHA-256:");
                                ui.add(
                                    egui::Label::new(egui::RichText::new(sha256).monospace())
                                        .wrap(),
                                );
                            }
                        } else {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "No manual runtime selected. AI tools need a file override or Automatic mode.",
                            );
                        }
                        crate::ui::theme::action_row(ui, |ui| {
                            if ui
                                .button("Choose ONNX Runtime…")
                                .on_hover_text(manual_help)
                                .clicked()
                            {
                                app.choose_onnx_runtime();
                            }
                            if ui
                                .add_enabled(
                                    app.ai.runtime_path.is_some(),
                                    eframe::egui::Button::new("Clear"),
                                )
                                .on_hover_text("Forget the selected manual runtime.")
                                .clicked()
                            {
                                app.clear_onnx_runtime();
                            }
                        });
                    }
                }
            });
        }

        crate::ui::theme::card_gap(ui);
        Self::group(ui, content_width, |ui| {
            crate::ui::theme::heading_with_help(
                ui,
                "Updates",
                "View the installed version and optionally ask GitHub for CalibRaw's latest stable release.",
            );
            ui.strong(format!("CalibRaw {}", env!("CARGO_PKG_VERSION")));

            ui.separator();
            let mut auto_check = app.preferences.auto_check_updates;
            if crate::ui::theme::checkbox_with_help(
                ui,
                &mut auto_check,
                "Automatically check for updates via GitHub",
                "Checks GitHub once when CalibRaw starts. GitHub receives standard connection information such as your IP address. No photo, path, account identifier, or telemetry is sent.",
            )
            .changed()
            {
                app.set_auto_check_updates(auto_check);
            }

            let checking = app.version_check_in_progress();
            let status = app.version_check_status_text();
            crate::ui::theme::action_row(ui, |ui| {
                if ui
                    .add_enabled(!checking, egui::Button::new("Check now"))
                    .on_hover_text("Check GitHub for the latest stable CalibRaw release.")
                    .clicked()
                {
                    app.check_for_updates(true);
                }
                ui.hyperlink_to("View releases", format!("{PROJECT_REPOSITORY}/releases"));
            });
            ui.small(status);
        });

        crate::ui::theme::card_gap(ui);
        Self::group(ui, content_width, |ui| {
            crate::ui::theme::heading_with_help(
                ui,
                "Legal & attributions",
                "Adapted code, bundled data, native libraries, optional AI models, Rust crates, fonts, and icons retain their listed upstream terms and notices.",
            );
            ui.strong(format!("CalibRaw {}", env!("CARGO_PKG_VERSION")));
            ui.label("Copyright (C) 2026 Duecki and CalibRaw contributors");
            ui.label(format!("Licensed under {PROJECT_LICENSE_ID}."));
            ui.hyperlink_to("Project source and license", PROJECT_REPOSITORY);

            crate::ui::theme::action_row(ui, |ui| {
                if ui
                    .button("Copy all legal text")
                    .on_hover_text("Copy the project license and every bundled third-party notice.")
                    .clicked()
                {
                    let legal_text = format!(
                        "{PROJECT_NOTICE}\n\n{PROJECT_LICENSE}\n\n{THIRD_PARTY_NOTICES}\n\n{RUST_DEPENDENCY_LICENSES}"
                    );
                    #[cfg(target_os = "android")]
                    match app.copy_text_to_clipboard("CalibRaw legal notices", &legal_text) {
                        Ok(()) => crate::diagnostics::record(
                            "CalibRaw legal notices copied to Android clipboard",
                        ),
                        Err(error) => crate::diagnostics::record(format!(
                            "Android legal-notice clipboard copy failed: {error}"
                        )),
                    }
                    #[cfg(not(target_os = "android"))]
                    ui.ctx().copy_text(legal_text);
                }
            });

            egui::CollapsingHeader::new("Project notice")
                .default_open(false)
                .show(ui, |ui| Self::legal_text(ui, PROJECT_NOTICE, 8));
            egui::CollapsingHeader::new("GNU GPL v3 or later")
                .default_open(false)
                .show(ui, |ui| Self::legal_text(ui, PROJECT_LICENSE, 16));
            egui::CollapsingHeader::new("Adapted source, data, native libraries, and AI models")
                .default_open(false)
                .show(ui, |ui| Self::legal_text(ui, THIRD_PARTY_NOTICES, 18));
            egui::CollapsingHeader::new("Rust dependencies, fonts, and icons")
                .default_open(false)
                .show(ui, |ui| Self::legal_text(ui, RUST_DEPENDENCY_LICENSES, 18));
        });

        crate::ui::theme::card_gap(ui);
        Self::group(ui, content_width, |ui| {
            crate::ui::theme::heading_with_help(
                ui,
                "Diagnostics",
                "For comparisons, open the same RAW and run an export on each device, then copy the log. It includes platform, GPU/backend, RAW calibration, fingerprints, and timing information.",
            );

            let mut diagnostic_log = diagnostics_snapshot_with_ai_backends();
            crate::ui::theme::action_row(ui, |ui| {
                if ui
                    .button("Copy log")
                    .on_hover_text("Copy the complete diagnostic report.")
                    .clicked()
                {
                    #[cfg(target_os = "android")]
                    match app.copy_text_to_clipboard("CalibRaw diagnostics", &diagnostic_log) {
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
                if ui
                    .button("Clear events")
                    .on_hover_text("Clear recorded runtime events from the diagnostic report.")
                    .clicked()
                {
                    crate::diagnostics::clear();
                    diagnostic_log = diagnostics_snapshot_with_ai_backends();
                }
            });

            let rows = match layout {
                ScreenLayout::Horizontal => 16,
                ScreenLayout::Vertical => 10,
            };
            let mut diagnostic_view = diagnostic_log.as_str();
            egui::CollapsingHeader::new("View diagnostic log")
                .default_open(false)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut diagnostic_view)
                            .font(egui::TextStyle::Monospace)
                            .interactive(false)
                            .desired_rows(rows)
                            .desired_width(f32::INFINITY),
                    );
                });
        });
    }

    fn group(ui: &mut Ui, total_width: f32, contents: impl FnOnce(&mut Ui)) {
        let frame_width = f32::from(crate::ui::theme::CONTENT_MARGIN) * 2.0 + 6.0;
        let inner_width = (total_width - frame_width).max(1.0);
        crate::ui::theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(inner_width);
            ui.set_max_width(inner_width);
            contents(ui);
        });
    }

    fn legal_text(ui: &mut Ui, text: &str, rows: usize) {
        let mut view = text;
        ui.add(
            egui::TextEdit::multiline(&mut view)
                .font(egui::TextStyle::Monospace)
                .interactive(false)
                .desired_rows(rows)
                .desired_width(f32::INFINITY),
        );
    }
}
