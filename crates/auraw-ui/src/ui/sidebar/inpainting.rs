impl Sidebar {
    pub(crate) fn show_inpainting(
        ui: &mut Ui,
        app: &mut AurawApp,
        _layout: ScreenLayout,
        frame: &eframe::Frame,
    ) {
        // Keep the context label and clear action in one predictable header row.
        // The horizontal parent prevents the right-aligned action from consuming
        // the scroll area's remaining vertical height.
        crate::ui::theme::toolbar_row(ui, |ui| {
            ui.strong("UwU");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let active_tool = app.inpaint_tool;
                let clear = crate::ui::icons::phosphor_icon_button_enabled(
                    ui,
                    app.inpaint_strokes
                        .iter()
                        .any(|stroke| stroke.kind == active_tool)
                        && !app.inpaint_busy(),
                    egui_phosphor::regular::TRASH,
                    crate::ui::theme::toolbar_icon_size(),
                    &format!("Clear all {} strokes", active_tool.label()),
                );
                if clear.clicked() {
                    app.clear_inpainting_tool(active_tool);
                }
            });
        });
        ui.add_space(4.0);
        crate::ui::theme::section_card(ui, "Tool", |ui| {
            ui.add_enabled_ui(!app.inpaint_busy(), |ui| {
                let previous_tool = app.inpaint_tool;
                ui.horizontal(|ui| {
                    for tool in InpaintStrokeKind::ALL {
                        ui.selectable_value(&mut app.inpaint_tool, tool, tool.label());
                    }
                });
                if app.inpaint_tool != previous_tool {
                    app.inpaint_stroke.clear();
                    app.last_inpaint_brush_point = None;
                    app.inpaint_stroke_texture = None;
                    app.inpaint_stroke_texture_key = None;
                    app.inpaint_selected_stroke = None;
                    app.inpaint_hovered_stroke = None;
                    app.inpaint_focus_texture = None;
                    app.inpaint_focus_texture_key = None;
                    app.inpaint_source_pick_active = app.inpaint_tool.requires_source()
                        && app.inpaint_source_anchor.is_none();
                }
                let help = match app.inpaint_tool {
                    InpaintStrokeKind::Remove => {
                        "Paint unwanted content. LaMa generates a local replacement when you release."
                    }
                    InpaintStrokeKind::Heal => {
                        "Copies source texture while adapting it to the destination color and light."
                    }
                    InpaintStrokeKind::Clone => {
                        "Copies source pixels exactly. Existing RAW pixels are never overwritten."
                    }
                };
                ui.label(
                    egui::RichText::new(help)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
                if app.inpaint_tool.requires_source() {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        let label = if app.inpaint_source_pick_active {
                            "Click image to set source…"
                        } else if app.inpaint_source_anchor.is_some() {
                            "Set source again"
                        } else {
                            "Set source"
                        };
                        if ui
                            .selectable_label(app.inpaint_source_pick_active, label)
                            .on_hover_text("You can also Alt-click the image to set the source.")
                            .clicked()
                        {
                            app.inpaint_source_pick_active = true;
                            app.inpaint_source_offset = None;
                            app.inpaint_stroke.clear();
                            app.last_inpaint_brush_point = None;
                            app.inpaint_stroke_texture = None;
                            app.inpaint_stroke_texture_key = None;
                        }
                        if app.inpaint_source_anchor.is_some() {
                            ui.label(
                                egui::RichText::new("Source set")
                                    .small()
                                    .color(egui::Color32::from_rgb(100, 210, 150)),
                            );
                        }
                    });
                }
                ui.add_space(8.0);
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
        });

        ui.add_space(crate::ui::theme::CARD_GAP);
        let active_tool = app.inpaint_tool;
        crate::ui::theme::section_card(ui, format!("{} strokes", active_tool.label()), |ui| {
            app.inpaint_hovered_stroke = None;
            let mut regenerate_stroke = None;
            let mut delete_stroke = None;
            let visible_strokes = app
                .inpaint_strokes
                .iter()
                .enumerate()
                .filter_map(|(index, stroke)| (stroke.kind == active_tool).then_some(index))
                .collect::<Vec<_>>();
            if visible_strokes.is_empty() {
                ui.label(
                    egui::RichText::new(format!("No {} strokes yet.", active_tool.label()))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            } else {
                for (tool_index, index) in visible_strokes.iter().copied().enumerate().rev() {
                    let dab_count = app.inpaint_strokes[index].dabs.len();
                    crate::ui::theme::toolbar_row(ui, |ui| {
                        let selected = app.inpaint_selected_stroke == Some(index);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::ui::icons::phosphor_icon_button_enabled(
                                ui,
                                !app.inpaint_busy(),
                                egui_phosphor::regular::TRASH,
                                crate::ui::theme::toolbar_icon_size(),
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
                                crate::ui::theme::toolbar_icon_size(),
                                "Regenerate this inpainting stroke",
                            )
                            .clicked()
                            {
                                regenerate_stroke = Some(index);
                            }

                            let stroke_response = crate::ui::theme::navigation_row(
                                ui,
                                format!("◎  Stroke {}  ·  {dab_count} dabs", tool_index + 1),
                                selected,
                                egui::Sense::click(),
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
                        });
                    });
                }
            }
            if let Some(index) = regenerate_stroke {
                app.regenerate_inpaint_stroke(frame, index);
            } else if let Some(index) = delete_stroke {
                app.delete_inpaint_stroke(index);
            }

            if app.gpu_pipeline.is_none() {
                ui.add_space(8.0);
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Open a RAW image to use Inpainting.",
                );
            }
        });
    }
}
