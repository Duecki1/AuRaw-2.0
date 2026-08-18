use super::*;

impl Sidebar {
    pub(super) fn mask_kind_menu(ui: &mut Ui, unavailable_message: &str) -> Option<MaskKind> {
        let mut selected = None;
        for kind in [
            MaskKind::Fullscreen,
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

    pub(super) fn submask_creation_menu(
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
    pub(super) fn mask_group_context_menu(
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
    pub(super) fn submask_context_menu(
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

    pub(super) fn show_mask_rename_dialog(ctx: &egui::Context, app: &mut AurawApp) {
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
                    app.masks.stack.masks.get_mut(mask_index).is_some_and(|mask| {
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
                } => app.masks.stack
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

    pub(super) fn duplicate_mask_group(app: &mut AurawApp, mask_index: usize, invert: bool) -> bool {
        let Some(mask) = app.masks.stack.masks.get(mask_index).cloned() else {
            return false;
        };
        Self::insert_mask_group_copy(app, mask_index, mask, invert)
    }

    pub(super) fn paste_mask_group(ctx: &egui::Context, app: &mut AurawApp, mask_index: usize) -> bool {
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
        if app.masks.stack.masks.len() >= MAX_LOCAL_MASKS || mask_index >= app.masks.stack.masks.len() {
            return false;
        }
        mask.name = Self::copied_mask_name(&app.masks.stack.masks, &mask.name);
        if invert {
            mask.invert = !mask.invert;
            // "Duplicate & Invert" is used to build a complementary mask,
            // not a second copy of the same local grade. Start the new mask
            // with neutral adjustments so only its coverage is duplicated.
            mask.adjustments.reset();
        }
        let insert_at = mask_index + 1;
        let mut candidate = app.masks.stack.clone();
        candidate.masks.insert(insert_at, mask);
        candidate.selected_mask = Some(insert_at);
        candidate.selected_component = Some(0);
        if let Err(error) = crate::sidecar::preflight_mask_change(&candidate, &app.inpaint.strokes)
        {
            app.report_mask_persistence_limit("Mask-group copy", &error);
            return false;
        }
        app.masks.stack = candidate;
        app.masks.thumbnail_component_mask = None;
        app.mark_all_mask_layers_dirty();
        if let Some(kind) = app.masks.stack
            .selected_component()
            .map(|component| component.kind)
        {
            app.select_mask_tool(kind);
        }
        app.blink_selected_mask();
        true
    }

    pub(super) fn duplicate_mask_component(
        app: &mut AurawApp,
        mask_index: usize,
        component_index: usize,
        invert: bool,
    ) -> bool {
        let Some(component) = app.masks.stack
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .cloned()
        else {
            return false;
        };
        Self::insert_mask_component_copy(app, mask_index, component_index, component, invert)
    }

    pub(super) fn paste_mask_component(
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
        let Some(mask) = app.masks.stack.masks.get(mask_index) else {
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
        let mut candidate = app.masks.stack.clone();
        candidate.masks[mask_index]
            .components
            .insert(insert_at, component);
        candidate.selected_mask = Some(mask_index);
        candidate.selected_component = Some(insert_at);
        if let Err(error) = crate::sidecar::preflight_mask_change(&candidate, &app.inpaint.strokes)
        {
            app.report_mask_persistence_limit("Sub-mask copy", &error);
            return false;
        }
        app.masks.stack = candidate;
        app.masks.thumbnail_component_mask = None;
        app.mark_mask_geometry_dirty(mask_index);
        if let Some(kind) = app.masks.stack
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
}
