use crate::app::AurawApp;
use crate::pipeline::HighlightReconstructionMethod;
use eframe::egui::{ComboBox, Slider, Ui};

pub struct Settings;

impl Settings {
    pub fn show(ui: &mut Ui, app: &mut AurawApp) {
        ui.heading("Settings");
        ui.add_space(8.0);

        let mut changed = false;

        ui.group(|ui| {
            ui.set_max_width(640.0);
            ui.heading("Highlight reconstruction");
            ui.label(
                "Reconstruct clipped sensor channels before demosaicing to avoid pink or grey highlights.",
            );
            ui.add_space(6.0);

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
                changed |= ui
                    .add(
                        Slider::new(&mut app.exposure.highlight_clip, 0.5..=2.0)
                            .text("Clip threshold")
                            .fixed_decimals(2),
                    )
                    .on_hover_text(
                        "Sensor-relative level at which a channel is treated as clipped.",
                    )
                    .changed();
                changed |= ui
                    .add(
                        Slider::new(&mut app.exposure.highlight_reconstruction, 0.0..=1.0)
                            .text("Reconstruction strength")
                            .fixed_decimals(2),
                    )
                    .on_hover_text("Blend between the original and reconstructed highlight.")
                    .changed();

                if app.exposure.highlight_method == HighlightReconstructionMethod::Guided {
                    ui.separator();
                    ui.strong("Guided reconstruction");
                    changed |= ui
                        .add(
                            Slider::new(&mut app.exposure.highlight_iterations, 1..=4)
                                .text("Iterations"),
                        )
                        .on_hover_text(
                            "More iterations propagate surrounding color farther into clipped regions.",
                        )
                        .changed();
                    changed |= ui
                        .add(
                            Slider::new(
                                &mut app.exposure.highlight_color_adaptation,
                                0.0..=1.0,
                            )
                            .text("Color adaptation")
                            .fixed_decimals(2),
                        )
                        .on_hover_text(
                            "Controls how strongly reconstructed highlights follow nearby color.",
                        )
                        .changed();
                }
            });

            ui.add_space(6.0);
            if ui.button("Restore reconstruction defaults").clicked() {
                app.reset_highlight_reconstruction_settings();
            }
        });

        if changed {
            app.mark_pipeline_dirty();
        }
    }
}
