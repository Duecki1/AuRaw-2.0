use super::*;

pub struct Library;

impl Library {
    pub(crate) fn show_folder_sidebar(ui: &mut Ui, app: &mut AurawApp) {
        let action_in_progress = platform::local_action_in_progress(app);
        let cloud_view = app.library.is_cloud_view();
        let folders_available = cloud_view || platform::local_folders_available(app);
        let can_create_folder = if cloud_view {
            !app.library.cloud_trash_open
        } else {
            platform::can_create_local_folder(app)
        };
        let navigation_enabled = platform::navigation_enabled(action_in_progress);
        let mut requested_local_toolbar_action = None;
        let mut requested_cloud_action = None;
        let mut requested_cloud_trash = false;
        let mut requested_view = None;

        crate::ui::theme::content_card(ui, |ui| {
            crate::ui::theme::toolbar_row(ui, |ui| {
                ui.strong("Folders");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::ui::icons::phosphor_icon_button(
                        ui,
                        egui_phosphor::regular::X,
                        crate::ui::theme::toolbar_icon_size(),
                        "Close folder sidebar",
                    )
                    .clicked()
                    {
                        app.set_library_folder_sidebar_open(false);
                    }
                    if crate::ui::icons::phosphor_icon_button_enabled(
                        ui,
                        folders_available && !action_in_progress,
                        egui_phosphor::regular::ARROW_CLOCKWISE,
                        crate::ui::theme::toolbar_icon_size(),
                        "Refresh folders",
                    )
                    .clicked()
                    {
                        if cloud_view {
                            requested_cloud_action = Some(CloudFolderUiAction::Refresh);
                        } else {
                            requested_local_toolbar_action =
                                Some(platform::LocalFolderToolbarAction::Refresh);
                        }
                    }
                    if crate::ui::icons::phosphor_icon_button_enabled(
                        ui,
                        can_create_folder && !action_in_progress,
                        egui_phosphor::regular::FOLDER_PLUS,
                        crate::ui::theme::toolbar_icon_size(),
                        "Create folder here",
                    )
                    .clicked()
                    {
                        if cloud_view {
                            requested_cloud_action = Some(CloudFolderUiAction::New(
                                app.library.cloud_folder_id.clone(),
                            ));
                        } else {
                            requested_local_toolbar_action =
                                Some(platform::LocalFolderToolbarAction::New);
                        }
                    }
                });
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let width = ((ui.available_width() - 6.0) * 0.5).max(72.0);
                let local_tab = ui
                    .add_enabled_ui(navigation_enabled, |ui| {
                        crate::ui::theme::segmented_button(ui, "Local", !cloud_view, width)
                    })
                    .inner;
                if local_tab.clicked() && cloud_view {
                    requested_view = Some(LibraryView::Local);
                }
                let cloud_tab = ui
                    .add_enabled_ui(app.library.cloud_enabled() && navigation_enabled, |ui| {
                        crate::ui::theme::segmented_button(ui, "Cloud", cloud_view, width)
                    })
                    .inner;
                if cloud_tab.clicked() && !cloud_view {
                    requested_view = Some(LibraryView::Cloud);
                }
                if !app.library.cloud_enabled() {
                    cloud_tab.on_disabled_hover_text("Enable AuRaw Cloud in Settings first.");
                }
            });
        });
        if let Some(view) = requested_view {
            app.show_library_view(view);
        }

        ui.add_space(crate::ui::theme::CARD_GAP);
        crate::ui::theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let tree_height = ui.available_height().max(32.0);
            if cloud_view {
                egui::ScrollArea::both()
                    .max_height(tree_height)
                    .min_scrolled_height(tree_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        show_cloud_folder_node(
                            ui,
                            None,
                            &app.library.cloud_folders,
                            &app.library.cloud_folder_id,
                            app.library.cloud_clipboard.as_ref(),
                            app.library.image_clipboard.as_ref(),
                            action_in_progress,
                            &mut app.library.cloud_expanded_folders,
                            &mut requested_cloud_action,
                        );
                        if crate::ui::theme::navigation_row(
                            ui,
                            format!("{}  Trash", egui_phosphor::regular::TRASH),
                            app.library.cloud_trash_open,
                            Sense::click(),
                        )
                        .clicked()
                        {
                            requested_cloud_trash = true;
                        }
                    });
            } else {
                platform::show_local_folder_tree(ui, app, tree_height, action_in_progress);
            }
        });

        if let Some(action) = requested_local_toolbar_action {
            platform::apply_local_toolbar_action(app, action, ui.ctx());
        }
        if let Some(action) = requested_cloud_action {
            let close_sidebar =
                platform::close_sidebar_after_navigation() && matches!(action, CloudFolderUiAction::Select(_));
            apply_cloud_folder_ui_action(app, action, ui.ctx());
            if close_sidebar {
                app.set_library_folder_sidebar_open(false);
            }
        }
        if requested_cloud_trash {
            app.show_cloud_library_trash();
            if platform::close_sidebar_after_navigation() {
                app.set_library_folder_sidebar_open(false);
            }
        }
        platform::show_sidebar_dialogs(ui, app);
    }

    pub fn show(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        app.library.resume_thumbnail_decoding();
        app.library.poll(ui.ctx());
        app.library.poll_cloud_trash();
        if let Some(completion) = app.library.poll_cloud_action() {
            match completion {
                CloudActionCompletion::Mutation {
                    result,
                    clear_clipboard,
                } => {
                    if clear_clipboard {
                        app.library.cloud_clipboard = None;
                    }
                    app.library.clear_selection();
                    #[cfg(target_os = "android")]
                    crate::android::set_back_navigation_active(false);
                    app.library.cloud_upload_completion =
                        Some(result.unwrap_or_else(|error| error));
                    app.library.refresh(ui.ctx());
                }
                CloudActionCompletion::Prepared { purpose, result } => match result {
                    Err(error) => app.library.status = error,
                    Ok(cached) => {
                        let paths = cached
                            .iter()
                            .map(|asset| asset.raw_path.clone())
                            .collect::<Vec<_>>();
                        match purpose {
                            CloudPreparedPurpose::Export => {
                                if !paths.is_empty() {
                                    #[cfg(not(target_os = "android"))]
                                    {
                                        app.library.export_dialog = Some(LibraryExportDialog {
                                            paths,
                                            settings: app.export_settings.clone(),
                                            format: ExportFormat::Jpeg,
                                        });
                                    }
                                    #[cfg(target_os = "android")]
                                    {
                                        app.library.export_dialog = Some(LibraryExportDialog {
                                            targets: cached
                                                .into_iter()
                                                .map(|asset| {
                                                    crate::app::AndroidLibraryExportTarget::Cloud {
                                                        path: asset.raw_path,
                                                        display_name: asset.label,
                                                    }
                                                })
                                                .collect(),
                                            settings: app.export_settings.clone(),
                                            format: ExportFormat::Jpeg,
                                        });
                                    }
                                }
                            }
                            CloudPreparedPurpose::CopyAdjustments => {
                                if let Some(path) = paths.first() {
                                    let status = match app.copy_library_adjustments_from_path(path)
                                    {
                                        Ok(()) => format!(
                                            "Copied adjustments from {}",
                                            cached
                                                .first()
                                                .map(|asset| asset.label.as_str())
                                                .unwrap_or("cloud RAW")
                                        ),
                                        Err(error) => {
                                            format!("Could not copy adjustments: {error}")
                                        }
                                    };
                                    app.library.status = status;
                                }
                            }
                            CloudPreparedPurpose::PasteAdjustments => {
                                #[cfg(not(target_os = "android"))]
                                apply_desktop_image_action(
                                    ui,
                                    app,
                                    frame,
                                    LibraryCardAction::PasteAdjustments(paths),
                                );
                                #[cfg(target_os = "android")]
                                prepare_android_cloud_adjustment_paste(ui, app, paths, frame);
                            }
                        }
                    }
                },
            }
        }
        if let Some(result) = app.library.poll_cloud_open() {
            match result {
                Ok(cached) => {
                    app.open_cloud_cached_asset(cached, frame);
                    return;
                }
                Err(error) => app.library.set_status(error),
            }
        }

        let mut refresh = false;
        let mut import_raw = false;
        let mut open_source = None;
        let mut library_action = None;
        let mut cloud_library_action = None;

        let compact_header = ui.available_width() < 520.0;
        crate::ui::theme::toolbar_row(ui, |ui| {
            if compact_header {
                ui.spacing_mut().item_spacing.x = 4.0;
            }
            if !app.library.folder_sidebar_open()
                && crate::ui::icons::phosphor_icon_button(
                    ui,
                    egui_phosphor::regular::SIDEBAR_SIMPLE,
                    crate::ui::theme::toolbar_icon_size(),
                    "Open folder sidebar",
                )
                .clicked()
            {
                app.set_library_folder_sidebar_open(true);
            }

            if app.library.cloud_trash_open {
                let count = app.library.cloud_trash_items.len();
                ui.strong(format!(
                    "{count} Trash item{}",
                    if count == 1 { "" } else { "s" }
                ));
            } else {
                let count = app.library.entries.len();
                ui.strong(format!(
                    "{count} RAW {}",
                    if count == 1 { "file" } else { "files" }
                ));
            }
            let mut selected_sort = app.library.sort_order();
            let mut selected_size = app.library.thumbnail_size();

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                #[cfg(target_os = "android")]
                if (if compact_header {
                    crate::ui::icons::phosphor_icon_button(
                        ui,
                        egui_phosphor::regular::GEAR,
                        crate::ui::theme::toolbar_icon_size(),
                        "Settings",
                    )
                } else {
                    crate::ui::theme::toolbar_button(ui, "Settings", 82.0)
                })
                .clicked()
                {
                    app.activate_tab(AppTab::Settings);
                }

                if crate::ui::icons::phosphor_icon_button_enabled(
                    ui,
                    app.library.location.is_some() && !app.library.scanning,
                    egui_phosphor::regular::ARROW_CLOCKWISE,
                    crate::ui::theme::toolbar_icon_size(),
                    "Refresh library",
                )
                .clicked()
                {
                    refresh = true;
                }

                if compact_header {
                    ui.menu_button(
                        egui::RichText::new(egui_phosphor::regular::SLIDERS_HORIZONTAL).size(17.0),
                        |ui| {
                            ui.set_min_width(220.0);
                            ui.strong("Thumbnail size");
                            for thumbnail_size in LibraryThumbnailSize::ALL {
                                ui.selectable_value(
                                    &mut selected_size,
                                    thumbnail_size,
                                    thumbnail_size.label(),
                                );
                            }
                            ui.separator();
                            ui.strong("Sort order");
                            for sort_order in LibrarySortOrder::ALL {
                                ui.selectable_value(
                                    &mut selected_sort,
                                    sort_order,
                                    sort_order.label(),
                                );
                            }
                        },
                    )
                    .response
                    .on_hover_text("Library view options");
                } else {
                    egui::ComboBox::from_id_salt("library-sort-order")
                        .selected_text(format!("Sort: {}", selected_sort.label()))
                        .width(154.0)
                        .show_ui(ui, |ui| {
                            for sort_order in LibrarySortOrder::ALL {
                                ui.selectable_value(
                                    &mut selected_sort,
                                    sort_order,
                                    sort_order.label(),
                                );
                            }
                        });

                    egui::ComboBox::from_id_salt("library-thumbnail-size")
                        .selected_text(format!("Size: {}", selected_size.label()))
                        .width(118.0)
                        .show_ui(ui, |ui| {
                            for thumbnail_size in LibraryThumbnailSize::ALL {
                                ui.selectable_value(
                                    &mut selected_size,
                                    thumbnail_size,
                                    thumbnail_size.label(),
                                );
                            }
                        });
                }
            });
            app.set_library_sort_order(selected_sort);
            app.set_library_thumbnail_size(selected_size);
        });

        #[cfg(not(target_os = "android"))]
        if !app.library.is_cloud_view() {
            if let Some(location) = app.library.location() {
                ui.label(
                    egui::RichText::new(location)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }
        }
        #[cfg(target_os = "android")]
        if !app.library.is_cloud_view() {
            let folder_label = if app.library.android_folder.is_empty() {
                "Local / Library".to_owned()
            } else {
                format!("Local / {}", app.library.android_folder)
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(folder_label)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .truncate(),
            );
        }
        #[cfg(not(target_os = "android"))]
        show_cloud_folder_bar(ui, app);
        show_local_image_paste_bar(ui, app);
        if !app.library.status.is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&app.library.status)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .wrap(),
            );
        }
        ui.separator();

        if app.library.cloud_trash_open {
            show_cloud_trash_panel(ui, app);
            return;
        }

        if app.library.location.is_none() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("Choose your top-level photo folder");
                    ui.label("AuRaw builds a folder hierarchy in the desktop sidebar. Select any folder there to show the RAW files directly inside the selected folder.");
                    ui.add_space(8.0);
                    #[cfg(not(target_os = "android"))]
                    if ui.button("Open Top Folder…").clicked() {
                        app.open_library_folder_dialog();
                    }
                });
            });
        } else if app.library.catalog_ready && app.library.entries.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading(if app.library.is_cloud_view() {
                        "No cloud RAW files yet"
                    } else {
                        "No RAW files here yet"
                    });
                    #[cfg(not(target_os = "android"))]
                    if app.library.is_cloud_view() {
                        ui.label("Click + to upload one or more RAW files.");
                    } else {
                        ui.label("Choose another folder or add RAW files to this folder.");
                    }
                    #[cfg(target_os = "android")]
                    if app.library.is_cloud_view() {
                        ui.label("Tap + to upload one or more RAW files.");
                    } else {
                        ui.label("Tap + to import one or more RAW files.");
                    }
                });
            });
        } else {
            #[cfg(not(target_os = "android"))]
            let current_path = app.current_path.clone();
            let available = ui.available_width().max(1.0);
            let available_height = ui.available_height().max(1.0);
            let gap = 6.0;
            let target_thumbnail_height = responsive_thumbnail_target_height(
                available,
                available_height,
                ui.ctx().pixels_per_point(),
                cfg!(target_os = "android"),
            ) * app.library.thumbnail_size().scale();
            let (placements, grid_height) = justified_thumbnail_layout(
                &app.library.entries,
                available,
                target_thumbnail_height,
                gap,
            );

            let mut protected_thumbnail_indices = HashSet::new();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_viewport(ui, |ui, viewport| {
                    let (content_rect, _) = ui.allocate_exact_size(
                        egui::vec2(available, grid_height.max(1.0)),
                        Sense::hover(),
                    );
                    let preload_viewport = viewport.expand(600.0);

                    for (index, relative_rect) in placements.iter().copied().enumerate() {
                        if !relative_rect.intersects(preload_viewport) {
                            continue;
                        }

                        // Protect the complete preload window from cache eviction, not
                        // only the currently painted rows. This keeps resize-driven layout
                        // changes from immediately discarding thumbnails we are about to use.
                        protected_thumbnail_indices.insert(index);
                        app.library.touch_and_request_thumbnail(index, ui.ctx());
                        if !relative_rect.intersects(viewport) {
                            continue;
                        }
                        let item_rect = relative_rect.translate(content_rect.min.to_vec2());

                        let entry = &app.library.entries[index];
                        let source = entry.info.source.clone();
                        let name = entry.info.name.clone();
                        let selected = if app.library.selection_mode() {
                            app.library.selected_sources.contains(&source)
                        } else {
                            match &source {
                                #[cfg(not(target_os = "android"))]
                                LibrarySource::File(path) => current_path.as_deref() == Some(path),
                                #[cfg(target_os = "android")]
                                LibrarySource::Android { .. } => false,
                                LibrarySource::Cloud(_) => false,
                            }
                        };
                        let response = thumbnail_tile(ui, entry, item_rect, selected);

                        #[cfg(target_os = "android")]
                        {
                            let checkbox =
                                thumbnail_selection_checkbox(ui, entry, item_rect, selected);
                            // The checkbox and thumbnail deliberately overlap. Some input
                            // backends award the primary click to the larger thumbnail
                            // response, so also route a thumbnail click by pointer position.
                            let checkbox_clicked = checkbox.clicked()
                                || (response.clicked()
                                    && response
                                        .interact_pointer_pos()
                                        .is_some_and(|pointer| checkbox.rect.contains(pointer)));
                            if checkbox_clicked {
                                let back_navigation_active =
                                    app.library.toggle_thumbnail_selection(&source);
                                crate::android::set_back_navigation_active(back_navigation_active);
                            } else if response.clicked() && !response.secondary_clicked() {
                                open_source = Some((source.clone(), name.clone()));
                            }
                        }

                        #[cfg(not(target_os = "android"))]
                        {
                            let path = match &source {
                                LibrarySource::File(path) => Some(path.clone()),
                                LibrarySource::Cloud(_) => None,
                            };

                            if response.clicked() && !response.secondary_clicked() {
                                if app.library.selection_mode() {
                                    app.library.toggle_thumbnail_selection(&source);
                                } else {
                                    open_source = Some((source.clone(), name.clone()));
                                }
                            }

                            // In desktop selection mode, right-click keeps the familiar
                            // context menu but targets the complete selection. Right-clicking
                            // an unselected thumbnail first adds it to that selection.
                            if response.secondary_clicked()
                                && app.library.selection_mode()
                                && !app.library.selected_sources.contains(&source)
                            {
                                app.library.selected_sources.insert(source.clone());
                            }

                            let context_paths = if app.library.selection_mode() {
                                app.library
                                    .entries
                                    .iter()
                                    .filter(|candidate| {
                                        app.library
                                            .selected_sources
                                            .contains(&candidate.info.source)
                                    })
                                    .filter_map(|candidate| match &candidate.info.source {
                                        LibrarySource::File(path) => Some(path.clone()),
                                        LibrarySource::Cloud(_) => None,
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                path.clone().into_iter().collect()
                            };
                            let context_assets = if app.library.selection_mode() {
                                app.library
                                    .entries
                                    .iter()
                                    .filter(|candidate| {
                                        app.library
                                            .selected_sources
                                            .contains(&candidate.info.source)
                                    })
                                    .filter_map(|candidate| match &candidate.info.source {
                                        LibrarySource::Cloud(asset) => Some(asset.clone()),
                                        LibrarySource::File(_) => None,
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                match &source {
                                    LibrarySource::Cloud(asset) => vec![asset.clone()],
                                    LibrarySource::File(_) => Vec::new(),
                                }
                            };
                            let mut select_from_context_menu = false;
                            response.context_menu(|ui| {
                                if !app.library.selection_mode() {
                                    if ui.button("Select").clicked() {
                                        select_from_context_menu = true;
                                        ui.close();
                                    }
                                    ui.separator();
                                }

                                match &source {
                                    LibrarySource::File(context_source_path) => {
                                        if let Some(action) = desktop_image_context_menu(
                                            ui,
                                            app,
                                            context_source_path,
                                            &context_paths,
                                        ) {
                                            library_action = Some(action);
                                        }
                                    }
                                    LibrarySource::Cloud(_) => {
                                        if let Some(action) =
                                            cloud_image_context_menu(ui, app, &context_assets)
                                        {
                                            cloud_library_action = Some(action);
                                        }
                                    }
                                }
                            });
                            if select_from_context_menu {
                                app.library.begin_selection();
                                app.library.selected_sources.insert(source.clone());
                            }
                        }
                    }
                });
            app.library.evict_old_textures(&protected_thumbnail_indices);
        }

        let selected_items = app
            .library
            .entries
            .iter()
            .filter(|entry| app.library.selected_sources.contains(&entry.info.source))
            .map(|entry| (entry.info.source.clone(), entry.info.name.clone()))
            .collect::<Vec<_>>();
        show_library_selection_action_bar(
            ui,
            app,
            &selected_items,
            &mut library_action,
            &mut cloud_library_action,
        );

        if let Some(action) = cloud_library_action {
            apply_cloud_image_action(app, action, ui.ctx());
        }

        #[cfg(not(target_os = "android"))]
        if let Some(action) = library_action {
            apply_desktop_image_action(ui, app, frame, action);
        }

        show_cloud_dialogs(ui, app);
        #[cfg(not(target_os = "android"))]
        show_local_raw_name_dialog(ui, app, frame);
        #[cfg(target_os = "android")]
        show_android_local_raw_name_dialog(ui, app);
        platform::show_page_dialogs(ui, app);

        #[cfg(target_os = "android")]
        if let Some(action) = library_action {
            match action {
                LibraryCardAction::Export(targets) => {
                    if !targets.is_empty() {
                        app.library.export_dialog = Some(LibraryExportDialog {
                            targets: targets
                                .into_iter()
                                .map(|(uri, display_name)| {
                                    crate::app::AndroidLibraryExportTarget::Local {
                                        uri,
                                        display_name,
                                    }
                                })
                                .collect(),
                            settings: app.export_settings.clone(),
                            format: ExportFormat::Jpeg,
                        });
                    }
                }
                LibraryCardAction::CopyAdjustments((uri, display_name)) => {
                    let status =
                        match app.copy_library_adjustments_from_android(&uri, &display_name) {
                            Ok(()) => format!("Copied adjustments from {display_name}"),
                            Err(error) => format!("Could not copy adjustments: {error}"),
                        };
                    app.library.status = status;
                }
                LibraryCardAction::PasteAdjustments(targets) => {
                    let (edited_count, failures) =
                        app.library_adjustment_edit_count_android(&targets);
                    if failures.is_empty() {
                        if edited_count > 0 {
                            app.library.adjustment_paste_dialog =
                                Some(LibraryAdjustmentPasteDialog {
                                    targets: AndroidAdjustmentPasteTargets::Local(targets),
                                    edited_count,
                                });
                        } else {
                            apply_library_adjustment_paste(
                                app,
                                targets,
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
                LibraryCardAction::Copy(items) => {
                    let count = items.len();
                    app.library.image_clipboard = Some(ImageClipboard {
                        mode: ImageClipboardMode::Copy,
                        content: ImageClipboardContent::Local(items),
                    });
                    app.library.cloud_clipboard = None;
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.status = format!(
                        "Copied {count} local RAW{}. Paste the selection in Local or any cloud folder.",
                        if count == 1 { "" } else { "s" }
                    );
                }
                LibraryCardAction::Cut(items) => {
                    let count = items.len();
                    app.library.image_clipboard = Some(ImageClipboard {
                        mode: ImageClipboardMode::Cut,
                        content: ImageClipboardContent::Local(items),
                    });
                    app.library.cloud_clipboard = None;
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.status = format!(
                        "Cut {count} local RAW{}. Paste the selection in Local or any cloud folder.",
                        if count == 1 { "" } else { "s" }
                    );
                }
                LibraryCardAction::Duplicate(targets) => {
                    let total = targets.len();
                    let mut failures = Vec::new();
                    for (uri, display_name) in targets {
                        if let Err(error) = app.duplicate_android_library_item(&uri, &display_name)
                        {
                            failures.push(format!("{display_name}: {error}"));
                        }
                    }
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.refresh(ui.ctx());
                    app.library.status = if failures.is_empty() {
                        format!(
                            "Duplicated {total} selected {}",
                            if total == 1 { "image" } else { "images" }
                        )
                    } else {
                        format!(
                            "Duplicated {} of {total} selected images. {}",
                            total.saturating_sub(failures.len()),
                            failures.join(" · ")
                        )
                    };
                }
                LibraryCardAction::Rename(source) => {
                    app.library.android_raw_name_dialog = Some(AndroidLibraryRawNameDialog {
                        name: source.display_name.clone(),
                        source,
                        error: None,
                        focus_requested: false,
                    });
                }
                LibraryCardAction::ResetAdjustments(targets) => {
                    let total = targets.len();
                    let mut failures = Vec::new();
                    for (uri, display_name) in targets {
                        match app.reset_android_library_adjustments(&uri, &display_name) {
                            Ok(()) => app.library.invalidate_android_adjustment_thumbnail(&uri),
                            Err(error) => failures.push(format!("{display_name}: {error}")),
                        }
                    }
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.refresh(ui.ctx());
                    app.library.status = if failures.is_empty() {
                        format!(
                            "Cleared all adjustments for {total} selected {}",
                            if total == 1 { "image" } else { "images" }
                        )
                    } else {
                        format!(
                            "Cleared all adjustments for {} of {total} selected images. {}",
                            total.saturating_sub(failures.len()),
                            failures.join(" · ")
                        )
                    };
                }
                LibraryCardAction::Delete(targets) => {
                    let total = targets.len();
                    let mut failures = Vec::new();
                    for (uri, display_name) in targets {
                        if let Err(error) = app.delete_android_library_item(&uri, &display_name) {
                            failures.push(format!("{display_name}: {error}"));
                        }
                    }
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.refresh(ui.ctx());
                    app.library.status = if failures.is_empty() {
                        format!(
                            "Deleted {total} selected {}",
                            if total == 1 { "image" } else { "images" }
                        )
                    } else {
                        format!(
                            "Completed {} of {total} selected actions. {}",
                            total.saturating_sub(failures.len()),
                            failures.join(" · ")
                        )
                    };
                }
            }
        }

        #[cfg(not(target_os = "android"))]
        show_desktop_image_action_overlays(ui, app, frame);

        #[cfg(target_os = "android")]
        {
            let paste_choice = app.library.adjustment_paste_dialog.as_ref().and_then(|dialog| {
                show_adjustment_paste_choice(
                    ui,
                    "android-library-adjustment-paste-conflict-dialog",
                    dialog.edited_count,
                    dialog.targets.len(),
                )
            });
            if let Some(choice) = paste_choice {
                if let Some(dialog) = app.library.adjustment_paste_dialog.take() {
                    let mode = match choice {
                        AdjustmentPasteChoice::Merge => {
                            Some(crate::sidecar::AdjustmentPasteMode::Merge)
                        }
                        AdjustmentPasteChoice::Replace => {
                            Some(crate::sidecar::AdjustmentPasteMode::Replace)
                        }
                        AdjustmentPasteChoice::Cancel => None,
                    };
                    match (mode, dialog.targets) {
                        (Some(mode), AndroidAdjustmentPasteTargets::Local(targets)) => {
                            apply_library_adjustment_paste(app, targets, mode, ui.ctx(), frame);
                        }
                        (Some(mode), AndroidAdjustmentPasteTargets::Cloud(paths)) => {
                            apply_android_cloud_adjustment_paste(app, paths, mode, ui.ctx(), frame);
                        }
                        (None, _) => {}
                    }
                }
            }

            let can_regenerate = app.can_start_library_ai_mask_refresh();
            let refresh_choice = app.library.ai_mask_refresh_prompt.as_ref().and_then(|prompt| {
                show_ai_mask_refresh_choice(
                    ui,
                    "android-library-ai-mask-refresh-prompt",
                    prompt.targets.len(),
                    can_regenerate,
                )
            });
            if let Some(choice) = refresh_choice {
                if let Some(prompt) = app.library.ai_mask_refresh_prompt.take() {
                    if choice == AiMaskRefreshChoice::Regenerate {
                        app.start_library_ai_mask_refresh_android(prompt.targets, frame);
                    }
                }
            }
        }

        #[cfg(target_os = "android")]
        if let Some((completed, total, failed, current_name)) = app.library_ai_mask_refresh_status()
        {
            if app.library_ai_mask_refresh_progress_open() {
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
        }

        #[cfg(target_os = "android")]
        {
            let mut close_export_dialog = false;
            let mut confirm_export = false;
            if let Some(dialog) = app.library.export_dialog.as_mut() {
                let count = dialog.targets.len();
                let title = if count == 1 {
                    "Export image".to_owned()
                } else {
                    format!("Export {count} images")
                };
                crate::ui::responsive_popup(egui::Window::new(title), ui.ctx(), 480.0)
                    .id(egui::Id::new("android-library-export-dialog"))
                    .collapsible(false)
                    .resizable(true)
                    .show(ui.ctx(), |ui| {
                        show_library_export_settings_controls(
                            ui,
                            &mut dialog.format,
                            &mut dialog.settings,
                            None,
                        );
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(
                                "Exports are saved to Pictures/AuRaw. File names are generated from each RAW name.",
                            )
                            .small()
                            .color(ui.visuals().weak_text_color()),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close_export_dialog = true;
                            }
                            let label = if count == 1 {
                                "Export 1 image".to_owned()
                            } else {
                                format!("Export {count} images")
                            };
                            if ui.button(label).clicked() {
                                confirm_export = true;
                            }
                        });
                    });
            }

            if confirm_export {
                if let Some(dialog) = app.library.export_dialog.clone() {
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.export_dialog = None;
                    app.start_android_library_exports(
                        dialog.targets,
                        dialog.settings.clone(),
                        dialog.format,
                    );
                }
            } else if close_export_dialog {
                app.library.export_dialog = None;
            }

            show_library_batch_export_progress(ui, app);
        }

        #[cfg(target_os = "android")]
        let show_import_fab = !app.library.has_selection();
        #[cfg(not(target_os = "android"))]
        let show_import_fab = app.library.is_cloud_view() && !app.library.selection_mode();
        if show_import_fab && !app.library.cloud_upload_in_progress() {
            let cloud_upload = app.library.is_cloud_view();
            let rect = library_import_fab_rect(ui.max_rect());
            let response = crate::ui::theme::floating_action_button(
                ui,
                rect,
                library_import_icon(),
                if cloud_upload {
                    "Upload RAW files to AuRaw Cloud"
                } else {
                    "Import RAW files"
                },
            );
            if response.clicked() {
                import_raw = true;
            }
        }

        if refresh {
            app.library.refresh(ui.ctx());
        }
        if import_raw {
            if app.library.is_cloud_view() {
                app.open_cloud_upload_dialog(frame);
            } else {
                #[cfg(target_os = "android")]
                app.open_file_dialog(frame);
            }
        }
        if let Some((source, display_name)) = open_source {
            match source {
                #[cfg(not(target_os = "android"))]
                LibrarySource::File(path) => {
                    let _ = display_name;
                    app.active_tab = AppTab::Develop;
                    app.open_path(path, frame);
                }
                #[cfg(target_os = "android")]
                LibrarySource::Android { uri, .. } => {
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.open_android_library_document(&uri, &display_name);
                }
                LibrarySource::Cloud(asset) => {
                    app.library.start_cloud_open(asset, ui.ctx());
                }
            }
        }
    }
}

