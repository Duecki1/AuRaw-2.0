use super::*;

pub(crate) struct Library;

impl Library {
    pub(crate) fn show_folder_sidebar(ui: &mut Ui, app: &mut CalibRawApp) {
        let action_in_progress = platform::local_action_in_progress(app);
        let folders_available = platform::local_folders_available(app);
        let can_create_folder = platform::can_create_local_folder(app);
        let mut requested_toolbar_action = None;

        crate::ui::theme::card_header(ui, |ui| {
            crate::ui::theme::toolbar_row(ui, |ui| {
                crate::ui::theme::toolbar_title(ui, "Folders");
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
        crate::ui::theme::card_gap(ui);
        ui.scope(|ui| {
            let mut scroll_style = egui::style::ScrollStyle::solid();
            scroll_style.bar_width = 7.0;
            scroll_style.bar_inner_margin = 7.0;
            ui.spacing_mut().scroll = scroll_style;

            egui::ScrollArea::vertical()
                .id_salt("library-folder-sidebar-content")
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let content_width = ui.available_width().max(1.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(content_width, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(content_width);
                            ui.set_max_width(content_width);
                            crate::ui::theme::content_card(ui, |ui| {
                                platform::show_local_folder_tree(ui, app, action_in_progress);
                            });
                            ui.add_space(10.0);
                        },
                    );
                });
        });

        if let Some(action) = requested_toolbar_action {
            platform::apply_local_toolbar_action(app, action, ui.ctx());
        }
        platform::show_sidebar_dialogs(ui, app);
    }

    pub(crate) fn show(ui: &mut Ui, app: &mut CalibRawApp, frame: &eframe::Frame) {
        app.library.resume_thumbnail_decoding();
        app.library.poll(ui.ctx());

        let mut refresh = false;
        #[cfg(target_os = "android")]
        let mut import_raw = false;
        let mut open_asset: Option<LibraryAsset> = None;
        let mut library_action = None;
        let search_active = app.library.search_active();
        let visible_indices = search_active.then(|| app.library.filtered_entry_indices());
        let visible_count = visible_indices
            .as_ref()
            .map_or(app.library.entries.len(), Vec::len);

        let compact_header = ui.available_width() < 520.0;
        let mut selected_sort = app.library.sort_order();
        let mut selected_size = app.library.thumbnail_size();
        let header_title = library_header_title(app);
        let header_summary = library_header_summary(app, visible_count, search_active);
        crate::ui::theme::card_header(ui, |ui| {
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

                if compact_header {
                    crate::ui::theme::toolbar_title(ui, "Library");
                } else {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        crate::ui::theme::toolbar_title(ui, &header_title);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&header_summary)
                                    .small()
                                    .color(ui.visuals().weak_text_color()),
                            )
                            .truncate(),
                        );
                    });
                }
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
                        crate::ui::theme::responsive_combo_box(
                            ui,
                            "library-view-options",
                            egui::RichText::new(egui_phosphor::regular::SLIDERS_HORIZONTAL)
                                .size(17.0),
                            64.0,
                            LibraryThumbnailSize::ALL.len() + LibrarySortOrder::ALL.len() + 2,
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
                        show_library_view_combos(ui, &mut selected_sort, &mut selected_size);
                    }
                });
            });
        });
        app.set_library_sort_order(selected_sort);
        app.set_library_thumbnail_size(selected_size);
        crate::ui::theme::card_gap(ui);

        if show_local_image_paste_bar(ui, app) {
            crate::ui::theme::card_gap(ui);
        }
        let show_status = !app.library.status.is_empty()
            && (app.library.location.is_some() || !app.library.status.starts_with("Open a folder"));
        if show_status {
            show_library_status(ui, &app.library.status);
            crate::ui::theme::card_gap(ui);
        }

        if app.library.location.is_none() {
            #[cfg(not(target_os = "android"))]
            if show_library_empty_state(
                ui,
                "Build your photo library",
                "Choose a top-level photo folder. CalibRaw keeps your hierarchy intact and shows the RAW files in each folder.",
                Some("Open Folder…"),
            ) {
                app.open_library_folder_dialog();
            }
            #[cfg(target_os = "android")]
            show_library_empty_state(
                ui,
                "Your library is ready",
                "Use the folder sidebar to browse your Library, or tap + to import RAW files.",
                None,
            );
        } else if app.library.catalog_ready && app.library.entries.is_empty() {
            #[cfg(not(target_os = "android"))]
            show_library_empty_state(
                ui,
                "No RAW photos here yet",
                "Choose another folder in the sidebar or add RAW files to this folder.",
                None,
            );
            #[cfg(target_os = "android")]
            show_library_empty_state(
                ui,
                "No RAW photos here yet",
                "Tap + to import one or more RAW files.",
                None,
            );
        } else if visible_indices
            .as_ref()
            .is_some_and(|indices| indices.is_empty())
        {
            if show_library_empty_state(
                ui,
                "No matching photos",
                "Try another filename, or clear the search to show every RAW photo.",
                Some("Clear search"),
            ) {
                app.library.clear_search();
            }
        } else {
            #[cfg(not(target_os = "android"))]
            let current_path = app.develop.current_path.clone();
            let available = ui.available_width().max(1.0);
            let available_height = ui.available_height().max(1.0);
            let gap = crate::ui::theme::SPACE_SM;
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

                    for (placement_index, relative_rect) in placements.iter().copied().enumerate() {
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
                                    && response
                                        .interact_pointer_pos()
                                        .is_some_and(|pointer| checkbox.rect.contains(pointer)));
                            if checkbox_clicked {
                                let back_navigation_active =
                                    app.library.toggle_thumbnail_selection(&asset.id);
                                crate::android::set_back_navigation_active(back_navigation_active);
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
                                        app.library.selected_assets.contains(&candidate.asset.id)
                                    })
                                    .map(|candidate| candidate.asset.clone())
                                    .collect::<Vec<_>>()
                            } else {
                                vec![asset.clone()]
                            };
                            let mut select_from_context_menu = false;
                            crate::ui::theme::context_menu(&response, |ui| {
                                if !app.library.selection_mode() {
                                    if crate::ui::theme::context_menu_item(ui, true, "Select")
                                        .clicked()
                                    {
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

fn library_header_title(app: &CalibRawApp) -> String {
    selected_library_folder_name(app).unwrap_or_else(|| "Library".to_owned())
}

fn library_header_summary(app: &CalibRawApp, visible_count: usize, search_active: bool) -> String {
    if app.library.scanning {
        return "Scanning photos…".to_owned();
    }

    let noun_count = if search_active {
        app.library.entries.len()
    } else {
        visible_count
    };
    let photo_label = if noun_count == 1 { "photo" } else { "photos" };
    let count = if search_active {
        format!(
            "{visible_count} of {} {photo_label}",
            app.library.entries.len()
        )
    } else {
        format!("{visible_count} {photo_label}")
    };
    count
}

#[cfg(not(target_os = "android"))]
fn selected_library_folder_name(app: &CalibRawApp) -> Option<String> {
    app.library.folder.as_deref().map(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| path.display().to_string())
    })
}

#[cfg(target_os = "android")]
fn selected_library_folder_name(app: &CalibRawApp) -> Option<String> {
    let folder = app.library.platform.folder.as_str();
    Some(if folder.is_empty() {
        "All photos".to_owned()
    } else {
        folder.rsplit('/').next().unwrap_or(folder).to_owned()
    })
}

fn show_library_status(ui: &mut Ui, status: &str) {
    crate::ui::theme::content_card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(egui_phosphor::regular::INFO)
                    .size(17.0)
                    .color(ui.visuals().selection.bg_fill),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(status)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .wrap(),
            );
        });
    });
}

fn show_library_empty_state(
    ui: &mut Ui,
    title: &str,
    description: &str,
    action: Option<&str>,
) -> bool {
    let mut action_clicked = false;
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            let text_width = ui.available_width().clamp(220.0, 420.0);
            ui.label(
                egui::RichText::new(title)
                    .strong()
                    .size(crate::ui::theme::PANEL_TITLE_TEXT_SIZE),
            );
            ui.add_space(crate::ui::theme::SPACE_XS);
            ui.add_sized(
                [text_width, 0.0],
                egui::Label::new(
                    egui::RichText::new(description).color(ui.visuals().weak_text_color()),
                )
                .wrap()
                .halign(egui::Align::Center),
            );
            if let Some(action) = action {
                ui.add_space(crate::ui::theme::SPACE_MD);
                action_clicked = crate::ui::theme::primary_button(ui, action, 132.0).clicked();
            }
        });
    });
    action_clicked
}

fn show_library_view_combos(
    ui: &mut Ui,
    selected_sort: &mut LibrarySortOrder,
    selected_size: &mut LibraryThumbnailSize,
) {
    const SORT_WIDTH: f32 = 154.0;
    const SIZE_WIDTH: f32 = 118.0;
    let available_width = ui.available_width().max(1.0);
    let minimum_width = SORT_WIDTH + crate::ui::theme::SPACE_SM + SIZE_WIDTH;
    let (sort_width, size_width) = if available_width < minimum_width {
        let width = ((available_width - crate::ui::theme::SPACE_SM).max(2.0)) * 0.5;
        (width, width)
    } else {
        (SORT_WIDTH, SIZE_WIDTH)
    };

    crate::ui::theme::responsive_combo_box(
        ui,
        "library-sort-order",
        format!("Sort: {}", selected_sort.label()),
        sort_width,
        LibrarySortOrder::ALL.len(),
        |ui| {
            for sort_order in LibrarySortOrder::ALL {
                ui.selectable_value(selected_sort, sort_order, sort_order.label());
            }
        },
    );
    crate::ui::theme::responsive_combo_box(
        ui,
        "library-thumbnail-size",
        format!("Size: {}", selected_size.label()),
        size_width,
        LibraryThumbnailSize::ALL.len(),
        |ui| {
            for thumbnail_size in LibraryThumbnailSize::ALL {
                ui.selectable_value(selected_size, thumbnail_size, thumbnail_size.label());
            }
        },
    );
}

fn show_local_image_paste_bar(ui: &mut Ui, app: &mut CalibRawApp) -> bool {
    let Some(clipboard) = app.library.image_clipboard.as_ref() else {
        return false;
    };
    let label = clipboard.paste_label();
    let in_progress = app.library.asset_transfer_in_progress();
    let mut paste = false;
    let mut clear = false;
    #[cfg(not(target_os = "android"))]
    let destination_available = app.library.folder.is_some();
    #[cfg(target_os = "android")]
    let destination_available = true;
    crate::ui::theme::content_card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(egui_phosphor::regular::CLIPBOARD)
                    .size(17.0)
                    .color(ui.visuals().selection.bg_fill),
            );
            ui.label(egui::RichText::new(&label).small());
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
    true
}
