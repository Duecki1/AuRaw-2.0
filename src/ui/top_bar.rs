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
    pub(crate) fn back_icon_button(ui: &mut Ui, size: egui::Vec2) -> egui::Response {
        let response = ui.add_sized(size, egui::Button::new(""));
        let visuals = ui.style().interact(&response);
        let stroke = egui::Stroke::new(
            visuals.fg_stroke.width.max(2.0),
            visuals.fg_stroke.color,
        );
        let center = response.rect.center();
        let arm = response.rect.height() * 0.18;
        let shaft_left = center.x - arm * 0.55;
        let shaft_right = center.x + arm * 0.90;
        ui.painter().line_segment(
            [egui::pos2(shaft_left, center.y), egui::pos2(shaft_right, center.y)],
            stroke,
        );
        ui.painter().line_segment(
            [
                egui::pos2(shaft_left, center.y),
                egui::pos2(shaft_left + arm, center.y - arm),
            ],
            stroke,
        );
        ui.painter().line_segment(
            [
                egui::pos2(shaft_left, center.y),
                egui::pos2(shaft_left + arm, center.y + arm),
            ],
            stroke,
        );
        response.on_hover_text("Back to Library")
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
        // Android uses a native navigation stack instead of persistent page tabs:
        // Library -> tap thumbnail -> Develop, and system Back returns to Library.
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);
        ui.horizontal(|ui| {
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

        });
    }
}
