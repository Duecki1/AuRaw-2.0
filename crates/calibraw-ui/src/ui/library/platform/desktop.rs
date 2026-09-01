use super::super::*;

pub(in crate::ui::library) fn default_thumbnail_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(2)
        .clamp(1, 4)
}

pub(in crate::ui::library) fn maximum_thumbnail_worker_count() -> usize {
    super::super::MAX_DESKTOP_THUMBNAIL_WORKERS
}

#[allow(clippy::too_many_arguments)]
pub(super) fn show_library_folder_node(
    ui: &mut Ui,
    node: &LibraryFolderNode,
    root_folder: &Path,
    selected_folder: Option<&Path>,
    clipboard: Option<&LibraryFolderClipboard>,
    image_clipboard: Option<&ImageClipboard>,
    action_in_progress: bool,
    expanded_folders: &mut HashSet<PathBuf>,
    requested_folder: &mut Option<PathBuf>,
    requested_action: &mut Option<LibraryFolderUiAction>,
) {
    let has_children = !node.children.is_empty();
    let expanded = has_children && expanded_folders.contains(&node.path);
    let selected = selected_folder == Some(node.path.as_path());

    ui.push_id(&node.path, |ui| {
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
                    .on_hover_text(if expanded {
                        "Collapse folder"
                    } else {
                        "Expand folder"
                    })
                    .clicked()
                {
                    if expanded {
                        expanded_folders.remove(&node.path);
                    } else {
                        expanded_folders.insert(node.path.clone());
                    }
                }
            } else {
                ui.allocate_space(egui::Vec2::splat(crate::ui::theme::CONTROL_HEIGHT));
            }

            let folder_icon = if expanded && has_children {
                egui_phosphor::regular::FOLDER_OPEN
            } else {
                egui_phosphor::regular::FOLDER
            };
            let response = crate::ui::theme::navigation_row(
                ui,
                egui::RichText::new(format!("{folder_icon}  {}", node.name)),
                selected,
                Sense::click_and_drag(),
            )
            .on_hover_text(format!(
                "{}\nDrag onto another folder to move",
                node.path.display()
            ));
            response.dnd_set_drag_payload(LibraryFolderDrag(node.path.clone()));
            if response.clicked() {
                *requested_folder = Some(node.path.clone());
            }

            let is_root = node.path == root_folder;
            response.context_menu(|ui| {
                let enabled = !action_in_progress;
                if ui
                    .add_enabled(enabled, egui::Button::new("New Folder…"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::New(node.path.clone()));
                    ui.close();
                }

                let paste_label = image_clipboard.map_or_else(
                    || {
                        clipboard
                            .and_then(|clipboard| clipboard.path.file_name())
                            .map(|name| format!("Paste “{}”", name.to_string_lossy()))
                            .unwrap_or_else(|| "Paste".to_owned())
                    },
                    ImageClipboard::paste_label,
                );
                if ui
                    .add_enabled(
                        enabled && (clipboard.is_some() || image_clipboard.is_some()),
                        egui::Button::new(paste_label),
                    )
                    .clicked()
                {
                    *requested_action = Some(if image_clipboard.is_some() {
                        LibraryFolderUiAction::PasteImages(node.path.clone())
                    } else {
                        LibraryFolderUiAction::Paste(node.path.clone())
                    });
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(enabled && !is_root, egui::Button::new("Copy Folder"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Copy(node.path.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(enabled && !is_root, egui::Button::new("Cut Folder"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Cut(node.path.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(enabled && !is_root, egui::Button::new("Rename Folder…"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Rename(node.path.clone()));
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(enabled && !is_root, egui::Button::new("Delete Folder…"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Delete(node.path.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Refresh Folders"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Refresh);
                    ui.close();
                }
            });

            if let Some(payload) = response.dnd_hover_payload::<LibraryFolderDrag>() {
                let source = &payload.0;
                let can_drop = !action_in_progress
                    && source != root_folder
                    && source != &node.path
                    && !node.path.starts_with(source);
                if can_drop {
                    ui.painter().rect_stroke(
                        response.rect.expand(2.0),
                        3.0,
                        Stroke::new(2.0, ui.visuals().selection.stroke.color),
                        StrokeKind::Outside,
                    );
                    if let Some(payload) = response.dnd_release_payload::<LibraryFolderDrag>() {
                        *requested_action = Some(LibraryFolderUiAction::Move {
                            source: payload.0.clone(),
                            destination_parent: node.path.clone(),
                        });
                    }
                }
            }
        });

        if expanded {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    for child in &node.children {
                        show_library_folder_node(
                            ui,
                            child,
                            root_folder,
                            selected_folder,
                            clipboard,
                            image_clipboard,
                            action_in_progress,
                            expanded_folders,
                            requested_folder,
                            requested_action,
                        );
                    }
                });
            });
        }
    });
}

pub(super) fn apply_library_folder_ui_action(
    app: &mut CalibRawApp,
    action: LibraryFolderUiAction,
    context: &egui::Context,
) {
    let Some(root) = app.library.root_folder.clone() else {
        return;
    };
    match action {
        LibraryFolderUiAction::New(parent) => {
            app.library.folder_name_dialog = Some(LibraryFolderNameDialog {
                kind: LibraryFolderNameDialogKind::Create { parent },
                name: String::new(),
                error: None,
            });
        }
        LibraryFolderUiAction::Copy(path) => {
            match canonical_library_directory(&root, &path, false) {
                Ok(_) => {
                    app.library.folder_clipboard = Some(LibraryFolderClipboard {
                        path: path.clone(),
                        mode: LibraryFolderClipboardMode::Copy,
                    });
                    app.library.image_clipboard = None;
                    app.library.status = format!(
                        "Copied folder {}. Choose Paste Folder in a destination.",
                        path.display()
                    );
                }
                Err(error) => app.library.status = error,
            }
        }
        LibraryFolderUiAction::Cut(path) => {
            match canonical_library_directory(&root, &path, false) {
                Ok(_) => {
                    app.library.folder_clipboard = Some(LibraryFolderClipboard {
                        path: path.clone(),
                        mode: LibraryFolderClipboardMode::Cut,
                    });
                    app.library.image_clipboard = None;
                    app.library.status = format!(
                        "Cut folder {}. Choose Paste Folder in a destination.",
                        path.display()
                    );
                }
                Err(error) => app.library.status = error,
            }
        }
        LibraryFolderUiAction::Paste(destination_parent) => {
            let Some(clipboard) = app.library.folder_clipboard.clone() else {
                app.library.status = "Copy or cut a folder first.".to_owned();
                return;
            };
            if clipboard.mode == LibraryFolderClipboardMode::Cut
                && app
                    .develop
                    .current_path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&clipboard.path))
            {
                app.library.status =
                    "Open an image outside this folder before moving it.".to_owned();
                return;
            }
            let operation = match clipboard.mode {
                LibraryFolderClipboardMode::Copy => LibraryFolderOperation::Copy {
                    root,
                    source: clipboard.path,
                    destination_parent,
                },
                LibraryFolderClipboardMode::Cut => LibraryFolderOperation::Move {
                    root,
                    source: clipboard.path,
                    destination_parent,
                    new_name: None,
                },
            };
            app.library.start_folder_operation(operation, context);
        }
        LibraryFolderUiAction::PasteImages(destination_parent) => {
            start_image_clipboard_paste(
                app,
                LibraryTransferDestination::LocalFolder(destination_parent),
                context,
            );
        }
        LibraryFolderUiAction::Rename(source) => {
            if app
                .develop
                .current_path
                .as_ref()
                .is_some_and(|path| path.starts_with(&source))
            {
                app.library.status =
                    "Open an image outside this folder before renaming it.".to_owned();
                return;
            }
            let Some(name) = source
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
            else {
                app.library.status = "This folder name cannot be edited as text.".to_owned();
                return;
            };
            app.library.folder_name_dialog = Some(LibraryFolderNameDialog {
                kind: LibraryFolderNameDialogKind::Rename { source },
                name,
                error: None,
            });
        }
        LibraryFolderUiAction::Delete(path) => {
            app.library.folder_delete_confirmation = Some(path);
        }
        LibraryFolderUiAction::Move {
            source,
            destination_parent,
        } => {
            if app
                .develop
                .current_path
                .as_ref()
                .is_some_and(|path| path.starts_with(&source))
            {
                app.library.status =
                    "Open an image outside this folder before moving it.".to_owned();
                return;
            }
            app.library.start_folder_operation(
                LibraryFolderOperation::Move {
                    root,
                    source,
                    destination_parent,
                    new_name: None,
                },
                context,
            );
        }
        LibraryFolderUiAction::Refresh => app.library.refresh(context),
    }
}

pub(in crate::ui::library) fn start_local_library_ai_mask_refresh(
    app: &mut CalibRawApp,
    assets: &[LibraryAsset],
    frame: &eframe::Frame,
) {
    app.start_library_ai_mask_refresh_paths(desktop_paths(assets), frame);
}

pub(in crate::ui::library) fn start_local_library_export(
    app: &mut CalibRawApp,
    assets: &[LibraryAsset],
    settings: ExportSettings,
    format: ExportFormat,
    frame: &eframe::Frame,
) -> bool {
    let Some(jobs) = library_export_jobs(&desktop_paths(assets), format) else {
        return false;
    };
    app.start_library_exports(jobs, settings, format, frame);
    true
}

pub(in crate::ui::library) fn local_action_in_progress(app: &CalibRawApp) -> bool {
    app.library.file_action_in_progress()
}

pub(in crate::ui::library) fn local_folders_available(app: &CalibRawApp) -> bool {
    app.library.root_folder.is_some()
}

pub(in crate::ui::library) fn can_create_local_folder(app: &CalibRawApp) -> bool {
    app.library.folder.is_some()
}

pub(in crate::ui::library) fn apply_local_toolbar_action(
    app: &mut CalibRawApp,
    action: super::LocalFolderToolbarAction,
    context: &egui::Context,
) {
    let Some(folder) = app.library.folder.clone() else {
        if matches!(action, super::LocalFolderToolbarAction::Refresh) {
            app.library.refresh(context);
        }
        return;
    };
    let action = match action {
        super::LocalFolderToolbarAction::Refresh => LibraryFolderUiAction::Refresh,
        super::LocalFolderToolbarAction::New => LibraryFolderUiAction::New(folder),
    };
    apply_library_folder_ui_action(app, action, context);
}

pub(in crate::ui::library) fn show_local_folder_tree(
    ui: &mut Ui,
    app: &mut CalibRawApp,
    action_in_progress: bool,
) {
    let tree = app.library.folder_tree.as_ref();
    let root_folder = app.library.root_folder.as_deref();
    let selected_folder = app.library.folder.as_deref();
    let clipboard = app.library.folder_clipboard.as_ref();
    let image_clipboard = app.library.image_clipboard.as_ref();
    let expanded_folders = &mut app.library.expanded_folders;
    let mut requested_folder = None;
    let mut requested_action = None;

    if let (Some(tree), Some(root_folder)) = (tree, root_folder) {
        show_library_folder_node(
            ui,
            tree,
            root_folder,
            selected_folder,
            clipboard,
            image_clipboard,
            action_in_progress,
            expanded_folders,
            &mut requested_folder,
            &mut requested_action,
        );
    } else {
        ui.label(
            egui::RichText::new("Open a top-level folder to browse its hierarchy.")
                .small()
                .color(ui.visuals().weak_text_color()),
        );
    }

    if let Some(folder) = requested_folder {
        app.select_library_folder(folder);
    }
    if let Some(action) = requested_action {
        apply_library_folder_ui_action(app, action, ui.ctx());
    }
}

pub(in crate::ui::library) fn show_sidebar_dialogs(ui: &mut Ui, app: &mut CalibRawApp) {
    show_library_folder_dialogs(ui, app);
}

pub(in crate::ui::library) fn show_page_dialogs(_ui: &mut Ui, _app: &mut CalibRawApp) {}
