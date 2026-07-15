use crate::app::{AppTab, AurawApp};
use eframe::egui::{self, Ui};

pub struct TopBar;

impl TopBar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, _frame: &eframe::Frame) {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

        ui.horizontal_wrapped(|ui| {
            ui.strong("AuRaw");
            ui.separator();
            let previous_tab = app.active_tab;
            ui.selectable_value(&mut app.active_tab, AppTab::Library, "Library");
            ui.selectable_value(&mut app.active_tab, AppTab::Develop, "Develop");
            ui.selectable_value(&mut app.active_tab, AppTab::Settings, "Settings");
            if previous_tab == AppTab::Library && app.active_tab != AppTab::Library {
                // Library::show resumes the worker on return. Pausing here also
                // covers a manual tab switch that does not open a new RAW, so
                // thumbnail LibRaw work cannot compete with Develop rendering.
                app.library.prepare_for_develop();
            }

            ui.separator();
            match app.active_tab {
                #[cfg(not(target_os = "android"))]
                AppTab::Library => {
                    if ui.button("Open Folder…").clicked() {
                        app.open_library_folder_dialog();
                    }
                }
                #[cfg(target_os = "android")]
                AppTab::Library => {}
                AppTab::Develop | AppTab::Settings => {}
            }

            if app.active_tab == AppTab::Develop {
                ui.separator();
                if ui
                    .add_enabled(app.can_undo_edit(), egui::Button::new("↶ Undo"))
                    .on_hover_text("Undo the last edit (Ctrl/Cmd+Z)")
                    .clicked()
                {
                    app.undo_edit();
                }
                if ui
                    .add_enabled(app.can_redo_edit(), egui::Button::new("↷ Redo"))
                    .on_hover_text("Redo the last edit (Ctrl/Cmd+Shift+Z or Ctrl+Y)")
                    .clicked()
                {
                    app.redo_edit();
                }
                if ui
                    .add_enabled(
                        app.can_save_edits(),
                        egui::Button::new(if app.sidecar_save_in_progress() {
                            "Saving Edits…"
                        } else {
                            "Save Edits"
                        }),
                    )
                    .on_hover_text("Save non-destructive edits beside the RAW (Ctrl/Cmd+S)")
                    .clicked()
                {
                    app.save_edits_now();
                }
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
