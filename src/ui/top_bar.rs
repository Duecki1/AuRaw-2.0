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

    fn history_icon_button(
        ui: &mut Ui,
        enabled: bool,
        redo: bool,
        size: egui::Vec2,
        hover_text: &str,
    ) -> egui::Response {
        // Paint the symbol instead of using Unicode arrow glyphs. Some Android
        // font fallbacks render those glyphs as empty squares.
        let response = ui
            .add_enabled_ui(enabled, |ui| ui.add_sized(size, egui::Button::new("")))
            .inner;
        let widget_visuals = ui.style().interact(&response);
        let stroke = egui::Stroke::new(
            widget_visuals.fg_stroke.width.max(2.0),
            widget_visuals.fg_stroke.color,
        );
        let radius = response.rect.width().min(response.rect.height()) * 0.25;
        let center = response.rect.center() + egui::vec2(0.0, radius * 0.06);
        let mut arc = Vec::with_capacity(17);
        for step in 0..=16 {
            let progress = step as f32 / 16.0;
            let angle = (150.0_f32 - 220.0 * progress).to_radians();
            let x = angle.cos() * radius;
            let y = -angle.sin() * radius;
            arc.push(center + egui::vec2(if redo { -x } else { x }, y));
        }

        let arrow_tip = arc[0];
        ui.painter().add(egui::Shape::line(arc, stroke));
        let head_direction = if redo { -1.0 } else { 1.0 };
        ui.painter().line_segment(
            [
                arrow_tip,
                arrow_tip + egui::vec2(head_direction * radius * 0.62, -radius * 0.44),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                arrow_tip,
                arrow_tip + egui::vec2(head_direction * radius * 0.62, radius * 0.44),
            ],
            stroke,
        );

        response.on_hover_text(hover_text)
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
                if Self::history_icon_button(
                    ui,
                    app.can_undo_edit(),
                    false,
                    egui::vec2(32.0, 26.0),
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
                    egui::vec2(32.0, 26.0),
                    "Redo the last edit (Ctrl/Cmd+Shift+Z or Ctrl+Y)",
                )
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
