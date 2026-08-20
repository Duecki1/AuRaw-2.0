impl Sidebar {
    pub(crate) fn show_inpainting(
        ui: &mut Ui,
        app: &mut AurawApp,
        _layout: ScreenLayout,
        _frame: &eframe::Frame,
    ) {
        crate::ui::theme::toolbar_row(ui, |ui| {
            ui.strong("Remove");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let clear = crate::ui::icons::phosphor_icon_button_enabled(
                    ui,
                    !app.inpaint.edits.strokes.is_empty() && !app.inpaint.processing(),
                    egui_phosphor::regular::TRASH,
                    crate::ui::theme::toolbar_icon_size(),
                    "Clear all Remove strokes",
                );
                if clear.clicked() {
                    app.clear_inpainting_tool();
                }
            });
        });
        ui.add_space(4.0);

        crate::ui::theme::section_card(ui, "Brush", |ui| {
            ui.label(
                egui::RichText::new(
                    "Paint unwanted content. On release, Big-LaMa repairs only a local native-resolution context crop.",
                )
                .small()
                .color(ui.visuals().weak_text_color()),
            );
            ui.add_space(8.0);
            ui.add_enabled_ui(!app.inpaint.processing(), |ui| {
                adjustment_slider(
                    ui,
                    "Size",
                    &mut app.inpaint.brush_size,
                    0.0025..=0.25,
                    3,
                    0.0025,
                    Some("Brush stays the same size on screen; zoom in for a smaller, more precise native-image footprint."),
                );
            });
            if let Some(status) = app.inpaint.processing_label.as_deref() {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new(status).small());
                });
            }
        });

        ui.add_space(crate::ui::theme::CARD_GAP);
        crate::ui::theme::section_card(ui, "Remove strokes", |ui| {
            app.inpaint.hovered_stroke = None;
            let mut delete_stroke = None;
            if app.inpaint.edits.strokes.is_empty() {
                ui.label(
                    egui::RichText::new("No Remove strokes yet.")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            } else {
                for index in (0..app.inpaint.edits.strokes.len()).rev() {
                    let stroke = &app.inpaint.edits.strokes[index];
                    let points = stroke.brush.points.len();
                    let patches = stroke.patches.len();
                    crate::ui::theme::toolbar_row(ui, |ui| {
                        let selected = app.inpaint.selected_stroke == Some(index);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::ui::icons::phosphor_icon_button_enabled(
                                ui,
                                !app.inpaint.processing(),
                                egui_phosphor::regular::TRASH,
                                crate::ui::theme::toolbar_icon_size(),
                                "Delete this Remove stroke",
                            )
                            .clicked()
                            {
                                delete_stroke = Some(index);
                            }
                            let stroke_response = crate::ui::theme::navigation_row(
                                ui,
                                format!(
                                    "◎  Stroke {}  ·  {} points · {} patch{}",
                                    index + 1,
                                    points,
                                    patches,
                                    if patches == 1 { "" } else { "es" }
                                ),
                                selected,
                                egui::Sense::click(),
                            )
                            .on_hover_text(
                                "Hover to show the stored native brush mask. Click to keep it highlighted.",
                            );
                            if stroke_response.hovered() {
                                app.inpaint.hovered_stroke = Some(index);
                                ui.ctx().request_repaint();
                            }
                            if stroke_response.clicked() {
                                app.inpaint.selected_stroke =
                                    if selected { None } else { Some(index) };
                                ui.ctx().request_repaint();
                            }
                        });
                    });
                }
            }
            if let Some(index) = delete_stroke {
                app.delete_inpaint_stroke(index);
            }

            if app.preview.gpu_pipeline.is_none() {
                ui.add_space(8.0);
                ui.colored_label(ui.visuals().warn_fg_color, "Open a RAW image to use Remove.");
            }
        });
    }
}
