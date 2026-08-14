use super::*;

impl LibraryState {
    /// Refreshes one cached cloud asset after a sidecar/thumbnail worker has
    /// changed its local sync metadata. This avoids a catalog rescan and keeps
    /// queued, failed, conflict, and synced badges live.
    pub(crate) fn update_cloud_sync_state_for_cached_raw(
        &mut self,
        raw_path: &Path,
        context: &egui::Context,
    ) {
        let Some((asset_id, state)) = crate::cloud::cached_asset_sync_state(raw_path) else {
            return;
        };
        let mut changed = false;
        for entry in &mut self.entries {
            let LibrarySource::Cloud(asset) = &entry.info.source else {
                continue;
            };
            if asset.id == asset_id && entry.info.cloud_sync_state != state {
                entry.info.cloud_sync_state = state;
                changed = true;
            }
        }
        if changed {
            context.request_repaint();
        }
    }

    #[cfg(all(not(target_os = "android"), test))]
    pub(crate) fn new(context: &egui::Context) -> Self {
        Self::new_desktop_with_preferences(
            context,
            default_thumbnail_worker_count(),
            LibraryThumbnailSize::default(),
            LibrarySortOrder::default(),
            true,
        )
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn new_desktop_with_preferences(
        context: &egui::Context,
        workers: usize,
        thumbnail_size: LibraryThumbnailSize,
        sort_order: LibrarySortOrder,
        folder_sidebar_open: bool,
    ) -> Self {
        let _ = context;
        let thumbnail_workers = workers.clamp(1, maximum_thumbnail_worker_count());
        crate::thumbnail_cache::set_rendered_thumbnail_worker_limit(thumbnail_workers);
        Self {
            location: None,
            local_location: None,
            view: LibraryView::Local,
            cloud_config: crate::cloud::CloudConfig::default(),
            cloud_cache_root: None,
            cloud_offline_reason: None,
            cloud_connection_receiver: None,
            cloud_connection_status: None,
            cloud_open_receiver: None,
            cloud_open_label: None,
            cloud_upload_receiver: None,
            cloud_upload_completion: None,
            cloud_folders: Vec::new(),
            cloud_asset_folders: HashMap::new(),
            cloud_folder_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
            cloud_expanded_folders: initial_cloud_expanded_folders(),
            cloud_action_receiver: None,
            cloud_clipboard: None,
            image_clipboard: None,
            image_paste_receiver: None,
            cloud_name_dialog: None,
            cloud_delete_confirmation: None,
            cloud_trash_open: false,
            cloud_trash_items: Vec::new(),
            cloud_trash_server_time: 0,
            cloud_trash_retention_days: 14,
            cloud_trash_receiver: None,
            cloud_trash_selection: HashSet::new(),
            cloud_trash_delete_confirmation: None,
            folder: None,
            root_folder: None,
            folder_tree: None,
            expanded_folders: HashSet::new(),
            folder_sidebar_open,
            entries: Vec::new(),
            entry_indices: HashMap::new(),
            event_receiver: None,
            request_sender: None,
            generation: Arc::new(AtomicU64::new(0)),
            decoding_paused: Arc::new(AtomicBool::new(false)),
            decode_gate: Arc::new(RwLock::new(())),
            scanning: false,
            catalog_ready: false,
            status: "Open a folder to build your RAW library.".to_owned(),
            usage_clock: 0,
            thumbnail_workers,
            sort_order,
            thumbnail_size,
            selected_sources: HashSet::new(),
            selection_mode: false,
            file_action_receiver: None,
            raw_import_receiver: None,
            folder_operation_receiver: None,
            folder_clipboard: None,
            folder_name_dialog: None,
            folder_delete_confirmation: None,
            raw_name_dialog: None,
            export_dialog: None,
            adjustment_paste_dialog: None,
            ai_mask_refresh_prompt: None,
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn new_android_with_workers(
        android_app: auraw_ffi::AndroidApp,
        context: &egui::Context,
        workers: usize,
        thumbnail_size: LibraryThumbnailSize,
        sort_order: LibrarySortOrder,
        selected_folder: String,
    ) -> Self {
        let root_location =
            crate::android::library_location(&android_app).unwrap_or_else(|error| {
                log::warn!("{error}");
                "Android/media/de.duecki.auraw/.library".to_owned()
            });
        let selected_folder =
            match crate::android::select_library_folder(&android_app, &selected_folder) {
                Ok(()) => selected_folder,
                Err(error) => {
                    log::warn!("{error}");
                    if let Err(root_error) = crate::android::select_library_folder(&android_app, "")
                    {
                        log::warn!("{root_error}");
                    }
                    String::new()
                }
            };
        let location = android_library_location_label(&root_location, &selected_folder);
        let thumbnail_workers = workers.clamp(1, maximum_thumbnail_worker_count());
        crate::thumbnail_cache::set_rendered_thumbnail_worker_limit(thumbnail_workers);
        let mut state = Self {
            location: Some(location.clone()),
            local_location: Some(location),
            view: LibraryView::Local,
            cloud_config: crate::cloud::CloudConfig::default(),
            cloud_cache_root: None,
            cloud_offline_reason: None,
            cloud_connection_receiver: None,
            cloud_connection_status: None,
            cloud_open_receiver: None,
            cloud_open_label: None,
            cloud_upload_receiver: None,
            cloud_upload_completion: None,
            cloud_folders: Vec::new(),
            cloud_asset_folders: HashMap::new(),
            cloud_folder_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
            cloud_expanded_folders: initial_cloud_expanded_folders(),
            cloud_action_receiver: None,
            cloud_clipboard: None,
            image_clipboard: None,
            image_paste_receiver: None,
            cloud_name_dialog: None,
            cloud_delete_confirmation: None,
            cloud_trash_open: false,
            cloud_trash_items: Vec::new(),
            cloud_trash_server_time: 0,
            cloud_trash_retention_days: 14,
            cloud_trash_receiver: None,
            cloud_trash_selection: HashSet::new(),
            cloud_trash_delete_confirmation: None,
            folder_sidebar_open: false,
            android_raw_name_dialog: None,
            android_folder_name_dialog: None,
            android_app,
            android_root_location: root_location,
            android_folder: selected_folder.clone(),
            android_folders: Vec::new(),
            android_expanded_folders: android_folder_ancestors(&selected_folder),
            entries: Vec::new(),
            entry_indices: HashMap::new(),
            event_receiver: None,
            request_sender: None,
            generation: Arc::new(AtomicU64::new(0)),
            decoding_paused: Arc::new(AtomicBool::new(false)),
            decode_gate: Arc::new(RwLock::new(())),
            scanning: false,
            catalog_ready: false,
            status: String::new(),
            usage_clock: 0,
            thumbnail_workers,
            sort_order,
            thumbnail_size,
            selected_sources: HashSet::new(),
            selection_mode: false,
            export_dialog: None,
            adjustment_paste_dialog: None,
            ai_mask_refresh_prompt: None,
        };
        state.refresh(context);
        state
    }

    pub(crate) fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    #[cfg(target_os = "android")]
    pub(crate) fn has_selection(&self) -> bool {
        !self.selected_sources.is_empty()
    }

    pub(crate) fn selection_mode(&self) -> bool {
        self.selection_mode
    }

    pub(crate) fn begin_selection(&mut self) {
        self.selection_mode = true;
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected_sources.clear();
        self.selection_mode = false;
    }

    pub(super) fn toggle_thumbnail_selection(&mut self, source: &LibrarySource) -> bool {
        self.begin_selection();
        if !self.selected_sources.remove(source) {
            self.selected_sources.insert(source.clone());
        }
        if self.selected_sources.is_empty() {
            self.clear_selection();
        }
        self.selection_mode()
    }

    pub(crate) fn thumbnail_worker_count(&self) -> usize {
        self.thumbnail_workers
    }

    pub(crate) fn thumbnail_size(&self) -> LibraryThumbnailSize {
        self.thumbnail_size
    }

    pub(crate) fn set_thumbnail_size(&mut self, thumbnail_size: LibraryThumbnailSize) -> bool {
        if self.thumbnail_size == thumbnail_size {
            return false;
        }
        self.thumbnail_size = thumbnail_size;
        true
    }

    pub(crate) fn sort_order(&self) -> LibrarySortOrder {
        self.sort_order
    }

    pub(crate) fn set_sort_order(&mut self, sort_order: LibrarySortOrder) -> bool {
        if self.sort_order == sort_order {
            return false;
        }
        self.sort_order = sort_order;
        self.sort_entries();
        true
    }

    pub(super) fn sort_entries(&mut self) {
        let sort_order = self.sort_order;
        self.entries
            .sort_by(|left, right| compare_library_entries(left, right, sort_order));
        self.rebuild_entry_indices();
    }

    pub(super) fn rebuild_entry_indices(&mut self) {
        self.entry_indices = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.info.source.clone(), index))
            .collect();
    }

    pub(crate) fn set_thumbnail_worker_count(&mut self, workers: usize, context: &egui::Context) {
        let workers = workers.clamp(1, maximum_thumbnail_worker_count());
        if self.thumbnail_workers == workers {
            return;
        }
        self.thumbnail_workers = workers;
        crate::thumbnail_cache::set_rendered_thumbnail_worker_limit(workers);
        if self.location.is_some() {
            self.refresh(context);
        }
    }

    /// Stops catalog-wide background thumbnail decoding while Develop owns the
    /// full RAW. Explicit display-priority requests from the desktop filmstrip
    /// may still run through the shared decode gate, which preserves full-RAW
    /// decode exclusivity while keeping visible Develop thumbnails responsive.
    pub(crate) fn prepare_for_develop(&mut self) {
        self.decoding_paused.store(true, Ordering::Release);
        #[cfg(target_os = "android")]
        self.evict_textures_to_limit(ANDROID_DEVELOP_TEXTURE_CACHE_LIMIT);
    }

    /// Returns the gate shared by all thumbnail generations. Full RAW workers
    /// use the same gate so a sensor decode cannot overlap a preview decode.
    pub(crate) fn decode_gate(&self) -> Arc<RwLock<()>> {
        Arc::clone(&self.decode_gate)
    }

    /// Cancels the current generation and drops every in-memory preview before
    /// the platform cache directory is cleared. The caller takes the shared
    /// decode gate's writer lock before deleting files, then calls `refresh`.
    pub(crate) fn prepare_for_thumbnail_cache_clear(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.event_receiver = None;
        self.request_sender = None;
        self.scanning = false;
        self.usage_clock = 0;
        for entry in &mut self.entries {
            entry.texture = None;
            entry.resident_thumbnail = None;
            entry.texture_is_resident = false;
            entry.thumbnail_size = None;
            entry.thumbnail_error = None;
            entry.thumbnail_failures = 0;
            entry.thumbnail_retry_after = None;
            entry.thumbnail_queued = false;
            entry.developed_thumbnail = false;
            entry.last_used = 0;
        }
    }

    pub(super) fn resume_thumbnail_decoding(&self) {
        self.decoding_paused.store(false, Ordering::Release);
    }

    pub(crate) fn refresh(&mut self, context: &egui::Context) {
        if self.view == LibraryView::Cloud && self.cloud_trash_open {
            self.refresh_cloud_trash(context);
            return;
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let cancellation = Arc::clone(&self.generation);
        let decoding_paused = Arc::clone(&self.decoding_paused);
        let decode_gate = Arc::clone(&self.decode_gate);
        let thumbnail_workers = self.thumbnail_workers;
        let repaint = context.clone();
        let (event_sender, event_receiver) = mpsc::sync_channel(MAX_PENDING_THUMBNAIL_RESULTS);
        let (request_sender, request_receiver) = mpsc::sync_channel(MAX_PENDING_THUMBNAILS);
        self.event_receiver = Some(event_receiver);
        self.request_sender = Some(request_sender);
        // Keep already decoded GPU textures visible while the same folder is
        // rescanned. Catalog reconciliation below reuses only entries whose
        // RAW identity is unchanged, so reopening/refreshing a folder does not
        // flash every card back to a placeholder or decode cached previews again.
        for entry in &mut self.entries {
            entry.thumbnail_queued = false;
            entry.thumbnail_error = None;
            entry.thumbnail_failures = 0;
            entry.thumbnail_retry_after = None;
        }
        self.scanning = true;
        self.catalog_ready = !self.entries.is_empty();
        self.usage_clock = 0;

        if self.view == LibraryView::Cloud {
            let config = self.cloud_config.clone();
            let folder_id = self.cloud_folder_id.clone();
            let allow_network = self.cloud_network_available();
            let Some(cache_root) = self.cloud_cache_root.clone() else {
                self.event_receiver = None;
                self.request_sender = None;
                self.scanning = false;
                self.catalog_ready = true;
                self.status = "AuRaw could not locate its private cloud cache.".to_owned();
                return;
            };
            self.status = "Refreshing AuRaw Cloud…".to_owned();
            let worker = std::thread::Builder::new()
                .name("auraw-cloud-library".to_owned())
                .spawn(move || {
                    let snapshot =
                        match crate::cloud::list_assets_cached(&config, &cache_root, allow_network)
                        {
                            Ok(snapshot) => snapshot,
                            Err(error) => {
                                send_scan_failure(&event_sender, generation, error, &repaint);
                                return;
                            }
                        };
                    let thumbnail_network_available = snapshot.offline_reason.is_none();
                    let folder_id = cloud_folder_id_for_catalog(&folder_id, &snapshot.folders);
                    let asset_folders = snapshot
                        .items
                        .iter()
                        .map(|asset| (asset.id.clone(), asset.folder_id.clone()))
                        .collect();
                    if event_sender
                        .send(ScanEvent::CloudAvailability {
                            generation,
                            offline_reason: snapshot.offline_reason,
                            folders: snapshot.folders,
                            folder_id: folder_id.clone(),
                            asset_folders,
                        })
                        .is_err()
                    {
                        return;
                    }
                    let files = snapshot
                        .items
                        .into_iter()
                        .filter(|asset| asset.folder_id == folder_id)
                        .map(|asset| {
                            let cloud_downloaded =
                                crate::cloud::asset_available_offline(&config, &cache_root, &asset);
                            let cloud_sync_state =
                                crate::cloud::asset_sync_state(&config, &cache_root, &asset);
                            LibraryFileInfo {
                                display_path: format!("AuRaw Cloud / {}", asset.name),
                                name: asset.name.clone(),
                                bytes: asset.bytes,
                                dimensions_hint: Some([asset.width, asset.height]),
                                cloud_downloaded,
                                cloud_sync_state,
                                #[cfg(not(target_os = "android"))]
                                modified: Some(crate::cloud::modified_time(asset.modified_seconds)),
                                source: LibrarySource::Cloud(asset),
                            }
                        })
                        .collect::<Vec<_>>();
                    let thumbnail_config = config.clone();
                    let thumbnail_cache = cache_root.clone();
                    run_thumbnail_workers(
                        ThumbnailWorker {
                            files,
                            warning_count: 0,
                            truncated: false,
                            generation,
                            cancellation,
                            decoding_paused,
                            decode_gate,
                            event_sender,
                            request_receiver,
                            repaint,
                        },
                        thumbnail_workers,
                        Arc::new(move |source| match source {
                            LibrarySource::Cloud(asset) => crate::cloud::load_thumbnail(
                                &thumbnail_config,
                                &thumbnail_cache,
                                asset,
                                THUMBNAIL_EDGE,
                                thumbnail_network_available,
                            )
                            .map(|thumbnail| loaded_library_thumbnail(thumbnail, false)),
                            _ => Err("invalid cloud thumbnail request".to_owned()),
                        }),
                    );
                });
            if let Err(error) = worker {
                self.event_receiver = None;
                self.request_sender = None;
                self.scanning = false;
                self.catalog_ready = true;
                self.status = format!("Could not start the cloud library scanner: {error}");
            }
            return;
        }

        #[cfg(not(target_os = "android"))]
        let worker = {
            let Some(folder) = self.folder.clone() else {
                self.event_receiver = None;
                self.request_sender = None;
                self.scanning = false;
                self.status = "Open a folder to build your RAW library.".to_owned();
                return;
            };
            let Some(root_folder) = self.root_folder.clone() else {
                self.event_receiver = None;
                self.request_sender = None;
                self.scanning = false;
                self.status = "Open a top-level folder to build your RAW library.".to_owned();
                return;
            };
            self.status = format!("Scanning {}…", folder.display());
            std::thread::Builder::new()
                .name("auraw-library".to_owned())
                .spawn(move || {
                    let tree_sender = event_sender.clone();
                    let tree_repaint = repaint.clone();
                    let tree_cancellation = Arc::clone(&cancellation);
                    let tree_worker = std::thread::Builder::new()
                        .name("auraw-library-folders".to_owned())
                        .spawn(move || {
                            if let Some(tree) = scan_folder_tree(&root_folder, || {
                                tree_cancellation.load(Ordering::Acquire) != generation
                            }) {
                                let _ =
                                    tree_sender.send(ScanEvent::FolderTree { generation, tree });
                                tree_repaint.request_repaint();
                            }
                        });
                    if let Err(error) = tree_worker {
                        log::warn!("could not start the library folder scanner: {error}");
                    }
                    let scan = match scan_folder(&folder, || {
                        cancellation.load(Ordering::Acquire) != generation
                    }) {
                        Ok(result) => result,
                        Err(error) => {
                            send_scan_failure(&event_sender, generation, error, &repaint);
                            return;
                        }
                    };
                    let Some((files, warning_count, truncated)) = scan else {
                        return;
                    };
                    run_thumbnail_workers(
                        ThumbnailWorker {
                            files,
                            warning_count,
                            truncated,
                            generation,
                            cancellation,
                            decoding_paused,
                            decode_gate,
                            event_sender,
                            request_receiver,
                            repaint,
                        },
                        thumbnail_workers,
                        Arc::new(load_desktop_library_thumbnail),
                    );
                })
        };

        #[cfg(target_os = "android")]
        let worker = {
            let android_app = self.android_app.clone();
            self.status = "Refreshing AuRaw library…".to_owned();
            std::thread::Builder::new()
                .name("auraw-library".to_owned())
                .spawn(move || {
                    let folders = match crate::android::list_library_folders(&android_app) {
                        Ok(folders) => folders,
                        Err(error) => {
                            send_scan_failure(&event_sender, generation, error, &repaint);
                            return;
                        }
                    };
                    if event_sender
                        .send(ScanEvent::AndroidFolders {
                            generation,
                            folders,
                        })
                        .is_err()
                    {
                        return;
                    }
                    let documents = match crate::android::list_library_documents(&android_app) {
                        Ok(documents) => documents,
                        Err(error) => {
                            send_scan_failure(&event_sender, generation, error, &repaint);
                            return;
                        }
                    };
                    let truncated = documents.len() > MAX_LIBRARY_FILES;
                    let files = documents
                        .into_iter()
                        .take(MAX_LIBRARY_FILES)
                        .map(|document| {
                            let dimensions_hint = crate::android::load_library_display_dimensions(
                                &android_app,
                                &document.uri,
                            )
                            .ok();
                            LibraryFileInfo {
                                source: LibrarySource::Android {
                                    uri: document.uri,
                                    display_name: document.display_name.clone(),
                                    bytes: document.bytes,
                                    modified_seconds: document.modified_seconds,
                                },
                                display_path: document.display_path,
                                name: document.display_name,
                                bytes: document.bytes,
                                dimensions_hint,
                                cloud_downloaded: false,
                                cloud_sync_state: crate::cloud::CloudSyncState::Synced,
                            }
                        })
                        .collect();
                    let thumbnail_app = android_app.clone();
                    run_thumbnail_workers(
                        ThumbnailWorker {
                            files,
                            warning_count: 0,
                            truncated,
                            generation,
                            cancellation,
                            decoding_paused,
                            decode_gate,
                            event_sender,
                            request_receiver,
                            repaint,
                        },
                        thumbnail_workers,
                        Arc::new(move |source| {
                            load_android_library_thumbnail(&thumbnail_app, source)
                        }),
                    );
                })
        };

        if let Err(error) = worker {
            self.event_receiver = None;
            self.request_sender = None;
            self.scanning = false;
            self.catalog_ready = true;
            self.status = format!("Could not start the library scanner: {error}");
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn poll_dropped_raw_import(&mut self, context: &egui::Context) {
        let imported = self
            .raw_import_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match imported {
            Some(Ok(result)) => {
                self.raw_import_receiver = None;
                if !result.imported.is_empty() || !result.imported_folders.is_empty() {
                    self.refresh(context);
                }
                self.status = raw_import_status(&result);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.raw_import_receiver = None;
                self.status = "The dropped RAW import stopped unexpectedly.".to_owned();
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }
    }

    pub(crate) fn poll(&mut self, context: &egui::Context) {
        let pasted = self
            .image_paste_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match pasted {
            Some(Ok(completion)) => {
                self.image_paste_receiver = None;
                if completion.clear_clipboard {
                    self.image_clipboard = None;
                } else if let Some(remaining) = completion.remaining_clipboard {
                    self.image_clipboard = Some(remaining);
                }
                self.clear_selection();
                #[cfg(target_os = "android")]
                crate::android::set_back_navigation_active(false);
                self.status = completion.result.unwrap_or_else(|error| error);
                self.refresh(context);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.image_paste_receiver = None;
                self.status = "The image paste stopped unexpectedly.".to_owned();
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }

        loop {
            let received = self
                .cloud_upload_receiver
                .as_ref()
                .map(mpsc::Receiver::try_recv);
            match received {
                Some(Ok(CloudUploadEvent::Progress {
                    position,
                    total,
                    label,
                })) => {
                    self.status =
                        format!("Uploading {position} of {total} to AuRaw Cloud · {label}…");
                }
                Some(Ok(CloudUploadEvent::Finished {
                    target,
                    uploaded,
                    failed,
                    errors,
                })) => {
                    self.cloud_upload_receiver = None;
                    let mut summary = match (uploaded, failed) {
                        (0, 0) => "No RAW files were uploaded to AuRaw Cloud.".to_owned(),
                        (_, 0) => format!(
                            "Uploaded {uploaded} RAW {} to AuRaw Cloud.",
                            if uploaded == 1 { "file" } else { "files" }
                        ),
                        _ => format!(
                            "Uploaded {uploaded} RAW {}; {failed} failed.",
                            if uploaded == 1 { "file" } else { "files" }
                        ),
                    };
                    if !errors.is_empty() {
                        summary.push('\n');
                        summary.push_str(&errors.join("\n"));
                    }
                    if uploaded > 0
                        && self.view == LibraryView::Cloud
                        && self.cloud_config == target
                    {
                        self.cloud_upload_completion = Some(summary);
                        self.refresh(context);
                    } else {
                        self.status = summary;
                    }
                    break;
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.cloud_upload_receiver = None;
                    self.status = "The AuRaw Cloud upload stopped unexpectedly.".to_owned();
                    break;
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => break,
            }
        }

        #[cfg(not(target_os = "android"))]
        {
            self.poll_dropped_raw_import(context);

            let completed = self
                .file_action_receiver
                .as_ref()
                .map(mpsc::Receiver::try_recv);
            match completed {
                Some(Ok(Ok(destinations))) => {
                    self.file_action_receiver = None;
                    self.refresh(context);
                    self.status = if destinations.len() == 1 {
                        format!("Duplicated as {}", destinations[0].display())
                    } else {
                        format!("Duplicated {} selected RAW files", destinations.len())
                    };
                }
                Some(Ok(Err(error))) => {
                    self.file_action_receiver = None;
                    self.refresh(context);
                    self.status = self
                        .cloud_upload_completion
                        .take()
                        .map(|summary| format!("{summary}\nCloud refresh failed: {error}"))
                        .unwrap_or(error);
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.file_action_receiver = None;
                    self.status = "The library file operation stopped unexpectedly.".to_owned();
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => {}
            }

            let folder_completed = self
                .folder_operation_receiver
                .as_ref()
                .map(mpsc::Receiver::try_recv);
            match folder_completed {
                Some(Ok(completion)) => {
                    self.folder_operation_receiver = None;
                    if self.root_folder.as_ref() != Some(&completion.root) {
                        self.status = match completion.result {
                            Ok(_) => format!(
                                "Folder operation completed in the previous library root {}.",
                                completion.root.display()
                            ),
                            Err(error) => error,
                        };
                    } else {
                        match completion.result {
                            Ok(result) => self.apply_folder_operation_result(result, context),
                            Err(error) => {
                                self.refresh(context);
                                self.status = error;
                            }
                        }
                    }
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.folder_operation_receiver = None;
                    self.status = "The folder operation stopped unexpectedly.".to_owned();
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => {}
            }
        }

        for _ in 0..MAX_EVENTS_PER_FRAME {
            let received = self.event_receiver.as_ref().map(mpsc::Receiver::try_recv);
            let event = match received {
                Some(Ok(event)) => event,
                Some(Err(mpsc::TryRecvError::Empty)) | None => break,
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.event_receiver = None;
                    self.request_sender = None;
                    self.scanning = false;
                    if !self.catalog_ready {
                        self.status = "The library scanner stopped unexpectedly.".to_owned();
                    }
                    break;
                }
            };

            match event {
                ScanEvent::CloudAvailability {
                    generation,
                    offline_reason,
                    folders,
                    folder_id,
                    asset_folders,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    self.cloud_offline_reason = offline_reason;
                    self.cloud_folders = folders;
                    self.cloud_asset_folders = asset_folders;
                    self.cloud_expanded_folders.retain(|folder_id| {
                        folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID
                            || self
                                .cloud_folders
                                .iter()
                                .any(|folder| &folder.id == folder_id)
                    });
                    self.cloud_folder_id = folder_id;
                    self.update_cloud_location();
                }
                #[cfg(not(target_os = "android"))]
                ScanEvent::FolderTree { generation, tree }
                    if generation == self.generation.load(Ordering::Acquire) =>
                {
                    self.folder_tree = Some(tree);
                }
                #[cfg(target_os = "android")]
                ScanEvent::AndroidFolders {
                    generation,
                    folders,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    self.android_folders = folders;
                    self.android_expanded_folders.retain(|path| {
                        path.is_empty()
                            || self
                                .android_folders
                                .iter()
                                .any(|folder| &folder.path == path)
                    });
                    self.android_expanded_folders
                        .extend(android_folder_ancestors(&self.android_folder));
                }
                ScanEvent::Catalog {
                    generation,
                    files,
                    warning_count,
                    truncated,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    let mut previous = std::mem::take(&mut self.entries)
                        .into_iter()
                        .map(|entry| (entry.info.source.clone(), entry))
                        .collect::<HashMap<_, _>>();
                    self.entries = files
                        .into_iter()
                        .map(|info| {
                            if let Some(mut entry) = previous.remove(&info.source) {
                                if same_library_file_identity(&entry.info, &info) {
                                    entry.info = info;
                                    entry.thumbnail_error = None;
                                    entry.thumbnail_queued = false;
                                    entry.last_used = 0;
                                    return entry;
                                }
                            }
                            new_library_entry(info)
                        })
                        .collect();
                    self.sort_entries();
                    self.selected_sources
                        .retain(|source| self.entry_indices.contains_key(source));
                    if self.selected_sources.is_empty() && !self.selection_mode {
                        #[cfg(target_os = "android")]
                        crate::android::set_back_navigation_active(false);
                    }
                    self.scanning = false;
                    self.catalog_ready = true;
                    let mut catalog_status = catalog_status(warning_count, truncated);
                    if self.view == LibraryView::Cloud {
                        if let Some(reason) = &self.cloud_offline_reason {
                            let prefix = "Offline · cached cloud library";
                            catalog_status = if catalog_status.is_empty() {
                                format!("{prefix}\n{reason}")
                            } else {
                                format!("{prefix} · {catalog_status}\n{reason}")
                            };
                        }
                    }
                    self.status = match (
                        self.cloud_upload_completion.take(),
                        catalog_status.is_empty(),
                    ) {
                        (Some(summary), true) => summary,
                        (Some(summary), false) => format!("{summary}\n{catalog_status}"),
                        (None, _) => catalog_status,
                    };
                }
                ScanEvent::Thumbnail {
                    generation,
                    source,
                    display_priority,
                    result,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    let Some(index) = self.entry_indices.get(&source).copied() else {
                        continue;
                    };
                    self.entries[index].thumbnail_queued = false;
                    match result {
                        Ok(loaded) => {
                            if self.entries[index].developed_thumbnail && !loaded.developed {
                                continue;
                            }
                            let LoadedLibraryThumbnail {
                                thumbnail,
                                resident_thumbnail,
                                developed,
                            } = loaded;
                            let decoded_size = [thumbnail.width, thumbnail.height];
                            let install_pixels =
                                display_priority || self.entries[index].texture.is_some();
                            self.entries[index].resident_thumbnail = Some(resident_thumbnail);
                            self.entries[index].texture_is_resident = false;
                            if install_pixels {
                                let image = egui::ColorImage::from_rgba_unmultiplied(
                                    [thumbnail.width as usize, thumbnail.height as usize],
                                    &thumbnail.rgba,
                                );
                                self.entries[index].texture = Some(context.load_texture(
                                    format!("library-thumbnail-{generation}-{index}"),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                            self.entries[index].thumbnail_size = Some(decoded_size);
                            self.entries[index].layout_size.get_or_insert(decoded_size);
                            self.entries[index].thumbnail_error = None;
                            self.entries[index].thumbnail_failures = 0;
                            self.entries[index].thumbnail_retry_after = None;
                            self.entries[index].developed_thumbnail = developed;
                        }
                        Err(error) => {
                            if !self.entries[index].developed_thumbnail {
                                let entry = &mut self.entries[index];
                                entry.thumbnail_failures =
                                    entry.thumbnail_failures.saturating_add(1);
                                let exponent =
                                    u32::from(entry.thumbnail_failures.saturating_sub(1).min(5));
                                let delay = Duration::from_secs(1_u64 << exponent)
                                    .min(THUMBNAIL_RETRY_MAX_DELAY);
                                entry.thumbnail_error = Some(error);
                                entry.thumbnail_retry_after = Some(Instant::now() + delay);
                                context.request_repaint_after(delay);
                            }
                        }
                    }
                }
                ScanEvent::Failed { generation, error }
                    if generation == self.generation.load(Ordering::Acquire) =>
                {
                    self.scanning = false;
                    self.catalog_ready = true;
                    self.event_receiver = None;
                    self.request_sender = None;
                    self.status = self
                        .cloud_upload_completion
                        .take()
                        .map(|summary| format!("{summary}\nCloud refresh failed: {error}"))
                        .unwrap_or(error);
                    break;
                }
                _ => {}
            }
        }
        let _ = context;
    }

}
