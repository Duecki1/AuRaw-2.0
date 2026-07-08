use crate::app::AurawApp;
use eframe::egui::{self, Slider, Ui};

pub struct Sidebar;

impl Sidebar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp) {
        ui.heading("Adjustments");

        macro_rules! slider {
            ($ui:ident, $field:ident, $range:expr, $text:expr) => {
                if $ui
                    .add(Slider::new(&mut app.exposure.$field, $range).text($text))
                    .changed()
                {
                    app.dirty = true;
                }
            };
        }

        egui::CollapsingHeader::new("Light")
            .default_open(true)
            .show(ui, |ui| {
                slider!(ui, exposure, -18.0..=18.0, "Exposure");
                slider!(ui, contrast, -1.0..=5.0, "Contrast");
                slider!(ui, hlcompr, 0.0..=100.0, "Highlights");
                slider!(ui, hlcomprthresh, 0.0..=100.0, "Highlight Range");
                slider!(ui, black, -1.0..=1.0, "Blacks");
                slider!(ui, brightness, -4.0..=4.0, "Brightness");
            });

        egui::CollapsingHeader::new("Color")
            .default_open(true)
            .show(ui, |ui| {
                slider!(ui, vibrance, -1.0..=1.0, "Vibrance");
                slider!(ui, saturation, -1.0..=1.0, "Saturation");
            });

        egui::CollapsingHeader::new("Tone Mapping")
            .default_open(true)
            .show(ui, |ui| {
                slider!(ui, middle_grey, 5.0..=100.0, "Middle Grey");
                slider!(ui, filmic_white, 0.1..=16.0, "White EV");
                slider!(ui, filmic_black, -16.0..=-0.1, "Black EV");
            });

        egui::CollapsingHeader::new("Raw")
            .default_open(false)
            .show(ui, |ui| {
                slider!(ui, clip, -1.0..=1.0, "Clip Point");
                if ui.button("Reset adjustments").clicked() {
                    app.exposure = Default::default();
                    app.dirty = true;
                }
            });
    }
}
