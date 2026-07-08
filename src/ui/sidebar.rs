use crate::app::AurawApp;
use eframe::egui::{Slider, Ui};

pub struct Sidebar;

impl Sidebar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp) {
        ui.heading("Adjustments");

        if ui.button("Reset").clicked() {
            app.exposure = Default::default();
            app.dirty = true;
        }

        ui.separator();
        ui.label("Basic adjustments");

        if ui
            .add(Slider::new(&mut app.exposure.exposure, -18.0..=18.0).text("Exposure"))
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(Slider::new(&mut app.exposure.black, -1.0..=1.0).text("Black Level"))
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(Slider::new(&mut app.exposure.hlcompr, 0.0..=100.0).text("Highlight Compression"))
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(
                Slider::new(&mut app.exposure.hlcomprthresh, 0.0..=100.0)
                    .text("Highlight Threshold"),
            )
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(Slider::new(&mut app.exposure.brightness, -4.0..=4.0).text("Brightness"))
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(Slider::new(&mut app.exposure.contrast, -1.0..=5.0).text("Contrast"))
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(Slider::new(&mut app.exposure.middle_grey, 5.0..=100.0).text("Middle Grey"))
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(Slider::new(&mut app.exposure.saturation, -1.0..=1.0).text("Saturation"))
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(Slider::new(&mut app.exposure.vibrance, -1.0..=1.0).text("Vibrance"))
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(Slider::new(&mut app.exposure.clip, -1.0..=1.0).text("Clip"))
            .changed()
        {
            app.dirty = true;
        }

        ui.separator();
        ui.label("Filmic");

        if ui
            .add(Slider::new(&mut app.exposure.filmic_white, 0.1..=16.0).text("White EV"))
            .changed()
        {
            app.dirty = true;
        }

        if ui
            .add(Slider::new(&mut app.exposure.filmic_black, -16.0..=-0.1).text("Black EV"))
            .changed()
        {
            app.dirty = true;
        }
    }
}
