impl Sidebar {
    pub(crate) fn show_inpainting(
        ui: &mut Ui,
        app: &mut AurawApp,
        _layout: ScreenLayout,
        _frame: &eframe::Frame,
    ) {
        ui.horizontal(|ui| {
            ui.heading("Inpainting");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let clear = ui.add_enabled(
                    !app.inpaint_strokes.is_empty() && !app.inpaint_busy(),
                    egui::Button::new("Clear all"),
                );
                if clear.clicked() {
                    app.clear_inpainting();
                }
            });
        });
        ui.label(
            egui::RichText::new("Paint over unwanted content")
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(2.0);
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
        if app.inpaint_strokes.is_empty() {
            ui.label(
                egui::RichText::new("No completed strokes yet.")
                    .size(11.5)
                    .color(ui.visuals().weak_text_color()),
            );
        } else {
            let mut delete_stroke = None;
            egui::ScrollArea::vertical()
                .id_salt("inpainting-stroke-list")
                .max_height(180.0)
                .show(ui, |ui| {
                    for index in (0..app.inpaint_strokes.len()).rev() {
                        let dab_count = app.inpaint_strokes[index].dabs.len();
                        ui.horizontal(|ui| {
                            ui.label(format!("Stroke {}", index + 1));
                            ui.label(
                                egui::RichText::new(format!("{dab_count} dabs"))
                                    .size(10.5)
                                    .color(ui.visuals().weak_text_color()),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add_enabled(
                                            !app.inpaint_busy(),
                                            egui::Button::new("Delete").small(),
                                        )
                                        .clicked()
                                    {
                                        delete_stroke = Some(index);
                                    }
                                },
                            );
                        });
                    }
                });
            if let Some(index) = delete_stroke {
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
            ui.colored_label(
                egui::Color32::YELLOW,
                "Open a RAW image to use Inpainting.",
            );
        }
    }
}
