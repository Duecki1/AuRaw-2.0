impl AurawApp {
    pub(crate) fn has_copied_adjustments(&self) -> bool {
        self.adjustment_clipboard.is_some()
    }

    pub(crate) fn copied_adjustments_source_label(&self) -> Option<&str> {
        self.adjustment_clipboard
            .as_ref()
            .map(|clipboard| clipboard.source_label.as_str())
    }

    fn install_adjustment_clipboard(&mut self, edits: SidecarEditState, source_label: String) {
        self.adjustment_clipboard = Some(LibraryAdjustmentClipboard {
            edits,
            settings: self.adjustment_copy_settings,
            source_label,
        });
    }

    fn apply_adjustment_clipboard_to_current(
        &mut self,
        clipboard: &LibraryAdjustmentClipboard,
        mode: AdjustmentPasteMode,
        frame: &eframe::Frame,
    ) -> Result<bool, String> {
        if self.loaded_raw.is_none() || self.sidecar_target.is_none() {
            return Err("The destination image is not loaded.".to_owned());
        }

        self.finish_mask_geometry_interaction();
        self.commit_edit_history_now();
        let mut merged = self.capture_sidecar_edit_state();
        crate::sidecar::apply_copied_adjustments_with_mode(
            &mut merged,
            &clipboard.edits,
            clipboard.settings,
            mode,
        );

        let previous_camera_profile = self.selected_camera_profile.clone();
        let pasted_camera_profile = merged.camera_profile.as_ref().and_then(|relative| {
            self.camera_profile_folder
                .as_ref()
                .map(|root| root.join(relative))
        });

        let replacing = mode == AdjustmentPasteMode::Replace;
        let adjustments_changed = clipboard.settings.adjustments || replacing;
        let geometry_changed = clipboard.settings.geometry || replacing;
        let camera_profile_category_changed = clipboard.settings.camera_profile || replacing;
        let pipeline_adjustments_changed =
            adjustments_changed || geometry_changed || camera_profile_category_changed;
        let masks_changed = clipboard.settings.masks || clipboard.settings.ai_masks || replacing;
        let inpainting_changed = clipboard.settings.inpainting || replacing;
        let inpainting_content_changed = inpainting_changed
            && self.inpaint_strokes.as_slice() != merged.inpainting.as_slice();
        let lens_changed = (clipboard.settings.lens_correction || replacing)
            && (self.lens_correction.enabled != merged.lens.enabled
                || self.lens_correction.selected_maker != merged.lens.maker
                || self.lens_correction.selected_model != merged.lens.model);

        if adjustments_changed {
            self.exposure = merged.exposure;
            self.exposure.sanitize_tone_curves();
        }
        if geometry_changed {
            self.geometry = merged.geometry.sanitized();
            self.crop_constraint_reference = None;
            self.note_geometry_changed();
        }
        if camera_profile_category_changed {
            self.selected_camera_profile = pasted_camera_profile.clone();
        }
        if pipeline_adjustments_changed {
            self.note_edit_changed();
            self.ai_masks_need_update |= merged.ai_masks_need_update;
        }

        if masks_changed {
            self.masks = Arc::unwrap_or_clone(merged.masks);
            self.ai_masks_need_update = merged.ai_masks_need_update;
            self.rehydrate_restored_mask_state();
            // Rehydration validates which generated masks exist; retain the
            // explicit cross-image stale marker for pasted content-aware masks.
            self.ai_masks_need_update |= merged.ai_masks_need_update;
            self.mark_all_mask_layers_dirty();
        }

        if inpainting_changed {
            self.inpaint_strokes = Arc::unwrap_or_clone(merged.inpainting);
            self.rebuild_inpaint_layer();
            self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
            self.note_inpainting_edit_changed();
            if inpainting_content_changed {
                self.note_inpainting_changed_for_ai_masks();
            }
            self.ai_masks_need_update |= merged.ai_masks_need_update;
            self.queue_preview_processing(crate::pipeline::ProcessingStage::Tone);
        }

        if clipboard.settings.lens_correction || replacing {
            self.lens_correction.enabled = merged.lens.enabled;
            self.lens_correction.selected_maker = merged.lens.maker;
            self.lens_correction.selected_model = merged.lens.model;
            if lens_changed {
                self.note_lens_correction_changed_for_masks();
                self.ai_masks_need_update |= merged.ai_masks_need_update;
                self.mark_lens_correction_dirty();
            }
        }

        if pipeline_adjustments_changed && !lens_changed {
            self.mark_pipeline_dirty();
        }

        self.commit_edit_history_now();
        self.queue_explicit_sidecar_save();
        let needs_ai_refresh = self.ai_masks_need_update();
        if camera_profile_category_changed && previous_camera_profile != pasted_camera_profile {
            let edit_override = self.capture_sidecar_edit_state();
            self.reload_current_after_adjustment_paste(
                frame,
                pasted_camera_profile,
                edit_override,
            );
        }
        Ok(needs_ai_refresh)
    }

    fn reload_current_after_adjustment_paste(
        &mut self,
        frame: &eframe::Frame,
        profile_selection: Option<PathBuf>,
        edit_override: SidecarEditState,
    ) {
        let Some(sidecar_target) = self.sidecar_target.clone() else {
            return;
        };

        #[cfg(not(target_os = "android"))]
        {
            let crate::sidecar::SidecarTarget::Desktop { raw_path } = sidecar_target;
            let label = self
                .current_label
                .clone()
                .unwrap_or_else(|| raw_path.display().to_string());
            let target = crate::sidecar::SidecarTarget::Desktop {
                raw_path: raw_path.clone(),
            };
            self.open_path_labeled_with_options(
                raw_path,
                label,
                false,
                target,
                frame,
                Some(profile_selection),
                Some(edit_override),
                None,
            );
            self.active_tab = AppTab::Library;
        }

        #[cfg(target_os = "android")]
        {
            let _ = frame;
            let crate::sidecar::SidecarTarget::Android {
                raw_uri,
                display_name,
            } = sidecar_target
            else {
                return;
            };
            match crate::android::open_library_document(
                &self.android_app,
                &raw_uri,
                &display_name,
            ) {
                Ok(()) => {
                    self.pending_android_profile_reload =
                        Some((profile_selection, edit_override));
                    self.picker_pending = true;
                    self.active_tab = AppTab::Library;
                }
                Err(error) => {
                    self.notice = Some(format!(
                        "Adjustments were pasted, but the camera profile could not be reloaded: {error}"
                    ));
                }
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    fn desktop_library_edit_state(
        &mut self,
        raw_path: &std::path::Path,
    ) -> Result<SidecarEditState, String> {
        let persisted = crate::sidecar::load_desktop(raw_path)
            .map_err(|error| error.to_string())?
            .map(|loaded| loaded.edits);
        if self.current_path.as_deref() == Some(raw_path) && self.loaded_raw.is_some() {
            self.finish_mask_geometry_interaction();
            self.commit_edit_history_now();
            if persisted.is_none() && !self.can_undo_edit() {
                return Ok(crate::sidecar::default_edit_state());
            }
            return Ok(self.capture_sidecar_edit_state());
        }
        Ok(persisted.unwrap_or_else(crate::sidecar::default_edit_state))
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn library_adjustment_edit_count_paths(
        &mut self,
        raw_paths: &[PathBuf],
    ) -> (usize, Vec<String>) {
        let mut edited = 0usize;
        let mut failures = Vec::new();
        for raw_path in raw_paths {
            match self.desktop_library_edit_state(raw_path) {
                Ok(state) => {
                    if crate::sidecar::edit_state_has_adjustments(&state) {
                        edited += 1;
                    }
                }
                Err(error) => failures.push(format!("{}: {error}", raw_path.display())),
            }
        }
        (edited, failures)
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn copy_library_adjustments_from_path(
        &mut self,
        raw_path: &std::path::Path,
    ) -> Result<(), String> {
        let edits = self.desktop_library_edit_state(raw_path)?;
        let label = raw_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw_path.display().to_string());
        self.install_adjustment_clipboard(edits, label);
        Ok(())
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn paste_library_adjustments_to_paths(
        &mut self,
        raw_paths: &[PathBuf],
        mode: AdjustmentPasteMode,
        frame: &eframe::Frame,
    ) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<String>) {
        let Some(clipboard) = self.adjustment_clipboard.clone() else {
            return (
                Vec::new(),
                Vec::new(),
                vec!["Copy adjustments from an image first.".to_owned()],
            );
        };
        let mut completed = Vec::new();
        let mut ai_refresh = Vec::new();
        let mut failures = Vec::new();

        for raw_path in raw_paths {
            let result = if self.current_path.as_deref() == Some(raw_path.as_path())
                && self.loaded_raw.is_some()
            {
                self.apply_adjustment_clipboard_to_current(&clipboard, mode, frame)
            } else {
                (|| {
                    let mut destination = crate::sidecar::load_desktop(raw_path)
                        .map_err(|error| error.to_string())?
                        .map(|loaded| loaded.edits)
                        .unwrap_or_else(crate::sidecar::default_edit_state);
                    crate::sidecar::apply_copied_adjustments_with_mode(
                        &mut destination,
                        &clipboard.edits,
                        clipboard.settings,
                        mode,
                    );
                    let needs_ai_refresh = destination.ai_masks_need_update;
                    crate::sidecar::save_desktop(raw_path, destination)
                        .map_err(|error| error.to_string())?;
                    crate::sidecar::invalidate_developed_thumbnail_cache(raw_path)?;
                    Ok(needs_ai_refresh)
                })()
            };

            match result {
                Ok(needs_ai_refresh) => {
                    completed.push(raw_path.clone());
                    if needs_ai_refresh {
                        ai_refresh.push(raw_path.clone());
                    }
                }
                Err(error) => failures.push(format!("{}: {error}", raw_path.display())),
            }
        }
        (completed, ai_refresh, failures)
    }

    #[cfg(target_os = "android")]
    fn current_android_library_target_matches(&self, raw_uri: &str, display_name: &str) -> bool {
        matches!(
            self.sidecar_target.as_ref(),
            Some(crate::sidecar::SidecarTarget::Android {
                raw_uri: current_uri,
                display_name: current_name,
            }) if current_uri == raw_uri && current_name == display_name
        ) && self.loaded_raw.is_some()
    }

    #[cfg(target_os = "android")]
    fn android_library_edit_state(
        &mut self,
        raw_uri: &str,
        display_name: &str,
    ) -> Result<SidecarEditState, String> {
        let persisted = crate::sidecar::load_android(&self.android_app, raw_uri, display_name)
            .map_err(|error| error.to_string())?
            .map(|loaded| loaded.edits);
        if self.current_android_library_target_matches(raw_uri, display_name) {
            self.finish_mask_geometry_interaction();
            self.commit_edit_history_now();
            if persisted.is_none() && !self.can_undo_edit() {
                return Ok(crate::sidecar::default_edit_state());
            }
            return Ok(self.capture_sidecar_edit_state());
        }
        Ok(persisted.unwrap_or_else(crate::sidecar::default_edit_state))
    }

    #[cfg(target_os = "android")]
    pub(crate) fn library_adjustment_edit_count_android(
        &mut self,
        targets: &[(String, String)],
    ) -> (usize, Vec<String>) {
        let mut edited = 0usize;
        let mut failures = Vec::new();
        for (raw_uri, display_name) in targets {
            match self.android_library_edit_state(raw_uri, display_name) {
                Ok(state) => {
                    if crate::sidecar::edit_state_has_adjustments(&state) {
                        edited += 1;
                    }
                }
                Err(error) => failures.push(format!("{display_name}: {error}")),
            }
        }
        (edited, failures)
    }

    #[cfg(target_os = "android")]
    pub(crate) fn copy_library_adjustments_from_android(
        &mut self,
        raw_uri: &str,
        display_name: &str,
    ) -> Result<(), String> {
        let edits = self.android_library_edit_state(raw_uri, display_name)?;
        self.install_adjustment_clipboard(edits, display_name.to_owned());
        Ok(())
    }

    #[cfg(target_os = "android")]
    pub(crate) fn paste_library_adjustments_to_android(
        &mut self,
        targets: &[(String, String)],
        mode: AdjustmentPasteMode,
        frame: &eframe::Frame,
    ) -> (Vec<(String, String)>, Vec<(String, String)>, Vec<String>) {
        let Some(clipboard) = self.adjustment_clipboard.clone() else {
            return (
                Vec::new(),
                Vec::new(),
                vec!["Copy adjustments from an image first.".to_owned()],
            );
        };
        let mut completed = Vec::new();
        let mut ai_refresh = Vec::new();
        let mut failures = Vec::new();

        for (raw_uri, display_name) in targets {
            let result = if self.current_android_library_target_matches(raw_uri, display_name) {
                self.apply_adjustment_clipboard_to_current(&clipboard, mode, frame)
            } else {
                (|| {
                    let mut destination = crate::sidecar::load_android(
                        &self.android_app,
                        raw_uri,
                        display_name,
                    )
                    .map_err(|error| error.to_string())?
                    .map(|loaded| loaded.edits)
                    .unwrap_or_else(crate::sidecar::default_edit_state);
                    crate::sidecar::apply_copied_adjustments_with_mode(
                        &mut destination,
                        &clipboard.edits,
                        clipboard.settings,
                        mode,
                    );
                    let needs_ai_refresh = destination.ai_masks_need_update;
                    crate::sidecar::save_android(
                        &self.android_app,
                        raw_uri,
                        display_name,
                        destination,
                    )
                    .map_err(|error| error.to_string())?;
                    Ok(needs_ai_refresh)
                })()
            };

            match result {
                Ok(needs_ai_refresh) => {
                    let target = (raw_uri.clone(), display_name.clone());
                    completed.push(target.clone());
                    if needs_ai_refresh {
                        ai_refresh.push(target);
                    }
                }
                Err(error) => failures.push(format!("{display_name}: {error}")),
            }
        }
        (completed, ai_refresh, failures)
    }
}

impl AurawApp {
    pub(crate) fn can_start_library_ai_mask_refresh(&self) -> bool {
        let ready = self.library_ai_mask_refresh.is_none()
            && self.library_batch_export.is_none()
            && self.export_receiver.is_none()
            && self.load_receiver.is_none()
            && !self.export_publish_pending;
        #[cfg(not(target_os = "android"))]
        {
            ready
        }
        #[cfg(target_os = "android")]
        {
            ready && !self.picker_pending
        }
    }

    pub(crate) fn library_ai_mask_refresh_status(
        &self,
    ) -> Option<(usize, usize, usize, Option<String>)> {
        self.library_ai_mask_refresh.as_ref().map(|state| {
            let current = state.current.as_ref().map(|job| {
                #[cfg(not(target_os = "android"))]
                {
                    job.source
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| job.source.display().to_string())
                }
                #[cfg(target_os = "android")]
                {
                    job.display_name.clone()
                }
            });
            (
                state.completed,
                state.total,
                state.failures.len(),
                current,
            )
        })
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn start_library_ai_mask_refresh_paths(
        &mut self,
        paths: Vec<PathBuf>,
        frame: &eframe::Frame,
    ) {
        if paths.is_empty() {
            return;
        }
        if !self.can_start_library_ai_mask_refresh() {
            self.notice = Some(
                "Wait for the current library operation to finish before refreshing AI masks."
                    .to_owned(),
            );
            return;
        }
        let total = paths.len();
        self.library_ai_mask_refresh = Some(LibraryAiMaskRefreshState {
            pending: paths
                .into_iter()
                .map(|source| LibraryAiMaskRefreshJob { source })
                .collect(),
            current: None,
            phase: LibraryAiMaskRefreshPhase::Loading,
            total,
            completed: 0,
            failures: Vec::new(),
        });
        self.start_next_library_ai_mask_refresh(frame);
    }

    #[cfg(target_os = "android")]
    pub(crate) fn start_library_ai_mask_refresh_android(
        &mut self,
        targets: Vec<(String, String)>,
        frame: &eframe::Frame,
    ) {
        if targets.is_empty() {
            return;
        }
        if !self.can_start_library_ai_mask_refresh() {
            self.notice = Some(
                "Wait for the current library operation to finish before refreshing AI masks."
                    .to_owned(),
            );
            return;
        }
        let total = targets.len();
        self.library_ai_mask_refresh = Some(LibraryAiMaskRefreshState {
            pending: targets
                .into_iter()
                .map(|(uri, display_name)| LibraryAiMaskRefreshJob { uri, display_name })
                .collect(),
            current: None,
            phase: LibraryAiMaskRefreshPhase::Loading,
            total,
            completed: 0,
            failures: Vec::new(),
        });
        self.start_next_library_ai_mask_refresh(frame);
    }

    #[cfg(not(target_os = "android"))]
    fn start_next_library_ai_mask_refresh(&mut self, frame: &eframe::Frame) {
        loop {
            let next = {
                let Some(state) = self.library_ai_mask_refresh.as_mut() else {
                    return;
                };
                if state.current.is_some() {
                    return;
                }
                state.pending.pop_front().map(|job| {
                    state.current = Some(job.clone());
                    state.phase = LibraryAiMaskRefreshPhase::Loading;
                    job
                })
            };

            let Some(job) = next else {
                self.finish_library_ai_mask_refresh();
                return;
            };

            self.open_path(job.source.clone(), frame);
            self.active_tab = AppTab::Library;
            if self.load_receiver.is_some() {
                return;
            }

            if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                state
                    .failures
                    .push(format!("{}: could not start RAW load", job.source.display()));
                state.completed += 1;
                state.current = None;
            }
        }
    }

    #[cfg(target_os = "android")]
    fn start_next_library_ai_mask_refresh(&mut self, _frame: &eframe::Frame) {
        loop {
            let next = {
                let Some(state) = self.library_ai_mask_refresh.as_mut() else {
                    return;
                };
                if state.current.is_some() {
                    return;
                }
                state.pending.pop_front().map(|job| {
                    state.current = Some(job.clone());
                    state.phase = LibraryAiMaskRefreshPhase::Loading;
                    job
                })
            };

            let Some(job) = next else {
                self.finish_library_ai_mask_refresh();
                return;
            };

            match crate::android::open_library_document(
                &self.android_app,
                &job.uri,
                &job.display_name,
            ) {
                Ok(()) => {
                    self.picker_pending = true;
                    self.notice = None;
                    self.status = format!("Opening {}…", job.display_name);
                    self.active_tab = AppTab::Library;
                    return;
                }
                Err(error) => {
                    if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                        state
                            .failures
                            .push(format!("{}: {error}", job.display_name));
                        state.completed += 1;
                        state.current = None;
                    }
                }
            }
        }
    }

    pub(super) fn on_library_ai_mask_refresh_load_finished(
        &mut self,
        success: bool,
        frame: &eframe::Frame,
    ) {
        let Some(state) = self.library_ai_mask_refresh.as_ref() else {
            return;
        };
        let Some(current) = state.current.as_ref() else {
            return;
        };

        if !success {
            #[cfg(not(target_os = "android"))]
            let label = current.source.display().to_string();
            #[cfg(target_os = "android")]
            let label = current.display_name.clone();
            if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                state.failures.push(format!("{label}: RAW load failed"));
                state.completed += 1;
                state.current = None;
            }
            self.start_next_library_ai_mask_refresh(frame);
            return;
        }

        if let Some(state) = self.library_ai_mask_refresh.as_mut() {
            state.phase = LibraryAiMaskRefreshPhase::Updating;
        }
        self.request_update_all_ai_masks(frame);
        self.active_tab = AppTab::Library;
    }

    #[cfg(target_os = "android")]
    pub(super) fn complete_android_library_ai_mask_open_failure(
        &mut self,
        error: String,
        frame: &eframe::Frame,
    ) {
        let label = self
            .library_ai_mask_refresh
            .as_ref()
            .and_then(|state| state.current.as_ref())
            .map(|job| job.display_name.clone())
            .unwrap_or_else(|| "image".to_owned());
        if let Some(state) = self.library_ai_mask_refresh.as_mut() {
            state.failures.push(format!("{label}: {error}"));
            state.completed += 1;
            state.current = None;
        }
        self.start_next_library_ai_mask_refresh(frame);
    }

    pub(crate) fn poll_library_ai_mask_refresh(&mut self, frame: &eframe::Frame) {
        let ready = self
            .library_ai_mask_refresh
            .as_ref()
            .is_some_and(|state| {
                state.current.is_some()
                    && state.phase == LibraryAiMaskRefreshPhase::Updating
                    && !self.ai_mask_update_busy()
            });
        if !ready {
            return;
        }

        let failed = self.ai_masks_need_update();
        let label = self
            .library_ai_mask_refresh
            .as_ref()
            .and_then(|state| state.current.as_ref())
            .map(|job| {
                #[cfg(not(target_os = "android"))]
                {
                    job.source.display().to_string()
                }
                #[cfg(target_os = "android")]
                {
                    job.display_name.clone()
                }
            })
            .unwrap_or_else(|| "image".to_owned());

        if failed {
            let reason = self
                .notice
                .clone()
                .unwrap_or_else(|| "AI masks still need an update".to_owned());
            if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                state.failures.push(format!("{label}: {reason}"));
            }
        } else {
            self.commit_edit_history_now();
            self.queue_explicit_sidecar_save();
        }

        if let Some(state) = self.library_ai_mask_refresh.as_mut() {
            state.completed += 1;
            state.current = None;
        }
        self.start_next_library_ai_mask_refresh(frame);
    }

    fn finish_library_ai_mask_refresh(&mut self) {
        let Some(state) = self.library_ai_mask_refresh.take() else {
            return;
        };
        let succeeded = state.completed.saturating_sub(state.failures.len());
        self.active_tab = AppTab::Library;
        self.library.refresh(&self.egui_ctx);
        self.notice = if state.failures.is_empty() {
            Some(format!(
                "Regenerated AI masks for {succeeded} {}.",
                if succeeded == 1 { "image" } else { "images" }
            ))
        } else {
            Some(format!(
                "Regenerated AI masks for {succeeded} of {} images. {}",
                state.total,
                state.failures.join(" · ")
            ))
        };
        self.egui_ctx.request_repaint();
    }
}
