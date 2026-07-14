use crate::app::AurawApp;
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
                        "No runtime selected. Subject and Background masks cannot run yet.",
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
