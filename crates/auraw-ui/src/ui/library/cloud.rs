use super::*;

pub(super) fn cloud_folder_id_for_catalog(
    requested_folder_id: &str,
    folders: &[crate::cloud::CloudFolder],
) -> String {
    if requested_folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID
        || folders
            .iter()
            .any(|folder| folder.id == requested_folder_id)
    {
        requested_folder_id.to_owned()
    } else {
        crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned()
    }
}

pub(super) fn initial_cloud_expanded_folders() -> HashSet<String> {
    HashSet::from([crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned()])
}

pub(super) fn run_cloud_action(
    config: &crate::cloud::CloudConfig,
    cache_root: Option<&Path>,
    allow_network: bool,
    request: CloudActionRequest,
) -> CloudActionCompletion {
    match request {
        CloudActionRequest::CreateFolder { parent_id, name } => {
            let result = crate::cloud::create_folder(config, &parent_id, &name)
                .map(|folder| format!("Created cloud folder {}.", folder.name));
            CloudActionCompletion::Mutation {
                result,
                clear_clipboard: false,
            }
        }
        CloudActionRequest::UpdateFolder {
            folder,
            parent_id,
            name,
            clear_clipboard,
        } => {
            let result = crate::cloud::update_folder(config, &folder, &parent_id, &name)
                .map(|updated| format!("Updated cloud folder {}.", updated.name));
            CloudActionCompletion::Mutation {
                clear_clipboard: clear_clipboard && result.is_ok(),
                result,
            }
        }
        CloudActionRequest::CopyFolder {
            folder,
            destination_parent_id,
            clear_clipboard,
        } => {
            let result = crate::cloud::copy_folder(config, &folder, &destination_parent_id)
                .map(|copied| format!("Copied cloud folder as {}.", copied.name));
            CloudActionCompletion::Mutation {
                clear_clipboard: clear_clipboard && result.is_ok(),
                result,
            }
        }
        CloudActionRequest::DeleteFolder { folder } => CloudActionCompletion::Mutation {
            result: crate::cloud::delete_folder(config, &folder.id)
                .map(|()| format!("Deleted cloud folder {}.", folder.name)),
            clear_clipboard: false,
        },
        CloudActionRequest::CopyAssets {
            assets,
            destination_folder_id,
            clear_clipboard,
        } => {
            let total = assets.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for asset in assets {
                match crate::cloud::copy_asset(config, &asset, &destination_folder_id) {
                    Ok(_) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", asset.name)),
                }
            }
            let result = cloud_batch_summary("Copied", total, completed, errors);
            CloudActionCompletion::Mutation {
                clear_clipboard: clear_clipboard && result.is_ok(),
                result,
            }
        }
        CloudActionRequest::RenameAsset { asset, name } => {
            let result = crate::cloud::update_asset(config, &asset, &asset.folder_id, &name)
                .map(|updated| format!("Renamed cloud RAW to {}.", updated.name));
            CloudActionCompletion::Mutation {
                result,
                clear_clipboard: false,
            }
        }
        CloudActionRequest::DeleteAssets { assets } => {
            let total = assets.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for asset in assets {
                match crate::cloud::delete_asset(config, &asset) {
                    Ok(()) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", asset.name)),
                }
            }
            CloudActionCompletion::Mutation {
                result: cloud_batch_summary("Deleted", total, completed, errors),
                clear_clipboard: false,
            }
        }
        CloudActionRequest::RestoreTrash { items } => {
            let total = items.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for item in items {
                match crate::cloud::restore_trash_item(config, &item, None) {
                    Ok(_) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", item.name)),
                }
            }
            CloudActionCompletion::Mutation {
                result: cloud_batch_summary("Restored", total, completed, errors),
                clear_clipboard: false,
            }
        }
        CloudActionRequest::PermanentlyDeleteTrash { items } => {
            let total = items.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for item in items {
                match crate::cloud::permanently_delete_trash_item(config, &item) {
                    Ok(()) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", item.name)),
                }
            }
            CloudActionCompletion::Mutation {
                result: cloud_batch_summary("Permanently deleted", total, completed, errors),
                clear_clipboard: false,
            }
        }
        CloudActionRequest::EmptyTrash => CloudActionCompletion::Mutation {
            result: crate::cloud::empty_trash(config)
                .map(|()| "Emptied AuRaw Cloud Trash.".to_owned()),
            clear_clipboard: false,
        },
        CloudActionRequest::ResetAssets { assets } => {
            let total = assets.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for asset in assets {
                match crate::cloud::reset_asset_sidecar(config, &asset) {
                    Ok(()) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", asset.name)),
                }
            }
            CloudActionCompletion::Mutation {
                result: cloud_batch_summary("Reset adjustments for", total, completed, errors),
                clear_clipboard: false,
            }
        }
        CloudActionRequest::PrepareAssets { assets, purpose } => {
            let result = (|| {
                let cache_root = cache_root
                    .ok_or_else(|| "AuRaw could not locate its private cloud cache.".to_owned())?;
                crate::cloud::open_assets(config, cache_root, &assets, allow_network)
            })();
            CloudActionCompletion::Prepared { purpose, result }
        }
    }
}


impl LibraryState {
    #[cfg(not(target_os = "android"))]
    pub(crate) fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }

    pub(crate) fn cloud_config(&self) -> &crate::cloud::CloudConfig {
        &self.cloud_config
    }

    pub(crate) fn cloud_enabled(&self) -> bool {
        self.cloud_config.enabled
    }

    pub(crate) fn is_cloud_view(&self) -> bool {
        self.view == LibraryView::Cloud
    }

    pub(crate) fn view(&self) -> LibraryView {
        self.view
    }

    pub(crate) fn cloud_folder_id(&self) -> &str {
        &self.cloud_folder_id
    }

    pub(super) fn cloud_folder(&self, folder_id: &str) -> Option<&crate::cloud::CloudFolder> {
        self.cloud_folders
            .iter()
            .find(|folder| folder.id == folder_id)
    }

    pub(super) fn cloud_folder_path(&self, folder_id: &str) -> String {
        if folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID {
            return "Cloud".to_owned();
        }
        let mut names = Vec::new();
        let mut current = folder_id;
        let mut remaining = self.cloud_folders.len();
        while current != crate::cloud::CLOUD_ROOT_FOLDER_ID && remaining > 0 {
            let Some(folder) = self.cloud_folder(current) else {
                break;
            };
            names.push(folder.name.clone());
            current = &folder.parent_id;
            remaining -= 1;
        }
        names.reverse();
        if names.is_empty() {
            "Cloud".to_owned()
        } else {
            format!("Cloud / {}", names.join(" / "))
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(super) fn cloud_breadcrumbs(&self) -> Vec<(String, String)> {
        let mut breadcrumbs = vec![(
            crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
            "Cloud".to_owned(),
        )];
        if self.cloud_folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID {
            return breadcrumbs;
        }
        let mut descendants = Vec::new();
        let mut current = self.cloud_folder_id.as_str();
        let mut remaining = self.cloud_folders.len();
        while current != crate::cloud::CLOUD_ROOT_FOLDER_ID && remaining > 0 {
            let Some(folder) = self.cloud_folder(current) else {
                break;
            };
            descendants.push((folder.id.clone(), folder.name.clone()));
            current = &folder.parent_id;
            remaining -= 1;
        }
        descendants.reverse();
        breadcrumbs.extend(descendants);
        breadcrumbs
    }

    pub(super) fn update_cloud_location(&mut self) {
        let path = if self.cloud_trash_open {
            "Cloud / Trash".to_owned()
        } else {
            self.cloud_folder_path(&self.cloud_folder_id)
        };
        self.location = self
            .cloud_config
            .normalized()
            .ok()
            .map(|config| format!("AuRaw Cloud · {} · {path}", config.server_url))
            .or_else(|| Some(format!("AuRaw Cloud · {path}")));
    }

    pub(crate) fn select_cloud_folder(
        &mut self,
        folder_id: String,
        context: &egui::Context,
    ) -> bool {
        if folder_id != crate::cloud::CLOUD_ROOT_FOLDER_ID
            && self.cloud_folder(&folder_id).is_none()
        {
            self.status = "That cloud folder no longer exists. Refresh the library.".to_owned();
            return false;
        }
        if self.view == LibraryView::Cloud
            && !self.cloud_trash_open
            && self.cloud_folder_id == folder_id
        {
            return false;
        }
        let mut ancestor_id = folder_id.clone();
        while ancestor_id != crate::cloud::CLOUD_ROOT_FOLDER_ID {
            let Some(parent_id) = self
                .cloud_folder(&ancestor_id)
                .map(|folder| folder.parent_id.clone())
            else {
                break;
            };
            self.cloud_expanded_folders.insert(parent_id.clone());
            ancestor_id = parent_id;
        }
        self.view = LibraryView::Cloud;
        self.cloud_trash_open = false;
        self.cloud_folder_id = folder_id;
        self.update_cloud_location();
        self.entries.clear();
        self.entry_indices.clear();
        self.clear_selection();
        self.catalog_ready = false;
        self.refresh(context);
        true
    }

    pub(crate) fn configure_cloud(
        &mut self,
        config: crate::cloud::CloudConfig,
        cache_root: Option<PathBuf>,
        context: &egui::Context,
    ) {
        let changed = self.cloud_config != config || self.cloud_cache_root != cache_root;
        self.cloud_config = config;
        self.cloud_cache_root = cache_root;
        self.cloud_connection_status = None;
        if changed {
            // Dropping the receiver safely discards any result produced with an
            // old server address or token. The detached worker may finish its
            // bounded cache write, but it can no longer navigate the UI.
            self.cloud_open_receiver = None;
            self.cloud_open_label = None;
            self.cloud_offline_reason = None;
            self.cloud_folders.clear();
            self.cloud_asset_folders.clear();
            self.cloud_folder_id = crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned();
            self.cloud_expanded_folders = initial_cloud_expanded_folders();
            self.cloud_action_receiver = None;
            self.cloud_clipboard = None;
            self.image_clipboard = None;
            self.image_paste_receiver = None;
            self.cloud_name_dialog = None;
            self.cloud_delete_confirmation = None;
            self.cloud_trash_open = false;
            self.cloud_trash_items.clear();
            self.cloud_trash_receiver = None;
            self.cloud_trash_selection.clear();
            self.cloud_trash_delete_confirmation = None;
        }
        if !self.cloud_config.enabled && self.view == LibraryView::Cloud {
            self.show_local(context);
        } else if changed && self.view == LibraryView::Cloud {
            self.refresh(context);
        }
    }

    pub(crate) fn restore_navigation(
        &mut self,
        view: LibraryView,
        cloud_folder_id: String,
        context: &egui::Context,
    ) {
        self.cloud_folder_id = if cloud_folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID
            || (cloud_folder_id.len() == 64
                && cloud_folder_id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            cloud_folder_id
        } else {
            crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned()
        };
        if view == LibraryView::Cloud && self.cloud_config.enabled {
            self.show_cloud(context);
        }
    }

    pub(crate) fn show_cloud(&mut self, context: &egui::Context) -> bool {
        if !self.cloud_config.enabled {
            self.status = "Enable AuRaw Cloud in Settings first.".to_owned();
            return false;
        }
        let changed_view = self.view != LibraryView::Cloud || self.cloud_trash_open;
        self.cloud_trash_open = false;
        if changed_view {
            self.view = LibraryView::Cloud;
            self.update_cloud_location();
            self.entries.clear();
            self.entry_indices.clear();
            self.clear_selection();
            self.catalog_ready = false;
        }
        self.refresh(context);
        changed_view
    }

    pub(crate) fn show_cloud_trash(&mut self, context: &egui::Context) -> bool {
        if !self.cloud_config.enabled {
            self.status = "Enable AuRaw Cloud in Settings first.".to_owned();
            return false;
        }
        let changed_view = self.view != LibraryView::Cloud || !self.cloud_trash_open;
        self.view = LibraryView::Cloud;
        self.cloud_trash_open = true;
        self.update_cloud_location();
        self.entries.clear();
        self.entry_indices.clear();
        self.clear_selection();
        self.catalog_ready = false;
        self.refresh(context);
        changed_view
    }

    pub(crate) fn show_local(&mut self, context: &egui::Context) -> bool {
        if self.cloud_action_receiver.is_some() {
            self.status = "Wait for the current cloud action to finish.".to_owned();
            return false;
        }
        self.cloud_open_receiver = None;
        self.cloud_open_label = None;
        self.cloud_offline_reason = None;
        let changed_view = self.view != LibraryView::Local;
        if changed_view {
            self.view = LibraryView::Local;
            self.location = self.local_location.clone();
            self.entries.clear();
            self.entry_indices.clear();
            self.clear_selection();
            self.catalog_ready = false;
        }
        self.refresh(context);
        changed_view
    }

    pub(crate) fn remember_cloud_folder_without_refresh(&mut self, folder_id: String) -> bool {
        if self.cloud_folder_id == folder_id {
            return false;
        }
        self.cloud_folder_id = folder_id;
        self.cloud_trash_open = false;
        self.update_cloud_location();
        true
    }

    pub(crate) fn start_cloud_connection_test(&mut self, context: &egui::Context) {
        if self.cloud_connection_receiver.is_some() {
            return;
        }
        let config = self.cloud_config.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_connection_status = None;
        self.cloud_connection_receiver = Some(receiver);
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-test".to_owned())
            .spawn(move || {
                let result = crate::cloud::test_connection(&config);
                let _ = sender.send(result);
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_connection_receiver = None;
            self.cloud_connection_status = Some(Err(format!(
                "Could not start the cloud connection test: {error}"
            )));
        }
    }

    pub(crate) fn cloud_connection_status(&mut self) -> Option<&Result<String, String>> {
        let received = self
            .cloud_connection_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match received {
            Some(Ok(result)) => {
                self.cloud_connection_receiver = None;
                self.cloud_connection_status = Some(result);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.cloud_connection_receiver = None;
                self.cloud_connection_status = Some(Err(
                    "The cloud connection test stopped unexpectedly.".to_owned(),
                ));
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }
        self.cloud_connection_status.as_ref()
    }

    pub(crate) fn cloud_connection_test_in_progress(&self) -> bool {
        self.cloud_connection_receiver.is_some()
    }

    pub(crate) fn start_cloud_open(
        &mut self,
        asset: crate::cloud::CloudAsset,
        context: &egui::Context,
    ) {
        if self.cloud_open_receiver.is_some() {
            self.status = "Wait for the current cloud RAW download to finish.".to_owned();
            return;
        }
        let Some(cache_root) = self.cloud_cache_root.clone() else {
            self.status = "AuRaw could not locate its private cloud cache.".to_owned();
            return;
        };
        let config = self.cloud_config.clone();
        let allow_network = self.cloud_network_available();
        let label = asset.name.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_open_receiver = Some(receiver);
        self.cloud_open_label = Some(label.clone());
        self.status = format!("Preparing {label} from AuRaw Cloud…");
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-raw-download".to_owned())
            .spawn(move || {
                let progress_sender = sender.clone();
                let progress_repaint = repaint.clone();
                let mut last_reported = 0u64;
                let result = crate::cloud::open_asset(
                    &config,
                    &cache_root,
                    &asset,
                    allow_network,
                    move |downloaded, total| {
                        if downloaded == total
                            || downloaded.saturating_sub(last_reported)
                                >= CLOUD_DOWNLOAD_PROGRESS_STEP
                        {
                            last_reported = downloaded;
                            let _ = progress_sender
                                .send(CloudOpenEvent::Progress { downloaded, total });
                            progress_repaint.request_repaint();
                        }
                    },
                );
                let _ = sender.send(CloudOpenEvent::Finished(result));
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_open_receiver = None;
            self.cloud_open_label = None;
            self.status = format!("Could not start the cloud RAW download: {error}");
        }
    }

    #[cfg(target_os = "android")]
    pub(super) fn cloud_network_available(&self) -> bool {
        crate::android::network_available(&self.android_app).unwrap_or_else(|error| {
            log::warn!("could not inspect Android network state: {error}");
            true
        })
    }

    #[cfg(not(target_os = "android"))]
    pub(super) fn cloud_network_available(&self) -> bool {
        true
    }

    pub(crate) fn poll_cloud_open(
        &mut self,
    ) -> Option<Result<crate::cloud::CachedCloudAsset, String>> {
        loop {
            let received = self
                .cloud_open_receiver
                .as_ref()
                .map(mpsc::Receiver::try_recv);
            match received {
                Some(Ok(CloudOpenEvent::Progress { downloaded, total })) => {
                    let label = self.cloud_open_label.as_deref().unwrap_or("cloud RAW");
                    self.status = if total > 0 {
                        format!(
                            "Downloading {label} from AuRaw Cloud… {:.0}%",
                            downloaded as f64 * 100.0 / total as f64
                        )
                    } else {
                        format!(
                            "Downloading {label} from AuRaw Cloud… {:.1} MiB",
                            downloaded as f64 / (1024.0 * 1024.0)
                        )
                    };
                }
                Some(Ok(CloudOpenEvent::Finished(result))) => {
                    self.cloud_open_receiver = None;
                    self.cloud_open_label = None;
                    if let Ok(cached) = &result {
                        let sync_state = crate::cloud::cached_asset_sync_state(&cached.raw_path)
                            .map(|(_, state)| state)
                            .unwrap_or(crate::cloud::CloudSyncState::Synced);
                        for entry in &mut self.entries {
                            if matches!(
                                &entry.info.source,
                                LibrarySource::Cloud(asset) if asset.id == cached.asset_id
                            ) {
                                entry.info.cloud_downloaded = true;
                                entry.info.cloud_sync_state = sync_state;
                            }
                        }
                    }
                    return Some(result);
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.cloud_open_receiver = None;
                    self.cloud_open_label = None;
                    return Some(Err(
                        "The cloud RAW download stopped unexpectedly.".to_owned()
                    ));
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => return None,
            }
        }
    }

    pub(crate) fn cloud_upload_in_progress(&self) -> bool {
        self.cloud_upload_receiver.is_some()
    }

    pub(super) fn cloud_action_in_progress(&self) -> bool {
        self.cloud_action_receiver.is_some()
    }

    pub(super) fn image_paste_in_progress(&self) -> bool {
        self.image_paste_receiver.is_some()
    }

    pub(super) fn start_image_paste(&mut self, destination: ImagePasteDestination, context: &egui::Context) {
        if self.image_paste_receiver.is_some()
            || self.cloud_action_receiver.is_some()
            || self.cloud_upload_receiver.is_some()
            || self.cloud_open_receiver.is_some()
            || {
                #[cfg(not(target_os = "android"))]
                {
                    self.file_action_receiver.is_some()
                        || self.raw_import_receiver.is_some()
                        || self.folder_operation_receiver.is_some()
                }
                #[cfg(target_os = "android")]
                {
                    false
                }
            }
        {
            self.status = "Wait for the current library transfer to finish.".to_owned();
            return;
        }
        let Some(clipboard) = self.image_clipboard.clone() else {
            self.status = "Copy or cut one or more RAW files first.".to_owned();
            return;
        };
        #[cfg(not(target_os = "android"))]
        let destination = match destination {
            ImagePasteDestination::LocalFolder(folder) => {
                let Some(root) = self.root_folder.as_deref() else {
                    self.status = "Open a top-level local library folder first.".to_owned();
                    return;
                };
                match canonical_library_directory(root, &folder, false) {
                    Ok(folder) => ImagePasteDestination::LocalFolder(folder),
                    Err(error) => {
                        self.status = error;
                        return;
                    }
                }
            }
            destination => destination,
        };
        if matches!(&destination, ImagePasteDestination::CloudFolder(_))
            && self.cloud_config.normalized().is_err()
        {
            self.status = "Configure AuRaw Cloud before pasting RAW files there.".to_owned();
            return;
        }

        let count = clipboard.count();
        let config = self.cloud_config.clone();
        let cache_root = self.cloud_cache_root.clone();
        let allow_network = self.cloud_network_available();
        #[cfg(target_os = "android")]
        let android_app = self.android_app.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.image_paste_receiver = Some(receiver);
        self.status = format!(
            "{} {count} RAW{}…",
            if clipboard.mode == ImageClipboardMode::Copy {
                "Copying"
            } else {
                "Moving"
            },
            if count == 1 { "" } else { "s" }
        );
        let spawn = std::thread::Builder::new()
            .name("auraw-image-paste".to_owned())
            .spawn(move || {
                let completion = run_image_paste(
                    &config,
                    cache_root.as_deref(),
                    allow_network,
                    clipboard,
                    destination,
                    #[cfg(target_os = "android")]
                    &android_app,
                );
                let _ = sender.send(completion);
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.image_paste_receiver = None;
            self.status = format!("Could not start the image paste: {error}");
        }
    }

    pub(super) fn start_cloud_action(&mut self, request: CloudActionRequest, context: &egui::Context) {
        if self.cloud_action_receiver.is_some() {
            self.status = "Another cloud action is still running.".to_owned();
            return;
        }
        if self.image_paste_receiver.is_some() {
            self.status = "Wait for the current image paste to finish.".to_owned();
            return;
        }
        if self.cloud_upload_receiver.is_some() || self.cloud_open_receiver.is_some() {
            self.status = "Wait for the current cloud transfer to finish.".to_owned();
            return;
        }
        if self.view != LibraryView::Cloud {
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.event_receiver = None;
            self.request_sender = None;
            self.scanning = false;
            self.catalog_ready = false;
            self.view = LibraryView::Cloud;
            self.update_cloud_location();
            self.entries.clear();
            self.entry_indices.clear();
            self.clear_selection();
        }
        let reveal_parent = match &request {
            CloudActionRequest::CreateFolder { parent_id, .. }
            | CloudActionRequest::UpdateFolder { parent_id, .. } => Some(parent_id),
            CloudActionRequest::CopyFolder {
                destination_parent_id,
                ..
            } => Some(destination_parent_id),
            _ => None,
        };
        if let Some(parent_id) = reveal_parent {
            self.cloud_expanded_folders.insert(parent_id.clone());
        }
        let status = match &request {
            CloudActionRequest::CreateFolder { .. } => "Creating cloud folder…".to_owned(),
            CloudActionRequest::UpdateFolder { .. } => "Updating cloud folder…".to_owned(),
            CloudActionRequest::CopyFolder { .. } => "Copying cloud folder…".to_owned(),
            CloudActionRequest::DeleteFolder { .. } => "Deleting cloud folder…".to_owned(),
            CloudActionRequest::CopyAssets { assets, .. } => {
                format!("Copying {} cloud RAWs…", assets.len())
            }
            CloudActionRequest::RenameAsset { .. } => "Renaming cloud RAW…".to_owned(),
            CloudActionRequest::DeleteAssets { assets } => {
                format!("Deleting {} cloud RAWs…", assets.len())
            }
            CloudActionRequest::RestoreTrash { items } => {
                format!(
                    "Restoring {} Trash item{}…",
                    items.len(),
                    if items.len() == 1 { "" } else { "s" }
                )
            }
            CloudActionRequest::PermanentlyDeleteTrash { items } => format!(
                "Permanently deleting {} Trash item{}…",
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            ),
            CloudActionRequest::EmptyTrash => "Emptying AuRaw Cloud Trash…".to_owned(),
            CloudActionRequest::ResetAssets { assets } => {
                format!("Resetting {} cloud RAWs…", assets.len())
            }
            CloudActionRequest::PrepareAssets { assets, purpose } => format!(
                "Preparing {} cloud RAW{} for {}…",
                assets.len(),
                if assets.len() == 1 { "" } else { "s" },
                match purpose {
                    CloudPreparedPurpose::Export => "export",
                    CloudPreparedPurpose::CopyAdjustments => "copying adjustments",
                    CloudPreparedPurpose::PasteAdjustments => "pasting adjustments",
                }
            ),
        };
        let config = self.cloud_config.clone();
        let cache_root = self.cloud_cache_root.clone();
        let allow_network = self.cloud_network_available();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_action_receiver = Some(receiver);
        self.status = status;
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-action".to_owned())
            .spawn(move || {
                let completion =
                    run_cloud_action(&config, cache_root.as_deref(), allow_network, request);
                let _ = sender.send(completion);
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_action_receiver = None;
            self.status = format!("Could not start the cloud action: {error}");
        }
    }

    pub(super) fn poll_cloud_action(&mut self) -> Option<CloudActionCompletion> {
        let received = self
            .cloud_action_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match received {
            Some(Ok(completion)) => {
                self.cloud_action_receiver = None;
                Some(completion)
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.cloud_action_receiver = None;
                Some(CloudActionCompletion::Mutation {
                    result: Err("The cloud action stopped unexpectedly.".to_owned()),
                    clear_clipboard: false,
                })
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => None,
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn start_desktop_cloud_upload(
        &mut self,
        paths: Vec<PathBuf>,
        context: &egui::Context,
    ) {
        if self.image_paste_receiver.is_some() {
            self.status = "Wait for the current image paste to finish.".to_owned();
            return;
        }
        if self.cloud_upload_receiver.is_some() {
            self.status = "Wait for the current AuRaw Cloud upload to finish.".to_owned();
            return;
        }
        if paths.is_empty() {
            self.status = "No RAW files selected for cloud upload.".to_owned();
            return;
        }
        if let Err(error) = self.cloud_config.normalized() {
            self.status = error;
            return;
        }

        let selected = paths.len();
        let paths = paths
            .into_iter()
            .take(MAX_CLOUD_UPLOAD_FILES)
            .collect::<Vec<_>>();
        let total = paths.len();
        let config = self.cloud_config.clone();
        let folder_id = self.cloud_folder_id.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_upload_receiver = Some(receiver);
        self.cloud_upload_completion = None;
        self.status = format!(
            "Preparing {} RAW {} for AuRaw Cloud…",
            total,
            if total == 1 { "file" } else { "files" }
        );
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-upload".to_owned())
            .spawn(move || {
                let mut uploaded = 0usize;
                let mut failed = selected.saturating_sub(total);
                let mut errors = Vec::new();
                if selected > total {
                    errors.push(format!(
                        "Only the first {MAX_CLOUD_UPLOAD_FILES} selected RAW files were uploaded."
                    ));
                }
                for (index, path) in paths.into_iter().enumerate() {
                    let label = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| path.display().to_string());
                    let _ = sender.send(CloudUploadEvent::Progress {
                        position: index + 1,
                        total,
                        label: label.clone(),
                    });
                    repaint.request_repaint();
                    match crate::cloud::upload_asset_path_to_folder(&config, &path, &folder_id) {
                        Ok(_) => uploaded += 1,
                        Err(error) => {
                            failed += 1;
                            if errors.len() < 5 {
                                errors.push(format!("{label}: {error}"));
                            }
                        }
                    }
                }
                let _ = sender.send(CloudUploadEvent::Finished {
                    target: config,
                    uploaded,
                    failed,
                    errors,
                });
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_upload_receiver = None;
            self.status = format!("Could not start the AuRaw Cloud upload: {error}");
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn start_android_cloud_upload(
        &mut self,
        documents: Vec<crate::android::CloudUploadDocument>,
        selection_failed: usize,
        selection_errors: String,
        context: &egui::Context,
    ) {
        if self.image_paste_receiver.is_some() {
            self.status = "Wait for the current image paste to finish.".to_owned();
            return;
        }
        if self.cloud_upload_receiver.is_some() {
            self.status = "Wait for the current AuRaw Cloud upload to finish.".to_owned();
            return;
        }
        if documents.is_empty() {
            self.status = if selection_failed == 0 {
                "No RAW files selected for cloud upload.".to_owned()
            } else {
                format!("No RAW files could be selected for cloud upload. {selection_errors}")
            };
            return;
        }
        if let Err(error) = self.cloud_config.normalized() {
            self.status = error;
            return;
        }

        let selected = documents.len();
        let documents = documents
            .into_iter()
            .take(MAX_CLOUD_UPLOAD_FILES)
            .collect::<Vec<_>>();
        let total = documents.len();
        let config = self.cloud_config.clone();
        let folder_id = self.cloud_folder_id.clone();
        let android_app = self.android_app.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_upload_receiver = Some(receiver);
        self.cloud_upload_completion = None;
        self.status = format!(
            "Preparing {} RAW {} for AuRaw Cloud…",
            total,
            if total == 1 { "file" } else { "files" }
        );
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-upload".to_owned())
            .spawn(move || {
                let mut uploaded = 0usize;
                let mut failed = selection_failed + selected.saturating_sub(total);
                let mut errors = selection_errors
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .take(5)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if selected > total && errors.len() < 5 {
                    errors.push(format!(
                        "Only the first {MAX_CLOUD_UPLOAD_FILES} selected RAW files were uploaded."
                    ));
                }
                for (index, document) in documents.into_iter().enumerate() {
                    let label = document.display_name.clone();
                    let _ = sender.send(CloudUploadEvent::Progress {
                        position: index + 1,
                        total,
                        label: label.clone(),
                    });
                    repaint.request_repaint();
                    let result =
                        crate::android::open_document_for_cloud_upload(&android_app, &document.uri)
                            .and_then(|raw| {
                                crate::cloud::upload_asset_file_to_folder(
                                    &config,
                                    raw,
                                    &document.display_name,
                                    document.bytes,
                                    &folder_id,
                                )
                            });
                    match result {
                        Ok(_) => uploaded += 1,
                        Err(error) => {
                            failed += 1;
                            if errors.len() < 5 {
                                errors.push(format!("{label}: {error}"));
                            }
                        }
                    }
                }
                let _ = sender.send(CloudUploadEvent::Finished {
                    target: config,
                    uploaded,
                    failed,
                    errors,
                });
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_upload_receiver = None;
            self.status = format!("Could not start the AuRaw Cloud upload: {error}");
        }
    }

}
