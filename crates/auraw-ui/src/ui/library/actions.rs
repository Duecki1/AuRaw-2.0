use super::*;

#[derive(Clone, Debug)]
pub(crate) enum LibraryAction {
    Export(Vec<LibraryAsset>),
    CopyAdjustments(LibraryAsset),
    PasteAdjustments(Vec<LibraryAsset>),
    Copy(Vec<LibraryAsset>),
    Cut(Vec<LibraryAsset>),
    Duplicate(Vec<LibraryAsset>),
    Rename(LibraryAsset),
    ResetAdjustments(Vec<LibraryAsset>),
    Delete(Vec<LibraryAsset>),
}

pub(crate) fn library_image_context_menu(
    ui: &mut Ui,
    app: &AurawApp,
    context_asset: &LibraryAsset,
    context_assets: &[LibraryAsset],
) -> Option<LibraryAction> {
    let selected_count = context_assets.len();
    let action_enabled = !local_action_in_progress(app)
        && app.library_batch_export_progress().is_none()
        && app.library_ai_mask_refresh_status().is_none()
        && !context_assets.is_empty();
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
        action = Some(LibraryAction::Export(context_assets.to_vec()));
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
        action = Some(LibraryAction::CopyAdjustments(context_asset.clone()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled && app.library.has_copied_adjustments(),
            egui::Button::new(if selected_count > 1 {
                "Paste adjustments to selected"
            } else {
                "Paste adjustments"
            }),
        )
        .on_disabled_hover_text("Copy adjustments from an image first")
        .clicked()
    {
        action = Some(LibraryAction::PasteAdjustments(context_assets.to_vec()));
        ui.close();
    }

    ui.separator();
    if ui
        .add_enabled(action_enabled, egui::Button::new("Copy"))
        .clicked()
    {
        action = Some(LibraryAction::Copy(context_assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(action_enabled, egui::Button::new("Cut"))
        .clicked()
    {
        action = Some(LibraryAction::Cut(context_assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(if selected_count > 1 {
                "Duplicate selected (RAW + sidecars)"
            } else {
                "Duplicate (RAW + sidecar)"
            }),
        )
        .clicked()
    {
        action = Some(LibraryAction::Duplicate(context_assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled && selected_count == 1,
            egui::Button::new("Rename…"),
        )
        .clicked()
    {
        action = Some(LibraryAction::Rename(context_asset.clone()));
        ui.close();
    }
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
        action = Some(LibraryAction::ResetAdjustments(context_assets.to_vec()));
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(if selected_count > 1 {
                "Delete selected"
            } else {
                "Delete"
            }),
        )
        .clicked()
    {
        action = Some(LibraryAction::Delete(context_assets.to_vec()));
        ui.close();
    }
    action
}

pub(crate) fn apply_library_action(
    ui: &mut Ui,
    app: &mut AurawApp,
    frame: &eframe::Frame,
    action: LibraryAction,
) {
    match action {
        LibraryAction::Export(assets) => {
            if !assets.is_empty() {
                app.library.export_dialog = Some(LibraryExportDialog {
                    assets,
                    settings: app.export.settings.clone(),
                    format: ExportFormat::Jpeg,
                });
            }
        }
        LibraryAction::CopyAdjustments(asset) => {
            app.library.status = match app.copy_library_adjustments(&asset) {
                Ok(()) => format!("Copied adjustments from {}", asset.display_name),
                Err(error) => format!("Could not copy adjustments: {error}"),
            };
        }
        LibraryAction::PasteAdjustments(assets) => {
            let (edited_count, failures) = app.library_adjustment_edit_count(&assets);
            if !failures.is_empty() {
                app.library.status = format!(
                    "Could not inspect selected adjustments. {}",
                    failures.join(" · ")
                );
            } else if edited_count > 0 {
                app.library.adjustment_paste_dialog = Some(LibraryAdjustmentPasteDialog {
                    assets,
                    edited_count,
                });
            } else {
                apply_library_adjustment_paste(
                    app,
                    assets,
                    crate::sidecar::AdjustmentPasteMode::Merge,
                    ui.ctx(),
                    frame,
                );
            }
        }
        LibraryAction::Copy(assets) => {
            set_library_clipboard(app, ImageClipboardMode::Copy, assets);
        }
        LibraryAction::Cut(assets) => {
            set_library_clipboard(app, ImageClipboardMode::Cut, assets);
        }
        LibraryAction::Duplicate(assets) => {
            app.library.clear_selection();
            #[cfg(target_os = "android")]
            crate::android::set_back_navigation_active(false);
            start_duplicate_assets(app, &assets, ui.ctx());
        }
        LibraryAction::Rename(asset) => {
            app.library.raw_name_dialog = Some(LibraryRawNameDialog {
                name: asset.display_name.clone(),
                asset,
                error: None,
                focus_requested: false,
            });
        }
        LibraryAction::ResetAdjustments(assets) => {
            #[cfg(not(target_os = "android"))]
            let current_to_reopen = app.develop.current_path.as_ref().and_then(|current| {
                assets
                    .iter()
                    .find(|asset| asset.desktop_path() == Some(current.as_path()))
                    .and_then(|asset| asset.desktop_path().map(Path::to_path_buf))
            });
            #[cfg(not(target_os = "android"))]
            if let Some(path) = current_to_reopen.as_deref() {
                app.detach_current_file_for_library_action(path);
            }

            let total = assets.len();
            let mut changed = 0usize;
            let mut failures = Vec::new();
            for asset in &assets {
                match reset_asset_adjustments(app, asset) {
                    Ok(reset) => {
                        changed += usize::from(reset);
                        app.library.invalidate_adjustment_thumbnail_for_asset(asset);
                    }
                    Err(error) => failures.push(format!("{}: {error}", asset.display_name)),
                }
            }
            app.library.clear_selection();
            #[cfg(target_os = "android")]
            crate::android::set_back_navigation_active(false);
            app.library.refresh(ui.ctx());
            app.library.status = if failures.is_empty() {
                format!(
                    "Cleared all adjustments for {total} selected {} ({changed} changed)",
                    if total == 1 { "image" } else { "images" }
                )
            } else {
                format!(
                    "Cleared all adjustments for {} of {total} selected images. {}",
                    total.saturating_sub(failures.len()),
                    failures.join(" · ")
                )
            };
            #[cfg(not(target_os = "android"))]
            if let Some(path) = current_to_reopen {
                app.reload_desktop_library_document_after_reset(path, frame);
            }
        }
        LibraryAction::Delete(assets) => {
            let total = assets.len();
            let mut deleted = 0usize;
            let mut failures = Vec::new();
            for asset in &assets {
                match delete_library_asset(app, asset) {
                    Ok(()) => deleted += 1,
                    Err(error) => failures.push(format!("{}: {error}", asset.display_name)),
                }
            }
            app.library.clear_selection();
            #[cfg(target_os = "android")]
            crate::android::set_back_navigation_active(false);
            app.library.refresh(ui.ctx());
            app.library.status = if failures.is_empty() {
                format!(
                    "Deleted {deleted} selected {}",
                    if deleted == 1 { "image" } else { "images" }
                )
            } else {
                format!(
                    "Deleted {deleted} of {total} selected images. {}",
                    failures.join(" · ")
                )
            };
        }
    }
}

fn set_library_clipboard(app: &mut AurawApp, mode: ImageClipboardMode, assets: Vec<LibraryAsset>) {
    let count = assets.len();
    app.library.image_clipboard = Some(ImageClipboard { mode, assets });
    #[cfg(not(target_os = "android"))]
    {
        app.library.folder_clipboard = None;
    }
    app.library.clear_selection();
    #[cfg(target_os = "android")]
    crate::android::set_back_navigation_active(false);
    app.library.status = format!(
        "{} {count} RAW{}. Choose Paste in a Library folder.",
        if mode == ImageClipboardMode::Copy {
            "Copied"
        } else {
            "Cut"
        },
        if count == 1 { "" } else { "s" }
    );
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

pub(super) fn library_selection_action(
    command: SelectionBarCommand,
    assets: &[LibraryAsset],
) -> Option<LibraryAction> {
    match command {
        SelectionBarCommand::Export => Some(LibraryAction::Export(assets.to_vec())),
        SelectionBarCommand::CopyAdjustments if assets.len() == 1 => {
            assets.first().cloned().map(LibraryAction::CopyAdjustments)
        }
        SelectionBarCommand::CopyAdjustments => None,
        SelectionBarCommand::PasteAdjustments => {
            Some(LibraryAction::PasteAdjustments(assets.to_vec()))
        }
        SelectionBarCommand::Copy => Some(LibraryAction::Copy(assets.to_vec())),
        SelectionBarCommand::Cut => Some(LibraryAction::Cut(assets.to_vec())),
        SelectionBarCommand::Duplicate => Some(LibraryAction::Duplicate(assets.to_vec())),
        SelectionBarCommand::Rename if assets.len() == 1 => {
            assets.first().cloned().map(LibraryAction::Rename)
        }
        SelectionBarCommand::Rename => None,
        SelectionBarCommand::ResetAdjustments => {
            Some(LibraryAction::ResetAdjustments(assets.to_vec()))
        }
        SelectionBarCommand::Delete => Some(LibraryAction::Delete(assets.to_vec())),
    }
}

pub(super) fn show_library_selection_action_bar(
    ui: &Ui,
    app: &mut AurawApp,
    selected: &[LibraryAsset],
    library_action: &mut Option<LibraryAction>,
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
                        let action_enabled = !local_action_in_progress(app)
                            && app.library_batch_export_progress().is_none()
                            && app.library_ai_mask_refresh_status().is_none();
                        if let Some(action) = selection_bar_actions(
                            ui,
                            count,
                            action_enabled,
                            app.library.has_copied_adjustments(),
                            compact,
                        )
                        .and_then(|command| library_selection_action(command, selected))
                        {
                            *library_action = Some(action);
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

pub(crate) fn show_library_action_overlays(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
    show_library_raw_name_dialog(ui, app, frame);

    let paste_choice = app
        .library
        .adjustment_paste_dialog
        .as_ref()
        .and_then(|dialog| {
            show_adjustment_paste_choice(
                ui,
                "library-adjustment-paste-conflict-dialog",
                dialog.edited_count,
                dialog.assets.len(),
            )
        });
    if let Some(choice) = paste_choice {
        if let Some(dialog) = app.library.adjustment_paste_dialog.take() {
            let mode = match choice {
                AdjustmentPasteChoice::Merge => Some(crate::sidecar::AdjustmentPasteMode::Merge),
                AdjustmentPasteChoice::Replace => {
                    Some(crate::sidecar::AdjustmentPasteMode::Replace)
                }
                AdjustmentPasteChoice::Cancel => None,
            };
            if let Some(mode) = mode {
                apply_library_adjustment_paste(app, dialog.assets, mode, ui.ctx(), frame);
            }
        }
    }

    let can_regenerate = app.can_start_library_ai_mask_refresh();
    let refresh_choice = app
        .library
        .ai_mask_refresh_prompt
        .as_ref()
        .and_then(|prompt| {
            show_ai_mask_refresh_choice(
                ui,
                "library-ai-mask-refresh-prompt",
                prompt.assets.len(),
                can_regenerate,
            )
        });
    if let Some(choice) = refresh_choice {
        if let Some(prompt) = app.library.ai_mask_refresh_prompt.take() {
            if choice == AiMaskRefreshChoice::Regenerate {
                start_library_ai_mask_refresh_for_assets(app, prompt.assets, frame);
            }
        }
    }

    if let Some((completed, total, failed, current_name)) = app.library_ai_mask_refresh_status() {
        let (_, cancel) = show_ai_mask_refresh_progress(
            ui,
            completed,
            total,
            failed,
            current_name.as_deref(),
            false,
        );
        if cancel {
            app.cancel_library_ai_mask_refresh();
        }
    }

    let mut close_export_dialog = false;
    let mut confirm_export = false;
    if let Some(dialog) = app.library.export_dialog.as_mut() {
        let count = dialog.assets.len();
        #[cfg(not(target_os = "android"))]
        let export_picker_directory = dialog
            .assets
            .first()
            .and_then(LibraryAsset::desktop_path)
            .and_then(Path::parent)
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf);
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
                    #[cfg(not(target_os = "android"))]
                    export_picker_directory.as_deref(),
                    #[cfg(target_os = "android")]
                    None,
                );
                ui.add_space(10.0);
                #[cfg(not(target_os = "android"))]
                let help = if count > 1 {
                    "A destination folder will be selected for the batch. File names are generated from each RAW name."
                } else {
                    "Choose the output file after pressing Export."
                };
                #[cfg(target_os = "android")]
                let help = "Exports are saved to Pictures/AuRaw. File names are generated from each RAW name.";
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_export_dialog = true;
                    }
                    let label = if count == 1 {
                        #[cfg(not(target_os = "android"))]
                        { "Export 1 image…".to_owned() }
                        #[cfg(target_os = "android")]
                        { "Export 1 image".to_owned() }
                    } else {
                        #[cfg(not(target_os = "android"))]
                        { format!("Export {count} images…") }
                        #[cfg(target_os = "android")]
                        { format!("Export {count} images") }
                    };
                    if ui.button(label).on_hover_text(help).clicked() {
                        confirm_export = true;
                    }
                    crate::ui::theme::help_button(ui, help);
                });
            });
    }

    if confirm_export {
        if let Some(dialog) = app.library.export_dialog.clone() {
            if start_local_library_export(
                app,
                &dialog.assets,
                dialog.settings,
                dialog.format,
                frame,
            ) {
                app.library.clear_selection();
                #[cfg(target_os = "android")]
                crate::android::set_back_navigation_active(false);
                app.library.export_dialog = None;
            }
        }
    } else if close_export_dialog {
        app.library.export_dialog = None;
    }
}
