use crate::app::{AppTab, AurawApp};
use eframe::egui::{self, Ui};

pub struct TopBar;

impl TopBar {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, _frame: &eframe::Frame) {
        #[cfg(target_os = "android")]
        Self::show_android(ui, app);
        #[cfg(not(target_os = "android"))]
        Self::show_desktop(ui, app);
    }

    #[cfg(target_os = "android")]
    fn show_android(ui: &mut Ui, app: &mut AurawApp) {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 4.0);

        ui.horizontal(|ui| {
            ui.strong("AuRaw");
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&app.status)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .truncate(),
            );
        });

        let tab_spacing = 4.0;
        let tab_height = 38.0;
        ui.spacing_mut().item_spacing.x = tab_spacing;
        let tab_width = ((ui.available_width() - tab_spacing * 2.0) / 3.0).max(1.0);
        let previous_tab = app.active_tab;
        ui.horizontal(|ui| {
            for (tab, label) in [
                (AppTab::Library, "Library"),
                (AppTab::Develop, "Develop"),
                (AppTab::Settings, "Settings"),
            ] {
                if ui
                    .add_sized(
                        [tab_width, tab_height],
                        egui::Button::new(label).selected(app.active_tab == tab),
                    )
                    .clicked()
                {
                    app.active_tab = tab;
                }
            }
        });
        Self::prepare_tab_transition(app, previous_tab);

        if app.active_tab == AppTab::Develop {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(app.can_undo_edit(), egui::Button::new("↶ Undo"))
                    .on_hover_text("Undo the last edit")
                    .clicked()
                {
                    app.undo_edit();
                }
                if ui
                    .add_enabled(app.can_redo_edit(), egui::Button::new("↷ Redo"))
                    .on_hover_text("Redo the last edit")
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
                    .on_hover_text("Save non-destructive edits beside the RAW")
                    .clicked()
                {
                    app.save_edits_now();
                }
            });
        }
    }

    #[cfg(not(target_os = "android"))]
    fn show_desktop(ui: &mut Ui, app: &mut AurawApp) {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

        ui.horizontal_wrapped(|ui| {
            ui.strong("AuRaw");
            ui.separator();
            let previous_tab = app.active_tab;
            ui.selectable_value(&mut app.active_tab, AppTab::Library, "Library");
            ui.selectable_value(&mut app.active_tab, AppTab::Develop, "Develop");
            ui.selectable_value(&mut app.active_tab, AppTab::Settings, "Settings");
            Self::prepare_tab_transition(app, previous_tab);

            ui.separator();
            match app.active_tab {
                AppTab::Library => {
                    if ui.button("Open Folder…").clicked() {
                        app.open_library_folder_dialog();
                    }
                }
                AppTab::Develop | AppTab::Settings => {}
            }

            if app.active_tab == AppTab::Develop {
                ui.separator();
                let comparison_available =
                    app.original_preview_texture.is_some() && app.gpu_pipeline.is_some();
                let label = if app.show_original_preview {
                    "Show Edited"
                } else {
                    "Show Original"
                };
                if ui
                    .add_enabled(comparison_available, egui::Button::new(label))
                    .on_hover_text("Toggle between the unedited original and the current edit")
                    .clicked()
                {
                    app.show_original_preview = !app.show_original_preview;
                }
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

    pub(crate) fn prepare_tab_transition(app: &mut AurawApp, previous_tab: AppTab) {
        if previous_tab == AppTab::Library && app.active_tab != AppTab::Library {
            // Library::show resumes the worker on return. Pausing here also
            // covers manual and swipe navigation away from the Library.
            app.library.prepare_for_develop();
        }
        if previous_tab != app.active_tab {
            app.show_original_preview = false;
            app.original_preview_hold_started = None;
            app.original_preview_hold_origin = None;
            app.original_preview_hold_cancelled = false;
        }
    }
}
