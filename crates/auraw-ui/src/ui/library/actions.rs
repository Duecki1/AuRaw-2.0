use super::*;

#[cfg(not(target_os = "android"))]
pub(crate) fn cloud_image_context_menu(
    ui: &mut Ui,
    app: &AurawApp,
    assets: &[crate::cloud::CloudAsset],
) -> Option<CloudLibraryCardAction> {
    let selected_count = assets.len();
    let action_enabled = !app.library.cloud_action_in_progress()
        && !app.library.cloud_upload_in_progress()
        && !app.library.image_paste_in_progress()
        && app.library.cloud_open_receiver.is_none()
        && app.library_batch_export_progress().is_none()
        && !assets.is_empty();
    let mut action = None;

    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(if selected_count > 1 {
                "Export selected…"
            } else {
                "Export…"
            }),
        )
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Export(assets.to_vec()));
        ui.close();
    }

    ui.separator();
    if ui
        .add_enabled(
            action_enabled && selected_count == 1,
            egui::Button::new("Copy adjustments"),
        )
        .clicked()
    {
        action = assets
            .first()
            .cloned()
            .map(CloudLibraryCardAction::CopyAdjustments);
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled && app.has_copied_adjustments(),
            egui::Button::new(if selected_count > 1 {
                "Paste adjustments to selected"
            } else {
                "Paste adjustments"
            }),
        )
        .on_disabled_hover_text("Copy adjustments from an image first")
        .clicked()
    {
        action = Some(CloudLibraryCardAction::PasteAdjustments(assets.to_vec()));
        ui.close();
    }

    ui.separator();
    if ui
        .add_enabled(action_enabled, egui::Button::new("Copy"))
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Copy(assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(action_enabled, egui::Button::new("Cut"))
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Cut(assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(if selected_count > 1 {
                "Duplicate selected"
            } else {
                "Duplicate"
            }),
        )
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Duplicate(assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled && selected_count == 1,
            egui::Button::new("Rename…"),
        )
        .clicked()
    {
        action = assets.first().cloned().map(CloudLibraryCardAction::Rename);
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(format!(
                "{}  {}",
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                if selected_count > 1 {
                    "Reset adjustments for selected"
                } else {
                    "Reset all adjustments"
                }
            )),
        )
        .clicked()
    {
        action = Some(CloudLibraryCardAction::ResetAdjustments(assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(if selected_count > 1 {
                "Delete selected…"
            } else {
                "Delete…"
            }),
        )
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Delete(assets.to_vec()));
        ui.close();
    }
    action
}

pub(super) fn detach_current_cloud_asset_if_selected(app: &mut AurawApp, assets: &[crate::cloud::CloudAsset]) {
    let current = app.current_path.clone();
    let selected_current = current.as_ref().is_some_and(|path| {
        crate::cloud::cached_asset_id_for_raw(path)
            .is_some_and(|asset_id| assets.iter().any(|asset| asset.id == asset_id))
    });
    if selected_current {
        if let Some(path) = current.as_deref() {
            app.detach_current_file_for_library_action(path);
        }
        app.current_path = None;
    }
}

pub(super) fn detach_current_cloud_asset_if_inside_folder(app: &mut AurawApp, folder_id: &str) {
    let current = app.current_path.clone();
    let current_folder_id = current
        .as_deref()
        .and_then(crate::cloud::cached_asset_id_for_raw)
        .and_then(|asset_id| app.library.cloud_asset_folders.get(&asset_id))
        .cloned();
    let inside_folder = current_folder_id
        .as_deref()
        .is_some_and(|current_folder_id| {
            cloud_folder_contains(&app.library.cloud_folders, folder_id, current_folder_id)
        });
    if inside_folder {
        if let Some(path) = current.as_deref() {
            app.detach_current_file_for_library_action(path);
        }
        app.current_path = None;
    }
}

pub(crate) fn apply_cloud_image_action(
    app: &mut AurawApp,
    action: CloudLibraryCardAction,
    context: &egui::Context,
) {
    match action {
        CloudLibraryCardAction::Export(assets) => app.library.start_cloud_action(
            CloudActionRequest::PrepareAssets {
                assets,
                purpose: CloudPreparedPurpose::Export,
            },
            context,
        ),
        CloudLibraryCardAction::CopyAdjustments(asset) => app.library.start_cloud_action(
            CloudActionRequest::PrepareAssets {
                assets: vec![asset],
                purpose: CloudPreparedPurpose::CopyAdjustments,
            },
            context,
        ),
        CloudLibraryCardAction::PasteAdjustments(assets) => app.library.start_cloud_action(
            CloudActionRequest::PrepareAssets {
                assets,
                purpose: CloudPreparedPurpose::PasteAdjustments,
            },
            context,
        ),
        CloudLibraryCardAction::Copy(assets) => {
            let count = assets.len();
            app.library.image_clipboard = Some(ImageClipboard {
                mode: ImageClipboardMode::Copy,
                content: ImageClipboardContent::Cloud(assets),
            });
            app.library.cloud_clipboard = None;
            #[cfg(not(target_os = "android"))]
            {
                app.library.folder_clipboard = None;
            }
            app.library.status = format!(
                "Copied {count} cloud RAW{}. Choose Paste in any local or cloud folder.",
                if count == 1 { "" } else { "s" }
            );
        }
        CloudLibraryCardAction::Cut(assets) => {
            let count = assets.len();
            app.library.image_clipboard = Some(ImageClipboard {
                mode: ImageClipboardMode::Cut,
                content: ImageClipboardContent::Cloud(assets),
            });
            app.library.cloud_clipboard = None;
            #[cfg(not(target_os = "android"))]
            {
                app.library.folder_clipboard = None;
            }
            app.library.status = format!(
                "Cut {count} cloud RAW{}. Choose Paste in any local or cloud folder.",
                if count == 1 { "" } else { "s" }
            );
        }
        CloudLibraryCardAction::Duplicate(assets) => app.library.start_cloud_action(
            CloudActionRequest::CopyAssets {
                assets,
                destination_folder_id: app.library.cloud_folder_id.clone(),
                clear_clipboard: false,
            },
            context,
        ),
        CloudLibraryCardAction::Rename(asset) => {
            app.library.cloud_name_dialog = Some(CloudNameDialog {
                name: asset.name.clone(),
                kind: CloudNameDialogKind::RenameAsset { asset },
                error: None,
                focus_requested: false,
            });
        }
        CloudLibraryCardAction::ResetAdjustments(assets) => {
            detach_current_cloud_asset_if_selected(app, &assets);
            app.library
                .start_cloud_action(CloudActionRequest::ResetAssets { assets }, context);
        }
        CloudLibraryCardAction::Delete(assets) => {
            app.library.cloud_delete_confirmation = Some(CloudDeleteTarget::Assets(assets));
        }
    }
}

#[cfg(not(target_os = "android"))]
pub(crate) enum LibraryCardAction {
    Export(Vec<PathBuf>),
    CopyAdjustments(PathBuf),
    PasteAdjustments(Vec<PathBuf>),
    Copy(Vec<PathBuf>),
    Cut(Vec<PathBuf>),
    Duplicate(Vec<PathBuf>),
    Rename(PathBuf),
    ResetAdjustments(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
}

#[cfg(not(target_os = "android"))]
pub(crate) fn desktop_image_context_menu(
    ui: &mut Ui,
    app: &AurawApp,
    context_source_path: &Path,
    context_paths: &[PathBuf],
) -> Option<LibraryCardAction> {
    let selected_count = context_paths.len();
    let action_enabled = !app.library.file_action_in_progress()
        && app.library_batch_export_progress().is_none()
        && app.library_ai_mask_refresh_status().is_none()
        && !context_paths.is_empty();
    let can_paste_adjustments = action_enabled && app.has_copied_adjustments();
    let mut action = None;

    let export_label = if selected_count > 1 {
        "Export selected…"
    } else {
        "Export…"
    };
    if ui
        .add_enabled(action_enabled, egui::Button::new(export_label))
        .clicked()
    {
        action = Some(LibraryCardAction::Export(context_paths.to_vec()));
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(
            action_enabled && selected_count == 1,
            egui::Button::new("Copy adjustments"),
        )
        .clicked()
    {
        action = Some(LibraryCardAction::CopyAdjustments(
            context_source_path.to_path_buf(),
        ));
        ui.close();
    }
    let paste_label = if selected_count > 1 {
        "Paste adjustments to selected"
    } else {
        "Paste adjustments"
    };
    if ui
        .add_enabled(can_paste_adjustments, egui::Button::new(paste_label))
        .on_disabled_hover_text("Copy adjustments from an image first")
        .clicked()
    {
        action = Some(LibraryCardAction::PasteAdjustments(context_paths.to_vec()));
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(action_enabled, egui::Button::new("Copy"))
        .clicked()
    {
        action = Some(LibraryCardAction::Copy(context_paths.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(action_enabled, egui::Button::new("Cut"))
        .clicked()
    {
        action = Some(LibraryCardAction::Cut(context_paths.to_vec()));
        ui.close();
    }
    let duplicate_label = if selected_count > 1 {
        "Duplicate selected (RAW + sidecars)"
    } else {
        "Duplicate (RAW + sidecar)"
    };
    if ui
        .add_enabled(action_enabled, egui::Button::new(duplicate_label))
        .clicked()
    {
        action = Some(LibraryCardAction::Duplicate(context_paths.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled && selected_count == 1,
            egui::Button::new("Rename…"),
        )
        .clicked()
    {
        action = context_paths
            .first()
            .cloned()
            .map(LibraryCardAction::Rename);
        ui.close();
    }
    let reset_label = if selected_count > 1 {
        "Reset adjustments for selected"
    } else {
        "Reset all adjustments"
    };
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(format!(
                "{}  {reset_label}",
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE
            )),
        )
        .clicked()
    {
        action = Some(LibraryCardAction::ResetAdjustments(context_paths.to_vec()));
        ui.close();
    }
    ui.separator();
    let delete_label = if selected_count > 1 {
        "Delete selected"
    } else {
        "Delete"
    };
    if ui
        .add_enabled(action_enabled, egui::Button::new(delete_label))
        .clicked()
    {
        action = Some(LibraryCardAction::Delete(context_paths.to_vec()));
        ui.close();
    }

    action
}

#[cfg(not(target_os = "android"))]
pub(crate) fn apply_desktop_image_action(
    ui: &mut Ui,
    app: &mut AurawApp,
    frame: &eframe::Frame,
    action: LibraryCardAction,
) {
    match action {
        LibraryCardAction::Export(paths) => {
            if !paths.is_empty() {
                app.library.export_dialog = Some(LibraryExportDialog {
                    paths,
                    settings: app.export_settings.clone(),
                    format: ExportFormat::Jpeg,
                });
            }
        }
        LibraryCardAction::CopyAdjustments(path) => {
            let status = match app.copy_library_adjustments_from_path(&path) {
                Ok(()) => format!(
                    "Copied adjustments from {}",
                    app.copied_adjustments_source_label().unwrap_or("image")
                ),
                Err(error) => format!("Could not copy adjustments: {error}"),
            };
            app.library.status = status;
        }
        LibraryCardAction::PasteAdjustments(paths) => {
            let (edited_count, failures) = app.library_adjustment_edit_count_paths(&paths);
            if failures.is_empty() {
                if edited_count > 0 {
                    app.library.adjustment_paste_dialog = Some(LibraryAdjustmentPasteDialog {
                        paths,
                        edited_count,
                    });
                } else {
                    apply_library_adjustment_paste(
                        app,
                        paths,
                        crate::sidecar::AdjustmentPasteMode::Merge,
                        ui.ctx(),
                        frame,
                    );
                }
            } else {
                app.library.status = format!(
                    "Could not inspect selected adjustments. {}",
                    failures.join(" · ")
                );
            }
        }
        LibraryCardAction::Copy(paths) => {
            let mode = ImageClipboardMode::Copy;
            let count = paths.len();
            app.library.image_clipboard = Some(ImageClipboard {
                mode,
                content: ImageClipboardContent::Local(paths),
            });
            app.library.cloud_clipboard = None;
            app.library.folder_clipboard = None;
            app.library.status = format!(
                "{} {count} local RAW{}. Choose Paste in any local or cloud folder.",
                if mode == ImageClipboardMode::Copy {
                    "Copied"
                } else {
                    "Cut"
                },
                if count == 1 { "" } else { "s" }
            );
        }
        LibraryCardAction::Cut(paths) => {
            let mode = ImageClipboardMode::Cut;
            let count = paths.len();
            app.library.image_clipboard = Some(ImageClipboard {
                mode,
                content: ImageClipboardContent::Local(paths),
            });
            app.library.cloud_clipboard = None;
            app.library.folder_clipboard = None;
            app.library.status = format!(
                "Cut {count} local RAW{}. Choose Paste in any local or cloud folder.",
                if count == 1 { "" } else { "s" }
            );
        }
        LibraryCardAction::Duplicate(paths) => {
            app.library.clear_selection();
            app.library.duplicate_raws_with_sidecars(paths, ui.ctx());
        }
        LibraryCardAction::Rename(path) => {
            let Some(name) = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
            else {
                app.library.status = "This RAW filename cannot be edited as text.".to_owned();
                return;
            };
            app.library.raw_name_dialog = Some(LibraryRawNameDialog {
                source: path,
                name,
                error: None,
                focus_requested: false,
            });
        }
        LibraryCardAction::ResetAdjustments(paths) => {
            let current_to_reopen = app
                .current_path
                .as_ref()
                .and_then(|current| paths.iter().find(|path| *path == current).cloned());
            if let Some(path) = current_to_reopen.as_deref() {
                app.detach_current_file_for_library_action(path);
            }

            let total = paths.len();
            let mut failures = Vec::new();
            let mut reset_count = 0usize;
            for path in &paths {
                match crate::sidecar::reset_desktop_adjustments(path) {
                    Ok(reset) => {
                        app.library.invalidate_adjustment_thumbnail_for_path(path);
                        if reset {
                            reset_count += 1;
                        }
                    }
                    Err(error) => failures.push(format!("{}: {error}", path.display())),
                }
            }
            app.library.clear_selection();
            app.library.refresh(ui.ctx());
            app.library.status = if failures.is_empty() {
                format!(
                    "Cleared all adjustments for {total} selected {} ({reset_count} changed)",
                    if total == 1 { "image" } else { "images" }
                )
            } else {
                format!(
                    "Cleared all adjustments for {} of {total} selected images. {}",
                    total.saturating_sub(failures.len()),
                    failures.join(" · ")
                )
            };
            if let Some(path) = current_to_reopen {
                app.reload_desktop_library_document_after_reset(path, frame);
            }
        }
        LibraryCardAction::Delete(paths) => {
            let current_target = app
                .current_path
                .as_ref()
                .and_then(|current| paths.iter().find(|path| *path == current).cloned());
            if let Some(path) = current_target.as_deref() {
                app.detach_current_file_for_library_action(path);
            }

            let total = paths.len();
            let mut failures = Vec::new();
            let mut cleanup_warnings = Vec::new();
            let mut deleted_current = false;
            for path in &paths {
                match fs::remove_file(path) {
                    Ok(()) => {
                        if current_target.as_ref() == Some(path) {
                            deleted_current = true;
                        }
                        if let Err(error) = crate::sidecar::remove_desktop_edits(path) {
                            cleanup_warnings.push(format!("{}: {error}", path.display()));
                        }
                    }
                    Err(error) => failures.push(format!("{}: {error}", path.display())),
                }
            }
            if deleted_current {
                app.current_path = None;
            }
            app.library.clear_selection();
            app.library.refresh(ui.ctx());
            let deleted = total.saturating_sub(failures.len());
            app.library.status = if failures.is_empty() && cleanup_warnings.is_empty() {
                format!(
                    "Deleted {deleted} selected {}",
                    if deleted == 1 { "image" } else { "images" }
                )
            } else {
                let mut details = failures;
                details.extend(cleanup_warnings);
                format!(
                    "Deleted {deleted} of {total} selected images. {}",
                    details.join(" · ")
                )
            };
            if !deleted_current {
                if let Some(path) = current_target {
                    app.open_path(path, frame);
                }
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
pub(crate) fn show_desktop_image_action_overlays(
    ui: &mut Ui,
    app: &mut AurawApp,
    frame: &eframe::Frame,
) {
    // These overlays normally live in `Library::show`. Develop's filmstrip now
    // exposes the same image actions, so keep their modal follow-up UI available
    // without forcing a tab switch back to Library.
    let paste_choice = app.library.adjustment_paste_dialog.as_ref().and_then(|dialog| {
        show_adjustment_paste_choice(
            ui,
            "library-adjustment-paste-conflict-dialog",
            dialog.edited_count,
            dialog.paths.len(),
        )
    });
    if let Some(choice) = paste_choice {
        if let Some(dialog) = app.library.adjustment_paste_dialog.take() {
            match choice {
                AdjustmentPasteChoice::Merge => apply_library_adjustment_paste(
                    app,
                    dialog.paths,
                    crate::sidecar::AdjustmentPasteMode::Merge,
                    ui.ctx(),
                    frame,
                ),
                AdjustmentPasteChoice::Replace => apply_library_adjustment_paste(
                    app,
                    dialog.paths,
                    crate::sidecar::AdjustmentPasteMode::Replace,
                    ui.ctx(),
                    frame,
                ),
                AdjustmentPasteChoice::Cancel => {}
            }
        }
    }

    let can_regenerate = app.can_start_library_ai_mask_refresh();
    let refresh_choice = app.library.ai_mask_refresh_prompt.as_ref().and_then(|prompt| {
        show_ai_mask_refresh_choice(
            ui,
            "library-ai-mask-refresh-prompt",
            prompt.paths.len(),
            can_regenerate,
        )
    });
    if let Some(choice) = refresh_choice {
        if let Some(prompt) = app.library.ai_mask_refresh_prompt.take() {
            if choice == AiMaskRefreshChoice::Regenerate {
                app.start_library_ai_mask_refresh_paths(prompt.paths, frame);
            }
        }
    }

    if let Some((completed, total, failed, current_name)) = app.library_ai_mask_refresh_status() {
        if app.library_ai_mask_refresh_progress_open() {
            let (minimize, cancel) = show_ai_mask_refresh_progress(
                ui,
                completed,
                total,
                failed,
                current_name.as_deref(),
                true,
            );
            if minimize {
                app.minimize_library_ai_mask_refresh_progress();
            }
            if cancel {
                app.cancel_library_ai_mask_refresh();
            }
        }
    }

    let mut close_export_dialog = false;
    let mut confirm_export = false;
    if let Some(dialog) = app.library.export_dialog.as_mut() {
        let count = dialog.paths.len();
        let export_picker_directory = dialog
            .paths
            .first()
            .and_then(|path| path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf);
        let title = if count == 1 {
            "Export image".to_owned()
        } else {
            format!("Export {count} images")
        };
        crate::ui::responsive_popup(egui::Window::new(title), ui.ctx(), 480.0)
            .id(egui::Id::new("library-export-dialog"))
            .collapsible(false)
            .resizable(true)
            .show(ui.ctx(), |ui| {
                show_library_export_settings_controls(
                    ui,
                    &mut dialog.format,
                    &mut dialog.settings,
                    export_picker_directory.as_deref(),
                );
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(if count > 1 {
                        "A destination folder will be selected for the batch. File names are generated from each RAW name."
                    } else {
                        "Choose the output file after pressing Export."
                    })
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_export_dialog = true;
                    }
                    let label = if count == 1 {
                        "Export 1 image…".to_owned()
                    } else {
                        format!("Export {count} images…")
                    };
                    if ui.button(label).clicked() {
                        confirm_export = true;
                    }
                });
            });
    }

    if confirm_export {
        if let Some(dialog) = app.library.export_dialog.clone() {
            if let Some(jobs) = library_export_jobs(&dialog.paths, dialog.format) {
                app.library.clear_selection();
                app.library.export_dialog = None;
                app.start_library_exports(jobs, dialog.settings.clone(), dialog.format, frame);
            }
        }
    } else if close_export_dialog {
        app.library.export_dialog = None;
    }

    show_library_batch_export_progress(ui, app);
}

#[cfg(target_os = "android")]
pub(super) enum LibraryCardAction {
    Export(Vec<(String, String)>),
    CopyAdjustments((String, String)),
    PasteAdjustments(Vec<(String, String)>),
    Copy(Vec<AndroidImageClipboardItem>),
    Cut(Vec<AndroidImageClipboardItem>),
    Duplicate(Vec<(String, String)>),
    Rename(AndroidImageClipboardItem),
    ResetAdjustments(Vec<(String, String)>),
    Delete(Vec<(String, String)>),
}

#[cfg(target_os = "android")]
pub(super) fn android_selection_targets(selected: &[(LibrarySource, String)]) -> Vec<(String, String)> {
    selected
        .iter()
        .filter_map(|(source, _)| match source {
            LibrarySource::Android {
                uri, display_name, ..
            } => Some((uri.clone(), display_name.clone())),
            LibrarySource::Cloud(_) => None,
        })
        .collect()
}

#[cfg(target_os = "android")]
pub(super) fn android_selection_clipboard_targets(
    selected: &[(LibrarySource, String)],
) -> Vec<AndroidImageClipboardItem> {
    selected
        .iter()
        .filter_map(|(source, _)| match source {
            LibrarySource::Android {
                uri,
                display_name,
                bytes,
                ..
            } => Some(AndroidImageClipboardItem {
                uri: uri.clone(),
                display_name: display_name.clone(),
                bytes: *bytes,
            }),
            LibrarySource::Cloud(_) => None,
        })
        .collect()
}

pub(super) fn selection_bar_action_button(
    ui: &mut Ui,
    enabled: bool,
    compact: bool,
    glyph: &'static str,
    label: &'static str,
) -> egui::Response {
    if compact {
        crate::ui::icons::phosphor_icon_button_enabled(
            ui,
            enabled,
            glyph,
            crate::ui::theme::toolbar_icon_size(),
            label,
        )
    } else {
        ui.add_enabled(
            enabled,
            egui::Button::new(format!("{glyph}  {label}"))
                .min_size(egui::vec2(0.0, crate::ui::theme::CONTROL_HEIGHT)),
        )
        .on_hover_text(label)
    }
}

pub(super) fn selection_bar_more_menu<R>(
    ui: &mut Ui,
    enabled: bool,
    compact: bool,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<Option<R>> {
    let label = if compact {
        egui::RichText::new(egui_phosphor::regular::DOTS_THREE)
            .size(crate::ui::theme::CONTROL_HEIGHT * 0.55)
    } else {
        egui::RichText::new(format!("{}  More", egui_phosphor::regular::DOTS_THREE))
    };
    ui.add_enabled_ui(enabled, |ui| ui.menu_button(label, add_contents))
        .inner
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SelectionBarCommand {
    Export,
    CopyAdjustments,
    PasteAdjustments,
    Copy,
    Cut,
    Duplicate,
    Rename,
    ResetAdjustments,
    Delete,
}

pub(super) fn selection_bar_actions(
    ui: &mut Ui,
    selected_count: usize,
    action_enabled: bool,
    can_paste_adjustments: bool,
    compact: bool,
) -> Option<SelectionBarCommand> {
    let mut action = None;

    if selection_bar_action_button(
        ui,
        action_enabled,
        compact,
        egui_phosphor::regular::EXPORT,
        "Export",
    )
    .clicked()
    {
        action = Some(SelectionBarCommand::Export);
    }
    if selected_count == 1
        && selection_bar_action_button(
            ui,
            action_enabled,
            compact,
            egui_phosphor::regular::SLIDERS_HORIZONTAL,
            "Copy adjustments",
        )
        .clicked()
    {
        action = Some(SelectionBarCommand::CopyAdjustments);
    }
    if selection_bar_action_button(
        ui,
        action_enabled && can_paste_adjustments,
        compact,
        egui_phosphor::regular::CLIPBOARD_TEXT,
        "Paste adjustments",
    )
    .on_disabled_hover_text("Copy adjustments from an image first")
    .clicked()
    {
        action = Some(SelectionBarCommand::PasteAdjustments);
    }
    if selection_bar_action_button(
        ui,
        action_enabled,
        compact,
        egui_phosphor::regular::COPY,
        "Copy",
    )
    .clicked()
    {
        action = Some(SelectionBarCommand::Copy);
    }

    selection_bar_more_menu(ui, action_enabled, compact, |ui| {
        if ui.button("Cut").clicked() {
            action = Some(SelectionBarCommand::Cut);
            ui.close();
        }
        if ui
            .button(if selected_count > 1 {
                "Duplicate selected (RAW + sidecars)"
            } else {
                "Duplicate (RAW + sidecar)"
            })
            .clicked()
        {
            action = Some(SelectionBarCommand::Duplicate);
            ui.close();
        }
        if selected_count == 1 && ui.button("Rename…").clicked() {
            action = Some(SelectionBarCommand::Rename);
            ui.close();
        }
        if ui
            .button(if selected_count > 1 {
                "Reset adjustments for selected"
            } else {
                "Reset all adjustments"
            })
            .clicked()
        {
            action = Some(SelectionBarCommand::ResetAdjustments);
            ui.close();
        }
        ui.separator();
        if ui
            .button(if selected_count > 1 {
                "Delete selected"
            } else {
                "Delete"
            })
            .clicked()
        {
            action = Some(SelectionBarCommand::Delete);
            ui.close();
        }
    })
    .response
    .on_hover_text("More selection actions");

    action
}

pub(super) fn cloud_selection_action(
    command: SelectionBarCommand,
    assets: &[crate::cloud::CloudAsset],
) -> Option<CloudLibraryCardAction> {
    match command {
        SelectionBarCommand::Export => Some(CloudLibraryCardAction::Export(assets.to_vec())),
        SelectionBarCommand::CopyAdjustments if assets.len() == 1 => assets
            .first()
            .cloned()
            .map(CloudLibraryCardAction::CopyAdjustments),
        SelectionBarCommand::CopyAdjustments => None,
        SelectionBarCommand::PasteAdjustments => {
            Some(CloudLibraryCardAction::PasteAdjustments(assets.to_vec()))
        }
        SelectionBarCommand::Copy => Some(CloudLibraryCardAction::Copy(assets.to_vec())),
        SelectionBarCommand::Cut => Some(CloudLibraryCardAction::Cut(assets.to_vec())),
        SelectionBarCommand::Duplicate => Some(CloudLibraryCardAction::Duplicate(assets.to_vec())),
        SelectionBarCommand::Rename if assets.len() == 1 => {
            assets.first().cloned().map(CloudLibraryCardAction::Rename)
        }
        SelectionBarCommand::Rename => None,
        SelectionBarCommand::ResetAdjustments => {
            Some(CloudLibraryCardAction::ResetAdjustments(assets.to_vec()))
        }
        SelectionBarCommand::Delete => Some(CloudLibraryCardAction::Delete(assets.to_vec())),
    }
}

#[cfg(not(target_os = "android"))]
pub(super) fn desktop_selection_action(
    command: SelectionBarCommand,
    paths: &[PathBuf],
) -> Option<LibraryCardAction> {
    match command {
        SelectionBarCommand::Export => Some(LibraryCardAction::Export(paths.to_vec())),
        SelectionBarCommand::CopyAdjustments if paths.len() == 1 => paths
            .first()
            .cloned()
            .map(LibraryCardAction::CopyAdjustments),
        SelectionBarCommand::CopyAdjustments => None,
        SelectionBarCommand::PasteAdjustments => {
            Some(LibraryCardAction::PasteAdjustments(paths.to_vec()))
        }
        SelectionBarCommand::Copy => Some(LibraryCardAction::Copy(paths.to_vec())),
        SelectionBarCommand::Cut => Some(LibraryCardAction::Cut(paths.to_vec())),
        SelectionBarCommand::Duplicate => Some(LibraryCardAction::Duplicate(paths.to_vec())),
        SelectionBarCommand::Rename if paths.len() == 1 => {
            paths.first().cloned().map(LibraryCardAction::Rename)
        }
        SelectionBarCommand::Rename => None,
        SelectionBarCommand::ResetAdjustments => {
            Some(LibraryCardAction::ResetAdjustments(paths.to_vec()))
        }
        SelectionBarCommand::Delete => Some(LibraryCardAction::Delete(paths.to_vec())),
    }
}

#[cfg(target_os = "android")]
pub(super) fn android_selection_action(
    command: SelectionBarCommand,
    targets: &[(String, String)],
    clipboard_targets: &[AndroidImageClipboardItem],
) -> Option<LibraryCardAction> {
    match command {
        SelectionBarCommand::Export => Some(LibraryCardAction::Export(targets.to_vec())),
        SelectionBarCommand::CopyAdjustments if targets.len() == 1 => targets
            .first()
            .cloned()
            .map(LibraryCardAction::CopyAdjustments),
        SelectionBarCommand::CopyAdjustments => None,
        SelectionBarCommand::PasteAdjustments => {
            Some(LibraryCardAction::PasteAdjustments(targets.to_vec()))
        }
        SelectionBarCommand::Copy => Some(LibraryCardAction::Copy(clipboard_targets.to_vec())),
        SelectionBarCommand::Cut => Some(LibraryCardAction::Cut(clipboard_targets.to_vec())),
        SelectionBarCommand::Duplicate => Some(LibraryCardAction::Duplicate(targets.to_vec())),
        SelectionBarCommand::Rename if clipboard_targets.len() == 1 => clipboard_targets
            .first()
            .cloned()
            .map(LibraryCardAction::Rename),
        SelectionBarCommand::Rename => None,
        SelectionBarCommand::ResetAdjustments => {
            Some(LibraryCardAction::ResetAdjustments(targets.to_vec()))
        }
        SelectionBarCommand::Delete => Some(LibraryCardAction::Delete(targets.to_vec())),
    }
}

pub(super) fn show_library_selection_action_bar(
    ui: &Ui,
    app: &mut AurawApp,
    selected: &[(LibrarySource, String)],
    library_action: &mut Option<LibraryCardAction>,
    cloud_library_action: &mut Option<CloudLibraryCardAction>,
) {
    if selected.is_empty() {
        return;
    }

    let bounds = ui.max_rect();
    let compact = bounds.width() < 820.0;
    let count = selected.len();
    let mut clear_selection = false;
    egui::Area::new(egui::Id::new("library-selection-action-bar"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(bounds.center().x, bounds.bottom() - 12.0))
        .constrain_to(bounds)
        .movable(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(egui::Margin::symmetric(8, 6))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.x = if compact { 4.0 } else { 6.0 };
                    ui.spacing_mut().interact_size.y = crate::ui::theme::CONTROL_HEIGHT;
                    ui.horizontal(|ui| {
                        let count_label = if compact && bounds.width() < 360.0 {
                            count.to_string()
                        } else {
                            format!("{count} selected")
                        };
                        ui.strong(count_label).on_hover_text(format!(
                            "{count} selected {}",
                            if count == 1 { "RAW" } else { "RAWs" }
                        ));
                        ui.separator();

                        if app.library.is_cloud_view() {
                            let assets = selected
                                .iter()
                                .filter_map(|(source, _)| match source {
                                    LibrarySource::Cloud(asset) => Some(asset.clone()),
                                    #[cfg(not(target_os = "android"))]
                                    LibrarySource::File(_) => None,
                                    #[cfg(target_os = "android")]
                                    LibrarySource::Android { .. } => None,
                                })
                                .collect::<Vec<_>>();
                            let action_enabled = !app.library.cloud_action_in_progress()
                                && !app.library.cloud_upload_in_progress()
                                && !app.library.image_paste_in_progress()
                                && app.library.cloud_open_receiver.is_none()
                                && app.library_batch_export_progress().is_none()
                                && !assets.is_empty();
                            if let Some(action) = selection_bar_actions(
                                ui,
                                assets.len(),
                                action_enabled,
                                app.has_copied_adjustments(),
                                compact,
                            )
                            .and_then(|command| cloud_selection_action(command, &assets))
                            {
                                *cloud_library_action = Some(action);
                            }
                        } else {
                            #[cfg(not(target_os = "android"))]
                            {
                                let paths = selected
                                    .iter()
                                    .filter_map(|(source, _)| match source {
                                        LibrarySource::File(path) => Some(path.clone()),
                                        LibrarySource::Cloud(_) => None,
                                    })
                                    .collect::<Vec<_>>();
                                let action_enabled = !app.library.file_action_in_progress()
                                    && app.library_batch_export_progress().is_none()
                                    && app.library_ai_mask_refresh_status().is_none()
                                    && !paths.is_empty();
                                if let Some(action) = selection_bar_actions(
                                    ui,
                                    paths.len(),
                                    action_enabled,
                                    app.has_copied_adjustments(),
                                    compact,
                                )
                                .and_then(|command| desktop_selection_action(command, &paths))
                                {
                                    *library_action = Some(action);
                                }
                            }
                            #[cfg(target_os = "android")]
                            {
                                let targets = android_selection_targets(selected);
                                let clipboard_targets =
                                    android_selection_clipboard_targets(selected);
                                let action_enabled = app.library_batch_export_progress().is_none()
                                    && app.library_ai_mask_refresh_status().is_none()
                                    && !app.library.image_paste_in_progress()
                                    && !app.library.cloud_action_in_progress()
                                    && !app.library.cloud_upload_in_progress()
                                    && app.library.cloud_open_receiver.is_none()
                                    && !targets.is_empty();
                                if let Some(action) = selection_bar_actions(
                                    ui,
                                    targets.len(),
                                    action_enabled,
                                    app.has_copied_adjustments(),
                                    compact,
                                )
                                .and_then(|command| {
                                    android_selection_action(command, &targets, &clipboard_targets)
                                }) {
                                    *library_action = Some(action);
                                }
                            }
                        }

                        ui.separator();
                        if crate::ui::icons::phosphor_icon_button(
                            ui,
                            egui_phosphor::regular::X,
                            crate::ui::theme::toolbar_icon_size(),
                            "Clear selection",
                        )
                        .clicked()
                        {
                            clear_selection = true;
                        }
                    });
                });
        });

    if clear_selection {
        app.library.clear_selection();
        #[cfg(target_os = "android")]
        crate::android::set_back_navigation_active(false);
    }
}
