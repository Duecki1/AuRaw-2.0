use crate::app::{AppTab, AurawApp};
use eframe::egui::{self, Ui};

pub struct TopBar;

impl TopBar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

        ui.horizontal_wrapped(|ui| {
            ui.strong("AuRaw");
            ui.separator();
            ui.selectable_value(&mut app.active_tab, AppTab::Library, "Library");
            ui.selectable_value(&mut app.active_tab, AppTab::Develop, "Develop");
            ui.selectable_value(&mut app.active_tab, AppTab::Settings, "Settings");

            ui.separator();
            if ui.button("Open RAW…").clicked() {
                app.open_file_dialog(frame);
            }

            if ui
                .add_enabled(app.can_export(), egui::Button::new("Export PNG…"))
                .clicked()
            {
                app.export_png(frame);
            }

            ui.add(
                egui::Label::new(
                    egui::RichText::new(&app.status)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .wrap(),
            );
        });
    }
}
