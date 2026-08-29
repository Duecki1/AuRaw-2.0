use super::*;

fn mask_effect_picker_visible(
    orientation: MaskStripOrientation,
    vertical_section: Option<MaskSection>,
) -> bool {
    orientation == MaskStripOrientation::Horizontal
        || vertical_section == Some(MaskSection::Properties)
}

impl Sidebar {
    pub(super) fn create_mask_group_card(
        ui: &mut Ui,
        new_mask: &mut Option<MaskKind>,
        orientation: MaskStripOrientation,
    ) {
        let size = MaskCardSize::Group.create_button_size(orientation);
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
            |ui| {
                ui.spacing_mut().interact_size = size;
                ui.menu_button(
                    egui::RichText::new(mask_creation_icon())
                        .size(20.0)
                        .strong(),
                    |ui| {
                        *new_mask = Self::mask_kind_menu(
                            ui,
                            "This mask type is planned but not implemented yet.",
                        );
                    },
                )
                .response
                .on_hover_text("Create a new mask group");
            },
        );
    }

    pub(super) fn create_submask_card(
        ui: &mut Ui,
        add_component: &mut Option<(MaskKind, MaskCombineMode)>,
        orientation: MaskStripOrientation,
    ) {
        let size = MaskCardSize::Submask.create_button_size(orientation);
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
            |ui| {
                ui.spacing_mut().interact_size = size;
                ui.menu_button(
                    egui::RichText::new(mask_creation_icon())
                        .size(18.0)
                        .strong(),
                    |ui| {
                        *add_component = Self::submask_creation_menu(
                            ui,
                            "This sub-mask type is planned but not implemented yet.",
                        );
                    },
                )
                .response
                .on_hover_text("Add a sub-mask to the selected group");
            },
        );
    }

    pub(super) fn submask_drop_placeholder(ui: &mut Ui) -> egui::Response {
        use eframe::egui::{Align2, FontId, Stroke, StrokeKind};

        let (rect, response) =
            ui.allocate_exact_size(MaskCardSize::Submask.card_size(), egui::Sense::hover());
        let red = crate::ui::theme::DROP_TARGET;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 5.0, red.gamma_multiply(0.18));
        painter.rect_stroke(rect, 5.0, Stroke::new(2.0, red), StrokeKind::Inside);
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "DROP",
            FontId::proportional(9.0),
            red,
        );
        response
    }

    pub(super) fn paint_floating_submask(ui: &Ui, drag: &SubmaskDragState, pointer: egui::Pos2) {
        use eframe::egui::{Align2, Color32, FontId, LayerId, Order, Stroke, StrokeKind};

        let card_size = MaskCardSize::Submask;
        let rect =
            egui::Rect::from_center_size(pointer + egui::vec2(12.0, 12.0), card_size.card_size());
        let painter = ui.ctx().layer_painter(LayerId::new(
            Order::Tooltip,
            egui::Id::new("floating-submask-drag-card"),
        ));
        let visuals = ui.visuals();
        painter.rect_filled(rect, 5.0, visuals.widgets.active.bg_fill);
        painter.rect_stroke(
            rect,
            5.0,
            Stroke::new(2.0, visuals.selection.bg_fill),
            StrokeKind::Inside,
        );

        let image_edge = card_size.image_edge();
        let image_rect = egui::Rect::from_min_size(
            egui::pos2(rect.center().x - image_edge * 0.5, rect.min.y + 5.0),
            egui::vec2(image_edge, image_edge),
        );
        painter.rect_filled(image_rect, 3.0, Color32::BLACK);
        if let Some(texture) = drag.source_texture.as_ref() {
            painter.image(
                texture.id(),
                image_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                if drag.source_enabled {
                    Color32::WHITE
                } else {
                    Color32::from_white_alpha(80)
                },
            );
        }

        let badge_height = 16.0;
        let badge_size = egui::vec2(
            (drag.source_badge.chars().count() as f32 * 9.0 * 0.62 + 8.0).max(badge_height + 2.0),
            badge_height,
        );
        let badge_rect =
            egui::Rect::from_min_size(image_rect.right_bottom() - badge_size, badge_size);
        painter.rect_filled(badge_rect, 3.0, Color32::from_black_alpha(210));
        painter.text(
            badge_rect.center(),
            Align2::CENTER_CENTER,
            &drag.source_badge,
            FontId::proportional(9.0),
            Color32::WHITE,
        );

        let display_label: String = drag.source_name.chars().take(10).collect();
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 9.0),
            Align2::CENTER_CENTER,
            display_label,
            FontId::proportional(card_size.label_font_size()),
            if drag.source_enabled {
                visuals.text_color()
            } else {
                visuals.weak_text_color()
            },
        );
    }

    pub(super) fn show_masks_horizontal_details(
        ui: &mut Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
    ) {
        Self::show_mask_details(ui, app, frame, MaskStripOrientation::Horizontal);
    }

    pub(super) fn show_masks_vertical_details(
        ui: &mut Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
    ) {
        Self::show_mask_details(ui, app, frame, MaskStripOrientation::Vertical);
    }

    fn show_mask_details(
        ui: &mut Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
        orientation: MaskStripOrientation,
    ) {
        let Some((mask_index, component_index)) = app.masks.stack.ensure_selection() else {
            return;
        };

        let vertical_section = (orientation == MaskStripOrientation::Vertical).then(|| {
            if !app.masks.stack.masks[mask_index].effect.uses_adjustments() {
                app.develop_ui.mask_section = MaskSection::Properties;
            }
            app.develop_ui.mask_section
        });

        let mut geometry_changed = false;
        let mut adjustments_changed = false;
        let mut effect_changed = false;
        let mut request_subject = false;
        let mut request_object = false;
        let mut brush_mode = app.masks.brush_mode;
        let selected_is_subject = app.masks.stack.masks[mask_index]
            .components
            .get(component_index)
            .is_some_and(|component| {
                matches!(component.kind, MaskKind::Subject | MaskKind::Background)
            });
        let mut refinement_active = app.masks.subject_refinement_active && selected_is_subject;
        let mut refinement_size = app.masks.stack.subject_refinement.size;
        let mut refinement_feather = app.masks.stack.subject_refinement.feather;
        let mut refinement_flow = app.masks.stack.subject_refinement.flow;
        let mut clear_refinement = false;
        let mut local_curve_tab = app.develop_ui.tone_curve_tab;
        let mut local_color_grade_tab = app.develop_ui.color_grade_tab;
        let mut local_hsl_mixer_color = app.develop_ui.hsl_mixer_color;
        let birefnet_quality = app.ai.birefnet_quality;
        let birefnet_quality_change_enabled = app.birefnet_quality_change_enabled();

        {
            let mask = &mut app.masks.stack.masks[mask_index];
            if mask_effect_picker_visible(orientation, vertical_section) {
                effect_changed |= Self::show_mask_effect_picker(ui, &mut mask.effect);
                crate::ui::theme::card_gap(ui);
            }

            match orientation {
                MaskStripOrientation::Horizontal => {
                    Self::adjustment_section(ui, "Mask Properties", true, true, |ui| {
                        geometry_changed |= Self::show_vertical_mask_properties(
                            ui,
                            mask,
                            component_index,
                            &mut brush_mode,
                            (
                                &mut request_subject,
                                birefnet_quality,
                                birefnet_quality_change_enabled,
                            ),
                            (
                                &mut refinement_active,
                                &mut refinement_size,
                                &mut refinement_feather,
                                &mut refinement_flow,
                                &mut clear_refinement,
                            ),
                            &mut request_object,
                        );
                    });

                    if mask.effect.uses_adjustments() {
                        crate::ui::theme::toolbar_row(ui, |ui| {
                            ui.strong("Local Adjustments");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if crate::ui::icons::phosphor_icon_button(
                                        ui,
                                        egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                                        crate::ui::theme::toolbar_icon_size(),
                                        "Reset local adjustments",
                                    )
                                    .clicked()
                                    {
                                        mask.adjustments.reset();
                                        adjustments_changed = true;
                                    }
                                },
                            );
                        });
                        ui.add_space(4.0);

                        for (section, label, default_open) in [
                            (MaskSection::Light, "Light", true),
                            (MaskSection::ToneCurve, "Tone Curve", false),
                            (MaskSection::Color, "Color", false),
                            (MaskSection::ColorGrading, "Color Grading", false),
                            (MaskSection::Effects, "Effects", false),
                            (MaskSection::ColorMixer, "Color Mixer", false),
                        ] {
                            Self::adjustment_section(ui, label, default_open, true, |ui| {
                                let (section_changed, _) = Self::show_local_mask_adjustment_section(
                                    ui,
                                    &mut mask.adjustments,
                                    section,
                                    &mut local_curve_tab,
                                    &mut local_color_grade_tab,
                                    &mut local_hsl_mixer_color,
                                );
                                adjustments_changed |= section_changed;
                            });
                        }
                    } else {
                        adjustments_changed |= Self::show_mask_effect_settings(ui, mask);
                    }
                }
                MaskStripOrientation::Vertical => {
                    let mask_section = vertical_section.expect("vertical details have a section");
                    let section_title = match mask_section {
                        MaskSection::Properties => "Mask Properties",
                        MaskSection::Light => "Light",
                        MaskSection::ToneCurve => "Tone Curve",
                        MaskSection::Color => "Color",
                        MaskSection::ColorGrading => "Color Grading",
                        MaskSection::Effects => "Effects",
                        MaskSection::ColorMixer => "Color Mixer",
                    };
                    if mask.effect.uses_adjustments() {
                        crate::ui::theme::toolbar_row(ui, |ui| {
                            ui.strong(section_title);
                            if mask_section != MaskSection::Properties {
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if crate::ui::icons::phosphor_icon_button(
                                            ui,
                                            egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                                            crate::ui::theme::toolbar_icon_size(),
                                            "Reset local adjustments",
                                        )
                                        .clicked()
                                        {
                                            mask.adjustments.reset();
                                            adjustments_changed = true;
                                        }
                                    },
                                );
                            }
                        });
                        ui.add_space(4.0);

                        match mask_section {
                            MaskSection::Properties => {
                                Self::adjustment_section(
                                    ui,
                                    "Mask Properties",
                                    true,
                                    false,
                                    |ui| {
                                        geometry_changed |= Self::show_vertical_mask_properties(
                                            ui,
                                            mask,
                                            component_index,
                                            &mut brush_mode,
                                            (
                                                &mut request_subject,
                                                birefnet_quality,
                                                birefnet_quality_change_enabled,
                                            ),
                                            (
                                                &mut refinement_active,
                                                &mut refinement_size,
                                                &mut refinement_feather,
                                                &mut refinement_flow,
                                                &mut clear_refinement,
                                            ),
                                            &mut request_object,
                                        );
                                    },
                                );
                            }
                            section => {
                                Self::adjustment_section(ui, section_title, true, false, |ui| {
                                    let (section_changed, _) =
                                        Self::show_local_mask_adjustment_section(
                                            ui,
                                            &mut mask.adjustments,
                                            section,
                                            &mut local_curve_tab,
                                            &mut local_color_grade_tab,
                                            &mut local_hsl_mixer_color,
                                        );
                                    adjustments_changed |= section_changed;
                                });
                            }
                        }
                    } else {
                        Self::adjustment_section(ui, "Mask Properties", true, false, |ui| {
                            geometry_changed |= Self::show_vertical_mask_properties(
                                ui,
                                mask,
                                component_index,
                                &mut brush_mode,
                                (
                                    &mut request_subject,
                                    birefnet_quality,
                                    birefnet_quality_change_enabled,
                                ),
                                (
                                    &mut refinement_active,
                                    &mut refinement_size,
                                    &mut refinement_feather,
                                    &mut refinement_flow,
                                    &mut clear_refinement,
                                ),
                                &mut request_object,
                            );
                        });
                        adjustments_changed |= Self::show_mask_effect_settings(ui, mask);
                    }
                }
            }
        }

        app.develop_ui.tone_curve_tab = local_curve_tab;
        app.develop_ui.color_grade_tab = local_color_grade_tab;
        app.develop_ui.hsl_mixer_color = local_hsl_mixer_color;
        app.masks.brush_mode = brush_mode;
        app.masks.subject_refinement_active = refinement_active;
        let refinement_settings_changed = app.masks.stack.subject_refinement.size
            != refinement_size
            || app.masks.stack.subject_refinement.feather != refinement_feather
            || app.masks.stack.subject_refinement.flow != refinement_flow;
        app.masks.stack.subject_refinement.size = refinement_size;
        app.masks.stack.subject_refinement.feather = refinement_feather;
        app.masks.stack.subject_refinement.flow = refinement_flow;
        if clear_refinement && !app.masks.stack.subject_refinement.is_empty() {
            app.masks.stack.subject_refinement.clear();
            app.mark_all_mask_layers_dirty();
        } else if refinement_settings_changed {
            app.note_mask_edit_changed();
        }
        if request_subject {
            app.request_subject_mask(frame);
        }
        if request_object {
            app.request_object_mask(mask_index, component_index);
        }
        Self::apply_mask_geometry_change(ui, app, mask_index, geometry_changed);
        if effect_changed {
            app.develop_ui.mask_section = MaskSection::Properties;
            app.mark_mask_geometry_dirty(mask_index);
        }
        if adjustments_changed || effect_changed {
            app.mark_mask_adjustments_dirty();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{mask_effect_picker_visible, MaskSection, MaskStripOrientation};

    #[test]
    fn portrait_mask_type_only_appears_in_properties() {
        assert!(mask_effect_picker_visible(
            MaskStripOrientation::Vertical,
            Some(MaskSection::Properties),
        ));
        assert!(!mask_effect_picker_visible(
            MaskStripOrientation::Vertical,
            Some(MaskSection::Light),
        ));
        assert!(!mask_effect_picker_visible(
            MaskStripOrientation::Vertical,
            Some(MaskSection::Color),
        ));
        assert!(mask_effect_picker_visible(
            MaskStripOrientation::Horizontal,
            None,
        ));
    }
}
