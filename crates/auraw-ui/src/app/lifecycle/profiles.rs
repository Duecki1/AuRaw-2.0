use super::*;

impl AurawApp {
    #[cfg(not(target_os = "android"))]
    pub(crate) fn choose_camera_profile_folder(&mut self) {
        if self.desktop_picker_receiver.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new();
        if let Some(folder) = &self.camera_profile_folder {
            dialog = dialog.set_directory(folder);
        }
        self.desktop_picker_receiver = Some(spawn_ui_worker(&self.egui_ctx, move || {
            let folder =
                pollster::block_on(dialog.pick_folder()).map(|handle| handle.path().to_path_buf());
            crate::app::DesktopPickerEvent::CameraProfileFolder(folder)
        }));
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn apply_camera_profile_folder(&mut self, folder: PathBuf) {
        crate::pipeline::invalidate_dcp_profile_index();
        self.camera_profile_folder_label = folder
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        self.camera_profile_folder = Some(folder);
        self.camera_profile_auto_detect = false;
        self.last_camera_profile = None;
        self.raw_cache.clear();
        self.persist_performance_settings();
        self.notice = Some(
            "Camera profile folder updated. Reopen the RAW to apply the new profile selection."
                .to_owned(),
        );
    }

    #[cfg(target_os = "android")]
    pub(crate) fn choose_camera_profile_folder(&mut self) {
        if self.picker_pending {
            self.notice = Some("Finish the current Android file picker first.".to_owned());
            return;
        }
        match crate::android::open_camera_profile_folder(&self.android_app) {
            Ok(()) => {
                self.picker_pending = true;
                self.camera_profile_folder_importing_label = None;
                self.notice = None;
                self.status = "Choose a CameraProfiles folder…".to_owned();
            }
            Err(error) => self.notice = Some(error),
        }
    }

    pub(crate) fn clear_camera_profile_folder(&mut self) {
        let previous_folder = self.camera_profile_folder.take();
        if previous_folder.is_some() || self.camera_profile_auto_detect {
            crate::pipeline::invalidate_dcp_profile_index();
            self.camera_profile_folder_label = None;
            self.camera_profile_auto_detect = false;
            self.last_camera_profile = None;
            self.raw_cache.clear();
            #[cfg(target_os = "android")]
            if let Err(error) =
                crate::android::clear_camera_profile_folder_picker_location(&self.android_app)
            {
                log::warn!("{error}");
            }
            if self.persist_performance_settings() {
                #[cfg(target_os = "android")]
                if let Some(previous_folder) = previous_folder {
                    if let Err(error) = crate::android::remove_camera_profile_mirror(
                        &self.android_app,
                        &previous_folder,
                    ) {
                        log::warn!("{error}");
                    }
                }
            }
            self.notice = Some(
                "Camera profile folder cleared. Reopen the RAW to apply the new profile selection."
                    .to_owned(),
            );
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn auto_detect_camera_profile_folder(&mut self) {
        crate::pipeline::invalidate_dcp_profile_index();
        self.camera_profile_auto_detect = true;
        match crate::performance_settings::detected_adobe_camera_profile_folder() {
            Some(folder) => {
                self.camera_profile_folder = Some(folder.clone());
                self.camera_profile_folder_label =
                    Some("Adobe Camera Raw (auto-detected)".to_owned());
                self.last_camera_profile = None;
                self.raw_cache.clear();
                self.persist_performance_settings();
                self.notice = Some(format!(
                    "Using Adobe Camera Raw camera profiles from {}. Reopen the RAW to apply them.",
                    folder.display()
                ));
            }
            None => {
                self.camera_profile_folder = None;
                self.camera_profile_folder_label = None;
                self.last_camera_profile = None;
                self.raw_cache.clear();
                self.persist_performance_settings();
                self.notice = Some(
                    "No Adobe Camera Raw CameraProfiles folder was found in the standard location."
                        .to_owned(),
                );
            }
        }
    }

    pub(crate) fn set_camera_profile_mode(&mut self, mode: CameraProfileMode) {
        if self.camera_profile_mode == mode {
            return;
        }
        self.camera_profile_mode = mode;
        self.raw_cache.clear();
        self.persist_performance_settings();
        self.notice = Some(
            "RAW color profile mode changed. Reopen the RAW to apply the new profile selection."
                .to_owned(),
        );
    }

    pub(crate) fn select_camera_profile_for_current(
        &mut self,
        selection: Option<PathBuf>,
        frame: &eframe::Frame,
    ) {
        #[cfg(target_os = "android")]
        let _ = frame;
        #[cfg(target_os = "android")]
        if self.android_foreground_task_active() {
            self.notice = Some(
                "Wait for the current foreground operation to finish before changing camera profile."
                    .to_owned(),
            );
            self.egui_ctx.request_repaint();
            return;
        }
        if self.load_receiver.is_some() {
            self.notice = Some(
                "Wait for the current RAW load to finish before changing camera profile."
                    .to_owned(),
            );
            return;
        }
        if self.selected_camera_profile == selection {
            return;
        }
        let Some(sidecar_target) = self.sidecar_target.clone() else {
            self.notice = Some("Open a RAW before choosing a camera profile.".to_owned());
            return;
        };
        if let Some(selected) = selection.as_ref() {
            let embedded_matrix = self
                .camera_profile_folder
                .as_ref()
                .is_some_and(|root| selected == root);
            let is_available = self.loaded_raw.as_ref().is_some_and(|raw| {
                raw.available_camera_profiles
                    .iter()
                    .any(|candidate| candidate.path == *selected)
            });
            if !embedded_matrix && !is_available {
                self.notice = Some(
                    "That DCP is no longer available for the current camera. Refresh the profile folder and reopen the RAW."
                        .to_owned(),
                );
                return;
            }
        }

        self.selected_camera_profile = selection.clone();
        // Only an explicit dropdown change updates the sticky default. Merely
        // opening an edited photo never mutates this preference.
        self.last_camera_profile = selection
            .as_ref()
            .zip(self.camera_profile_folder.as_ref())
            .and_then(|(selected, root)| selected.strip_prefix(root).ok())
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf);
        self.persist_performance_settings();
        self.queue_explicit_sidecar_save();
        let edit_override = self.capture_sidecar_edit_state();

        #[cfg(not(target_os = "android"))]
        {
            let crate::sidecar::SidecarTarget::Desktop { raw_path } = sidecar_target;
            let label = self
                .current_label
                .clone()
                .unwrap_or_else(|| raw_path.display().to_string());
            let sidecar_target = crate::sidecar::SidecarTarget::Desktop {
                raw_path: raw_path.clone(),
            };
            self.open_path_labeled_with_options(
                raw_path,
                label,
                false,
                sidecar_target,
                frame,
                Some(selection),
                Some(edit_override),
                None,
            );
        }

        #[cfg(target_os = "android")]
        {
            let (raw_uri, display_name) = match sidecar_target {
                crate::sidecar::SidecarTarget::Android {
                    raw_uri,
                    display_name,
                } => (raw_uri, display_name),
                crate::sidecar::SidecarTarget::Desktop { .. } => {
                    self.notice = Some(
                        "The current Android RAW does not have a reloadable library target."
                            .to_owned(),
                    );
                    return;
                }
            };
            match crate::android::open_library_document(&self.android_app, &raw_uri, &display_name)
            {
                Ok(()) => {
                    self.pending_android_profile_reload = Some((selection, edit_override));
                    self.picker_pending = true;
                    self.notice = None;
                    self.status = format!("Applying camera profile to {display_name}…");
                }
                Err(error) => {
                    self.notice = Some(format!("Could not reload RAW for camera profile: {error}"));
                }
            }
        }
    }
}
