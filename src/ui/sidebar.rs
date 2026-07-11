use crate::app::AurawApp;
use eframe::egui::{self, Slider, Ui};

pub struct Sidebar;

impl Sidebar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp) {
        ui.heading("Adjustments");

        let mut changed = false;

        macro_rules! slider {
            ($ui:expr, $value:expr, $range:expr, $text:expr, $decimals:expr) => {
                changed |= $ui
                    .add(
                        Slider::new(&mut $value, $range)
                            .text($text)
                            .fixed_decimals($decimals),
                    )
                    .changed();
            };
        }

        ui.horizontal(|ui| {
            ui.label("Lightroom-style controls");
            if ui.button("Reset all").clicked() {
                app.reset_develop_adjustments();
            }
        });
        ui.separator();

        egui::CollapsingHeader::new("Basic (Tone & Exposure)")
            .default_open(true)
            .show(ui, |ui| {
                slider!(ui, app.exposure.exposure, -5.0..=5.0, "Exposure (EV)", 2);
                slider!(ui, app.exposure.contrast, -100.0..=100.0, "Contrast", 0);
                slider!(ui, app.exposure.highlights, -100.0..=100.0, "Highlights", 0);
                slider!(ui, app.exposure.shadows, -100.0..=100.0, "Shadows", 0);
                slider!(ui, app.exposure.whites, -100.0..=100.0, "Whites", 0);
                slider!(ui, app.exposure.blacks, -100.0..=100.0, "Blacks", 0);
            });

        egui::CollapsingHeader::new("Presence")
            .default_open(true)
            .show(ui, |ui| {
                slider!(ui, app.exposure.texture, -100.0..=100.0, "Texture", 0);
                slider!(ui, app.exposure.clarity, -100.0..=100.0, "Clarity", 0);
                slider!(ui, app.exposure.dehaze, -100.0..=100.0, "Dehaze", 0);
                slider!(ui, app.exposure.vibrance, -100.0..=100.0, "Vibrance", 0);
                slider!(ui, app.exposure.saturation, -100.0..=100.0, "Saturation", 0);
            });

        egui::CollapsingHeader::new("HSL / Color Mixer")
            .default_open(false)
            .show(ui, |ui| {
                const COLORS: [&str; 8] = [
                    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
                ];

                for (index, color) in COLORS.iter().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.strong(*color);
                        slider!(ui, app.exposure.hsl_hue[index], -100.0..=100.0, "Hue", 0);
                        slider!(
                            ui,
                            app.exposure.hsl_saturation[index],
                            -100.0..=100.0,
                            "Saturation",
                            0
                        );
                        slider!(
                            ui,
                            app.exposure.hsl_luminance[index],
                            -100.0..=100.0,
                            "Luminance",
                            0
                        );
                    });
                    if index + 1 < COLORS.len() {
                        ui.separator();
                    }
                }
            });

        egui::CollapsingHeader::new("Advanced (Ansel)")
            .default_open(false)
            .show(ui, |ui| {
                ui.label("Ansel basic controls");
                slider!(
                    ui,
                    app.exposure.hlcompr,
                    0.0..=500.0,
                    "Highlight Compression",
                    0
                );
                slider!(
                    ui,
                    app.exposure.hlcomprthresh,
                    0.0..=100.0,
                    "Compression Threshold",
                    0
                );
                slider!(ui, app.exposure.brightness, -4.0..=4.0, "Brightness", 2);
                slider!(ui, app.exposure.middle_grey, 5.0..=90.0, "Middle Grey %", 1);

                ui.separator();
                ui.label("Filmic tone mapping");
                slider!(
                    ui,
                    app.exposure.filmic_white,
                    0.1..=10.0,
                    "White Point (EV)",
                    1
                );
                slider!(
                    ui,
                    app.exposure.filmic_black,
                    -10.0..=-0.1,
                    "Black Point (EV)",
                    1
                );
            });

        egui::CollapsingHeader::new("Raw")
            .default_open(false)
            .show(ui, |ui| {
                slider!(
                    ui,
                    app.exposure.black_point,
                    -1.0..=1.0,
                    "Raw Black Point",
                    3
                );
                slider!(
                    ui,
                    app.exposure.chroma_denoise,
                    0.0..=1.0,
                    "Chroma Denoise",
                    2
                );
                slider!(ui, app.exposure.ca_red, -2.0..=2.0, "Red CA", 2);
                slider!(ui, app.exposure.ca_blue, -2.0..=2.0, "Blue CA", 2);
            });

        if changed {
            app.mark_pipeline_dirty();
        }
    }
}
