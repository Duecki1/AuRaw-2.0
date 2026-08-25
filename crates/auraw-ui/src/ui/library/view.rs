use super::*;

pub struct Library;

impl Library {
    pub(crate) fn show_folder_sidebar(ui: &mut Ui, app: &mut AurawApp) {
        let action_in_progress = platform::local_action_in_progress(app);
        let folders_available = platform::local_folders_available(app);
        let can_create_folder = platform::can_create_local_folder(app);
        let mut requested_toolbar_action = None;

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
                        requested_toolbar_action =
                            Some(platform::LocalFolderToolbarAction::Refresh);
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
                        requested_toolbar_action = Some(platform::LocalFolderToolbarAction::New);
                    }
                });
            });
        });

        ui.add_space(crate::ui::theme::CARD_GAP);
        crate::ui::theme::card_frame(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let tree_height = ui.available_height().max(32.0);
            platform::show_local_folder_tree(ui, app, tree_height, action_in_progress);
        });

        if let Some(action) = requested_toolbar_action {
            platform::apply_local_toolbar_action(app, action, ui.ctx());
        }
        platform::show_sidebar_dialogs(ui, app);
    }

    pub fn show(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        app.library.resume_thumbnail_decoding();
        app.library.poll(ui.ctx());

        let mut refresh = false;
        #[cfg(target_os = "android")]
        let mut import_raw = false;
        let mut open_asset: Option<LibraryAsset> = None;
        let mut library_action = None;
        let search_active = app.library.search_active();
        let visible_indices = search_active.then(|| app.library.filtered_entry_indices());

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

            let total_count = app.library.entries.len();
            let visible_count = visible_indices
                .as_ref()
                .map_or(total_count, |indices| indices.len());
            let count_label = if search_active {
                format!("{visible_count} of {total_count} RAW files")
            } else {
                format!(
                    "{total_count} RAW {}",
                    if total_count == 1 { "file" } else { "files" }
                )
            };
            ui.strong(count_label);
            let mut selected_sort = app.library.sort_order();
            let mut selected_size = app.library.thumbnail_size();

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                #[cfg(target_os = "android")]
                app.show_export_task_indicator(ui);

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

        if let Some(_location) = app.library.location.as_deref() {
            #[cfg(not(target_os = "android"))]
            let location_label = _location.to_owned();
            #[cfg(target_os = "android")]
            let location_label = if app.library.platform.folder.is_empty() {
                "Local / Library".to_owned()
            } else {
                format!("Local / {}", app.library.platform.folder)
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(location_label)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .truncate(),
            );
        }
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

        if app.library.location.is_none() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("Choose your top-level photo folder");
                    #[cfg(not(target_os = "android"))]
                    {
                        ui.label("AuRaw builds a folder hierarchy in the desktop sidebar. Select any folder there to show the RAW files directly inside it.");
                        ui.add_space(8.0);
                        if ui.button("Open Top Folder…").clicked() {
                            app.open_library_folder_dialog();
                        }
                    }
                    #[cfg(target_os = "android")]
                    ui.label("Use the folder sidebar to browse the app Library, or tap + to import RAW files.");
                });
            });
        } else if app.library.catalog_ready && app.library.entries.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("No RAW files here yet");
                    #[cfg(not(target_os = "android"))]
                    ui.label("Choose another folder or add RAW files to this folder.");
                    #[cfg(target_os = "android")]
                    ui.label("Tap + to import one or more RAW files.");
                });
            });
        } else {
            if visible_indices
                .as_ref()
                .is_some_and(|indices| indices.is_empty())
            {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading("No filenames match your search");
                        ui.label("Try another name, or clear the search to show every RAW file.");
                        ui.add_space(8.0);
                        if ui.button("Clear Search").clicked() {
                            app.library.clear_search();
                        }
                    });
                });
            } else {
                #[cfg(not(target_os = "android"))]
                let current_path = app.develop.current_path.clone();
                let available = ui.available_width().max(1.0);
                let available_height = ui.available_height().max(1.0);
                let gap = 6.0;
                let target_thumbnail_height = responsive_thumbnail_target_height(
                    available,
                    available_height,
                    ui.ctx().pixels_per_point(),
                    cfg!(target_os = "android"),
                ) * app.library.thumbnail_size().scale();
                let (placements, grid_height) = if let Some(indices) = &visible_indices {
                    justified_thumbnail_layout_for_indices(
                        &app.library.entries,
                        indices,
                        available,
                        target_thumbnail_height,
                        gap,
                    )
                } else {
                    justified_thumbnail_layout(
                        &app.library.entries,
                        available,
                        target_thumbnail_height,
                        gap,
                    )
                };

                let mut protected_thumbnail_indices = HashSet::new();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_viewport(ui, |ui, viewport| {
                        let (content_rect, _) = ui.allocate_exact_size(
                            egui::vec2(available, grid_height.max(1.0)),
                            Sense::hover(),
                        );
                        let preload_viewport = viewport.expand(600.0);

                        for (placement_index, relative_rect) in
                            placements.iter().copied().enumerate()
                        {
                            if !relative_rect.intersects(preload_viewport) {
                                continue;
                            }
                            let index = visible_indices
                                .as_ref()
                                .map_or(placement_index, |indices| indices[placement_index]);
                            protected_thumbnail_indices.insert(index);
                            app.library.touch_and_request_thumbnail(index, ui.ctx());
                            if !relative_rect.intersects(viewport) {
                                continue;
                            }
                            let item_rect = relative_rect.translate(content_rect.min.to_vec2());
                            let entry = &app.library.entries[index];
                            let asset = entry.asset.clone();
                            let selected = if app.library.selection_mode() {
                                app.library.selected_assets.contains(&asset.id)
                            } else {
                                #[cfg(not(target_os = "android"))]
                                {
                                    current_path.as_deref() == asset.desktop_path()
                                }
                                #[cfg(target_os = "android")]
                                {
                                    false
                                }
                            };
                            let response = thumbnail_tile(ui, entry, item_rect, selected);

                            #[cfg(target_os = "android")]
                            {
                                let checkbox =
                                    thumbnail_selection_checkbox(ui, entry, item_rect, selected);
                                let checkbox_clicked = checkbox.clicked()
                                    || (response.clicked()
                                        && response.interact_pointer_pos().is_some_and(
                                            |pointer| checkbox.rect.contains(pointer),
                                        ));
                                if checkbox_clicked {
                                    let back_navigation_active =
                                        app.library.toggle_thumbnail_selection(&asset.id);
                                    crate::android::set_back_navigation_active(
                                        back_navigation_active,
                                    );
                                } else if response.clicked() && !response.secondary_clicked() {
                                    open_asset = Some(asset);
                                }
                            }

                            #[cfg(not(target_os = "android"))]
                            {
                                if response.clicked() && !response.secondary_clicked() {
                                    if app.library.selection_mode() {
                                        app.library.toggle_thumbnail_selection(&asset.id);
                                    } else {
                                        open_asset = Some(asset.clone());
                                    }
                                }
                                if response.secondary_clicked()
                                    && app.library.selection_mode()
                                    && !app.library.selected_assets.contains(&asset.id)
                                {
                                    app.library.selected_assets.insert(asset.id.clone());
                                }
                                let context_assets = if app.library.selection_mode() {
                                    app.library
                                        .entries
                                        .iter()
                                        .filter(|candidate| {
                                            app.library
                                                .selected_assets
                                                .contains(&candidate.asset.id)
                                        })
                                        .map(|candidate| candidate.asset.clone())
                                        .collect::<Vec<_>>()
                                } else {
                                    vec![asset.clone()]
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
                                    if let Some(action) =
                                        library_image_context_menu(ui, app, &asset, &context_assets)
                                    {
                                        library_action = Some(action);
                                    }
                                });
                                if select_from_context_menu {
                                    app.library.begin_selection();
                                    app.library.selected_assets.insert(asset.id.clone());
                                }
                            }
                        }
                    });
                app.library.evict_old_textures(&protected_thumbnail_indices);
            }
        }

        let selected_assets = app
            .library
            .entries
            .iter()
            .filter(|entry| app.library.selected_assets.contains(&entry.asset.id))
            .map(|entry| entry.asset.clone())
            .collect::<Vec<_>>();
        show_library_selection_action_bar(ui, app, &selected_assets, &mut library_action);

        if let Some(action) = library_action {
            apply_library_action(ui, app, frame, action);
        }

        platform::show_page_dialogs(ui, app);
        show_library_action_overlays(ui, app, frame);

        #[cfg(target_os = "android")]
        if !app.library.has_selection() {
            let rect = library_import_fab_rect(ui.max_rect());
            let response = crate::ui::theme::floating_action_button(
                ui,
                rect,
                library_import_icon(),
                "Import RAW files",
            );
            if response.clicked() {
                import_raw = true;
            }
        }

        if refresh {
            app.library.refresh(ui.ctx());
        }
        #[cfg(target_os = "android")]
        if import_raw {
            app.open_file_dialog(frame);
        }
        if let Some(asset) = open_asset {
            #[cfg(not(target_os = "android"))]
            if let Some(path) = asset.desktop_path().map(Path::to_path_buf) {
                app.ui.active_tab = AppTab::Develop;
                app.open_path(path, frame);
            }
            #[cfg(target_os = "android")]
            if let Some(uri) = asset.android_uri() {
                app.library.clear_selection();
                crate::android::set_back_navigation_active(false);
                app.open_android_library_document(uri, &asset.display_name);
            }
        }
    }
}

fn show_local_image_paste_bar(ui: &mut Ui, app: &mut AurawApp) {
    let Some(clipboard) = app.library.image_clipboard.as_ref() else {
        return;
    };
    let label = clipboard.paste_label();
    let in_progress = app.library.asset_transfer_in_progress();
    let mut paste = false;
    let mut clear = false;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("{}  {label}", egui_phosphor::regular::CLIPBOARD)).small(),
        );
        #[cfg(not(target_os = "android"))]
        let destination_available = app.library.folder.is_some();
        #[cfg(target_os = "android")]
        let destination_available = true;
        if ui
            .add_enabled(
                !in_progress && destination_available,
                egui::Button::new("Paste here"),
            )
            .clicked()
        {
            paste = true;
        }
        if ui
            .add_enabled(!in_progress, egui::Button::new("Clear"))
            .clicked()
        {
            clear = true;
        }
    });
    if clear {
        app.library.image_clipboard = None;
    }
    if paste {
        #[cfg(not(target_os = "android"))]
        if let Some(folder) = app.library.folder.clone() {
            start_image_clipboard_paste(
                app,
                LibraryTransferDestination::LocalFolder(folder),
                ui.ctx(),
            );
        }
        #[cfg(target_os = "android")]
        {
            let path = app.library.location.clone().unwrap_or_default();
            start_image_clipboard_paste(
                app,
                LibraryTransferDestination::LocalLibrary { path },
                ui.ctx(),
            );
        }
    }
}
