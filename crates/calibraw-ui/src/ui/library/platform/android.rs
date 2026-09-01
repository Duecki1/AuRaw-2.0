use super::super::*;

pub(in crate::ui::library) fn default_thumbnail_worker_count() -> usize {
    1
}

pub(in crate::ui::library) fn maximum_thumbnail_worker_count() -> usize {
    super::super::MAX_ANDROID_THUMBNAIL_WORKERS
}

#[allow(clippy::too_many_arguments)]
pub(super) fn show_android_library_folder_node(
    ui: &mut Ui,
    path: &str,
    name: &str,
    children_by_parent: &HashMap<&str, Vec<&crate::android::LibraryFolder>>,
    selected_folder: &str,
    action_in_progress: bool,
    expanded_folders: &mut HashSet<String>,
    requested_action: &mut Option<AndroidLibraryFolderUiAction>,
) {
    let children = children_by_parent
        .get(path)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let has_children = !children.is_empty();
    let expanded = has_children && expanded_folders.contains(path);
    let selected = selected_folder == path;

    ui.push_id(("android-library-folder", path), |ui| {
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
                        crate::ui::theme::toolbar_icon_size(),
                        egui::Button::new(egui::RichText::new(caret).size(13.0)).frame(false),
                    )
                    .clicked()
                {
                    if expanded {
                        expanded_folders.remove(path);
                    } else {
                        expanded_folders.insert(path.to_owned());
                    }
                }
            } else {
                ui.allocate_space(crate::ui::theme::toolbar_icon_size());
            }

            let icon = if expanded && has_children {
                egui_phosphor::regular::FOLDER_OPEN
            } else {
                egui_phosphor::regular::FOLDER
            };
            let response = ui.add_enabled_ui(!action_in_progress, |ui| {
                crate::ui::theme::navigation_row(
                    ui,
                    egui::RichText::new(format!("{icon}  {name}")),
                    selected,
                    Sense::click(),
                )
            });
            if response.inner.clicked() {
                *requested_action = Some(AndroidLibraryFolderUiAction::Select(path.to_owned()));
            }
            response.inner.context_menu(|ui| {
                if ui.button("New folder here…").clicked() {
                    *requested_action = Some(AndroidLibraryFolderUiAction::New(path.to_owned()));
                    ui.close();
                }
                if ui.button("Refresh folders").clicked() {
                    *requested_action = Some(AndroidLibraryFolderUiAction::Refresh);
                    ui.close();
                }
            });
        });

        if expanded {
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    for child in children {
                        show_android_library_folder_node(
                            ui,
                            &child.path,
                            &child.name,
                            children_by_parent,
                            selected_folder,
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

pub(super) fn apply_android_library_folder_ui_action(
    app: &mut CalibRawApp,
    action: AndroidLibraryFolderUiAction,
    context: &egui::Context,
) {
    match action {
        AndroidLibraryFolderUiAction::Select(folder) => {
            app.select_android_library_folder(folder);
            app.set_library_folder_sidebar_open(false);
        }
        AndroidLibraryFolderUiAction::New(parent) => {
            app.library.platform.folder_name_dialog = Some(AndroidLibraryFolderNameDialog {
                parent,
                name: String::new(),
                error: None,
                focus_requested: false,
            });
        }
        AndroidLibraryFolderUiAction::Refresh => app.library.refresh(context),
    }
}

pub(in crate::ui::library) fn start_local_library_ai_mask_refresh(
    app: &mut CalibRawApp,
    assets: &[LibraryAsset],
    frame: &eframe::Frame,
) {
    app.start_library_ai_mask_refresh_android(android_targets(assets), frame);
}

pub(in crate::ui::library) fn start_local_library_export(
    app: &mut CalibRawApp,
    assets: &[LibraryAsset],
    settings: ExportSettings,
    format: ExportFormat,
    _frame: &eframe::Frame,
) -> bool {
    let targets = android_targets(assets)
        .into_iter()
        .map(|(uri, display_name)| crate::app::AndroidLibraryExportTarget { uri, display_name })
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return false;
    }
    app.start_android_library_exports(targets, settings, format);
    true
}

pub(in crate::ui::library) fn local_action_in_progress(app: &CalibRawApp) -> bool {
    app.library.local_mutation_in_progress() || app.android.picker_pending
}

pub(in crate::ui::library) fn local_folders_available(_app: &CalibRawApp) -> bool {
    true
}

pub(in crate::ui::library) fn can_create_local_folder(_app: &CalibRawApp) -> bool {
    true
}

pub(in crate::ui::library) fn apply_local_toolbar_action(
    app: &mut CalibRawApp,
    action: super::LocalFolderToolbarAction,
    context: &egui::Context,
) {
    let action = match action {
        super::LocalFolderToolbarAction::Refresh => AndroidLibraryFolderUiAction::Refresh,
        super::LocalFolderToolbarAction::New => {
            AndroidLibraryFolderUiAction::New(app.library.platform.folder.clone())
        }
    };
    apply_android_library_folder_ui_action(app, action, context);
}

pub(in crate::ui::library) fn show_local_folder_tree(
    ui: &mut Ui,
    app: &mut CalibRawApp,
    action_in_progress: bool,
) {
    let PlatformLibraryState {
        folder: selected_folder,
        folders,
        expanded_folders,
        ..
    } = &mut app.library.platform;
    let mut children_by_parent = HashMap::<&str, Vec<&crate::android::LibraryFolder>>::new();
    for folder in folders.iter() {
        children_by_parent
            .entry(android_folder_parent(&folder.path))
            .or_default()
            .push(folder);
    }
    let mut requested_action = None;

    show_android_library_folder_node(
        ui,
        "",
        "Library",
        &children_by_parent,
        selected_folder.as_str(),
        action_in_progress,
        expanded_folders,
        &mut requested_action,
    );

    if let Some(action) = requested_action {
        apply_android_library_folder_ui_action(app, action, ui.ctx());
    }
}

pub(in crate::ui::library) fn show_sidebar_dialogs(_ui: &mut Ui, _app: &mut CalibRawApp) {}

pub(in crate::ui::library) fn show_page_dialogs(ui: &mut Ui, app: &mut CalibRawApp) {
    show_android_library_folder_dialog(ui, app);
}
