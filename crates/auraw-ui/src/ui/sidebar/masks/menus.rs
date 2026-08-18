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
        let mut enabled = mask.enabled;
        if ui.checkbox(&mut enabled, "Enabled").changed() {
            *enabled_changed |= mask.common.set_enabled(enabled);
        }
        if ui
            .add_enabled(can_add_group, egui::Button::new("Duplicate"))
            .clicked()
        {
            *duplicate_mask = Some((mask_index, false));
            ui.close();
        }
        if ui.selectable_label(mask.invert, "Invert").clicked() {
            mask.common.toggle_invert();
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
        let mut enabled = component.enabled;
        if ui.checkbox(&mut enabled, "Enabled").changed() {
            *geometry_changed |= component.common.set_enabled(enabled);
        }
        if ui
            .add_enabled(can_add_component, egui::Button::new("Duplicate"))
            .clicked()
        {
            *duplicate_component = Some((mask_index, component_index, false));
            ui.close();
        }
        if ui.selectable_label(component.invert, "Invert").clicked() {
            component.common.toggle_invert();
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
            let renamed = match dialog.target {
                MaskRenameTarget::Group(mask_index) => app
                    .masks
                    .stack
                    .masks
                    .get_mut(mask_index)
                    .is_some_and(|mask| mask.common.rename(dialog.name.trim())),
                MaskRenameTarget::Component {
                    mask_index,
                    component_index,
                } => app
                    .masks
                    .stack
                    .masks
                    .get_mut(mask_index)
                    .and_then(|mask| mask.components.get_mut(component_index))
                    .is_some_and(|component| component.common.rename(dialog.name.trim())),
            };
            if renamed {
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
        Self::commit_mask_change(app, "Mask-group copy", None, false, |stack| {
            stack.duplicate_mask(mask_index, invert)
        })
    }

    pub(super) fn paste_mask_group(ctx: &egui::Context, app: &mut AurawApp, mask_index: usize) -> bool {
        let Some(mask) = Self::copied_mask_group(ctx) else {
            return false;
        };
        Self::commit_mask_change(app, "Mask-group copy", None, false, |stack| {
            stack.insert_mask_copy(mask_index, mask, false)
        })
    }

    pub(super) fn duplicate_mask_component(
        app: &mut AurawApp,
        mask_index: usize,
        component_index: usize,
        invert: bool,
    ) -> bool {
        Self::commit_mask_change(app, "Sub-mask copy", Some(mask_index), true, |stack| {
            stack.duplicate_component(mask_index, component_index, invert)
        })
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
        Self::commit_mask_change(app, "Sub-mask copy", Some(mask_index), true, |stack| {
            stack.insert_component_copy(mask_index, component_index, component, false)
        })
    }

    fn commit_mask_change(
        app: &mut AurawApp,
        action: &str,
        dirty_layer: Option<usize>,
        component_selection: bool,
        edit: impl FnOnce(&mut crate::pipeline::MaskStack) -> bool,
    ) -> bool {
        let mut candidate = app.masks.stack.clone();
        if !edit(&mut candidate) {
            return false;
        }
        if let Err(error) = crate::sidecar::preflight_mask_change(&candidate, &app.inpaint.strokes) {
            app.report_mask_persistence_limit(action, &error);
            return false;
        }
        app.masks.stack = candidate;
        if let Some(mask_index) = dirty_layer {
            app.mark_mask_geometry_dirty(mask_index);
        } else {
            app.mark_all_mask_layers_dirty();
        }
        app.sync_selected_mask_tool();
        if component_selection {
            app.blink_selected_component();
        } else {
            app.blink_selected_mask();
        }
        true
    }
}
