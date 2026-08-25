use super::*;

fn show_dialog_error(ui: &mut Ui, error: Option<&str>) {
    if let Some(error) = error {
        ui.label(
            egui::RichText::new(error)
                .small()
                .color(ui.visuals().error_fg_color),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AdjustmentPasteChoice {
    Cancel,
    Merge,
    Replace,
}

pub(super) fn show_adjustment_paste_choice(
    ui: &mut Ui,
    id: &'static str,
    edited_count: usize,
    target_count: usize,
) -> Option<AdjustmentPasteChoice> {
    let mut choice = None;
    crate::ui::responsive_popup(egui::Window::new("Paste adjustments"), ui.ctx(), 480.0)
        .id(egui::Id::new(id))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label(format!(
                "{} of the {} selected {} already contain edits.",
                edited_count,
                target_count,
                if target_count == 1 { "image" } else { "images" }
            ));
            ui.add_space(4.0);
            ui.label(
                "Merge overwrites only the copied categories and preserves every unchecked category already on the destination.",
            );
            ui.label(
                "Replace clears the destination edit state first, then applies the categories stored in the adjustment clipboard.",
            );
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    choice = Some(AdjustmentPasteChoice::Cancel);
                }
                if ui.button("Merge").clicked() {
                    choice = Some(AdjustmentPasteChoice::Merge);
                }
                if ui.button("Replace").clicked() {
                    choice = Some(AdjustmentPasteChoice::Replace);
                }
            });
        });
    choice
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AiMaskRefreshChoice {
    Dismiss,
    Regenerate,
}

pub(super) fn show_ai_mask_refresh_choice(
    ui: &mut Ui,
    id: &'static str,
    target_count: usize,
    can_regenerate: bool,
) -> Option<AiMaskRefreshChoice> {
    let mut choice = None;
    crate::ui::responsive_popup(egui::Window::new("Regenerate AI masks?"), ui.ctx(), 460.0)
        .id(egui::Id::new(id))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label(format!(
                "{} pasted {} contain content-aware masks that belong to the source image.",
                target_count,
                if target_count == 1 { "image" } else { "images" }
            ));
            ui.label(
                "Regenerate them now for each destination image? Mask groups, settings, object strokes, and local adjustments are preserved.",
            );
            if !can_regenerate {
                ui.label(
                    egui::RichText::new("Waiting for the current RAW load or edit save to finish…")
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("Not now").clicked() {
                    choice = Some(AiMaskRefreshChoice::Dismiss);
                }
                if ui
                    .add_enabled(can_regenerate, egui::Button::new("Regenerate"))
                    .clicked()
                {
                    choice = Some(AiMaskRefreshChoice::Regenerate);
                }
            });
        });
    choice
}

pub(super) fn show_ai_mask_refresh_progress(
    ui: &mut Ui,
    completed: usize,
    total: usize,
    failed: usize,
    current_name: Option<&str>,
    allow_minimize: bool,
) -> (bool, bool) {
    let fraction = if total == 0 {
        0.0
    } else {
        (completed as f32 / total as f32).clamp(0.0, 1.0)
    };
    let mut minimize = false;
    let mut cancel = false;
    crate::ui::responsive_popup(egui::Window::new("Regenerating AI masks"), ui.ctx(), 360.0)
        .id(egui::Id::new("library-ai-mask-refresh-progress"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label(
                egui::RichText::new(format!("{completed} / {total} AI masks updated")).strong(),
            );
            ui.add_space(6.0);
            ui.add(
                egui::ProgressBar::new(fraction)
                    .show_percentage()
                    .animate(completed < total),
            );
            if let Some(name) = current_name {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Refreshing {name}…"));
                });
            }
            if failed > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "{failed} {} failed",
                        if failed == 1 { "image" } else { "images" }
                    ))
                    .small()
                    .color(ui.visuals().warn_fg_color),
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if allow_minimize {
                    minimize = ui.button("Minimize").clicked();
                }
                cancel = ui.button("Cancel").clicked();
            });
        });
    (minimize, cancel)
}

#[cfg(target_os = "android")]
pub(super) fn show_android_library_folder_dialog(ui: &mut Ui, app: &mut AurawApp) {
    let mut close = false;
    let mut create = None;
    if let Some(dialog) = app.library.platform.folder_name_dialog.as_mut() {
        crate::ui::responsive_popup(egui::Window::new("New folder"), ui.ctx(), 380.0)
            .id(egui::Id::new("android-library-folder-name-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label("Folder name");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(f32::INFINITY)
                        .id_source("android-library-folder-name-input"),
                );
                if !dialog.focus_requested {
                    response.request_focus();
                    dialog.focus_requested = true;
                }
                show_dialog_error(ui, dialog.error.as_deref());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let enter = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if ui.button("Create").clicked() || enter {
                        create = Some((dialog.parent.clone(), dialog.name.clone()));
                    }
                });
            });
    }
    if close {
        app.library.platform.folder_name_dialog = None;
    }
    if let Some((parent, name)) = create {
        match crate::android::create_library_folder(&app.library.platform.app, &parent, &name) {
            Ok(folder) => {
                app.library.platform.folder_name_dialog = None;
                app.library.platform.expanded_folders.insert(parent);
                app.library.status = format!("Created folder {folder}");
                app.library.refresh(ui.ctx());
            }
            Err(error) => {
                if let Some(dialog) = app.library.platform.folder_name_dialog.as_mut() {
                    dialog.error = Some(error);
                }
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
pub(super) fn show_library_folder_dialogs(ui: &mut Ui, app: &mut AurawApp) {
    let mut close_name_dialog = false;
    let mut name_operation = None;
    if let Some(dialog) = app.library.folder_name_dialog.as_mut() {
        let title = match dialog.kind {
            LibraryFolderNameDialogKind::Create { .. } => "New folder",
            LibraryFolderNameDialogKind::Rename { .. } => "Rename folder",
        };
        egui::Window::new(title)
            .id(egui::Id::new("library-folder-name-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label("Folder name");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(320.0)
                        .id_source("library-folder-name-input"),
                );
                response.request_focus();
                show_dialog_error(ui, dialog.error.as_deref());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_name_dialog = true;
                    }
                    let enter = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    let confirm_label = match dialog.kind {
                        LibraryFolderNameDialogKind::Create { .. } => "Create",
                        LibraryFolderNameDialogKind::Rename { .. } => "Rename",
                    };
                    if ui.button(confirm_label).clicked() || enter {
                        match validate_folder_name(&dialog.name) {
                            Ok(_) => {
                                let Some(root) = app.library.root_folder.clone() else {
                                    close_name_dialog = true;
                                    return;
                                };
                                name_operation = Some(match &dialog.kind {
                                    LibraryFolderNameDialogKind::Create { parent } => {
                                        LibraryFolderOperation::Create {
                                            root,
                                            parent: parent.clone(),
                                            name: dialog.name.clone(),
                                        }
                                    }
                                    LibraryFolderNameDialogKind::Rename { source } => {
                                        let Some(parent) = source.parent() else {
                                            dialog.error = Some(
                                                "This folder has no parent folder.".to_owned(),
                                            );
                                            return;
                                        };
                                        LibraryFolderOperation::Move {
                                            root,
                                            source: source.clone(),
                                            destination_parent: parent.to_path_buf(),
                                            new_name: Some(dialog.name.clone()),
                                        }
                                    }
                                });
                                close_name_dialog = true;
                            }
                            Err(error) => dialog.error = Some(error),
                        }
                    }
                });
            });
    }
    if close_name_dialog {
        app.library.folder_name_dialog = None;
    }
    if let Some(operation) = name_operation {
        app.library.start_folder_operation(operation, ui.ctx());
    }

    let delete_target = app.library.folder_delete_confirmation.clone();
    let mut close_delete = false;
    let mut confirm_delete = false;
    if let Some(target) = delete_target.as_ref() {
        egui::Window::new("Delete folder?")
            .id(egui::Id::new("library-folder-delete-confirmation"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "Delete {} and everything inside it?",
                    target.display()
                ));
                ui.label(
                    egui::RichText::new("This cannot be undone.")
                        .strong()
                        .color(ui.visuals().warn_fg_color),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_delete = true;
                    }
                    if ui.button("Delete Folder").clicked() {
                        confirm_delete = true;
                        close_delete = true;
                    }
                });
            });
    }
    if close_delete {
        app.library.folder_delete_confirmation = None;
    }
    if confirm_delete {
        if let (Some(root), Some(target)) = (app.library.root_folder.clone(), delete_target) {
            if let Some(current) = app
                .develop
                .current_path
                .clone()
                .filter(|current| current.starts_with(&target))
            {
                app.detach_current_file_for_library_action(&current);
                app.develop.current_path = None;
            }
            app.library
                .start_folder_operation(LibraryFolderOperation::Delete { root, target }, ui.ctx());
        }
    }
}

pub(super) fn validate_library_item_name(name: &str, raw: bool) -> Result<(), String> {
    if name.is_empty()
        || name.trim() != name
        || name.contains(['/', '\\'])
        || name.contains('"')
        || name.chars().any(char::is_control)
    {
        return Err("Enter a single safe name without leading or trailing spaces.".to_owned());
    }
    if raw && !crate::pipeline::is_supported_raw_path(Path::new(name)) {
        return Err("Keep a supported RAW filename extension.".to_owned());
    }
    Ok(())
}

pub(super) fn show_library_raw_name_dialog(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
    #[cfg(target_os = "android")]
    let _ = frame;
    let mut close = false;
    let mut rename = None;
    if let Some(dialog) = app.library.raw_name_dialog.as_mut() {
        crate::ui::responsive_popup(egui::Window::new("Rename RAW"), ui.ctx(), 420.0)
            .id(egui::Id::new("library-raw-name-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label("RAW filename");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(320.0)
                        .id_source("library-raw-name-input"),
                );
                if !dialog.focus_requested {
                    response.request_focus();
                    dialog.focus_requested = true;
                }
                show_dialog_error(ui, dialog.error.as_deref());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let enter = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if ui.button("Rename").clicked() || enter {
                        match validate_library_item_name(&dialog.name, true) {
                            Ok(()) => rename = Some((dialog.asset.clone(), dialog.name.clone())),
                            Err(error) => {
                                dialog.error = Some(error);
                                dialog.focus_requested = false;
                            }
                        }
                    }
                });
            });
    }
    if close {
        app.library.raw_name_dialog = None;
    }

    let Some((asset, name)) = rename else {
        return;
    };

    #[cfg(not(target_os = "android"))]
    let current_path = asset.desktop_path().and_then(|path| {
        (app.develop.current_path.as_deref() == Some(path)).then(|| path.to_path_buf())
    });
    #[cfg(not(target_os = "android"))]
    if let Some(path) = current_path.as_deref() {
        app.detach_current_file_for_library_action(path);
        app.develop.current_path = None;
    }

    match rename_asset(app, &asset, &name) {
        Ok(renamed_asset) => {
            if let Some(clipboard) = app.library.image_clipboard.as_mut() {
                for clipboard_asset in &mut clipboard.assets {
                    if clipboard_asset.id == asset.id {
                        *clipboard_asset = renamed_asset.clone();
                    }
                }
            }
            app.library.raw_name_dialog = None;
            app.library.clear_selection();
            #[cfg(target_os = "android")]
            crate::android::set_back_navigation_active(false);
            app.library.refresh(ui.ctx());
            app.library.status = format!("Renamed RAW to {name}.");

            #[cfg(not(target_os = "android"))]
            if current_path.is_some() {
                if let Some(destination) = renamed_asset.desktop_path().map(Path::to_path_buf) {
                    app.open_path_labeled(
                        destination.clone(),
                        name,
                        false,
                        crate::sidecar::SidecarTarget::Desktop {
                            raw_path: destination,
                        },
                        frame,
                        None,
                    );
                }
            }
        }
        Err(error) => {
            if let Some(dialog) = app.library.raw_name_dialog.as_mut() {
                dialog.error = Some(error);
                dialog.focus_requested = false;
            }
            #[cfg(not(target_os = "android"))]
            if let Some(source) = current_path.filter(|path| path.is_file()) {
                let label = source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("local RAW")
                    .to_owned();
                app.open_path_labeled(
                    source.clone(),
                    label,
                    false,
                    crate::sidecar::SidecarTarget::Desktop { raw_path: source },
                    frame,
                    None,
                );
            }
        }
    }
}
