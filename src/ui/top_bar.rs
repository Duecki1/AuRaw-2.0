use crate::app::{AppTab, AurawApp};
use eframe::egui::{self, Ui};

pub struct TopBar;

impl TopBar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        #[cfg(target_os = "android")]
        Self::show_android(ui, app, frame);
        #[cfg(not(target_os = "android"))]
        Self::show_desktop(ui, app, frame);
    }

    #[cfg(target_os = "android")]
    fn show_android(ui: &mut Ui, app: &mut AurawApp, _frame: &eframe::Frame) {
        let mut requested_tab = None;
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 3.0);
        ui.horizontal(|ui| {
            let tab_width = (ui.available_width() / 3.0).max(1.0);
            for (tab, label) in [
                (AppTab::Library, "Library"),
                (AppTab::Develop, "Develop"),
                (AppTab::Settings, "Settings"),
            ] {
                let button = egui::Button::new(label)
                    .selected(app.active_tab == tab)
                    .corner_radius(0.0);
                if ui.add_sized(egui::vec2(tab_width, 42.0), button).clicked() {
                    requested_tab = Some(tab);
                }
            }
        });
        if let Some(tab) = requested_tab {
            app.activate_tab(tab);
        }

        if app.active_tab == AppTab::Develop {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                if ui
                    .add_enabled(app.can_undo_edit(), egui::Button::new("↶ Undo"))
                    .clicked()
                {
                    app.undo_edit();
                }
                if ui
                    .add_enabled(app.can_redo_edit(), egui::Button::new("↷ Redo"))
                    .clicked()
                {
                    app.redo_edit();
                }
                if ui
                    .add_enabled(
                        app.can_save_edits(),
                        egui::Button::new(if app.sidecar_save_in_progress() {
                            "Saving…"
                        } else {
                            "Save"
                        }),
                    )
                    .clicked()
                {
                    app.save_edits_now();
                }
            });
        }

        ui.add(
            egui::Label::new(
                egui::RichText::new(&app.status)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            )
            .wrap(),
        );
    }

    #[cfg(not(target_os = "android"))]
    fn show_desktop(ui: &mut Ui, app: &mut AurawApp, _frame: &eframe::Frame) {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

        ui.horizontal_wrapped(|ui| {
            ui.strong("AuRaw");
            ui.separator();
            let previous_tab = app.active_tab;
            let mut selected_tab = app.active_tab;
            ui.selectable_value(&mut selected_tab, AppTab::Library, "Library");
            ui.selectable_value(&mut selected_tab, AppTab::Develop, "Develop");
            ui.selectable_value(&mut selected_tab, AppTab::Settings, "Settings");
            if selected_tab != previous_tab {
                app.activate_tab(selected_tab);
            }

            ui.separator();
            if app.active_tab == AppTab::Library && ui.button("Open Folder…").clicked() {
                app.open_library_folder_dialog();
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
                if ui
                    .add_enabled(
                        app.gpu_pipeline.is_some(),
                        egui::Button::new(if app.original_preview_visible() {
                            "Show Edited"
                        } else {
                            "Show Original"
                        })
                        .selected(app.original_preview_visible()),
                    )
                    .on_hover_text("Toggle between the original and edited preview")
                    .clicked()
                {
                    app.toggle_original_preview();
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
