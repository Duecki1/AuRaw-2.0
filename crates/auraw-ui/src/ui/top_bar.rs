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
        ui.add_sized(
            size,
            egui::Button::new(
                egui::RichText::new(egui_phosphor::regular::ARROW_LEFT).size(size.y * 0.55),
            ),
        )
        .on_hover_text("Back to Library")
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
        ui.add_enabled(
            enabled,
            egui::Button::new(egui::RichText::new(icon).size(size.y * 0.55)).min_size(size),
        )
        .on_hover_text(hover_text)
    }

    #[cfg(target_os = "android")]
    fn show_android(ui: &mut Ui, app: &mut AurawApp, _frame: &eframe::Frame) {
        theme::prepare_toolbar(ui);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            app.show_global_task_control(ui);

            if let Some(status) = crate::cloud::cached_status(app.current_path.as_deref()) {
                ui.label(
                    egui::RichText::new(status)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }

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
            let save_response = ui
                .add_enabled_ui(app.can_save_edits(), |ui| {
                    ui.add_sized(
                        egui::vec2(42.0, 36.0),
                        egui::Button::new(egui::RichText::new(save_icon).size(19.8)),
                    )
                })
                .inner
                .on_hover_text(save_tooltip);
            if save_response.clicked() {
                app.save_edits_now();
            }

            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                if Self::back_icon_button(ui, egui::vec2(42.0, 36.0)).clicked() {
                    app.activate_tab(AppTab::Library);
                }
                if Self::history_icon_button(
                    ui,
                    app.can_undo_edit(),
                    false,
                    egui::vec2(42.0, 36.0),
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
                    egui::vec2(42.0, 36.0),
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
            app.show_global_task_control(ui);
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
                    if theme::tab_button(ui, label, app.active_tab == tab, tab_width).clicked() {
                        app.activate_tab(tab);
                    }
                }

                ui.separator();
                if app.active_tab == AppTab::Library {
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

                if app.active_tab == AppTab::Develop {
                    if let Some(status) = crate::cloud::cached_status(app.current_path.as_deref()) {
                        ui.label(
                            egui::RichText::new(status)
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
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
                    let save_response = ui
                        .add_enabled_ui(app.can_save_edits(), |ui| {
                            ui.add_sized(
                                theme::toolbar_icon_size(),
                                egui::Button::new(
                                    egui::RichText::new(save_icon)
                                        .size(theme::TOOLBAR_ICON_EDGE * 0.55),
                                ),
                            )
                        })
                        .inner
                        .on_hover_text(save_tooltip);
                    if save_response.clicked() {
                        app.save_edits_now();
                    }
                    let original_visible = app.original_preview_visible();
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
                    if crate::ui::icons::phosphor_icon_button_enabled(
                        ui,
                        app.gpu_pipeline.is_some(),
                        preview_icon,
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
