use eframe::egui::Ui;

pub struct Library;

impl Library {
    pub fn show(ui: &mut Ui) {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Library");
                ui.label("Library management is coming soon.");
                ui.add_space(4.0);
                ui.label("Open a RAW file, then switch to Develop to edit it.");
            });
        });
    }
}
