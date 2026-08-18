use crate::app::{AppTab, AurawApp};
use crate::ui::theme;
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
    pub(crate) fn back_icon_button(ui: &mut Ui, size: egui::Vec2) -> egui::Response {
        crate::ui::icons::phosphor_icon_button(
            ui,
            egui_phosphor::regular::ARROW_LEFT,
            size,
            "Back to Library",
        )
    }

    fn history_icon_button(
        ui: &mut Ui,
        enabled: bool,
        redo: bool,
        size: egui::Vec2,
        hover_text: &str,
    ) -> egui::Response {
        let icon = if redo {
            egui_phosphor::regular::ARROW_U_UP_RIGHT
        } else {
            egui_phosphor::regular::ARROW_U_UP_LEFT
        };
        crate::ui::icons::phosphor_icon_button_enabled(ui, enabled, icon, size, hover_text)
    }

    #[cfg(target_os = "android")]
    fn show_android(ui: &mut Ui, app: &mut AurawApp, _frame: &eframe::Frame) {
        theme::prepare_toolbar(ui);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            app.show_export_task_indicator(ui);

            let save_tooltip = if app.sidecar_save_in_progress() {
                "Saving non-destructive edits…"
            } else if app.sidecar_save_succeeded_recently() {
                "Edits saved"
            } else {
                "Save non-destructive edits"
            };
            let save_icon = if app.sidecar_save_succeeded_recently() {
                egui_phosphor::regular::CHECK
            } else {
                egui_phosphor::regular::FLOPPY_DISK
            };
            let save_response = crate::ui::icons::phosphor_icon_button_enabled(
                ui,
                app.can_save_edits(),
                save_icon,
                theme::toolbar_icon_size(),
                save_tooltip,
            );
            if save_response.clicked() {
                app.save_edits_now();
            }

            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if Self::back_icon_button(ui, theme::toolbar_icon_size()).clicked() {
                    app.activate_tab(AppTab::Library);
                }
                if Self::history_icon_button(
                    ui,
                    app.can_undo_edit(),
                    false,
                    theme::toolbar_icon_size(),
                    "Undo the last edit",
                )
                .clicked()
                {
                    app.undo_edit();
                }
                if Self::history_icon_button(
                    ui,
                    app.can_redo_edit(),
                    true,
                    theme::toolbar_icon_size(),
                    "Redo the last edit",
                )
                .clicked()
                {
                    app.redo_edit();
                }
            });
        });
    }

    #[cfg(not(target_os = "android"))]
    fn show_desktop(ui: &mut Ui, app: &mut AurawApp, _frame: &eframe::Frame) {
        theme::prepare_toolbar(ui);
        let compact = ui.available_width() < 620.0;
        let brand_width = if compact { 52.0 } else { 68.0 };
        let tab_width = if compact { 72.0 } else { 82.0 };
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            app.show_export_task_indicator(ui);
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_sized(
                    [brand_width, theme::CONTROL_HEIGHT],
                    egui::Label::new(egui::RichText::new("AuRaw").strong()),
                );
                ui.separator();
                for (tab, label) in [
                    (AppTab::Library, "Library"),
                    (AppTab::Develop, "Develop"),
                    (AppTab::Settings, "Settings"),
                ] {
                    if theme::tab_button(ui, label, app.ui.active_tab == tab, tab_width).clicked() {
                        app.activate_tab(tab);
                    }
                }

                ui.separator();
                if app.ui.active_tab == AppTab::Library {
                    let open = if compact {
                        crate::ui::icons::phosphor_icon_button(
                            ui,
                            egui_phosphor::regular::FOLDER_OPEN,
                            theme::toolbar_icon_size(),
                            "Open photo folder",
                        )
                    } else {
                        theme::toolbar_button(ui, "Open Folder…", 108.0)
                            .on_hover_text("Open photo folder")
                    };
                    if open.clicked() {
                        app.open_library_folder_dialog();
                    }
                }

                if app.ui.active_tab == AppTab::Develop {
                    if Self::history_icon_button(
                        ui,
                        app.can_undo_edit(),
                        false,
                        theme::toolbar_icon_size(),
                        "Undo the last edit (Ctrl/Cmd+Z)",
                    )
                    .clicked()
                    {
                        app.undo_edit();
                    }
                    if Self::history_icon_button(
                        ui,
                        app.can_redo_edit(),
                        true,
                        theme::toolbar_icon_size(),
                        "Redo the last edit (Ctrl/Cmd+Shift+Z or Ctrl+Y)",
                    )
                    .clicked()
                    {
                        app.redo_edit();
                    }
                    let save_tooltip = if app.sidecar_save_in_progress() {
                        "Saving non-destructive edits…"
                    } else if app.sidecar_save_succeeded_recently() {
                        "Edits saved"
                    } else {
                        "Save non-destructive edits beside the RAW (Ctrl/Cmd+S)"
                    };
                    let save_icon = if app.sidecar_save_succeeded_recently() {
                        egui_phosphor::regular::CHECK
                    } else {
                        egui_phosphor::regular::FLOPPY_DISK
                    };
                    let save_response = crate::ui::icons::phosphor_icon_button_enabled(
                        ui,
                        app.can_save_edits(),
                        save_icon,
                        theme::toolbar_icon_size(),
                        save_tooltip,
                    );
                    if save_response.clicked() {
                        app.save_edits_now();
                    }
                    let original_visible = app.preview.original_visible();
                    let preview_icon = if original_visible {
                        egui_phosphor::regular::EYE
                    } else {
                        egui_phosphor::regular::EYE_SLASH
                    };
                    let preview_tooltip = if original_visible {
                        "Show edited preview"
                    } else {
                        "Show original preview"
                    };
                    if crate::ui::icons::phosphor_icon_toggle_button_enabled(
                        ui,
                        app.preview.gpu_pipeline.is_some(),
                        preview_icon,
                        original_visible,
                        theme::toolbar_icon_size(),
                        preview_tooltip,
                    )
                    .clicked()
                    {
                        app.toggle_original_preview();
                    }
                }
            });
        });
    }
}
