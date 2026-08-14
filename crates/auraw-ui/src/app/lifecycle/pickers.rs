use super::*;

impl AurawApp {
    #[cfg(not(target_os = "android"))]
    pub fn open_file_dialog(&mut self, _frame: &eframe::Frame) {
        if self.desktop_picker_receiver.is_some() {
            return;
        }
        let extensions = crate::pipeline::SUPPORTED_RAW_EXTENSIONS
            .iter()
            .flat_map(|extension| [extension.to_string(), extension.to_ascii_uppercase()])
            .collect::<Vec<_>>();
        let initial_directory = self
            .current_path
            .as_deref()
            .and_then(selected_picker_directory)
            .or_else(|| self.library.folder().map(std::path::Path::to_path_buf));
        let mut dialog = rfd::AsyncFileDialog::new().add_filter("RAW and TIFF images", &extensions);
        if let Some(directory) = initial_directory {
            dialog = dialog.set_directory(directory);
        }
        self.desktop_picker_receiver = Some(spawn_ui_worker(&self.egui_ctx, move || {
            let path =
                pollster::block_on(dialog.pick_file()).map(|handle| handle.path().to_path_buf());
            crate::app::DesktopPickerEvent::RawFile(path)
        }));
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn open_cloud_upload_dialog(&mut self, _frame: &eframe::Frame) {
        if self.desktop_picker_receiver.is_some() || self.library.cloud_upload_in_progress() {
            return;
        }
        let extensions = crate::pipeline::SUPPORTED_RAW_EXTENSIONS
            .iter()
            .flat_map(|extension| [extension.to_string(), extension.to_ascii_uppercase()])
            .collect::<Vec<_>>();
        let initial_directory = self
            .library
            .folder()
            .map(std::path::Path::to_path_buf)
            .or_else(|| {
                self.current_path
                    .as_deref()
                    .and_then(selected_picker_directory)
            });
        let mut dialog = rfd::AsyncFileDialog::new().add_filter("RAW and TIFF images", &extensions);
        if let Some(directory) = initial_directory {
            dialog = dialog.set_directory(directory);
        }
        self.desktop_picker_receiver = Some(spawn_ui_worker(&self.egui_ctx, move || {
            let paths = pollster::block_on(dialog.pick_files()).map(|handles| {
                handles
                    .into_iter()
                    .map(|handle| handle.path().to_path_buf())
                    .collect()
            });
            crate::app::DesktopPickerEvent::CloudRawFiles(paths)
        }));
    }

    #[cfg(not(target_os = "android"))]
    pub fn open_library_folder_dialog(&mut self) {
        if self.desktop_picker_receiver.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new();
        if let Some(folder) = self.library.folder() {
            dialog = dialog.set_directory(folder);
        }
        self.desktop_picker_receiver = Some(spawn_ui_worker(&self.egui_ctx, move || {
            let folder =
                pollster::block_on(dialog.pick_folder()).map(|handle| handle.path().to_path_buf());
            crate::app::DesktopPickerEvent::LibraryFolder(folder)
        }));
    }

    #[cfg(target_os = "android")]
    pub fn open_file_dialog(&mut self, _frame: &eframe::Frame) {
        if self.android_foreground_task_active() {
            self.notice = Some(
                "Wait for the current foreground operation to finish before opening another RAW."
                    .to_owned(),
            );
            self.egui_ctx.request_repaint();
            return;
        }
        if self.picker_pending {
            return;
        }
        self.develop_loading_thumbnail.clear();
        match crate::android::open_raw_document(&self.android_app) {
            Ok(()) => {
                self.picker_pending = true;
                self.notice = None;
                self.status = "Choose one or more RAW or TIFF files…".to_owned();
            }
            Err(error) => self.notice = Some(error),
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn open_cloud_upload_dialog(&mut self, _frame: &eframe::Frame) {
        if self.android_foreground_task_active() {
            self.notice = Some(
                "Wait for the current foreground operation before selecting cloud uploads."
                    .to_owned(),
            );
            self.egui_ctx.request_repaint();
            return;
        }
        if self.picker_pending || self.library.cloud_upload_in_progress() {
            return;
        }
        match crate::android::open_cloud_raw_documents(&self.android_app) {
            Ok(()) => {
                self.picker_pending = true;
                self.notice = None;
                self.status = "Choose one or more RAW or TIFF files for AuRaw Cloud…".to_owned();
            }
            Err(error) => self.notice = Some(error),
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn poll_desktop_picker(&mut self, frame: &eframe::Frame) {
        let result = self
            .desktop_picker_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some(result) = result else {
            return;
        };
        self.desktop_picker_receiver = None;
        match result {
            crate::app::DesktopPickerEvent::RawFile(Some(path)) => self.open_path(path, frame),
            crate::app::DesktopPickerEvent::CloudRawFiles(Some(paths)) => {
                self.active_tab = AppTab::Library;
                self.library
                    .start_desktop_cloud_upload(paths, &self.egui_ctx);
            }
            crate::app::DesktopPickerEvent::LibraryFolder(Some(folder)) => {
                self.library.open_folder(folder, &self.egui_ctx);
                self.persist_performance_settings();
                self.active_tab = AppTab::Library;
            }
            crate::app::DesktopPickerEvent::CameraProfileFolder(Some(folder)) => {
                self.apply_camera_profile_folder(folder);
            }
            crate::app::DesktopPickerEvent::OnnxRuntime(Ok(Some((path, sha256)))) => {
                self.onnx_runtime_path = Some(path);
                self.onnx_runtime_sha256 = Some(sha256);
                self.notice = Some(
                    "ONNX Runtime selection and SHA-256 pin saved. Restart AuRaw before generating another subject mask."
                        .to_owned(),
                );
            }
            crate::app::DesktopPickerEvent::OnnxRuntime(Err(error)) => {
                self.notice = Some(error);
            }
            crate::app::DesktopPickerEvent::DisplayProfile(Some(path)) => {
                self.apply_display_profile_override(path);
            }
            _ => {}
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn poll_android_picker(&mut self, frame: &eframe::Frame) {
        while let Some(result) = crate::android::take_camera_profile_folder_result() {
            match result {
                crate::android::CameraProfileFolderResult::ImportStarted { label } => {
                    self.camera_profile_folder_importing_label = Some(label.clone());
                    self.status = format!("Importing DCP profiles from {label}…");
                    self.notice = None;
                    // The SAF picker has returned, but its tree is copied on a Java
                    // worker thread. Keep picker_pending set so a second picker cannot
                    // race the import; eframe_impl keeps polling while it is pending.
                }
                crate::android::CameraProfileFolderResult::Picked {
                    path,
                    label,
                    profiles,
                } => {
                    self.picker_pending = false;
                    self.camera_profile_folder_importing_label = None;
                    crate::pipeline::invalidate_dcp_profile_index();
                    let previous_folder = self.camera_profile_folder.replace(path);
                    self.camera_profile_folder_label = Some(label.clone());
                    self.camera_profile_auto_detect = false;
                    self.last_camera_profile = None;
                    self.raw_cache.clear();
                    if self.persist_performance_settings() {
                        if let Some(previous_folder) = previous_folder {
                            if self.camera_profile_folder.as_deref()
                                != Some(previous_folder.as_path())
                            {
                                if let Err(error) = crate::android::remove_camera_profile_mirror(
                                    &self.android_app,
                                    &previous_folder,
                                ) {
                                    log::warn!("{error}");
                                }
                            }
                        }
                    }
                    self.notice = Some(format!(
                        "Camera profile folder '{label}' imported with {profiles} DCP {}. Reopen the RAW to apply the new profile selection.",
                        if profiles == 1 { "profile" } else { "profiles" }
                    ));
                }
                crate::android::CameraProfileFolderResult::Cancelled => {
                    self.picker_pending = false;
                    self.camera_profile_folder_importing_label = None;
                    self.notice = Some("No camera profile folder selected.".to_owned());
                }
                crate::android::CameraProfileFolderResult::Failed(error) => {
                    self.picker_pending = false;
                    self.camera_profile_folder_importing_label = None;
                    self.notice = Some(format!("Could not import camera profiles: {error}"));
                }
            }
        }

        while let Some(result) = crate::android::take_picker_result() {
            self.picker_pending = false;
            match result {
                crate::android::PickerResult::Picked(document) => {
                    self.library.refresh(&self.egui_ctx);
                    let batch_owned_open = self.android_batch_load_pending;
                    let profile_reload_owned_open = self.pending_android_profile_reload.is_some();
                    let reset_reload_owned_open =
                        std::mem::take(&mut self.pending_android_library_reset_reload);
                    let library_refresh_owned_open = !batch_owned_open
                        && !profile_reload_owned_open
                        && !reset_reload_owned_open
                        && self.library_ai_mask_refresh.is_some();
                    let keep_library_for_profile_reload =
                        profile_reload_owned_open && self.active_tab == AppTab::Library;
                    let keep_library_for_reset =
                        reset_reload_owned_open && self.active_tab == AppTab::Library;
                    self.active_tab = if batch_owned_open
                        || library_refresh_owned_open
                        || keep_library_for_profile_reload
                        || keep_library_for_reset
                    {
                        AppTab::Library
                    } else {
                        AppTab::Develop
                    };
                    let sidecar_target = crate::sidecar::SidecarTarget::Android {
                        raw_uri: document.library_uri,
                        display_name: document.display_name.clone(),
                    };
                    if let Some((selection, edit_override)) =
                        self.pending_android_profile_reload.take()
                    {
                        self.open_path_labeled_with_options(
                            document.path,
                            document.display_name,
                            document.delete_after_decode,
                            sidecar_target,
                            frame,
                            Some(selection),
                            Some(edit_override),
                            document.raw_fd_guard,
                        );
                    } else {
                        self.open_path_labeled(
                            document.path,
                            document.display_name,
                            document.delete_after_decode,
                            sidecar_target,
                            frame,
                            document.raw_fd_guard,
                        );
                    }

                    // `open_path_labeled*` reports setup failures synchronously by
                    // leaving `load_receiver` empty. Route that failure back to the
                    // internal owner immediately; otherwise Android batch export or
                    // library AI refresh would wait forever for a load event that can
                    // never arrive.
                    if self.load_receiver.is_none() {
                        let error = self.notice.clone().unwrap_or_else(|| {
                            "The RAW decode worker could not be started.".to_owned()
                        });
                        if batch_owned_open {
                            self.android_batch_load_pending = false;
                            self.complete_android_library_batch_export_item(Err(error));
                        } else if library_refresh_owned_open {
                            self.complete_android_library_ai_mask_open_failure(error, frame);
                        }
                    }
                }
                crate::android::PickerResult::CloudSelected {
                    documents,
                    failed,
                    errors,
                } => {
                    self.develop_loading_thumbnail.clear();
                    self.pending_android_profile_reload = None;
                    self.pending_android_library_reset_reload = false;
                    self.active_tab = AppTab::Library;
                    self.library.start_android_cloud_upload(
                        documents,
                        failed,
                        errors,
                        &self.egui_ctx,
                    );
                }
                crate::android::PickerResult::BatchImported {
                    imported,
                    failed,
                    errors,
                } => {
                    self.develop_loading_thumbnail.clear();
                    self.pending_android_library_reset_reload = false;
                    self.active_tab = AppTab::Library;
                    self.library.refresh(&self.egui_ctx);
                    self.status = match (imported, failed) {
                        (0, 0) => "No RAW files were imported.".to_owned(),
                        (_, 0) => format!(
                            "Imported {imported} RAW {}.",
                            if imported == 1 { "file" } else { "files" }
                        ),
                        _ => format!(
                            "Imported {imported} RAW {}; {failed} failed.",
                            if imported == 1 { "file" } else { "files" }
                        ),
                    };
                    self.notice = if failed > 0 {
                        Some(if errors.is_empty() {
                            format!("{failed} selected RAW imports failed.")
                        } else {
                            format!("Some RAW files could not be imported:\n{errors}")
                        })
                    } else {
                        None
                    };
                }
                crate::android::PickerResult::Cancelled => {
                    self.develop_loading_thumbnail.clear();
                    self.pending_android_profile_reload = None;
                    let was_reset_reload =
                        std::mem::take(&mut self.pending_android_library_reset_reload);
                    if self.android_batch_load_pending {
                        self.android_batch_load_pending = false;
                        self.complete_android_library_batch_export_item(Err(
                            "RAW open was canceled".to_owned(),
                        ));
                    } else if self.library_ai_mask_refresh.is_some() {
                        self.complete_android_library_ai_mask_open_failure(
                            "RAW open was canceled".to_owned(),
                            frame,
                        );
                    } else if was_reset_reload {
                        self.notice = Some(
                            "The RAW could not be reloaded after resetting adjustments. Reopen it from the Library before continuing in Develop."
                                .to_owned(),
                        );
                    } else {
                        self.notice = Some("No RAW files selected.".to_owned());
                    }
                }
                crate::android::PickerResult::Failed(error) => {
                    self.develop_loading_thumbnail.clear();
                    let was_profile_reload = self.pending_android_profile_reload.take().is_some();
                    let was_reset_reload =
                        std::mem::take(&mut self.pending_android_library_reset_reload);
                    if self.android_batch_load_pending && !was_profile_reload && !was_reset_reload {
                        self.android_batch_load_pending = false;
                        self.complete_android_library_batch_export_item(Err(error));
                    } else if self.library_ai_mask_refresh.is_some()
                        && !was_profile_reload
                        && !was_reset_reload
                    {
                        self.complete_android_library_ai_mask_open_failure(error, frame);
                    } else {
                        self.notice = Some(if was_profile_reload {
                            format!("Could not reload RAW for camera profile: {error}")
                        } else if was_reset_reload {
                            format!("Could not reload RAW after resetting adjustments: {error}")
                        } else {
                            format!("Could not import the selected file: {error}")
                        });
                    }
                }
            }
        }
    }
}
