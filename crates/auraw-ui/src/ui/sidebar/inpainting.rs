impl Sidebar {
    pub(crate) fn show_inpainting(
        ui: &mut Ui,
        app: &mut AurawApp,
        _layout: ScreenLayout,
        frame: &eframe::Frame,
    ) {
        // This action used to share a horizontal row with the removed heading.
        // Preserve that compact row so the right-to-left child cannot consume and
        // center itself inside the entire remaining scroll-area height.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let clear = crate::ui::icons::phosphor_icon_button_enabled(
                    ui,
                    !app.inpaint_strokes.is_empty() && !app.inpaint_busy(),
                    egui_phosphor::regular::TRASH,
                    egui::vec2(28.0, 22.0),
                    "Clear all inpainting strokes",
                );
                if clear.clicked() {
                    app.clear_inpainting();
                }
            });
        });
        ui.separator();

        ui.add_enabled_ui(!app.inpaint_busy(), |ui| {
            adjustment_slider(
                ui,
                "Size",
                &mut app.inpaint_brush_size,
                0.0025..=0.25,
                3,
                0.0025,
                Some("Brush stays the same size on screen; zoom in for a smaller, more precise image-space footprint."),
            );
        });

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Drag on the image. Releasing each stroke runs the local LaMa eraser.",
            )
            .size(11.5)
            .color(ui.visuals().weak_text_color()),
        );

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(egui::RichText::new("Brush strokes").strong());
        app.inpaint_hovered_stroke = None;
        if app.inpaint_strokes.is_empty() {
            ui.label(
                egui::RichText::new("No completed strokes yet.")
                    .size(11.5)
                    .color(ui.visuals().weak_text_color()),
            );
        } else {
            let mut regenerate_stroke = None;
            let mut delete_stroke = None;
            egui::ScrollArea::vertical()
                .id_salt("inpainting-stroke-list")
                .max_height(180.0)
                .show(ui, |ui| {
                    for index in (0..app.inpaint_strokes.len()).rev() {
                        let dab_count = app.inpaint_strokes[index].dabs.len();
                        ui.horizontal(|ui| {
                            let selected = app.inpaint_selected_stroke == Some(index);
                            let stroke_response = ui
                                .selectable_label(
                                    selected,
                                    format!("◎  Stroke {}  ·  {dab_count} dabs", index + 1),
                                )
                                .on_hover_text(
                                    "Show this stroke on the image. Click to keep it highlighted.",
                                );
                            if stroke_response.hovered() {
                                app.inpaint_hovered_stroke = Some(index);
                                ui.ctx().request_repaint();
                            }
                            if stroke_response.clicked() {
                                app.inpaint_selected_stroke =
                                    if selected { None } else { Some(index) };
                                app.inpaint_focus_texture_key = None;
                                ui.ctx().request_repaint();
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if crate::ui::icons::phosphor_icon_button_enabled(
                                        ui,
                                        !app.inpaint_busy(),
                                        egui_phosphor::regular::TRASH,
                                        egui::vec2(28.0, 22.0),
                                        "Delete this inpainting stroke",
                                    )
                                    .clicked()
                                    {
                                        delete_stroke = Some(index);
                                    }
                                    if crate::ui::icons::phosphor_icon_button_enabled(
                                        ui,
                                        !app.inpaint_busy(),
                                        egui_phosphor::regular::ARROW_CLOCKWISE,
                                        egui::vec2(28.0, 22.0),
                                        "Regenerate this inpainting stroke",
                                    )
                                    .clicked()
                                    {
                                        regenerate_stroke = Some(index);
                                    }
                                },
                            );
                        });
                    }
                });
            if let Some(index) = regenerate_stroke {
                app.regenerate_inpaint_stroke(frame, index);
            } else if let Some(index) = delete_stroke {
                app.delete_inpaint_stroke(index);
            }
        }

        if let Some((downloaded, total)) = app.inpaint_progress() {
            ui.add_space(8.0);
            ui.label("Downloading lama_fp32.onnx…");
            ui.add(
                egui::ProgressBar::new(downloaded as f32 / total.max(1) as f32)
                    .show_percentage()
                    .text(format!(
                        "{:.1} / {:.1} MB",
                        downloaded as f64 / 1_000_000.0,
                        total as f64 / 1_000_000.0
                    )),
            );
        } else if app.inpaint_inferencing() {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Erasing…");
            });
        }

        if app.gpu_pipeline.is_none() {
            ui.add_space(8.0);
            ui.colored_label(egui::Color32::YELLOW, "Open a RAW image to use Inpainting.");
        }
    }
}
