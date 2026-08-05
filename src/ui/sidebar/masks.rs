fn mask_component_badge(component_index: usize, combine: MaskCombineMode) -> &'static str {
    if component_index == 0 {
        "BASE"
    } else {
        match combine {
            MaskCombineMode::Add => egui_phosphor::regular::PLUS,
            MaskCombineMode::Subtract => egui_phosphor::regular::MINUS,
            MaskCombineMode::Intersect => egui_phosphor::regular::INTERSECT,
        }
    }
}

fn mask_creation_icon() -> &'static str {
    egui_phosphor::regular::PLUS
}

fn mask_strip_scroll_source() -> egui::scroll_area::ScrollSource {
    if cfg!(target_os = "android") {
        // Force content-drag scrolling for touch and stylus input. Card widgets
        // intentionally use click-only sense on Android so they cannot steal it.
        egui::scroll_area::ScrollSource::ALL
    } else {
        egui::scroll_area::ScrollSource::default()
    }
}

#[derive(Clone, Debug)]
enum MaskRenameTarget {
    Group(usize),
    Component {
        mask_index: usize,
        component_index: usize,
    },
}

#[derive(Clone, Debug)]
struct MaskRenameDialog {
    target: MaskRenameTarget,
    name: String,
    request_focus: bool,
}

#[derive(Clone)]
struct SubmaskDragState {
    source_mask: usize,
    source_component: usize,
    source_texture: Option<egui::TextureHandle>,
    source_name: String,
    source_badge: String,
    source_enabled: bool,
    hover_group: Option<(usize, std::time::Instant)>,
    drop_target: Option<(usize, usize)>,
    target_loss_started: Option<std::time::Instant>,
}

impl Sidebar {
    fn submask_drag_id() -> egui::Id {
        egui::Id::new("submask-component-drag")
    }

    fn show_masks(ui: &mut Ui, app: &mut AurawApp, layout: ScreenLayout, frame: &eframe::Frame) {
        if app.ai_masks_need_update() {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
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
            ui.add_space(6.0);
        }

        if app.masks.masks.is_empty() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.weak("No masks created yet.");
                ui.weak("Use the + card in the mask strip to create one.");
            });
            return;
        }

        // Portrait keeps the compact fixed tabs. Wider screens expose all
        // mask controls as normal collapsible sections, matching the desktop
        // adjustment sidebar while the thumbnail strip remains beside it.
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

        if app.masks.masks.is_empty() {
            app.masks.selected_mask = None;
            app.masks.selected_component = None;
        } else if app
            .masks
            .selected_mask
            .is_none_or(|index| index >= app.masks.masks.len())
        {
            app.masks.selected_mask = Some(app.masks.masks.len().saturating_sub(1));
            app.masks.selected_component = Some(0);
        }

        Self::refresh_mask_thumbnails(ui, app);

        let selected_mask_before = app.masks.selected_mask;
        let selected_component_before = app.masks.selected_component;
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
        // Layout uses the target found on the previous frame to reserve a real
        // card-sized insertion slot. Pointer movement schedules another frame,
        // so this follows the before/after half of the hovered card without
        // trying to mutate an already-built egui layout.
        let displayed_drop_target = submask_drag.as_ref().and_then(|drag| drag.drop_target);
        if let Some(drag) = &mut submask_drag {
            drag.drop_target = None;
        }
        let mut hovered_group_this_frame = None;
        let mut hover_open_mask = None;

        {
            let mut show_cards = |ui: &mut Ui| {
                ui.add_enabled_ui(app.masks.masks.len() < MAX_LOCAL_MASKS, |ui| {
                    Self::create_mask_group_card(ui, &mut new_mask, orientation);
                });
                ui.add_space(2.0);

                for index in (0..app.masks.masks.len()).rev() {
                    let mask_name = app.masks.masks[index].name.clone();
                    let mask_enabled = app.masks.masks[index].enabled;
                    let component_count = app.masks.masks[index].components.len();
                    let badge = component_count.to_string();
                    let response = Self::mask_thumbnail_card(
                        ui,
                        app.mask_thumbnail_group_textures.get(index),
                        &mask_name,
                        selected_mask_before == Some(index),
                        Some(&badge),
                        mask_enabled,
                        MaskCardSize::Group,
                    );
                    let can_add_group = app.masks.masks.len() < MAX_LOCAL_MASKS;
                    #[cfg(target_os = "android")]
                    let overflow_clicked = {
                        let menu_id = ui.make_persistent_id(("android-mask-group-overflow", index));
                        crate::ui::android_overflow_menu(ui, response.rect, menu_id, 22.0, |ui| {
                            let mut geometry_changed = false;
                            Self::mask_group_context_menu(
                                ui,
                                &mut app.masks.masks[index],
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
                                Some((index, app.masks.masks[index].components.len()));
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
                            &mut app.masks.masks[index],
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

                    // The selected group's sub-masks are inserted directly
                    // after the parent. That means to its right in portrait
                    // mode and directly below it in the desktop vertical strip.
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
                            let component = &app.masks.masks[index].components[component_index];
                            let component_name = component.name.clone();
                            let component_enabled = component.enabled;
                            let component_badge =
                                mask_component_badge(component_index, component.combine);
                            let source_is_dragging = submask_drag.as_ref().is_some_and(|drag| {
                                drag.source_mask == index
                                    && drag.source_component == component_index
                            });
                            if source_is_dragging {
                                // Do not reserve an invisible source slot: the
                                // neighboring cards should immediately close the
                                // gap while the floating card represents it.
                                continue;
                            }
                            let response = Self::mask_thumbnail_card(
                                ui,
                                app.mask_thumbnail_component_textures.get(component_index),
                                &component_name,
                                selected_component_before == Some(component_index),
                                Some(component_badge),
                                component_enabled,
                                MaskCardSize::Submask,
                            );
                            let component_can_drag = component_count > 1;
                            // `drag_started` only becomes true after the pointer
                            // travels beyond egui's drag threshold. Give immediate
                            // feedback while the primary button is merely held so
                            // the component already feels ready to move.
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
                                            &mut app.masks.masks[index].components[component_index],
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
                                    source_texture: app
                                        .mask_thumbnail_component_textures
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
                                    &mut app.masks.masks[index].components[component_index],
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
                    // Crossing the small layout boundary between a placeholder
                    // and its neighbor can produce one frame with no hit. Keep
                    // the last slot briefly so the red preview does not flash.
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
            app.masks.selected_mask = Some(index);
            app.masks.selected_component = Some(0);
            app.mask_thumbnail_component_mask = None;
            ui.ctx().request_repaint();
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
                .move_submask_component(
                    drag.source_mask,
                    drag.source_component,
                    target_mask,
                    target_insert,
                )
                .is_some()
            {
                app.mark_all_mask_layers_dirty();
                app.mask_thumbnail_component_mask = None;
                if let Some(kind) = app
                    .masks
                    .selected_component()
                    .map(|component| component.kind)
                {
                    app.select_mask_tool(kind);
                }
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(index) = remove_mask {
            app.masks.selected_mask = Some(index);
            app.masks.remove_selected_mask();
            app.mark_all_mask_layers_dirty();
            app.mask_thumbnail_component_mask = None;
            if let Some(kind) = app
                .masks
                .selected_component()
                .map(|component| component.kind)
            {
                app.select_mask_tool(kind);
            } else {
                app.active_mask_tool = None;
            }
            Self::refresh_mask_thumbnails(ui, app);
        } else if let Some((index, invert)) = duplicate_mask {
            if Self::duplicate_mask_group(app, index, invert) {
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(index) = paste_mask {
            if Self::paste_mask_group(ui.ctx(), app, index) {
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some((mask_index, component_index)) = remove_component {
            app.masks.selected_mask = Some(mask_index);
            app.masks.selected_component = Some(component_index);
            if app.masks.remove_selected_component().is_some() {
                app.mark_mask_geometry_dirty(mask_index);
                app.mask_thumbnail_component_mask = None;
                if let Some(kind) = app
                    .masks
                    .selected_component()
                    .map(|component| component.kind)
                {
                    app.select_mask_tool(kind);
                }
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
            if let Some((mask_index, _)) = app.masks.add_mask(kind) {
                app.activate_mask_tool(kind);
                Self::prepare_content_mask(app, frame, kind);
                app.mark_mask_geometry_dirty(mask_index);
                app.mask_thumbnail_component_mask = None;
                app.blink_selected_mask();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some((kind, combine)) = add_component {
            if let Some((mask_index, _)) = app.masks.add_component(kind, combine) {
                app.activate_mask_tool(kind);
                Self::prepare_content_mask(app, frame, kind);
                app.mark_mask_geometry_dirty(mask_index);
                app.mask_thumbnail_component_mask = None;
                app.blink_selected_component();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(index) = select_mask {
            app.masks.selected_mask = Some(index);
            app.masks.selected_component = Some(0);
            app.mask_thumbnail_component_mask = None;
            if let Some(kind) = app
                .masks
                .selected_component()
                .map(|component| component.kind)
            {
                app.select_mask_tool(kind);
            }
            app.blink_selected_mask();
            Self::refresh_mask_thumbnails(ui, app);
        } else if let Some(component_index) = select_component {
            app.masks.selected_component = Some(component_index);
            if let Some(kind) = app
                .masks
                .selected_component()
                .map(|component| component.kind)
            {
                app.select_mask_tool(kind);
            }
            app.blink_selected_component();
        }

        Self::show_mask_rename_dialog(ui.ctx(), app);
    }

    fn mask_kind_menu(ui: &mut Ui, unavailable_message: &str) -> Option<MaskKind> {
        let mut selected = None;
        for kind in [
            MaskKind::Brush,
            MaskKind::Radial,
            MaskKind::Linear,
            MaskKind::Subject,
            MaskKind::Background,
            MaskKind::Object,
            MaskKind::Landscape,
            MaskKind::LuminanceRange,
            MaskKind::ColorRange,
            MaskKind::DepthRange,
        ] {
            let label = if kind.is_available() {
                kind.label().to_owned()
            } else {
                format!("{} · soon", kind.label())
            };
            if ui
                .add_enabled(kind.is_available(), egui::Button::new(label))
                .on_disabled_hover_text(unavailable_message)
                .clicked()
            {
                selected = Some(kind);
                ui.close();
            }
        }
        selected
    }

    fn submask_creation_menu(
        ui: &mut Ui,
        unavailable_message: &str,
    ) -> Option<(MaskKind, MaskCombineMode)> {
        let mut selected = None;
        ui.label(egui::RichText::new("Combine as").weak());
        for combine in [
            MaskCombineMode::Add,
            MaskCombineMode::Subtract,
            MaskCombineMode::Intersect,
        ] {
            ui.menu_button(combine.label(), |ui| {
                if let Some(kind) = Self::mask_kind_menu(ui, unavailable_message) {
                    selected = Some((kind, combine));
                }
            });
        }
        selected
    }

    #[allow(clippy::too_many_arguments)]
    fn mask_group_context_menu(
        ui: &mut Ui,
        mask: &mut LocalMask,
        can_add_group: bool,
        enabled_changed: &mut bool,
        geometry_changed: &mut bool,
        duplicate_mask: &mut Option<(usize, bool)>,
        paste_mask: &mut Option<usize>,
        remove_mask: &mut Option<usize>,
        mask_index: usize,
    ) {
        if ui.button("Rename…").clicked() {
            Self::open_mask_rename_dialog(
                ui.ctx(),
                MaskRenameTarget::Group(mask_index),
                mask.name.clone(),
            );
            ui.close();
        }
        ui.separator();
        *enabled_changed |= ui.checkbox(&mut mask.enabled, "Enabled").changed();
        if ui
            .add_enabled(can_add_group, egui::Button::new("Duplicate"))
            .clicked()
        {
            *duplicate_mask = Some((mask_index, false));
            ui.close();
        }
        if ui.selectable_label(mask.invert, "Invert").clicked() {
            mask.invert = !mask.invert;
            *geometry_changed = true;
            ui.close();
        }
        if ui
            .add_enabled(can_add_group, egui::Button::new("Duplicate & Invert"))
            .clicked()
        {
            *duplicate_mask = Some((mask_index, true));
            ui.close();
        }
        ui.separator();
        if ui.button("Copy Mask Group").clicked() {
            ui.ctx().data_mut(|data| {
                data.insert_temp(Self::mask_group_clipboard_id(), mask.clone());
            });
            ui.close();
        }
        let can_paste = can_add_group && Self::copied_mask_group(ui.ctx()).is_some();
        if ui
            .add_enabled(can_paste, egui::Button::new("Paste Mask Group"))
            .on_disabled_hover_text("Copy a mask group first")
            .clicked()
        {
            *paste_mask = Some(mask_index);
            ui.close();
        }
        ui.separator();
        if ui
            .button(format!(
                "{}  Delete mask group",
                egui_phosphor::regular::TRASH
            ))
            .clicked()
        {
            *remove_mask = Some(mask_index);
            ui.close();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn submask_context_menu(
        ui: &mut Ui,
        component: &mut MaskComponent,
        can_delete: bool,
        can_add_component: bool,
        geometry_changed: &mut bool,
        duplicate_component: &mut Option<(usize, usize, bool)>,
        paste_component: &mut Option<(usize, usize)>,
        remove_component: &mut Option<usize>,
        mask_index: usize,
        component_index: usize,
    ) {
        if ui.button("Rename…").clicked() {
            Self::open_mask_rename_dialog(
                ui.ctx(),
                MaskRenameTarget::Component {
                    mask_index,
                    component_index,
                },
                component.name.clone(),
            );
            ui.close();
        }
        ui.separator();
        *geometry_changed |= ui.checkbox(&mut component.enabled, "Enabled").changed();
        if ui
            .add_enabled(can_add_component, egui::Button::new("Duplicate"))
            .clicked()
        {
            *duplicate_component = Some((mask_index, component_index, false));
            ui.close();
        }
        if ui.selectable_label(component.invert, "Invert").clicked() {
            component.invert = !component.invert;
            *geometry_changed = true;
            ui.close();
        }
        if ui
            .add_enabled(can_add_component, egui::Button::new("Duplicate & Invert"))
            .clicked()
        {
            *duplicate_component = Some((mask_index, component_index, true));
            ui.close();
        }
        ui.separator();
        if ui.button("Copy Component").clicked() {
            ui.ctx().data_mut(|data| {
                data.insert_temp(Self::mask_component_clipboard_id(), component.clone());
            });
            ui.close();
        }
        let can_paste = can_add_component && Self::copied_mask_component(ui.ctx()).is_some();
        if ui
            .add_enabled(can_paste, egui::Button::new("Paste Component"))
            .on_disabled_hover_text("Copy a component first")
            .clicked()
        {
            *paste_component = Some((mask_index, component_index));
            ui.close();
        }
        ui.separator();
        if ui
            .add_enabled(
                can_delete,
                egui::Button::new(format!(
                    "{}  Delete sub-mask",
                    egui_phosphor::regular::TRASH
                )),
            )
            .on_disabled_hover_text("A mask group must contain at least one sub-mask")
            .clicked()
        {
            *remove_component = Some(component_index);
            ui.close();
        }
    }

    fn mask_group_clipboard_id() -> egui::Id {
        egui::Id::new("mask-group-clipboard")
    }

    fn mask_component_clipboard_id() -> egui::Id {
        egui::Id::new("mask-component-clipboard")
    }

    fn mask_rename_dialog_id() -> egui::Id {
        egui::Id::new("mask-rename-dialog-state")
    }

    fn copied_mask_group(ctx: &egui::Context) -> Option<LocalMask> {
        ctx.data(|data| data.get_temp::<LocalMask>(Self::mask_group_clipboard_id()))
    }

    fn copied_mask_component(ctx: &egui::Context) -> Option<MaskComponent> {
        ctx.data(|data| data.get_temp::<MaskComponent>(Self::mask_component_clipboard_id()))
    }

    fn open_mask_rename_dialog(ctx: &egui::Context, target: MaskRenameTarget, name: String) {
        ctx.data_mut(|data| {
            data.insert_temp(
                Self::mask_rename_dialog_id(),
                MaskRenameDialog {
                    target,
                    name,
                    request_focus: true,
                },
            );
        });
    }

    fn show_mask_rename_dialog(ctx: &egui::Context, app: &mut AurawApp) {
        let Some(mut dialog) =
            ctx.data(|data| data.get_temp::<MaskRenameDialog>(Self::mask_rename_dialog_id()))
        else {
            return;
        };

        let title = match &dialog.target {
            MaskRenameTarget::Group(_) => "Rename mask group",
            MaskRenameTarget::Component { .. } => "Rename sub-mask",
        };
        let mut save = false;
        let mut cancel = false;
        crate::ui::responsive_popup(egui::Window::new(title), ctx, 360.0)
            .id(egui::Id::new("mask-rename-dialog-window"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                let response = ui.add_sized(
                    [ui.available_width(), ui.spacing().interact_size.y],
                    egui::TextEdit::singleline(&mut dialog.name),
                );
                if dialog.request_focus {
                    response.request_focus();
                    dialog.request_focus = false;
                }
                let trimmed_is_empty = dialog.name.trim().is_empty();
                let enter_pressed = ui.input(|input| input.key_pressed(egui::Key::Enter));
                let escape_pressed = ui.input(|input| input.key_pressed(egui::Key::Escape));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!trimmed_is_empty, egui::Button::new("Rename"))
                        .clicked()
                        || (enter_pressed && !trimmed_is_empty)
                    {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() || escape_pressed {
                        cancel = true;
                    }
                });
            });

        if save {
            let name = dialog.name.trim().to_owned();
            let changed = match dialog.target {
                MaskRenameTarget::Group(mask_index) => {
                    app.masks.masks.get_mut(mask_index).is_some_and(|mask| {
                        if mask.name == name {
                            false
                        } else {
                            mask.name = name.clone();
                            true
                        }
                    })
                }
                MaskRenameTarget::Component {
                    mask_index,
                    component_index,
                } => app
                    .masks
                    .masks
                    .get_mut(mask_index)
                    .and_then(|mask| mask.components.get_mut(component_index))
                    .is_some_and(|component| {
                        if component.name == name {
                            false
                        } else {
                            component.name = name.clone();
                            true
                        }
                    }),
            };
            if changed {
                app.note_mask_edit_changed();
            }
            ctx.data_mut(|data| data.remove::<MaskRenameDialog>(Self::mask_rename_dialog_id()));
        } else if cancel {
            ctx.data_mut(|data| data.remove::<MaskRenameDialog>(Self::mask_rename_dialog_id()));
        } else {
            ctx.data_mut(|data| data.insert_temp(Self::mask_rename_dialog_id(), dialog));
        }
    }

    fn duplicate_mask_group(app: &mut AurawApp, mask_index: usize, invert: bool) -> bool {
        let Some(mask) = app.masks.masks.get(mask_index).cloned() else {
            return false;
        };
        Self::insert_mask_group_copy(app, mask_index, mask, invert)
    }

    fn paste_mask_group(ctx: &egui::Context, app: &mut AurawApp, mask_index: usize) -> bool {
        let Some(mask) = Self::copied_mask_group(ctx) else {
            return false;
        };
        Self::insert_mask_group_copy(app, mask_index, mask, false)
    }

    fn insert_mask_group_copy(
        app: &mut AurawApp,
        mask_index: usize,
        mut mask: LocalMask,
        invert: bool,
    ) -> bool {
        if app.masks.masks.len() >= MAX_LOCAL_MASKS || mask_index >= app.masks.masks.len() {
            return false;
        }
        mask.name = Self::copied_mask_name(&app.masks.masks, &mask.name);
        if invert {
            mask.invert = !mask.invert;
            // "Duplicate & Invert" is used to build a complementary mask,
            // not a second copy of the same local grade. Start the new mask
            // with neutral adjustments so only its coverage is duplicated.
            mask.adjustments.reset();
        }
        let insert_at = mask_index + 1;
        let mut candidate = app.masks.clone();
        candidate.masks.insert(insert_at, mask);
        candidate.selected_mask = Some(insert_at);
        candidate.selected_component = Some(0);
        if let Err(error) =
            crate::sidecar::preflight_mask_change(&candidate, &app.inpaint_strokes)
        {
            app.report_mask_persistence_limit("Mask-group copy", &error);
            return false;
        }
        app.masks = candidate;
        app.mask_thumbnail_component_mask = None;
        app.mark_all_mask_layers_dirty();
        if let Some(kind) = app
            .masks
            .selected_component()
            .map(|component| component.kind)
        {
            app.select_mask_tool(kind);
        }
        app.blink_selected_mask();
        true
    }

    fn duplicate_mask_component(
        app: &mut AurawApp,
        mask_index: usize,
        component_index: usize,
        invert: bool,
    ) -> bool {
        let Some(component) = app
            .masks
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .cloned()
        else {
            return false;
        };
        Self::insert_mask_component_copy(app, mask_index, component_index, component, invert)
    }

    fn paste_mask_component(
        ctx: &egui::Context,
        app: &mut AurawApp,
        mask_index: usize,
        component_index: usize,
    ) -> bool {
        let Some(component) = Self::copied_mask_component(ctx) else {
            return false;
        };
        Self::insert_mask_component_copy(app, mask_index, component_index, component, false)
    }

    fn insert_mask_component_copy(
        app: &mut AurawApp,
        mask_index: usize,
        component_index: usize,
        mut component: MaskComponent,
        invert: bool,
    ) -> bool {
        let Some(mask) = app.masks.masks.get(mask_index) else {
            return false;
        };
        if mask.components.len() >= MAX_MASK_COMPONENTS || component_index >= mask.components.len()
        {
            return false;
        }
        component.name = Self::copied_component_name(&mask.components, &component.name);
        if invert {
            component.invert = !component.invert;
        }
        let insert_at = component_index + 1;
        let mut candidate = app.masks.clone();
        candidate.masks[mask_index]
            .components
            .insert(insert_at, component);
        candidate.selected_mask = Some(mask_index);
        candidate.selected_component = Some(insert_at);
        if let Err(error) =
            crate::sidecar::preflight_mask_change(&candidate, &app.inpaint_strokes)
        {
            app.report_mask_persistence_limit("Sub-mask copy", &error);
            return false;
        }
        app.masks = candidate;
        app.mask_thumbnail_component_mask = None;
        app.mark_mask_geometry_dirty(mask_index);
        if let Some(kind) = app
            .masks
            .selected_component()
            .map(|component| component.kind)
        {
            app.select_mask_tool(kind);
        }
        app.blink_selected_component();
        true
    }

    fn copied_mask_name(masks: &[LocalMask], base: &str) -> String {
        for number in 1..=10_000usize {
            let candidate = if number == 1 {
                format!("{base} Copy")
            } else {
                format!("{base} Copy {number}")
            };
            if masks.iter().all(|mask| mask.name != candidate) {
                return candidate;
            }
        }
        format!("{base} Copy")
    }

    fn copied_component_name(components: &[MaskComponent], base: &str) -> String {
        for number in 1..=10_000usize {
            let candidate = if number == 1 {
                format!("{base} Copy")
            } else {
                format!("{base} Copy {number}")
            };
            if components
                .iter()
                .all(|component| component.name != candidate)
            {
                return candidate;
            }
        }
        format!("{base} Copy")
    }

    fn create_mask_group_card(
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

    fn create_submask_card(
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

    fn submask_drop_placeholder(ui: &mut Ui) -> egui::Response {
        use eframe::egui::{Align2, Color32, FontId, Stroke, StrokeKind};

        let (rect, response) =
            ui.allocate_exact_size(MaskCardSize::Submask.card_size(), egui::Sense::hover());
        let red = Color32::from_rgb(225, 62, 62);
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

    fn paint_floating_submask(ui: &Ui, drag: &SubmaskDragState, pointer: egui::Pos2) {
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

    fn show_masks_horizontal_details(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        let Some(mask_index) = app.masks.selected_mask else {
            return;
        };
        if mask_index >= app.masks.masks.len() {
            return;
        }

        ui.label(
            egui::RichText::new(app.masks.masks[mask_index].name.clone())
                .strong()
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(5.0);

        let component_index = app.masks.selected_component.unwrap_or(0).min(
            app.masks.masks[mask_index]
                .components
                .len()
                .saturating_sub(1),
        );
        app.masks.selected_component = Some(component_index);

        let mut geometry_changed = false;
        let mut adjustments_changed = false;
        let mut request_subject = false;
        let mut request_object = false;
        let mut request_landscape = false;
        let mut brush_mode = app.brush_mode;
        let mut local_curve_tab = app.tone_curve_tab;
        let mut local_color_grade_tab = app.color_grade_tab;

        {
            let mask = &mut app.masks.masks[mask_index];

            Self::adjustment_section(ui, "Mask Properties", true, true, |ui| {
                geometry_changed |= Self::show_vertical_mask_properties(
                    ui,
                    mask,
                    component_index,
                    &mut brush_mode,
                    &mut request_subject,
                    &mut request_object,
                    &mut request_landscape,
                );
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.strong("Local Adjustments");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::ui::icons::phosphor_icon_button(
                        ui,
                        egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                        egui::vec2(28.0, 22.0),
                        "Reset local adjustments",
                    )
                    .clicked()
                    {
                        mask.adjustments.reset();
                        adjustments_changed = true;
                    }
                });
            });

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
                    );
                    adjustments_changed |= section_changed;
                });
            }
        }

        app.tone_curve_tab = local_curve_tab;
        app.color_grade_tab = local_color_grade_tab;
        app.brush_mode = brush_mode;
        if request_subject {
            app.request_subject_mask(frame);
        }
        if request_object {
            app.request_object_mask(mask_index, component_index);
        }
        if request_landscape {
            app.request_landscape_mask(frame, mask_index, component_index);
        }
        Self::apply_mask_geometry_change(ui, app, mask_index, geometry_changed);
        if adjustments_changed {
            app.mark_mask_adjustments_dirty();
        }
    }

    fn show_masks_vertical_details(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        let Some(mask_index) = app.masks.selected_mask else {
            return;
        };
        if mask_index >= app.masks.masks.len() {
            return;
        }

        ui.label(
            egui::RichText::new(app.masks.masks[mask_index].name.clone())
                .strong()
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(5.0);

        let mask_section = app.mask_section;
        let component_index = app.masks.selected_component.unwrap_or(0).min(
            app.masks.masks[mask_index]
                .components
                .len()
                .saturating_sub(1),
        );
        app.masks.selected_component = Some(component_index);

        let mut geometry_changed = false;
        let mut adjustments_changed = false;
        let mut request_subject = false;
        let mut request_object = false;
        let mut request_landscape = false;
        let mut brush_mode = app.brush_mode;
        let mut local_curve_tab = app.tone_curve_tab;
        let mut local_color_grade_tab = app.color_grade_tab;

        {
            let mask = &mut app.masks.masks[mask_index];
            match mask_section {
                MaskSection::Properties => {
                    geometry_changed |= Self::show_vertical_mask_properties(
                        ui,
                        mask,
                        component_index,
                        &mut brush_mode,
                        &mut request_subject,
                        &mut request_object,
                        &mut request_landscape,
                    );
                }
                section => {
                    ui.horizontal(|ui| {
                        ui.strong(match section {
                            MaskSection::Light => "Light",
                            MaskSection::ToneCurve => "Tone Curve",
                            MaskSection::Color => "Color",
                            MaskSection::ColorGrading => "Color Grading",
                            MaskSection::Effects => "Effects",
                            MaskSection::ColorMixer => "Color Mixer",
                            MaskSection::Properties => "Mask Properties",
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::ui::icons::phosphor_icon_button(
                                ui,
                                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                                egui::vec2(28.0, 22.0),
                                "Reset local adjustments",
                            )
                            .clicked()
                            {
                                mask.adjustments.reset();
                                adjustments_changed = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    let (section_changed, _) = Self::show_local_mask_adjustment_section(
                        ui,
                        &mut mask.adjustments,
                        section,
                        &mut local_curve_tab,
                        &mut local_color_grade_tab,
                    );
                    adjustments_changed |= section_changed;
                }
            }
        }

        app.tone_curve_tab = local_curve_tab;
        app.color_grade_tab = local_color_grade_tab;
        app.brush_mode = brush_mode;
        if request_subject {
            app.request_subject_mask(frame);
        }
        if request_object {
            app.request_object_mask(mask_index, component_index);
        }
        if request_landscape {
            app.request_landscape_mask(frame, mask_index, component_index);
        }
        Self::apply_mask_geometry_change(ui, app, mask_index, geometry_changed);
        if adjustments_changed {
            app.mark_mask_adjustments_dirty();
        }
    }

    fn apply_mask_geometry_change(ui: &Ui, app: &mut AurawApp, mask_index: usize, changed: bool) {
        if changed && ui.input(|input| input.pointer.primary_down()) {
            app.note_mask_geometry_interaction(mask_index);
        } else if changed {
            app.finish_mask_geometry_interaction();
            app.mark_mask_geometry_dirty(mask_index);
        } else if !ui.input(|input| input.pointer.primary_down()) {
            // The last value of a drag may arrive in the frame after its final
            // movement. Commit it as soon as the pointer is released.
            app.finish_mask_geometry_interaction();
        }
    }

    fn show_vertical_mask_properties(
        ui: &mut Ui,
        mask: &mut crate::pipeline::LocalMask,
        component_index: usize,
        brush_mode: &mut BrushMode,
        request_subject: &mut bool,
        request_object: &mut bool,
        request_landscape: &mut bool,
    ) -> bool {
        let mut geometry_changed = adjustment_slider(
            ui,
            "Mask opacity",
            &mut mask.opacity,
            0.0..=1.0,
            2,
            0.01,
            Some("Controls the strength of the entire mask before local adjustments."),
        );

        let Some(component) = mask.components.get_mut(component_index) else {
            return geometry_changed;
        };

        ui.add_space(4.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong(component.name.as_str());
                ui.weak(component.kind.label());
                geometry_changed |= ui.checkbox(&mut component.invert, "Invert").changed();
                if component_index > 0 {
                    let before = component.combine;
                    egui::ComboBox::from_id_salt("vertical-mask-combine")
                        .selected_text(component.combine.label())
                        .show_ui(ui, |ui| {
                            for mode in [
                                MaskCombineMode::Add,
                                MaskCombineMode::Subtract,
                                MaskCombineMode::Intersect,
                            ] {
                                ui.selectable_value(&mut component.combine, mode, mode.label());
                            }
                        });
                    geometry_changed |= before != component.combine;
                }
            });

            match &mut component.geometry {
                MaskGeometry::Brush {
                    size,
                    feather,
                    opacity_enabled,
                    opacity,
                    overlap_enabled,
                    stroke_starts,
                    dabs,
                } => {
                    ui.horizontal(|ui| {
                        ui.selectable_value(brush_mode, BrushMode::Paint, "Brush");
                        ui.selectable_value(brush_mode, BrushMode::Erase, "Eraser");
                    });
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Size",
                        size,
                        0.0025..=0.25,
                        3,
                        0.0025,
                        Some("Brush stays the same size on screen; zoom in for finer image-space detail."),
                    );
                    geometry_changed |= adjustment_slider_with_reset(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softness from the brush core to its edge."),
                        0.55,
                    );
                    ui.horizontal(|ui| {
                        geometry_changed |= ui
                            .checkbox(opacity_enabled, "Opacity")
                            .on_hover_text(
                                "Use the opacity setting for newly drawn brush and eraser strokes. \
                                 Disabled strokes always use 100% opacity.",
                            )
                            .changed();
                        geometry_changed |= ui
                            .checkbox(overlap_enabled, "Overlapping")
                            .on_hover_text(
                                "Allow separate brush strokes to build opacity where they overlap. \
                                 For example, 10% over 10% produces about 19% coverage.",
                            )
                            .changed();
                    });
                    ui.add_enabled_ui(*opacity_enabled, |ui| {
                        geometry_changed |= adjustment_slider(
                            ui,
                            "Stroke opacity",
                            opacity,
                            0.0..=1.0,
                            2,
                            0.01,
                            Some(
                                "Controls only newly drawn brush and eraser strokes. Existing \
                                 strokes and the whole-mask opacity are unchanged.",
                            ),
                        );
                    });
                    if crate::ui::icons::phosphor_icon_button(
                        ui,
                        egui_phosphor::regular::ERASER,
                        egui::vec2(28.0, 22.0),
                        "Clear brush strokes",
                    )
                    .clicked()
                    {
                        dabs.clear();
                        stroke_starts.clear();
                        geometry_changed = true;
                    }
                    ui.small(format!("{} brush dabs", dabs.len()));
                }
                MaskGeometry::Radial { feather, .. } => {
                    geometry_changed |= adjustment_slider_with_reset(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Soft transition from the ellipse interior to its edge."),
                        0.55,
                    );
                }
                MaskGeometry::Linear { feather, .. } => {
                    geometry_changed |= adjustment_slider_with_reset(
                        ui,
                        "Feather",
                        feather,
                        0.02..=1.0,
                        2,
                        0.01,
                        Some("Controls the width of the gradient transition."),
                        1.0,
                    );
                }
                MaskGeometry::Ai {
                    mask: generated_mask,
                    grow,
                    feather,
                } => {
                    if generated_mask.is_none() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Waiting for subject selection");
                        });
                        if ui.button("Generate subject mask").clicked() {
                            *request_subject = true;
                        }
                    }
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Grow",
                        grow,
                        -1.0..=1.0,
                        2,
                        0.01,
                        Some("Positive values expand the mask; negative values shrink it inward."),
                    );
                    geometry_changed |= adjustment_slider_with_reset(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softens the BiRefNet subject boundary."),
                        0.0,
                    );
                }
                MaskGeometry::Object {
                    mask: generated_mask,
                    grow,
                    feather,
                    brush_size,
                    edge_refine,
                    strokes,
                } => {
                    *brush_mode = BrushMode::Paint;
                    ui.label(if generated_mask.is_some() {
                        "Draw again on the image to replace this object selection from scratch."
                    } else {
                        "Paint through the middle of the object part you want to select."
                    });
                    ui.strong("Selection brush");
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Size",
                        brush_size,
                        0.0025..=0.25,
                        3,
                        0.0025,
                        Some("Controls the hard-edged selection brush. Its on-screen size stays constant while zooming for finer detail."),
                    );
                    ui.add_space(4.0);
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Grow",
                        grow,
                        -1.0..=1.0,
                        2,
                        0.01,
                        Some("Positive values expand the mask; negative values shrink it inward."),
                    );
                    geometry_changed |= adjustment_slider_with_reset(
                        ui,
                        "Mask feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softens the final object mask after SAM selection."),
                        0.0,
                    );
                    let refine_changed = adjustment_slider(
                        ui,
                        "Edge refine",
                        edge_refine,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Aligns uncertain SAM boundaries to local image edges."),
                    );
                    geometry_changed |= refine_changed;
                    if refine_changed && !strokes.is_empty() {
                        *request_object = true;
                    }
                    ui.horizontal_wrapped(|ui| {
                        if crate::ui::icons::phosphor_icon_button(
                            ui,
                            egui_phosphor::regular::ARROW_CLOCKWISE,
                            egui::vec2(28.0, 22.0),
                            "Recalculate object selection",
                        )
                        .clicked()
                        {
                            *request_object = true;
                        }
                        if crate::ui::icons::phosphor_icon_button(
                            ui,
                            egui_phosphor::regular::X,
                            egui::vec2(28.0, 22.0),
                            "Clear object selection",
                        )
                        .clicked()
                        {
                            strokes.clear();
                            *generated_mask = None;
                            geometry_changed = true;
                        }
                    });
                    ui.small(format!("{} selection stroke(s)", strokes.len()));
                }
                MaskGeometry::Landscape {
                    mask: generated_mask,
                    category,
                    grow,
                    feather,
                } => {
                    ui.label("Choose a landscape element, then generate its semantic mask.");
                    let before = *category;
                    egui::ComboBox::from_id_salt("landscape-mask-category")
                        .selected_text(category.label())
                        .show_ui(ui, |ui| {
                            for option in crate::pipeline::LandscapeCategory::ALL {
                                ui.selectable_value(category, option, option.label());
                            }
                        });
                    if before != *category {
                        *generated_mask = None;
                        geometry_changed = true;
                    }
                    if ui.button("Generate Mask").clicked() {
                        *request_landscape = true;
                    }
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Grow",
                        grow,
                        -1.0..=1.0,
                        2,
                        0.01,
                        Some("Positive values expand the mask; negative values shrink it inward."),
                    );
                    geometry_changed |= adjustment_slider_with_reset(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softens the semantic boundary after generation."),
                        0.0,
                    );
                }
                MaskGeometry::LuminanceRange {
                    low,
                    high,
                    grow,
                    feather,
                    ..
                } => {
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Range low",
                        low,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Lowest included scene luminance."),
                    );
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Range high",
                        high,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Highest included scene luminance."),
                    );
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Grow",
                        grow,
                        -1.0..=1.0,
                        2,
                        0.01,
                        Some("Positive values expand the mask; negative values shrink it inward."),
                    );
                    geometry_changed |= adjustment_slider_with_reset(
                        ui,
                        "Range feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softens both luminance-range boundaries."),
                        0.15,
                    );
                }
                MaskGeometry::ColorRange {
                    tolerance,
                    grow,
                    feather,
                    sampled,
                    ..
                } => {
                    ui.label(if *sampled {
                        "Drag on the image to choose another color."
                    } else {
                        "Drag on the image to sample a color."
                    });
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Tolerance",
                        tolerance,
                        0.005..=1.0,
                        3,
                        0.005,
                        Some("Expands the selected color region in perceptual OkLab space."),
                    );
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Grow",
                        grow,
                        -1.0..=1.0,
                        2,
                        0.01,
                        Some("Positive values expand the mask; negative values shrink it inward."),
                    );
                    geometry_changed |= adjustment_slider_with_reset(
                        ui,
                        "Color feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softens the color-distance cutoff."),
                        0.12,
                    );
                }
                MaskGeometry::Placeholder => {
                    ui.label("This mask type is not implemented yet.");
                }
            }
        });

        geometry_changed
    }

    fn refresh_mask_thumbnails(ui: &mut Ui, app: &mut AurawApp) {
        let selected_mask = app.masks.selected_mask;
        let group_cache_valid = app.mask_thumbnail_revision == app.mask_overlay_revision
            && app.mask_thumbnail_group_textures.len() == app.masks.masks.len();
        let component_len = selected_mask
            .and_then(|index| app.masks.masks.get(index))
            .map_or(0, |mask| mask.components.len());
        let component_cache_valid = group_cache_valid
            && app.mask_thumbnail_component_mask == selected_mask
            && app.mask_thumbnail_component_textures.len() == component_len;
        if group_cache_valid && component_cache_valid {
            return;
        }

        let (image_width, image_height) = app
            .preview_raw
            .as_ref()
            .map(|raw| (raw.width, raw.height))
            .unwrap_or((1, 1));
        let edge = Self::MASK_THUMBNAIL_EDGE;
        let (thumbnail_width, thumbnail_height) =
            Self::thumbnail_fit_size(image_width, image_height, edge);

        if !group_cache_valid {
            let images: Vec<_> = (0..app.masks.masks.len())
                .map(|index| {
                    let gray = app.masks.rasterize_layer(
                        index,
                        thumbnail_width,
                        thumbnail_height,
                        image_width,
                        image_height,
                    );
                    Self::gray_thumbnail_image(gray, thumbnail_width, thumbnail_height, edge)
                })
                .collect();
            Self::update_thumbnail_textures(
                ui,
                &mut app.mask_thumbnail_group_textures,
                images,
                "mask-group-thumbnail",
            );
        }

        if !component_cache_valid {
            let images: Vec<_> = selected_mask
                .and_then(|mask_index| {
                    app.masks
                        .masks
                        .get(mask_index)
                        .map(|mask| (mask_index, mask))
                })
                .map(|(mask_index, mask)| {
                    (0..mask.components.len())
                        .map(|component_index| {
                            let gray = app.masks.rasterize_component_layer(
                                mask_index,
                                component_index,
                                thumbnail_width,
                                thumbnail_height,
                                image_width,
                                image_height,
                            );
                            Self::gray_thumbnail_image(
                                gray,
                                thumbnail_width,
                                thumbnail_height,
                                edge,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            Self::update_thumbnail_textures(
                ui,
                &mut app.mask_thumbnail_component_textures,
                images,
                "mask-component-thumbnail",
            );
        }

        app.mask_thumbnail_revision = app.mask_overlay_revision;
        app.mask_thumbnail_component_mask = selected_mask;
    }

    fn thumbnail_fit_size(image_width: u32, image_height: u32, edge: u32) -> (u32, u32) {
        let image_width = image_width.max(1);
        let image_height = image_height.max(1);
        if image_width >= image_height {
            let height = ((edge as f64 * image_height as f64 / image_width as f64).round() as u32)
                .clamp(1, edge);
            (edge, height)
        } else {
            let width = ((edge as f64 * image_width as f64 / image_height as f64).round() as u32)
                .clamp(1, edge);
            (width, edge)
        }
    }

    fn gray_thumbnail_image(gray: Vec<u8>, width: u32, height: u32, edge: u32) -> egui::ColorImage {
        let width = width.min(edge) as usize;
        let height = height.min(edge) as usize;
        let edge = edge as usize;
        let mut square = vec![0_u8; edge * edge];
        let offset_x = (edge - width) / 2;
        let offset_y = (edge - height) / 2;

        for row in 0..height {
            let source_start = row * width;
            let source_end = (source_start + width).min(gray.len());
            let copied = source_end.saturating_sub(source_start);
            if copied == 0 {
                break;
            }
            let destination_start = (offset_y + row) * edge + offset_x;
            square[destination_start..destination_start + copied]
                .copy_from_slice(&gray[source_start..source_end]);
        }

        egui::ColorImage::from_gray([edge, edge], &square)
    }

    fn update_thumbnail_textures(
        ui: &mut Ui,
        textures: &mut Vec<egui::TextureHandle>,
        images: Vec<egui::ColorImage>,
        prefix: &str,
    ) {
        let desired_len = images.len();
        for (index, image) in images.into_iter().enumerate() {
            if let Some(texture) = textures.get_mut(index) {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                textures.push(ui.ctx().load_texture(
                    format!("{prefix}-{index}"),
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        textures.truncate(desired_len);
    }

    fn mask_thumbnail_card(
        ui: &mut Ui,
        texture: Option<&egui::TextureHandle>,
        label: &str,
        selected: bool,
        badge: Option<&str>,
        enabled: bool,
        card_size: MaskCardSize,
    ) -> egui::Response {
        use eframe::egui::{Align2, Color32, FontId, Stroke, StrokeKind};

        let size = card_size.card_size();
        let image_edge = card_size.image_edge();
        let thumbnail_sense = if cfg!(target_os = "android") {
            // A drag anywhere on a card belongs to the enclosing strip on
            // touch devices. Desktop retains drag-to-reorder for submasks.
            egui::Sense::click()
        } else {
            egui::Sense::click_and_drag()
        };
        let (rect, response) = ui.allocate_exact_size(size, thumbnail_sense);
        let visuals = ui.visuals();
        let fill = if selected {
            visuals.selection.bg_fill.gamma_multiply(0.24)
        } else if response.hovered() {
            visuals.widgets.hovered.bg_fill
        } else {
            visuals.widgets.inactive.bg_fill
        };
        let stroke = if selected {
            Stroke::new(2.0, visuals.selection.bg_fill)
        } else {
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 5.0, fill);
        painter.rect_stroke(rect, 5.0, stroke, StrokeKind::Inside);

        // The thumbnail well is always square. The texture itself contains a
        // centered, letterboxed rendering at the RAW image's aspect ratio.
        let image_rect = egui::Rect::from_min_size(
            egui::pos2(rect.center().x - image_edge * 0.5, rect.min.y + 5.0),
            egui::vec2(image_edge, image_edge),
        );
        painter.rect_filled(image_rect, 3.0, Color32::BLACK);
        if let Some(texture) = texture {
            let tint = if enabled {
                Color32::WHITE
            } else {
                Color32::from_white_alpha(80)
            };
            painter.image(
                texture.id(),
                image_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                tint,
            );
        }

        if let Some(badge) = badge {
            let (font_size, badge_height, horizontal_padding) = match card_size {
                MaskCardSize::Group => (10.5, 18.0, 10.0),
                MaskCardSize::Submask => (9.0, 16.0, 8.0),
            };
            let badge_size = egui::vec2(
                (badge.chars().count() as f32 * font_size * 0.62 + horizontal_padding)
                    .max(badge_height + 2.0),
                badge_height,
            );
            let badge_rect =
                egui::Rect::from_min_size(image_rect.right_bottom() - badge_size, badge_size);
            painter.rect_filled(badge_rect, 3.0, Color32::from_black_alpha(210));
            painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                badge,
                FontId::proportional(font_size),
                Color32::WHITE,
            );
        }

        let max_label_chars = match card_size {
            MaskCardSize::Group => 13,
            MaskCardSize::Submask => 10,
        };
        let display_label: String = label.chars().take(max_label_chars).collect();
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 9.0),
            Align2::CENTER_CENTER,
            display_label,
            FontId::proportional(card_size.label_font_size()),
            if enabled {
                visuals.text_color()
            } else {
                visuals.weak_text_color()
            },
        );
        response
    }

    fn prepare_content_mask(app: &mut AurawApp, frame: &eframe::Frame, kind: MaskKind) {
        match kind {
            MaskKind::Subject | MaskKind::Background => app.request_subject_mask(frame),
            MaskKind::Object => {
                if let Err(error) = app.capture_mask_source(frame) {
                    app.report_ai_mask_error(error);
                }
            }
            MaskKind::Landscape => {
                if let Err(error) = app.capture_mask_source(frame) {
                    app.report_ai_mask_error(error);
                }
            }
            MaskKind::LuminanceRange | MaskKind::ColorRange => {
                if let Err(error) = app.capture_mask_source(frame) {
                    app.status = error;
                    return;
                }
                let source = app.mask_source_cache.clone();
                if let Some(component) = app.masks.selected_component_mut() {
                    match &mut component.geometry {
                        MaskGeometry::LuminanceRange { source: target, .. }
                        | MaskGeometry::ColorRange { source: target, .. } => *target = source,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn show_local_mask_adjustment_section(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        section: MaskSection,
        selected_tab: &mut ToneCurveTab,
        selected_grade_tab: &mut ColorGradeTab,
    ) -> (bool, bool) {
        match section {
            MaskSection::Properties => (false, false),
            MaskSection::Light => Self::show_local_mask_light(ui, adjustment),
            MaskSection::ToneCurve => (
                Self::show_local_mask_tone_curve(ui, adjustment, selected_tab),
                false,
            ),
            MaskSection::Color => (Self::show_local_mask_color(ui, adjustment), false),
            MaskSection::ColorGrading => (
                Self::show_local_mask_color_grading(ui, adjustment, selected_grade_tab),
                false,
            ),
            MaskSection::Effects => (Self::show_local_mask_effects(ui, adjustment), false),
            MaskSection::ColorMixer => (Self::show_local_mask_color_mixer(ui, adjustment), false),
        }
    }

    fn show_local_mask_light(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> (bool, bool) {
        let mut changed = false;
        let shadows_before = adjustment.shadows;
        let blacks_before = adjustment.blacks;
        changed |= adjustment_slider(
            ui,
            "Exposure",
            &mut adjustment.exposure,
            -5.0..=5.0,
            2,
            0.05,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Contrast",
            &mut adjustment.contrast,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Highlights",
            &mut adjustment.highlights,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Shadows",
            &mut adjustment.shadows,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Whites",
            &mut adjustment.whites,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Blacks",
            &mut adjustment.blacks,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        (
            changed,
            adjustment.shadows != shadows_before || adjustment.blacks != blacks_before,
        )
    }

    fn show_local_mask_color(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        let mut changed = false;
        changed |= adjustment_slider(
            ui,
            "Temperature",
            &mut adjustment.temperature,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Tint",
            &mut adjustment.tint,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Saturation",
            &mut adjustment.saturation,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed
    }

    fn show_local_mask_effects(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        let mut changed = false;
        changed |= adjustment_slider(
            ui,
            "Texture",
            &mut adjustment.texture,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Clarity",
            &mut adjustment.clarity,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Dehaze",
            &mut adjustment.dehaze,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed
    }

    fn show_local_mask_color_grading(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        selected_grade_tab: &mut ColorGradeTab,
    ) -> bool {
        color_grading_editor(ui, &mut adjustment.color_grading, selected_grade_tab)
    }

    fn show_local_mask_tone_curve(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        selected_tab: &mut ToneCurveTab,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            for (tab, label, color) in [
                (ToneCurveTab::Rgb, "RGB", egui::Color32::WHITE),
                (ToneCurveTab::Red, "R", egui::Color32::from_rgb(238, 84, 84)),
                (
                    ToneCurveTab::Green,
                    "G",
                    egui::Color32::from_rgb(92, 210, 116),
                ),
                (
                    ToneCurveTab::Blue,
                    "B",
                    egui::Color32::from_rgb(88, 150, 245),
                ),
            ] {
                ui.selectable_value(selected_tab, tab, egui::RichText::new(label).color(color));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::ui::icons::phosphor_icon_button(
                    ui,
                    egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                    egui::vec2(28.0, 22.0),
                    "Reset the selected tone curve",
                )
                .clicked()
                {
                    match *selected_tab {
                        ToneCurveTab::Rgb => adjustment.tone_curve.reset(),
                        ToneCurveTab::Red => adjustment.tone_curve_red.reset(),
                        ToneCurveTab::Green => adjustment.tone_curve_green.reset(),
                        ToneCurveTab::Blue => adjustment.tone_curve_blue.reset(),
                    }
                    changed = true;
                }
            });
        });
        let (curve, color, description) = match *selected_tab {
            ToneCurveTab::Rgb => (
                &mut adjustment.tone_curve,
                egui::Color32::WHITE,
                "Composite luminance curve",
            ),
            ToneCurveTab::Red => (
                &mut adjustment.tone_curve_red,
                egui::Color32::from_rgb(238, 84, 84),
                "Red channel curve",
            ),
            ToneCurveTab::Green => (
                &mut adjustment.tone_curve_green,
                egui::Color32::from_rgb(92, 210, 116),
                "Green channel curve",
            ),
            ToneCurveTab::Blue => (
                &mut adjustment.tone_curve_blue,
                egui::Color32::from_rgb(88, 150, 245),
                "Blue channel curve",
            ),
        };
        ui.label(
            egui::RichText::new(description)
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        changed |= tone_curve_editor(ui, curve, color);
        if changed {
            adjustment.sanitize_tone_curves();
        }
        changed
    }

    fn show_local_mask_color_mixer(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        const COLORS: [&str; 8] = [
            "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
        ];
        let mut changed = false;
        for (index, color) in COLORS.iter().enumerate() {
            ui.push_id(("local-hsl", index), |ui| {
                ui.strong(*color);
                changed |= adjustment_slider(
                    ui,
                    "Hue",
                    &mut adjustment.hsl_hue[index],
                    -HSL_HUE_LIMIT..=HSL_HUE_LIMIT,
                    0,
                    1.0,
                    None,
                );
                changed |= adjustment_slider(
                    ui,
                    "Saturation",
                    &mut adjustment.hsl_saturation[index],
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                );
                changed |= adjustment_slider(
                    ui,
                    "Luminance",
                    &mut adjustment.hsl_luminance[index],
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                );
            });
            if index + 1 < COLORS.len() {
                ui.separator();
            }
        }
        changed
    }
}
