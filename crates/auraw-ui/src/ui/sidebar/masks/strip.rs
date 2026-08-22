use super::*;

impl Sidebar {
    fn submask_drag_id() -> egui::Id {
        egui::Id::new("submask-component-drag")
    }

    pub(in crate::ui::sidebar) fn show_masks(ui: &mut Ui, app: &mut AurawApp, layout: ScreenLayout, frame: &eframe::Frame) {
        if app.ai.masks_need_update {
            crate::ui::theme::section_card(ui, "Masks need updating", |ui| {
                ui.label(
                    "The image used by existing masks changed. Refresh masks to rebuild content-aware masks and mask sources without deleting your edits.",
                );
                ui.add_space(4.0);
                if app.ai_mask_update_busy() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Refreshing masks…");
                    });
                } else if ui.button("Update masks").clicked() {
                    app.request_update_all_ai_masks(frame);
                }
            });
            ui.add_space(crate::ui::theme::CARD_GAP);
        }

        if app.masks.stack.masks.is_empty() {
            crate::ui::theme::section_card(ui, "No masks yet", |_| {});
            return;
        }

        match layout {
            ScreenLayout::Vertical => Self::show_masks_vertical_details(ui, app, frame),
            ScreenLayout::Horizontal => Self::show_masks_horizontal_details(ui, app, frame),
        }
    }

    pub(crate) fn show_vertical_mask_strip(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        Self::show_mask_strip(ui, app, frame, MaskStripOrientation::Horizontal);
    }

    pub(crate) fn show_horizontal_mask_strip(
        ui: &mut Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
    ) {
        Self::show_mask_strip(ui, app, frame, MaskStripOrientation::Vertical);
    }

    fn show_mask_strip(
        ui: &mut Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
        orientation: MaskStripOrientation,
    ) {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);

        app.masks.stack.ensure_selection();

        Self::refresh_mask_thumbnails(ui, app);

        let selected_mask_before = app.masks.stack.selected_mask;
        let selected_component_before = app.masks.stack.selected_component;
        let mut select_mask = None;
        let mut select_component = None;
        let mut new_mask = None;
        let mut add_component = None;
        let mut remove_mask = None;
        let mut remove_component = None;
        let mut duplicate_mask = None;
        let mut paste_mask = None;
        let mut duplicate_component = None;
        let mut paste_component = None;
        let mut group_enabled_changed = false;
        let mut group_geometry_dirty = None;
        let mut component_dirty_mask = None;
        let (pointer_pos, pointer_down, pointer_released) = ui.input(|input| {
            (
                input.pointer.interact_pos(),
                input.pointer.primary_down(),
                input.pointer.primary_released(),
            )
        });
        let mut submask_drag = ui
            .ctx()
            .data(|data| data.get_temp::<SubmaskDragState>(Self::submask_drag_id()));
        let displayed_drop_target = submask_drag.as_ref().and_then(|drag| drag.drop_target);
        if let Some(drag) = &mut submask_drag {
            drag.drop_target = None;
        }
        let mut hovered_group_this_frame = None;
        let mut hover_open_mask = None;

        {
            let mut show_cards = |ui: &mut Ui| {
                ui.add_enabled_ui(app.masks.stack.masks.len() < MAX_LOCAL_MASKS, |ui| {
                    Self::create_mask_group_card(ui, &mut new_mask, orientation);
                });
                ui.add_space(2.0);

                for index in (0..app.masks.stack.masks.len()).rev() {
                    let mask_name = app.masks.stack.masks[index].name.clone();
                    let mask_enabled = app.masks.stack.masks[index].enabled;
                    let component_count = app.masks.stack.masks[index].components.len();
                    let badge = component_count.to_string();
                    let response = Self::mask_thumbnail_card(
                        ui,
                        app.masks.thumbnail_group_textures.get(index),
                        &mask_name,
                        selected_mask_before == Some(index),
                        Some(&badge),
                        mask_enabled,
                        MaskCardSize::Group,
                    );
                    let can_add_group = app.masks.stack.masks.len() < MAX_LOCAL_MASKS;
                    #[cfg(target_os = "android")]
                    let overflow_clicked = {
                        let menu_id = ui.make_persistent_id(("android-mask-group-overflow", index));
                        crate::ui::android_overflow_menu(ui, response.rect, menu_id, 22.0, |ui| {
                            let mut geometry_changed = false;
                            Self::mask_group_context_menu(
                                ui,
                                &mut app.masks.stack.masks[index],
                                can_add_group,
                                &mut group_enabled_changed,
                                &mut geometry_changed,
                                &mut duplicate_mask,
                                &mut paste_mask,
                                &mut remove_mask,
                                index,
                            );
                            if geometry_changed {
                                group_geometry_dirty = Some(index);
                            }
                        })
                        .clicked()
                    };
                    #[cfg(not(target_os = "android"))]
                    let overflow_clicked = false;
                    if response.clicked() && !overflow_clicked {
                        select_mask = Some(index);
                    }
                    if let (Some(drag), Some(pointer)) = (&mut submask_drag, pointer_pos) {
                        if response.rect.contains(pointer) {
                            ui.painter().rect_stroke(
                                response.rect.shrink(1.0),
                                5.0,
                                egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                                egui::StrokeKind::Inside,
                            );
                            hovered_group_this_frame = Some(index);
                            drag.drop_target =
                                Some((index, app.masks.stack.masks[index].components.len()));
                            match drag.hover_group {
                                Some((hovered, started)) if hovered == index => {
                                    if started.elapsed() >= std::time::Duration::from_millis(650) {
                                        hover_open_mask = Some(index);
                                    }
                                }
                                _ => {
                                    drag.hover_group = Some((index, std::time::Instant::now()));
                                }
                            }
                        }
                    }
                    response.context_menu(|ui| {
                        let mut geometry_changed = false;
                        Self::mask_group_context_menu(
                            ui,
                            &mut app.masks.stack.masks[index],
                            can_add_group,
                            &mut group_enabled_changed,
                            &mut geometry_changed,
                            &mut duplicate_mask,
                            &mut paste_mask,
                            &mut remove_mask,
                            index,
                        );
                        if geometry_changed {
                            group_geometry_dirty = Some(index);
                        }
                    });

                    if selected_mask_before == Some(index) {
                        ui.add_space(1.0);
                        for component_index in 0..component_count {
                            if displayed_drop_target == Some((index, component_index)) {
                                let placeholder = Self::submask_drop_placeholder(ui);
                                if pointer_pos
                                    .is_some_and(|pointer| placeholder.rect.contains(pointer))
                                {
                                    if let Some(drag) = &mut submask_drag {
                                        drag.drop_target = displayed_drop_target;
                                        drag.hover_group = None;
                                    }
                                }
                            }
                            let component = &app.masks.stack.masks[index].components[component_index];
                            let component_name = component.name.clone();
                            let component_enabled = component.enabled;
                            let component_badge =
                                mask_component_badge(component_index, component.combine);
                            let source_is_dragging = submask_drag.as_ref().is_some_and(|drag| {
                                drag.source_mask == index
                                    && drag.source_component == component_index
                            });
                            if source_is_dragging {
                                continue;
                            }
                            let response = Self::mask_thumbnail_card(
                                ui,
                                app.masks.thumbnail_component_textures.get(component_index),
                                &component_name,
                                selected_component_before == Some(component_index),
                                Some(component_badge),
                                component_enabled,
                                MaskCardSize::Submask,
                            );
                            let component_can_drag = component_count > 1;
                            if component_can_drag && response.is_pointer_button_down_on() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                            }
                            let mut menu_geometry_changed = false;
                            let mut menu_remove_component = None;
                            #[cfg(target_os = "android")]
                            let overflow_clicked = {
                                let menu_id = ui.make_persistent_id((
                                    "android-submask-overflow",
                                    index,
                                    component_index,
                                ));
                                crate::ui::android_overflow_menu(
                                    ui,
                                    response.rect,
                                    menu_id,
                                    20.0,
                                    |ui| {
                                        Self::submask_context_menu(
                                            ui,
                                            &mut app.masks.stack.masks[index].components[component_index],
                                            component_count > 1,
                                            component_count < MAX_MASK_COMPONENTS,
                                            &mut menu_geometry_changed,
                                            &mut duplicate_component,
                                            &mut paste_component,
                                            &mut menu_remove_component,
                                            index,
                                            component_index,
                                        );
                                    },
                                )
                                .clicked()
                            };
                            #[cfg(not(target_os = "android"))]
                            let overflow_clicked = false;
                            if response.clicked() && !overflow_clicked {
                                select_component = Some(component_index);
                            }
                            if response.drag_started() && component_can_drag {
                                submask_drag = Some(SubmaskDragState {
                                    source_mask: index,
                                    source_component: component_index,
                                    source_texture: app.masks.thumbnail_component_textures
                                        .get(component_index)
                                        .cloned(),
                                    source_name: component_name.clone(),
                                    source_badge: component_badge.to_owned(),
                                    source_enabled: component_enabled,
                                    hover_group: None,
                                    drop_target: Some((index, component_index)),
                                    target_loss_started: None,
                                });
                            }
                            if let (Some(drag), Some(pointer)) = (&mut submask_drag, pointer_pos) {
                                if response.rect.contains(pointer) {
                                    ui.painter().rect_stroke(
                                        response.rect.shrink(1.0),
                                        5.0,
                                        egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                                        egui::StrokeKind::Inside,
                                    );
                                    let before = match orientation {
                                        MaskStripOrientation::Horizontal => {
                                            pointer.x < response.rect.center().x
                                        }
                                        MaskStripOrientation::Vertical => {
                                            pointer.y < response.rect.center().y
                                        }
                                    };
                                    drag.drop_target =
                                        Some((index, component_index + usize::from(!before)));
                                    drag.hover_group = None;
                                }
                            }
                            response.context_menu(|ui| {
                                Self::submask_context_menu(
                                    ui,
                                    &mut app.masks.stack.masks[index].components[component_index],
                                    component_count > 1,
                                    component_count < MAX_MASK_COMPONENTS,
                                    &mut menu_geometry_changed,
                                    &mut duplicate_component,
                                    &mut paste_component,
                                    &mut menu_remove_component,
                                    index,
                                    component_index,
                                );
                            });
                            if menu_geometry_changed {
                                component_dirty_mask = Some(index);
                            }
                            if let Some(component_index) = menu_remove_component {
                                remove_component = Some((index, component_index));
                            }
                        }
                        if displayed_drop_target.is_some_and(|(mask, insert)| {
                            mask == index && insert >= component_count
                        }) {
                            let placeholder = Self::submask_drop_placeholder(ui);
                            if pointer_pos.is_some_and(|pointer| placeholder.rect.contains(pointer))
                            {
                                if let Some(drag) = &mut submask_drag {
                                    drag.drop_target = displayed_drop_target;
                                    drag.hover_group = None;
                                }
                            }
                        }
                        Self::create_submask_card(ui, &mut add_component, orientation);
                        ui.add_space(2.0);
                    }
                }
            };

            match orientation {
                MaskStripOrientation::Horizontal => {
                    egui::ScrollArea::horizontal()
                        .id_salt("vertical-mask-card-strip")
                        .scroll_source(mask_strip_scroll_source())
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| show_cards(ui));
                        });
                }
                MaskStripOrientation::Vertical => {
                    egui::ScrollArea::vertical()
                        .id_salt("horizontal-mask-card-strip")
                        .scroll_source(mask_strip_scroll_source())
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| show_cards(ui));
                        });
                }
            }
        }

        if let Some(drag) = &mut submask_drag {
            if drag.drop_target.is_some() {
                drag.target_loss_started = None;
            } else if let Some(previous_target) = displayed_drop_target {
                let lost_at = drag
                    .target_loss_started
                    .get_or_insert_with(std::time::Instant::now);
                if lost_at.elapsed() < std::time::Duration::from_millis(120) {
                    drag.drop_target = Some(previous_target);
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(16));
                }
            }
            if hovered_group_this_frame.is_none() && drag.drop_target.is_none() {
                drag.hover_group = None;
            }
            if drag.drop_target.is_none() {
                if let Some(pointer) = pointer_pos {
                    Self::paint_floating_submask(ui, drag, pointer);
                }
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Some(index) = hover_open_mask {
            if app.masks.stack.select_mask(index) {
                app.masks.thumbnail_component_mask = None;
                ui.ctx().request_repaint();
            }
        }

        let component_drop = if pointer_released {
            submask_drag
                .take()
                .and_then(|drag| drag.drop_target.map(|target| (drag, target)))
        } else {
            None
        };
        if !pointer_down && !pointer_released {
            submask_drag = None;
        }
        ui.ctx().data_mut(|data| {
            if let Some(drag) = submask_drag.clone() {
                data.insert_temp(Self::submask_drag_id(), drag);
            } else {
                data.remove::<SubmaskDragState>(Self::submask_drag_id());
            }
        });

        if group_enabled_changed {
            app.mark_mask_adjustments_dirty();
        }
        if let Some(mask_index) = group_geometry_dirty {
            app.mark_mask_geometry_dirty(mask_index);
        }
        if let Some(mask_index) = component_dirty_mask {
            app.mark_mask_geometry_dirty(mask_index);
        }

        if let Some((drag, (target_mask, target_insert))) = component_drop {
            if app
                .masks
                .stack
                .move_submask_component(
                    drag.source_mask,
                    drag.source_component,
                    target_mask,
                    target_insert,
                )
                .is_some()
            {
                app.mark_all_mask_layers_dirty();
                app.sync_selected_mask_tool();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(index) = remove_mask {
            if app.masks.stack.delete_mask(index) {
                app.mark_all_mask_layers_dirty();
                app.sync_selected_mask_tool();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some((index, invert)) = duplicate_mask {
            if Self::duplicate_mask_group(app, index, invert) {
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(index) = paste_mask {
            if Self::paste_mask_group(ui.ctx(), app, index) {
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some((mask_index, component_index)) = remove_component {
            if app.masks.stack.delete_component(mask_index, component_index) {
                app.mark_mask_geometry_dirty(mask_index);
                app.sync_selected_mask_tool();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some((mask_index, component_index, invert)) = duplicate_component {
            if Self::duplicate_mask_component(app, mask_index, component_index, invert) {
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some((mask_index, component_index)) = paste_component {
            if Self::paste_mask_component(ui.ctx(), app, mask_index, component_index) {
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(kind) = new_mask {
            if let Some((mask_index, _)) = app.masks.stack.add_mask(kind) {
                app.activate_mask_tool(kind);
                Self::prepare_content_mask(app, frame, kind);
                app.mark_mask_geometry_dirty(mask_index);
                app.masks.thumbnail_component_mask = None;
                app.blink_selected_mask();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some((kind, combine)) = add_component {
            if let Some((mask_index, _)) = app.masks.stack.add_component(kind, combine) {
                app.activate_mask_tool(kind);
                Self::prepare_content_mask(app, frame, kind);
                app.mark_mask_geometry_dirty(mask_index);
                app.masks.thumbnail_component_mask = None;
                app.blink_selected_component();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(index) = select_mask {
            if app.masks.stack.select_mask(index) {
                app.sync_selected_mask_tool();
                app.blink_selected_mask();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(component_index) = select_component {
            if let Some(mask_index) = app.masks.stack.selected_mask {
                if app.masks.stack.select_component(mask_index, component_index) {
                    app.sync_selected_mask_tool();
                    app.blink_selected_component();
                }
            }
        }

        Self::show_mask_rename_dialog(ui.ctx(), app);
    }
}
