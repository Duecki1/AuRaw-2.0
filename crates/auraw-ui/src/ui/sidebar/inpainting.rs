impl Sidebar {
    pub(crate) fn show_inpainting(
        ui: &mut Ui,
        app: &mut AurawApp,
        _layout: ScreenLayout,
        _frame: &eframe::Frame,
    ) {
        crate::ui::theme::toolbar_row(ui, |ui| {
            ui.strong("Inpainting");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let clear = crate::ui::icons::phosphor_icon_button_enabled(
                    ui,
                    !app.inpaint.edits.strokes.is_empty() && !app.inpaint.processing(),
                    egui_phosphor::regular::TRASH,
                    crate::ui::theme::toolbar_icon_size(),
                    "Clear all inpainting strokes",
                );
                if clear.clicked() {
                    app.clear_inpainting_tool();
                }
            });
        });
        ui.add_space(4.0);

        crate::ui::theme::section_card(ui, "Tool", |ui| {
            let previous_tool = app.inpaint.tool;
            ui.horizontal_wrapped(|ui| {
                for tool in InpaintTool::ALL {
                    ui.selectable_value(&mut app.inpaint.tool, tool, tool.label());
                }
            });
            if app.inpaint.tool != previous_tool {
                app.inpaint.active_points.clear();
                app.inpaint.last_brush_uv = None;
                app.inpaint.source_pick_active = false;
                app.inpaint.aligned_offset = None;
            }
            ui.add_space(8.0);
            let help = match app.inpaint.tool {
                InpaintTool::Remove => {
                    "Paint unwanted content. Big-LaMa repairs a native-resolution local context crop after release."
                }
                InpaintTool::Clone => {
                    "Copy pixels from a source. Ctrl-click (Command-click on macOS) or right-click the image to choose it."
                }
                InpaintTool::Heal => {
                    "Copy source texture while matching the destination's color and light with GIMP-style perceptual Poisson healing."
                }
            };
            ui.label(
                egui::RichText::new(help)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );

            if app.inpaint.tool.retouch().is_some() {
                ui.add_space(8.0);
                let previous_alignment = app.inpaint.alignment;
                egui::ComboBox::from_id_salt("retouch-source-alignment")
                    .selected_text(app.inpaint.alignment.label())
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for alignment in RetouchAlignment::ALL {
                            ui.selectable_value(
                                &mut app.inpaint.alignment,
                                alignment,
                                alignment.label(),
                            );
                        }
                    });
                if previous_alignment != app.inpaint.alignment {
                    app.inpaint.aligned_offset = None;
                }
                ui.label(
                    egui::RichText::new(match app.inpaint.alignment {
                        RetouchAlignment::None => {
                            "Source follows this stroke, then returns to the selected point."
                        }
                        RetouchAlignment::Aligned => {
                            "Source keeps the same offset between separate strokes."
                        }
                        RetouchAlignment::Registered => {
                            "Source and destination use the same image coordinates."
                        }
                        RetouchAlignment::Fixed => {
                            "Every brush dab starts from the selected source point."
                        }
                    })
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
                if ui
                    .add_enabled(
                        !app.inpaint.processing(),
                        egui::Button::new(if app.inpaint.source_pick_active {
                            "Cancel source placement"
                        } else {
                            "Set source on canvas"
                        })
                        .selected(app.inpaint.source_pick_active),
                    )
                    .clicked()
                {
                    app.inpaint.source_pick_active = !app.inpaint.source_pick_active;
                }
                let source_status = if app.inpaint.source_pick_active {
                    "Click or tap the image to place the source"
                } else if app.inpaint.source_point.is_some() {
                    "Source set · Ctrl/right-click to move it"
                } else {
                    "Source not set · Ctrl/right-click the image"
                };
                ui.label(egui::RichText::new(source_status).small().color(
                    if app.inpaint.source_point.is_some() {
                        ui.visuals().weak_text_color()
                    } else {
                        ui.visuals().warn_fg_color
                    },
                ));
            }
        });

        ui.add_space(crate::ui::theme::CARD_GAP);
        crate::ui::theme::section_card(ui, "Brush", |ui| {
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
                if app.inpaint.tool.retouch().is_some() {
                    let mut feather = 1.0 - app.inpaint.brush_hardness;
                    if adjustment_slider(
                        ui,
                        "Feather",
                        &mut feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Width of the soft outer edge as a fraction of the brush radius."),
                    ) {
                        app.inpaint.brush_hardness = 1.0 - feather;
                    }
                }
                adjustment_slider(
                    ui,
                    "Opacity",
                    &mut app.inpaint.brush_opacity,
                    0.01..=1.0,
                    2,
                    0.01,
                    Some("Initial strength of each new Remove, Clone, or Heal stroke."),
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
        crate::ui::theme::section_card(ui, "Inpainting strokes", |ui| {
            app.inpaint.hovered_stroke = None;
            let mut delete_stroke = None;
            if app.inpaint.edits.strokes.is_empty() {
                ui.label(
                    egui::RichText::new("No inpainting strokes yet.")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            } else {
                for index in (0..app.inpaint.edits.strokes.len()).rev() {
                    let stroke = &app.inpaint.edits.strokes[index];
                    let points = stroke.brush.points.len();
                    let patches = stroke.patches.len();
                    let tool = stroke
                        .retouch
                        .map(|retouch| retouch.tool.label())
                        .unwrap_or("Remove");
                    crate::ui::theme::toolbar_row(ui, |ui| {
                        let selected = app.inpaint.selected_stroke == Some(index);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::ui::icons::phosphor_icon_button_enabled(
                                ui,
                                !app.inpaint.processing(),
                                egui_phosphor::regular::TRASH,
                                crate::ui::theme::toolbar_icon_size(),
                                "Delete this inpainting stroke",
                            )
                            .clicked()
                            {
                                delete_stroke = Some(index);
                            }
                            let stroke_response = crate::ui::theme::navigation_row(
                                ui,
                                format!(
                                    "◎  {} {}  ·  {} points · {} patch{}",
                                    tool,
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
            let selected_settings = app
                .inpaint
                .selected_stroke
                .and_then(|index| {
                    app.inpaint
                        .edits
                        .strokes
                        .get(index)
                        .map(|stroke| (index, stroke))
                })
                .map(|(index, stroke)| {
                    (
                        index,
                        stroke.opacity,
                        stroke
                            .retouch
                            .map(|retouch| 1.0 - retouch.hardness.clamp(0.0, 1.0)),
                    )
                });
            if let Some((index, mut opacity, feather)) = selected_settings {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);
                ui.strong(format!("Selected stroke {}", index + 1));
                if let Some(feather) = feather {
                    ui.label(
                        egui::RichText::new(format!("Recorded feather: {:.0}%", feather * 100.0))
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
                let changed = ui
                    .add_enabled_ui(!app.inpaint.processing(), |ui| {
                        adjustment_slider(
                            ui,
                            "Opacity",
                            &mut opacity,
                            0.0..=1.0,
                            2,
                            0.01,
                            Some("Non-destructively changes this stored stroke without rerunning its model or heal solver."),
                        )
                    })
                    .inner;
                if changed {
                    app.set_inpaint_stroke_opacity(index, opacity);
                }
            }
            if let Some(index) = delete_stroke {
                app.delete_inpaint_stroke(index);
            }
            if !ui.input(|input| input.pointer.any_down()) {
                app.finish_inpaint_stroke_opacity_edit();
            }

            if app.preview.gpu_pipeline.is_none() {
                ui.add_space(8.0);
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Open a RAW image to use inpainting tools.",
                );
            }
        });
    }
}
