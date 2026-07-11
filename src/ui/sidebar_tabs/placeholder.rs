use eframe::egui::{self, Ui};

pub struct PlaceholderTab;

impl PlaceholderTab {
    pub fn show(ui: &mut Ui, title: &str, description: &str) {
        ui.add_space(4.0);
        ui.heading(title);
        ui.add_space(2.0);
        ui.label(
            egui::RichText::new(description)
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(12.0);

        ui.group(|ui| {
            let width = ui.available_width();
            ui.set_min_width(width);
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                ui.strong("Coming soon");
                ui.label(
                    egui::RichText::new(
                        "This tool is a placeholder for the next implementation phase.",
                    )
                    .color(ui.visuals().weak_text_color()),
                );
            });
            ui.add_space(8.0);
        });
    }
}
