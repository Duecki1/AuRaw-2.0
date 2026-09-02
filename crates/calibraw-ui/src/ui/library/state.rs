use super::*;

impl LibraryState {
    #[cfg(all(not(target_os = "android"), test))]
    pub(crate) fn new() -> Self {
        Self::new_desktop_with_preferences(
            default_thumbnail_worker_count(),
            LibraryThumbnailSize::default(),
            LibrarySortOrder::default(),
            true,
            false,
        )
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn new_desktop_with_preferences(
        workers: usize,
        thumbnail_size: LibraryThumbnailSize,
        sort_order: LibrarySortOrder,
        folder_sidebar_open: bool,
        render_edited_thumbnails_during_indexing: bool,
    ) -> Self {
        let thumbnail_workers = workers.clamp(1, maximum_thumbnail_worker_count());
        crate::thumbnail_cache::set_rendered_thumbnail_worker_limit(thumbnail_workers);
        Self {
            location: None,
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
            thumbnail_progress: ThumbnailBackgroundProgress::default(),
            scanning: false,
            catalog_ready: false,
            status: "Open a folder to build your RAW library.".to_owned(),
            usage_clock: 0,
            thumbnail_workers,
            render_edited_thumbnails_during_indexing,
            sort_order,
            thumbnail_size,
            search_query: String::new(),
            selected_assets: HashSet::new(),
            selection_mode: false,
            image_clipboard: None,
            adjustment_clipboard: None,
            asset_transfer_receiver: None,
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
        android_app: calibraw_ffi::AndroidApp,
        context: &egui::Context,
        workers: usize,
        thumbnail_size: LibraryThumbnailSize,
        sort_order: LibrarySortOrder,
        selected_folder: String,
        render_edited_thumbnails_during_indexing: bool,
    ) -> Self {
        let root_location =
            crate::android::library_location(&android_app).unwrap_or_else(|error| {
                log::warn!("{error}");
                "Android/media/de.duecki.calibraw/.library".to_owned()
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
            location: Some(location),
            folder_sidebar_open: false,
            platform: PlatformLibraryState {
                app: android_app,
                root_location,
                folder: selected_folder.clone(),
                folders: Vec::new(),
                expanded_folders: android_folder_ancestors(&selected_folder),
                folder_name_dialog: None,
            },
            entries: Vec::new(),
            entry_indices: HashMap::new(),
            event_receiver: None,
            request_sender: None,
            generation: Arc::new(AtomicU64::new(0)),
            decoding_paused: Arc::new(AtomicBool::new(false)),
            decode_gate: Arc::new(RwLock::new(())),
            thumbnail_progress: ThumbnailBackgroundProgress::default(),
            scanning: false,
            catalog_ready: false,
            status: String::new(),
            usage_clock: 0,
            thumbnail_workers,
            render_edited_thumbnails_during_indexing,
            sort_order,
            thumbnail_size,
            search_query: String::new(),
            selected_assets: HashSet::new(),
            selection_mode: false,
            image_clipboard: None,
            adjustment_clipboard: None,
            asset_transfer_receiver: None,
            raw_name_dialog: None,
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

    pub(crate) fn has_copied_adjustments(&self) -> bool {
        self.adjustment_clipboard.is_some()
    }

    pub(crate) fn install_adjustment_clipboard(
        &mut self,
        edits: crate::sidecar::EditState,
        settings: crate::sidecar::AdjustmentCopySettings,
    ) {
        self.adjustment_clipboard = Some(LibraryAdjustmentClipboard { edits, settings });
    }

    #[cfg(any(target_os = "android", test))]
    pub(crate) fn has_selection(&self) -> bool {
        !self.selected_assets.is_empty()
    }

    pub(crate) fn asset_transfer_in_progress(&self) -> bool {
        self.asset_transfer_receiver.is_some()
    }

    pub(crate) fn local_mutation_in_progress(&self) -> bool {
        if self.asset_transfer_in_progress() {
            return true;
        }
        #[cfg(not(target_os = "android"))]
        {
            self.raw_import_receiver.is_some() || self.folder_operation_receiver.is_some()
        }
        #[cfg(target_os = "android")]
        {
            false
        }
    }

    pub(crate) fn selection_mode(&self) -> bool {
        self.selection_mode
    }

    pub(crate) fn begin_selection(&mut self) {
        self.selection_mode = true;
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected_assets.clear();
        self.selection_mode = false;
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn search_query_mut(&mut self) -> &mut String {
        &mut self.search_query
    }

    pub(crate) fn clear_search(&mut self) {
        self.search_query.clear();
    }

    pub(crate) fn search_active(&self) -> bool {
        !library_search_terms(&self.search_query).is_empty()
    }

    pub(super) fn filtered_entry_indices(&self) -> Vec<usize> {
        let terms = library_search_terms(&self.search_query);
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                library_filename_matches(&entry.asset.display_name, &terms).then_some(index)
            })
            .collect()
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn select_search_matches(&mut self) -> usize {
        let terms = library_search_terms(&self.search_query);
        if terms.is_empty() {
            return 0;
        }

        self.selected_assets = self
            .entries
            .iter()
            .filter(|entry| library_filename_matches(&entry.asset.display_name, &terms))
            .map(|entry| entry.asset.id.clone())
            .collect();
        self.selection_mode = !self.selected_assets.is_empty();

        #[cfg(target_os = "android")]
        crate::android::set_back_navigation_active(self.selection_mode);

        self.selected_assets.len()
    }

    pub(super) fn toggle_thumbnail_selection(&mut self, asset_id: &LibraryAssetId) -> bool {
        self.begin_selection();
        if !self.selected_assets.remove(asset_id) {
            self.selected_assets.insert(asset_id.clone());
        }
        if self.selected_assets.is_empty() {
            self.clear_selection();
        }
        self.selection_mode()
    }

    pub(crate) fn thumbnail_worker_count(&self) -> usize {
        self.thumbnail_workers
    }

    pub(crate) fn renders_edited_thumbnails_during_indexing(&self) -> bool {
        self.render_edited_thumbnails_during_indexing
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_render_edited_thumbnails_during_indexing(
        &mut self,
        enabled: bool,
        context: &egui::Context,
    ) -> bool {
        if self.render_edited_thumbnails_during_indexing == enabled {
            return false;
        }
        self.render_edited_thumbnails_during_indexing = enabled;
        self.refresh(context);
        true
    }

    pub(crate) fn thumbnail_background_progress(&self) -> Option<ThumbnailProgress> {
        self.thumbnail_progress
            .snapshot(self.decoding_paused.load(Ordering::Acquire))
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
            .map(|(index, entry)| (entry.asset.id.clone(), index))
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

    pub(crate) fn prepare_for_develop(&mut self) {
        self.decoding_paused.store(true, Ordering::Release);
        #[cfg(target_os = "android")]
        self.evict_textures_to_limit(ANDROID_DEVELOP_TEXTURE_CACHE_LIMIT);
    }

    pub(crate) fn decode_gate(&self) -> Arc<RwLock<()>> {
        Arc::clone(&self.decode_gate)
    }

    pub(crate) fn prepare_for_thumbnail_cache_clear(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.event_receiver = None;
        self.request_sender = None;
        self.thumbnail_progress = ThumbnailBackgroundProgress::default();
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
            entry.developed_thumbnail_pending = false;
            entry.last_used = 0;
        }
    }

    pub(super) fn resume_thumbnail_decoding(&self) {
        self.decoding_paused.store(false, Ordering::Release);
    }

    pub(crate) fn refresh(&mut self, context: &egui::Context) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let cancellation = Arc::clone(&self.generation);
        let decoding_paused = Arc::clone(&self.decoding_paused);
        let decode_gate = Arc::clone(&self.decode_gate);
        let thumbnail_workers = self.thumbnail_workers;
        let render_edited_thumbnails_during_indexing =
            self.render_edited_thumbnails_during_indexing;
        let repaint = context.clone();
        let (event_sender, event_receiver) = mpsc::sync_channel(MAX_PENDING_THUMBNAIL_RESULTS);
        let (request_sender, request_receiver) = mpsc::sync_channel(MAX_PENDING_THUMBNAILS);
        self.event_receiver = Some(event_receiver);
        self.request_sender = Some(request_sender);
        self.thumbnail_progress = ThumbnailBackgroundProgress::default();
        for entry in &mut self.entries {
            entry.thumbnail_queued = false;
            entry.thumbnail_error = None;
            entry.thumbnail_failures = 0;
            entry.thumbnail_retry_after = None;
            if !render_edited_thumbnails_during_indexing {
                entry.developed_thumbnail = false;
                entry.developed_thumbnail_pending = false;
            }
        }
        self.scanning = true;
        self.catalog_ready = !self.entries.is_empty();
        self.usage_clock = 0;

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
                .name("calibraw-library".to_owned())
                .spawn(move || {
                    let tree_sender = event_sender.clone();
                    let tree_repaint = repaint.clone();
                    let tree_cancellation = Arc::clone(&cancellation);
                    if let Err(error) = std::thread::Builder::new()
                        .name("calibraw-library-folders".to_owned())
                        .spawn(move || {
                            if let Some(tree) = scan_folder_tree(&root_folder, || {
                                tree_cancellation.load(Ordering::Acquire) != generation
                            }) {
                                let _ =
                                    tree_sender.send(ScanEvent::FolderTree { generation, tree });
                                tree_repaint.request_repaint();
                            }
                        })
                    {
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
                    let Some((assets, warning_count, truncated)) = scan else {
                        return;
                    };
                    run_thumbnail_workers(
                        ThumbnailWorker {
                            assets,
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
                        Arc::new(move |asset, stage| {
                            load_desktop_library_thumbnail(
                                asset,
                                stage,
                                render_edited_thumbnails_during_indexing,
                            )
                        }),
                    );
                })
        };

        #[cfg(target_os = "android")]
        let worker = {
            let android_app = self.platform.app.clone();
            self.status = "Refreshing CalibRaw library…".to_owned();
            std::thread::Builder::new()
                .name("calibraw-library".to_owned())
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
                    let mut assets = documents
                        .into_iter()
                        .take(MAX_LIBRARY_FILES)
                        .map(LibraryAsset::from_android_document)
                        .collect::<Vec<_>>();
                    for asset in &mut assets {
                        if let Some(uri) = asset.android_uri() {
                            asset.metadata.dimensions_hint =
                                crate::android::load_library_display_dimensions(&android_app, uri)
                                    .ok();
                        }
                    }
                    let thumbnail_app = android_app.clone();
                    run_thumbnail_workers(
                        ThumbnailWorker {
                            assets,
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
                        Arc::new(move |asset, stage| {
                            load_android_library_thumbnail(
                                &thumbnail_app,
                                asset,
                                stage,
                                render_edited_thumbnails_during_indexing,
                            )
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
            .asset_transfer_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match pasted {
            Some(Ok(completion)) => {
                self.asset_transfer_receiver = None;
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
                self.asset_transfer_receiver = None;
                self.status = "The Library asset transfer stopped unexpectedly.".to_owned();
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }

        #[cfg(not(target_os = "android"))]
        {
            self.poll_dropped_raw_import(context);

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
                    self.platform.folders = folders;
                    let folder_paths = self
                        .platform
                        .folders
                        .iter()
                        .map(|folder| folder.path.as_str())
                        .collect::<HashSet<_>>();
                    self.platform
                        .expanded_folders
                        .retain(|path| path.is_empty() || folder_paths.contains(path.as_str()));
                    let selected_folder = self.platform.folder.clone();
                    self.platform
                        .expanded_folders
                        .extend(android_folder_ancestors(&selected_folder));
                }
                ScanEvent::Catalog {
                    generation,
                    assets,
                    warning_count,
                    truncated,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    let mut previous = std::mem::take(&mut self.entries)
                        .into_iter()
                        .map(|entry| (entry.asset.id.clone(), entry))
                        .collect::<HashMap<_, _>>();
                    self.entries = assets
                        .into_iter()
                        .map(|asset| {
                            if let Some(mut entry) = previous.remove(&asset.id) {
                                if same_library_asset_identity(&entry.asset, &asset) {
                                    entry.asset = asset;
                                    entry.thumbnail_error = None;
                                    entry.thumbnail_queued = false;
                                    entry.developed_thumbnail_pending = false;
                                    entry.last_used = 0;
                                    return entry;
                                }
                            }
                            new_library_entry(asset)
                        })
                        .collect();
                    self.sort_entries();
                    self.selected_assets
                        .retain(|asset_id| self.entry_indices.contains_key(asset_id));
                    if self.selected_assets.is_empty() && !self.selection_mode {
                        #[cfg(target_os = "android")]
                        crate::android::set_back_navigation_active(false);
                    }
                    self.scanning = false;
                    self.catalog_ready = true;
                    self.thumbnail_progress
                        .begin(generation, self.entries.len());
                    self.status = catalog_status(warning_count, truncated);
                }
                ScanEvent::Thumbnail {
                    generation,
                    asset_id,
                    display_priority,
                    final_thumbnail,
                    result,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    if final_thumbnail {
                        self.thumbnail_progress
                            .record_completion(generation, asset_id.clone());
                    }
                    let Some(index) = self.entry_indices.get(&asset_id).copied() else {
                        continue;
                    };
                    self.entries[index].thumbnail_queued = false;
                    match result {
                        Ok(loaded) => {
                            if self.entries[index].developed_thumbnail
                                && !loaded.developed
                                && !loaded.developed_render_pending
                            {
                                continue;
                            }
                            let LoadedLibraryThumbnail {
                                thumbnail,
                                resident_thumbnail,
                                developed,
                                developed_thumbnail_stale,
                                developed_render_pending,
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
                            self.entries[index].thumbnail_queued = developed_render_pending;
                            self.entries[index].developed_thumbnail_pending =
                                developed_thumbnail_stale;
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
                                entry.developed_thumbnail_pending = false;
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
                    self.status = error;
                    break;
                }
                _ => {}
            }
        }
    }
}

pub(super) fn library_search_terms(query: &str) -> Vec<String> {
    query
        .split(',')
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

pub(super) fn library_filename_matches(display_name: &str, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let display_name = display_name.to_lowercase();
    terms.iter().any(|term| display_name.contains(term))
}
