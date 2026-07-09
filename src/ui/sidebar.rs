use crate::app::AurawApp;
use eframe::egui::{self, Slider, Ui};

pub struct Sidebar;

impl Sidebar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp) {
        ui.heading("Adjustments");

        macro_rules! slider {
            ($ui:ident, $field:ident, $range:expr, $text:expr, $decimals:expr) => {
                if $ui
                    .add(
                        Slider::new(&mut app.exposure.$field, $range)
                            .text($text)
                            .fixed_decimals($decimals),
                    )
                    .changed()
                {
                    app.dirty = true;
                }
            };
        }

        egui::CollapsingHeader::new("Light")
            .default_open(true)
            .show(ui, |ui| {
                slider!(ui, exposure, -4.0..=4.0, "Exposure (EV)", 2);
                slider!(ui, contrast, -1.0..=1.0, "Contrast", 2);
                slider!(ui, brightness, -1.0..=1.0, "Brightness", 2);
                slider!(ui, black, -0.1..=0.1, "Black Level", 3);

                ui.separator();
                ui.label("Highlight Recovery");
                slider!(ui, hlcompr, 0.0..=100.0, "Compression %", 0);
                slider!(ui, hlcomprthresh, 0.0..=100.0, "Threshold %", 0);
            });

        egui::CollapsingHeader::new("Color")
            .default_open(true)
            .show(ui, |ui| {
                slider!(ui, vibrance, -1.0..=1.0, "Vibrance", 2);
                slider!(ui, saturation, -1.0..=1.0, "Saturation", 2);
                slider!(ui, chroma_denoise, 0.0..=1.0, "Chroma Denoise", 2);
            });

        egui::CollapsingHeader::new("Tone Mapping")
            .default_open(true)
            .show(ui, |ui| {
                slider!(ui, middle_grey, 5.0..=90.0, "Middle Grey %", 1);
                slider!(ui, filmic_white, 0.1..=10.0, "White Point (EV)", 1);
                slider!(ui, filmic_black, -10.0..=-0.1, "Black Point (EV)", 1);
            });

        egui::CollapsingHeader::new("Raw")
            .default_open(false)
            .show(ui, |ui| {
                slider!(ui, clip, -1.0..=1.0, "Clip Point", 2);
                slider!(ui, ca_red, -2.0..=2.0, "Red CA", 2);
                slider!(ui, ca_blue, -2.0..=2.0, "Blue CA", 2);
                if ui.button("Reset adjustments").clicked() {
                    app.exposure = Default::default();
                    app.dirty = true;
                }
            });
    }
}
