use super::*;

pub(super) fn cloud_folder_contains(
    folders: &[crate::cloud::CloudFolder],
    ancestor_id: &str,
    candidate_id: &str,
) -> bool {
    let mut current = candidate_id;
    let mut remaining = folders.len();
    while current != crate::cloud::CLOUD_ROOT_FOLDER_ID && remaining > 0 {
        if current == ancestor_id {
            return true;
        }
        let Some(folder) = folders.iter().find(|folder| folder.id == current) else {
            return false;
        };
        current = &folder.parent_id;
        remaining -= 1;
    }
    ancestor_id == crate::cloud::CLOUD_ROOT_FOLDER_ID
}

#[allow(clippy::too_many_arguments)]
pub(super) fn show_cloud_folder_node(
    ui: &mut Ui,
    folder: Option<&crate::cloud::CloudFolder>,
    folders: &[crate::cloud::CloudFolder],
    selected_folder_id: &str,
    clipboard: Option<&CloudClipboard>,
    image_clipboard: Option<&ImageClipboard>,
    action_in_progress: bool,
    expanded_folders: &mut HashSet<String>,
    requested_action: &mut Option<CloudFolderUiAction>,
) {
    let folder_id = folder
        .map(|folder| folder.id.as_str())
        .unwrap_or(crate::cloud::CLOUD_ROOT_FOLDER_ID);
    let name = folder.map(|folder| folder.name.as_str()).unwrap_or("Cloud");
    let children = folders
        .iter()
        .filter(|candidate| candidate.parent_id == folder_id)
        .collect::<Vec<_>>();
    let has_children = !children.is_empty();
    let expanded = expanded_folders.contains(folder_id);
    let selected = selected_folder_id == folder_id;
    let is_root = folder.is_none();

    ui.push_id(("cloud-folder", folder_id), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if has_children {
                let caret = if expanded {
                    egui_phosphor::regular::CARET_DOWN
                } else {
                    egui_phosphor::regular::CARET_RIGHT
                };
                if ui
                    .add_sized(
                        egui::Vec2::splat(crate::ui::theme::CONTROL_HEIGHT),
                        egui::Button::new(egui::RichText::new(caret).size(12.0)).frame(false),
                    )
                    .clicked()
                {
                    if expanded {
                        expanded_folders.remove(folder_id);
                    } else {
                        expanded_folders.insert(folder_id.to_owned());
                    }
                }
            } else {
                ui.allocate_space(egui::Vec2::splat(crate::ui::theme::CONTROL_HEIGHT));
            }

            let icon = if is_root {
                egui_phosphor::regular::CLOUD
            } else if expanded && has_children {
                egui_phosphor::regular::FOLDER_OPEN
            } else {
                egui_phosphor::regular::FOLDER
            };
            let response = crate::ui::theme::navigation_row(
                ui,
                egui::RichText::new(format!("{icon}  {name}")),
                selected,
                Sense::click_and_drag(),
            );
            if response.clicked() {
                *requested_action = Some(CloudFolderUiAction::Select(folder_id.to_owned()));
            }
            if let Some(folder) = folder {
                response.dnd_set_drag_payload(CloudFolderDrag(folder.id.clone()));
            }

            response.context_menu(|ui| {
                let enabled = !action_in_progress;
                if ui
                    .add_enabled(enabled, egui::Button::new("New Folder…"))
                    .clicked()
                {
                    *requested_action = Some(CloudFolderUiAction::New(folder_id.to_owned()));
                    ui.close();
                }
                let paste_label = if let Some(clipboard) = image_clipboard {
                    clipboard.paste_label()
                } else {
                    match clipboard.map(|clipboard| &clipboard.content) {
                        Some(CloudClipboardContent::Folder(folder)) => {
                            format!("Paste “{}”", folder.name)
                        }
                        None => "Paste".to_owned(),
                    }
                };
                if ui
                    .add_enabled(
                        enabled && (clipboard.is_some() || image_clipboard.is_some()),
                        egui::Button::new(paste_label),
                    )
                    .clicked()
                {
                    *requested_action = Some(CloudFolderUiAction::Paste(folder_id.to_owned()));
                    ui.close();
                }
                ui.separator();
                if let Some(folder) = folder {
                    if ui
                        .add_enabled(enabled, egui::Button::new("Copy Folder"))
                        .clicked()
                    {
                        *requested_action = Some(CloudFolderUiAction::Copy(folder.clone()));
                        ui.close();
                    }
                    if ui
                        .add_enabled(enabled, egui::Button::new("Cut Folder"))
                        .clicked()
                    {
                        *requested_action = Some(CloudFolderUiAction::Cut(folder.clone()));
                        ui.close();
                    }
                    if ui
                        .add_enabled(enabled, egui::Button::new("Rename Folder…"))
                        .clicked()
                    {
                        *requested_action = Some(CloudFolderUiAction::Rename(folder.clone()));
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(enabled, egui::Button::new("Delete Folder…"))
                        .clicked()
                    {
                        *requested_action = Some(CloudFolderUiAction::Delete(folder.clone()));
                        ui.close();
                    }
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Refresh Cloud"))
                    .clicked()
                {
                    *requested_action = Some(CloudFolderUiAction::Refresh);
                    ui.close();
                }
            });

            if let Some(payload) = response.dnd_hover_payload::<CloudFolderDrag>() {
                let source_id = &payload.0;
                let can_drop = !action_in_progress
                    && source_id != folder_id
                    && !cloud_folder_contains(folders, source_id, folder_id);
                if can_drop {
                    ui.painter().rect_stroke(
                        response.rect.expand(2.0),
                        3.0,
                        Stroke::new(2.0, ui.visuals().selection.stroke.color),
                        StrokeKind::Outside,
                    );
                    if let Some(payload) = response.dnd_release_payload::<CloudFolderDrag>() {
                        if let Some(source) = folders
                            .iter()
                            .find(|candidate| candidate.id == payload.0)
                            .cloned()
                        {
                            *requested_action = Some(CloudFolderUiAction::Move {
                                folder: source,
                                destination_parent_id: folder_id.to_owned(),
                            });
                        }
                    }
                }
            }
        });

        if expanded {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    for child in children {
                        show_cloud_folder_node(
                            ui,
                            Some(child),
                            folders,
                            selected_folder_id,
                            clipboard,
                            image_clipboard,
                            action_in_progress,
                            expanded_folders,
                            requested_action,
                        );
                    }
                });
            });
        }
    });
}

pub(super) fn apply_cloud_folder_ui_action(
    app: &mut AurawApp,
    action: CloudFolderUiAction,
    context: &egui::Context,
) {
    match action {
        CloudFolderUiAction::Select(folder_id) => {
            app.select_cloud_library_folder(folder_id);
        }
        CloudFolderUiAction::New(parent_id) => {
            app.library.cloud_name_dialog = Some(CloudNameDialog {
                kind: CloudNameDialogKind::CreateFolder { parent_id },
                name: String::new(),
                error: None,
                focus_requested: false,
            });
        }
        CloudFolderUiAction::Copy(folder) => {
            app.library.cloud_clipboard = Some(CloudClipboard {
                mode: CloudClipboardMode::Copy,
                content: CloudClipboardContent::Folder(folder.clone()),
            });
            app.library.image_clipboard = None;
            #[cfg(not(target_os = "android"))]
            {
                app.library.folder_clipboard = None;
            }
            app.library.status = format!(
                "Copied cloud folder {}. Choose Paste in a destination.",
                folder.name
            );
        }
        CloudFolderUiAction::Cut(folder) => {
            app.library.cloud_clipboard = Some(CloudClipboard {
                mode: CloudClipboardMode::Cut,
                content: CloudClipboardContent::Folder(folder.clone()),
            });
            app.library.image_clipboard = None;
            #[cfg(not(target_os = "android"))]
            {
                app.library.folder_clipboard = None;
            }
            app.library.status = format!(
                "Cut cloud folder {}. Choose Paste in a destination.",
                folder.name
            );
        }
        CloudFolderUiAction::Paste(destination_folder_id) => {
            paste_cloud_clipboard(app, destination_folder_id, context);
        }
        CloudFolderUiAction::Rename(folder) => {
            app.library.cloud_name_dialog = Some(CloudNameDialog {
                name: folder.name.clone(),
                kind: CloudNameDialogKind::RenameFolder { folder },
                error: None,
                focus_requested: false,
            });
        }
        CloudFolderUiAction::Delete(folder) => {
            app.library.cloud_delete_confirmation = Some(CloudDeleteTarget::Folder(folder));
        }
        CloudFolderUiAction::Move {
            folder,
            destination_parent_id,
        } => app.library.start_cloud_action(
            CloudActionRequest::UpdateFolder {
                name: folder.name.clone(),
                folder,
                parent_id: destination_parent_id,
                clear_clipboard: false,
            },
            context,
        ),
        CloudFolderUiAction::Refresh => {
            if app.library.cloud_trash_open {
                app.library.refresh(context);
            } else {
                app.show_library_view(LibraryView::Cloud);
            }
        }
    }
}

pub(super) fn validate_cloud_item_name(name: &str, raw: bool) -> Result<(), String> {
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

pub(super) fn paste_cloud_clipboard(
    app: &mut AurawApp,
    destination_folder_id: String,
    context: &egui::Context,
) {
    if app.library.image_clipboard.is_some() {
        start_image_clipboard_paste(
            app,
            ImagePasteDestination::CloudFolder(destination_folder_id),
            context,
        );
        return;
    }
    let Some(clipboard) = app.library.cloud_clipboard.clone() else {
        app.library.status = "Copy or cut images or a cloud folder first.".to_owned();
        return;
    };
    let request = match clipboard.content {
        CloudClipboardContent::Folder(folder) => match clipboard.mode {
            CloudClipboardMode::Copy => CloudActionRequest::CopyFolder {
                folder,
                destination_parent_id: destination_folder_id,
                clear_clipboard: false,
            },
            CloudClipboardMode::Cut => CloudActionRequest::UpdateFolder {
                name: folder.name.clone(),
                folder,
                parent_id: destination_folder_id,
                clear_clipboard: true,
            },
        },
    };
    app.library.start_cloud_action(request, context);
}

pub(super) fn start_image_clipboard_paste(
    app: &mut AurawApp,
    destination: ImagePasteDestination,
    context: &egui::Context,
) {
    let Some(clipboard) = app.library.image_clipboard.clone() else {
        app.library.status = "Copy or cut one or more RAW files first.".to_owned();
        return;
    };
    let busy = app.library.image_paste_receiver.is_some()
        || app.library.cloud_action_receiver.is_some()
        || app.library.cloud_upload_receiver.is_some()
        || app.library.cloud_open_receiver.is_some()
        || {
            #[cfg(not(target_os = "android"))]
            {
                app.library.file_action_receiver.is_some()
                    || app.library.raw_import_receiver.is_some()
                    || app.library.folder_operation_receiver.is_some()
            }
            #[cfg(target_os = "android")]
            {
                false
            }
        };
    if busy {
        app.library.status = "Wait for the current library transfer to finish.".to_owned();
        return;
    }
    if matches!(&destination, ImagePasteDestination::CloudFolder(_))
        && app.library.cloud_config.normalized().is_err()
    {
        app.library.status = "Configure AuRaw Cloud before pasting RAW files there.".to_owned();
        return;
    }
    if clipboard.mode == ImageClipboardMode::Cut {
        match &clipboard.content {
            #[cfg(not(target_os = "android"))]
            ImageClipboardContent::Local(paths) => {
                let moves_current = app.current_path.as_ref().is_some_and(|current| {
                    paths.iter().any(|path| path == current)
                        && match &destination {
                            ImagePasteDestination::LocalFolder(folder) => {
                                current.parent() != Some(folder.as_path())
                            }
                            ImagePasteDestination::CloudFolder(_) => true,
                        }
                });
                if moves_current {
                    if let Some(current) = app.current_path.clone() {
                        app.detach_current_file_for_library_action(&current);
                        app.current_path = None;
                    }
                }
            }
            #[cfg(target_os = "android")]
            ImageClipboardContent::Local(items) => {
                if matches!(&destination, ImagePasteDestination::CloudFolder(_)) {
                    for item in items {
                        app.detach_current_android_document_for_library_action(
                            &item.uri,
                            &item.display_name,
                        );
                    }
                }
            }
            ImageClipboardContent::Cloud(assets) => {
                let deletes_server_copy = match &destination {
                    ImagePasteDestination::CloudFolder(_) => false,
                    #[cfg(not(target_os = "android"))]
                    ImagePasteDestination::LocalFolder(_) => true,
                    #[cfg(target_os = "android")]
                    ImagePasteDestination::LocalLibrary => true,
                };
                if deletes_server_copy {
                    detach_current_cloud_asset_if_selected(app, assets);
                }
            }
        }
    }
    app.library.start_image_paste(destination, context);
}

#[cfg(not(target_os = "android"))]
pub(super) fn show_cloud_folder_bar(ui: &mut Ui, app: &mut AurawApp) {
    if !app.library.is_cloud_view() {
        return;
    }
    if app.library.cloud_trash_open {
        let action_enabled =
            !app.library.cloud_action_in_progress() && app.library.cloud_trash_receiver.is_none();
        let mut back = false;
        let mut refresh = false;
        ui.horizontal_wrapped(|ui| {
            if ui.button("Cloud").clicked() {
                back = true;
            }
            ui.label(egui_phosphor::regular::CARET_RIGHT);
            ui.strong(format!("{} Trash", egui_phosphor::regular::TRASH));
            ui.separator();
            if ui
                .add_enabled(action_enabled, egui::Button::new("Refresh"))
                .clicked()
            {
                refresh = true;
            }
        });
        if back {
            app.show_library_view(LibraryView::Cloud);
        } else if refresh {
            app.library.refresh(ui.ctx());
        }
        return;
    }
    let breadcrumbs = app.library.cloud_breadcrumbs();
    let children = app
        .library
        .cloud_folders
        .iter()
        .filter(|folder| folder.parent_id == app.library.cloud_folder_id)
        .cloned()
        .collect::<Vec<_>>();
    let current_folder = app
        .library
        .cloud_folder(&app.library.cloud_folder_id)
        .cloned();
    let action_enabled = !app.library.cloud_action_in_progress()
        && !app.library.cloud_upload_in_progress()
        && !app.library.image_paste_in_progress()
        && app.library.cloud_open_receiver.is_none();
    let has_clipboard =
        app.library.cloud_clipboard.is_some() || app.library.image_clipboard.is_some();
    let mut navigate_to = None;
    let mut create_folder = false;
    let mut paste = false;
    let mut folder_action = None;
    let mut open_trash = false;

    ui.horizontal_wrapped(|ui| {
        for (index, (folder_id, name)) in breadcrumbs.iter().enumerate() {
            if index > 0 {
                ui.label(egui_phosphor::regular::CARET_RIGHT);
            }
            if ui
                .add_enabled(
                    folder_id != &app.library.cloud_folder_id,
                    egui::Button::new(name).frame(false),
                )
                .clicked()
            {
                navigate_to = Some(folder_id.clone());
            }
        }
        ui.separator();
        if has_clipboard
            && ui
                .add_enabled(
                    action_enabled,
                    egui::Button::new(
                        app.library
                            .image_clipboard
                            .as_ref()
                            .map(ImageClipboard::paste_label)
                            .unwrap_or_else(|| "Paste here".to_owned()),
                    ),
                )
                .clicked()
        {
            paste = true;
        }
        ui.menu_button(egui_phosphor::regular::DOTS_THREE, |ui| {
            if ui
                .add_enabled(action_enabled, egui::Button::new("New folder…"))
                .clicked()
            {
                create_folder = true;
                ui.close();
            }
            if let Some(folder) = current_folder.as_ref() {
                ui.separator();
                if ui
                    .add_enabled(action_enabled, egui::Button::new("Copy folder"))
                    .clicked()
                {
                    folder_action = Some((CloudClipboardMode::Copy, folder.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(action_enabled, egui::Button::new("Cut folder"))
                    .clicked()
                {
                    folder_action = Some((CloudClipboardMode::Cut, folder.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(action_enabled, egui::Button::new("Rename folder…"))
                    .clicked()
                {
                    app.library.cloud_name_dialog = Some(CloudNameDialog {
                        name: folder.name.clone(),
                        kind: CloudNameDialogKind::RenameFolder {
                            folder: folder.clone(),
                        },
                        error: None,
                        focus_requested: false,
                    });
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(action_enabled, egui::Button::new("Delete folder…"))
                    .clicked()
                {
                    app.library.cloud_delete_confirmation =
                        Some(CloudDeleteTarget::Folder(folder.clone()));
                    ui.close();
                }
            }
        });
        if ui
            .button(format!("{} Trash", egui_phosphor::regular::TRASH))
            .clicked()
        {
            open_trash = true;
        }
    });

    if !children.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for folder in &children {
                if ui
                    .button(format!(
                        "{}  {}",
                        egui_phosphor::regular::FOLDER,
                        folder.name
                    ))
                    .clicked()
                {
                    navigate_to = Some(folder.id.clone());
                }
            }
        });
    }
    if let Some(folder_id) = navigate_to {
        app.select_cloud_library_folder(folder_id);
    }
    if open_trash {
        app.show_cloud_library_trash();
    }
    if create_folder {
        app.library.cloud_name_dialog = Some(CloudNameDialog {
            kind: CloudNameDialogKind::CreateFolder {
                parent_id: app.library.cloud_folder_id.clone(),
            },
            name: String::new(),
            error: None,
            focus_requested: false,
        });
    }
    if paste {
        paste_cloud_clipboard(app, app.library.cloud_folder_id.clone(), ui.ctx());
    }
    if let Some((mode, folder)) = folder_action {
        app.library.cloud_clipboard = Some(CloudClipboard {
            mode,
            content: CloudClipboardContent::Folder(folder.clone()),
        });
        app.library.image_clipboard = None;
        #[cfg(not(target_os = "android"))]
        {
            app.library.folder_clipboard = None;
        }
        app.library.status = format!(
            "{} cloud folder {}. Choose Paste in a destination.",
            if mode == CloudClipboardMode::Copy {
                "Copied"
            } else {
                "Cut"
            },
            folder.name
        );
    }
}

pub(super) fn show_local_image_paste_bar(ui: &mut Ui, app: &mut AurawApp) {
    if app.library.is_cloud_view() {
        return;
    }
    let Some(clipboard) = app.library.image_clipboard.as_ref() else {
        return;
    };
    let label = format!("{} here", clipboard.paste_label());
    let enabled = {
        #[cfg(not(target_os = "android"))]
        {
            !app.library.file_action_in_progress() && app.library.folder.is_some()
        }
        #[cfg(target_os = "android")]
        {
            !app.library.image_paste_in_progress()
                && !app.library.cloud_action_in_progress()
                && !app.library.cloud_upload_in_progress()
                && app.library.cloud_open_receiver.is_none()
        }
    };
    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        #[cfg(not(target_os = "android"))]
        if let Some(folder) = app.library.folder.clone() {
            start_image_clipboard_paste(app, ImagePasteDestination::LocalFolder(folder), ui.ctx());
        }
        #[cfg(target_os = "android")]
        start_image_clipboard_paste(app, ImagePasteDestination::LocalLibrary, ui.ctx());
    }
}

