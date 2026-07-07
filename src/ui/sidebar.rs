use eframe::egui::{Ui, Slider};
use crate::app::AurawApp;

pub struct Sidebar;

impl Sidebar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp) {
        ui.heading("Adjustments");

        // Adjust the manual exposure slider from -18.0 to 18.0 EV
        if ui
            .add(Slider::new(&mut app.exposure.exposure, -18.0..=18.0).text("Exposure"))
            .changed()
        {
            app.dirty = true;
        }

        // Adjust the black level slider from -1.0 to 1.0
        if ui
            .add(Slider::new(&mut app.exposure.black, -1.0..=1.0).text("Black Level"))
            .changed()
        {
            app.dirty = true;
        }
    }
}