use super::*;

impl AurawApp {
    pub(crate) fn has_copied_adjustments(&self) -> bool {
        self.adjustment_clipboard.is_some()
    }

    pub(crate) fn copied_adjustments_source_label(&self) -> Option<&str> {
        self.adjustment_clipboard
            .as_ref()
            .map(|clipboard| clipboard.source_label.as_str())
    }

    pub(super) fn install_adjustment_clipboard(
        &mut self,
        edits: SidecarEditState,
        source_label: String,
    ) {
        self.adjustment_clipboard = Some(LibraryAdjustmentClipboard {
            edits,
            settings: self.adjustment_copy_settings,
            source_label,
        });
    }

    pub(super) fn apply_adjustment_clipboard_to_current(
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

        if masks_changed || inpainting_changed {
            crate::sidecar::preflight_mask_change(&merged.masks, &merged.inpainting).map_err(
                |error| {
                    format!(
                        "Paste was not applied because the resulting edit could not be saved: {error}"
                    )
                },
            )?;
        }

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

    pub(super) fn reload_current_after_adjustment_paste(
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
            match sidecar_target {
                crate::sidecar::SidecarTarget::Desktop { raw_path } => {
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
                crate::sidecar::SidecarTarget::Android {
                    raw_uri,
                    display_name,
                } => match crate::android::open_library_document(
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
    }

    fn library_asset_is_current(&self, asset: &crate::ui::library::LibraryAsset) -> bool {
        #[cfg(not(target_os = "android"))]
        {
            asset
                .desktop_path()
                .is_some_and(|path| self.current_path.as_deref() == Some(path))
                && self.loaded_raw.is_some()
        }
        #[cfg(target_os = "android")]
        {
            let Some(uri) = asset.android_uri() else {
                return false;
            };
            matches!(
                self.sidecar_target.as_ref(),
                Some(crate::sidecar::SidecarTarget::Android {
                    raw_uri: current_uri,
                    display_name: current_name,
                }) if current_uri == uri && current_name == &asset.display_name
            ) && self.loaded_raw.is_some()
        }
    }

    fn library_asset_edit_state(
        &mut self,
        asset: &crate::ui::library::LibraryAsset,
    ) -> Result<SidecarEditState, String> {
        #[cfg(not(target_os = "android"))]
        let persisted = {
            let path = asset
                .desktop_path()
                .ok_or_else(|| "Library asset is not available from desktop storage".to_owned())?;
            desktop_library_sidecar_edits(path)?
        };
        #[cfg(target_os = "android")]
        let persisted = {
            let uri = asset
                .android_uri()
                .ok_or_else(|| "Library asset is not available from Android storage".to_owned())?;
            crate::sidecar::load_android(&self.android_app, uri, &asset.display_name)
                .map_err(|error| error.to_string())?
                .map(|loaded| loaded.edits)
        };

        if self.library_asset_is_current(asset) {
            self.finish_mask_geometry_interaction();
            self.commit_edit_history_now();
            if persisted.is_none() && !self.can_undo_edit() {
                return Ok(crate::sidecar::default_edit_state());
            }
            return Ok(self.capture_sidecar_edit_state());
        }
        Ok(persisted.unwrap_or_else(crate::sidecar::default_edit_state))
    }

    fn save_library_asset_edit_state(
        &mut self,
        asset: &crate::ui::library::LibraryAsset,
        edits: SidecarEditState,
    ) -> Result<(), String> {
        #[cfg(not(target_os = "android"))]
        {
            let path = asset
                .desktop_path()
                .ok_or_else(|| "Library asset is not available from desktop storage".to_owned())?;
            crate::sidecar::save_desktop(path, edits).map_err(|error| error.to_string())?;
            crate::sidecar::invalidate_developed_thumbnail_cache(path)?;
            Ok(())
        }
        #[cfg(target_os = "android")]
        {
            let uri = asset
                .android_uri()
                .ok_or_else(|| "Library asset is not available from Android storage".to_owned())?;
            crate::sidecar::save_android(&self.android_app, uri, &asset.display_name, edits)
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
    }

    pub(crate) fn library_adjustment_edit_count(
        &mut self,
        assets: &[crate::ui::library::LibraryAsset],
    ) -> (usize, Vec<String>) {
        let mut edited = 0usize;
        let mut failures = Vec::new();
        for asset in assets {
            match self.library_asset_edit_state(asset) {
                Ok(state) => {
                    if crate::sidecar::edit_state_has_adjustments(&state) {
                        edited += 1;
                    }
                }
                Err(error) => failures.push(format!("{}: {error}", asset.display_name)),
            }
        }
        (edited, failures)
    }

    pub(crate) fn copy_library_adjustments(
        &mut self,
        asset: &crate::ui::library::LibraryAsset,
    ) -> Result<(), String> {
        let edits = self.library_asset_edit_state(asset)?;
        self.install_adjustment_clipboard(edits, asset.display_name.clone());
        Ok(())
    }

    pub(crate) fn paste_library_adjustments(
        &mut self,
        assets: &[crate::ui::library::LibraryAsset],
        mode: AdjustmentPasteMode,
        frame: &eframe::Frame,
    ) -> (usize, Vec<crate::ui::library::LibraryAsset>, Vec<String>) {
        let Some(clipboard) = self.adjustment_clipboard.clone() else {
            return (
                0,
                Vec::new(),
                vec!["Copy adjustments from an image first.".to_owned()],
            );
        };
        let mut completed = 0usize;
        let mut ai_refresh = Vec::new();
        let mut failures = Vec::new();

        // Applying a copied camera profile to the loaded image starts an
        // asynchronous reopen. Handle sidecar-only assets first so that reload
        // cannot interrupt a multi-image paste halfway through the selection.
        let mut ordered_assets = assets.to_vec();
        ordered_assets.sort_by_key(|asset| self.library_asset_is_current(asset));

        for asset in &ordered_assets {
            let result = if self.library_asset_is_current(asset) {
                self.apply_adjustment_clipboard_to_current(&clipboard, mode, frame)
            } else {
                (|| {
                    let mut destination = self.library_asset_edit_state(asset)?;
                    crate::sidecar::apply_copied_adjustments_with_mode(
                        &mut destination,
                        &clipboard.edits,
                        clipboard.settings,
                        mode,
                    );
                    let needs_ai_refresh = destination.ai_masks_need_update;
                    self.save_library_asset_edit_state(asset, destination)?;
                    Ok(needs_ai_refresh)
                })()
            };

            match result {
                Ok(needs_ai_refresh) => {
                    completed += 1;
                    if needs_ai_refresh {
                        ai_refresh.push(asset.clone());
                    }
                }
                Err(error) => failures.push(format!("{}: {error}", asset.display_name)),
            }
        }
        (completed, ai_refresh, failures)
    }

}

pub(super) fn desktop_library_sidecar_edits(
    raw_path: &std::path::Path,
) -> Result<Option<SidecarEditState>, String> {
    match crate::sidecar::load_desktop(raw_path) {
        Ok(loaded) => Ok(loaded.map(|loaded| loaded.edits)),
        // Opening a RAW already recovers from malformed sidecars by using the
        // default edit state. Library copy/paste must do the same so one bad
        // JSON file cannot make its thumbnail disappear or block a paste that
        // will replace it with a valid sidecar.
        Err(crate::sidecar::SidecarError::Invalid(error)) => {
            log::warn!(
                "ignoring invalid sidecar while handling library adjustments for {}: {error}",
                raw_path.display()
            );
            Ok(None)
        }
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn ai_mask_refresh_target_count(masks: &crate::pipeline::MaskStack) -> usize {
    masks
        .masks
        .iter()
        .flat_map(|mask| &mask.components)
        .filter(|component| match (component.kind, &component.geometry) {
            (
                crate::pipeline::MaskKind::Subject | crate::pipeline::MaskKind::Background,
                crate::pipeline::MaskGeometry::Ai { .. },
            ) => true,
            (
                crate::pipeline::MaskKind::Object,
                crate::pipeline::MaskGeometry::Object { strokes, .. },
            ) => strokes
                .iter()
                .any(|stroke| stroke.positive && !stroke.points.is_empty()),
            (
            (
                crate::pipeline::MaskKind::LuminanceRange,
                crate::pipeline::MaskGeometry::LuminanceRange { .. },
            )
            | (
                crate::pipeline::MaskKind::ColorRange,
                crate::pipeline::MaskGeometry::ColorRange { .. },
            ) => true,
            _ => false,
        })
        .count()
}

pub(super) fn complete_library_ai_mask_refresh_item(state: &mut LibraryAiMaskRefreshState) {
    if let Some(job) = state.current.take() {
        state.completed += 1;
        state.mask_completed += job.mask_targets;
    }
}

impl AurawApp {
    pub(crate) fn can_start_library_ai_mask_refresh(&self) -> bool {
        let duplicate = self
            .background_task_snapshots()
            .iter()
            .any(|task| matches!(task.kind, TaskKind::LibraryAiMaskRefresh));
        let ready = !duplicate && !self.sidecar_save_in_progress();
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
            let current_mask_progress = state.current.as_ref().map_or(0, |job| {
                if state.phase == LibraryAiMaskRefreshPhase::Loading {
                    return 0;
                }
                if state.phase == LibraryAiMaskRefreshPhase::Updating
                    && self.ai_masks_need_update()
                    && !self.ai_mask_update_busy()
                {
                    // The update could not start (for example a missing model
                    // or runtime). Keep its progress at zero until the batch
                    // records the failure on this frame.
                    return 0;
                }
                job.mask_targets
                    .saturating_sub(self.ai_mask_update_remaining_target_count())
            });
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
                state.mask_completed + current_mask_progress,
                state.mask_total,
                state.failures.len(),
                current,
            )
        })
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn start_library_ai_mask_refresh_paths(
        &mut self,
        paths: Vec<PathBuf>,
        _frame: &eframe::Frame,
    ) {
        if paths.is_empty() {
            return;
        }
        if !self.can_start_library_ai_mask_refresh() {
            self.notice = Some(
                "AI-mask refresh is already queued, or edits are still being saved.".to_owned(),
            );
            return;
        }
        // The sidecar is loaded by the normal asynchronous document loader.
        // Counting AI targets here would perform file I/O on the UI thread.
        let pending = paths
            .into_iter()
            .map(|source| LibraryAiMaskRefreshJob {
                source,
                mask_targets: 0,
            })
            .collect::<VecDeque<_>>();
        let total = pending.len();
        let task_id = self.enqueue_background_action(
            TaskKind::LibraryAiMaskRefresh,
            format!(
                "Refreshing AI masks for {total} {}",
                if total == 1 { "image" } else { "images" }
            ),
            TaskProgress::units(
                0,
                total as u64,
                Some("images".to_owned()),
                "Waiting for earlier background work…",
            ),
            true,
            BackgroundAction::LibraryAiMaskRefresh { jobs: pending },
        );
        self.background_tasks.set_global_visible(task_id, false);
        self.library_ai_mask_refresh_task_id = Some(task_id);
    }

    #[cfg(target_os = "android")]
    pub(crate) fn start_library_ai_mask_refresh_android(
        &mut self,
        targets: Vec<(String, String)>,
        _frame: &eframe::Frame,
    ) {
        if targets.is_empty() {
            return;
        }
        if !self.can_start_library_ai_mask_refresh() {
            self.notice = Some(
                "AI-mask refresh is already queued, or edits are still being saved.".to_owned(),
            );
            return;
        }
        // The sidecar is loaded by the normal asynchronous document loader.
        // Counting AI targets here would perform file I/O on the UI thread.
        let pending = targets
            .into_iter()
            .map(|(uri, display_name)| LibraryAiMaskRefreshJob {
                uri,
                display_name,
                mask_targets: 0,
            })
            .collect::<VecDeque<_>>();
        let total = pending.len();
        let task_id = self.enqueue_background_action(
            TaskKind::LibraryAiMaskRefresh,
            format!(
                "Refreshing AI masks for {total} {}",
                if total == 1 { "image" } else { "images" }
            ),
            TaskProgress::units(
                0,
                total as u64,
                Some("images".to_owned()),
                "Waiting for earlier background work…",
            ),
            true,
            BackgroundAction::LibraryAiMaskRefresh { jobs: pending },
        );
        self.background_tasks.set_global_visible(task_id, false);
        self.library_ai_mask_refresh_task_id = Some(task_id);
    }

    #[cfg(not(target_os = "android"))]
    pub(super) fn start_next_library_ai_mask_refresh(&mut self, frame: &eframe::Frame) {
        loop {
            let next = {
                let Some(state) = self.library_ai_mask_refresh.as_mut() else {
                    return;
                };
                if state.current.is_some() {
                    return;
                }
                (if state.cancel_requested {
                    state.pending.clear();
                    None
                } else {
                    state.pending.pop_front()
                })
                .inspect(|job| {
                    state.current = Some(job.clone());
                    state.phase = LibraryAiMaskRefreshPhase::Loading;
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
                complete_library_ai_mask_refresh_item(state);
            }
        }
    }

    #[cfg(target_os = "android")]
    pub(super) fn start_next_library_ai_mask_refresh(&mut self, _frame: &eframe::Frame) {
        loop {
            let next = {
                let Some(state) = self.library_ai_mask_refresh.as_mut() else {
                    return;
                };
                if state.current.is_some() {
                    return;
                }
                (if state.cancel_requested {
                    state.pending.clear();
                    None
                } else {
                    state.pending.pop_front()
                })
                .inspect(|job| {
                    state.current = Some(job.clone());
                    state.phase = LibraryAiMaskRefreshPhase::Loading;
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
                        complete_library_ai_mask_refresh_item(state);
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

        let cancel_requested = self
            .library_ai_mask_refresh
            .as_ref()
            .is_some_and(|state| state.cancel_requested);
        if cancel_requested {
            if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                state.current = None;
            }
            self.start_next_library_ai_mask_refresh(frame);
            return;
        }

        if !success {
            #[cfg(not(target_os = "android"))]
            let label = current.source.display().to_string();
            #[cfg(target_os = "android")]
            let label = current.display_name.clone();
            if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                state.failures.push(format!("{label}: RAW load failed"));
                complete_library_ai_mask_refresh_item(state);
            }
            self.start_next_library_ai_mask_refresh(frame);
            return;
        }

        let mask_targets = ai_mask_refresh_target_count(&self.masks);
        if let Some(state) = self.library_ai_mask_refresh.as_mut() {
            let previous_targets = state
                .current
                .as_ref()
                .map_or(0, |job| job.mask_targets);
            state.mask_total = state.mask_total.saturating_sub(previous_targets);
            if let Some(current) = state.current.as_mut() {
                current.mask_targets = mask_targets;
            }
            state.mask_total = state.mask_total.saturating_add(mask_targets);
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
            if state.cancel_requested {
                state.current = None;
            } else {
                state.failures.push(format!("{label}: {error}"));
                complete_library_ai_mask_refresh_item(state);
            }
        }
        self.start_next_library_ai_mask_refresh(frame);
    }

    pub(crate) fn poll_library_ai_mask_refresh(&mut self, frame: &eframe::Frame) {
        self.sync_library_ai_mask_background_progress();
        let cancel_now = self
            .library_ai_mask_refresh
            .as_ref()
            .is_some_and(|state| state.cancel_requested && state.current.is_none());
        if cancel_now {
            self.finish_library_ai_mask_refresh();
            return;
        }
        let phase = self
            .library_ai_mask_refresh
            .as_ref()
            .and_then(|state| state.current.as_ref().map(|_| state.phase));
        let Some(phase) = phase else {
            return;
        };
        let cancel_after_phase = self
            .library_ai_mask_refresh
            .as_ref()
            .is_some_and(|state| state.cancel_requested);
        if cancel_after_phase
            && phase == LibraryAiMaskRefreshPhase::Updating
            && !self.ai_mask_update_busy()
        {
            if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                state.current = None;
            }
            self.start_next_library_ai_mask_refresh(frame);
            return;
        }

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

        match phase {
            LibraryAiMaskRefreshPhase::Loading => (),
            LibraryAiMaskRefreshPhase::Updating => {
                if self.ai_mask_update_busy() {
                    return;
                }

                if self.ai_masks_need_update() {
                    let reason = self
                        .notice
                        .clone()
                        .unwrap_or_else(|| "AI masks still need an update".to_owned());
                    if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                        state.failures.push(format!("{label}: {reason}"));
                        complete_library_ai_mask_refresh_item(state);
                    }
                    self.start_next_library_ai_mask_refresh(frame);
                    return;
                }

                // Do not advance to the next RAW until the regenerated masks
                // are durable. Otherwise the final item can be reported as
                // complete (and the library refreshed) while its sidecar is
                // still writing, and a following load can observe stale data.
                self.commit_edit_history_now();
                self.queue_explicit_sidecar_save();
                if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                    state.phase = LibraryAiMaskRefreshPhase::Saving;
                }
            }
            LibraryAiMaskRefreshPhase::Saving => {
                if self.sidecar_save_in_progress() {
                    return;
                }

                let revision = self.edit_commit_revision();
                if self.sidecar_failed_revision == Some(revision) {
                    let reason = self
                        .notice
                        .clone()
                        .unwrap_or_else(|| "could not save regenerated AI masks".to_owned());
                    if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                        state.failures.push(format!("{label}: {reason}"));
                    }
                }

                if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                    complete_library_ai_mask_refresh_item(state);
                }
                self.start_next_library_ai_mask_refresh(frame);
            }
        }
    }

    pub(super) fn finish_library_ai_mask_refresh(&mut self) {
        let Some(state) = self.library_ai_mask_refresh.take() else {
            return;
        };
        let task_id = self.library_ai_mask_refresh_task_id.take();
        let cancelled = state.cancel_requested
            || task_id.is_some_and(|id| self.background_task_cancelled(id));
        let succeeded = state.completed.saturating_sub(state.failures.len());
        self.active_tab = AppTab::Library;
        self.library.refresh(&self.egui_ctx);
        let summary = if cancelled {
            format!(
                "AI-mask refresh cancelled after {}/{} images.",
                state.completed, state.total
            )
        } else if state.failures.is_empty() {
            format!(
                "Regenerated AI masks for {succeeded} {}.",
                if succeeded == 1 { "image" } else { "images" }
            )
        } else {
            format!(
                "Regenerated AI masks for {succeeded} of {} images. {}",
                state.total,
                state.failures.join(" · ")
            )
        };
        self.library.set_status(summary.clone());
        self.notice = Some(summary);
        if let Some(task_id) = task_id {
            if cancelled || state.failures.is_empty() {
                self.finish_background_task(task_id);
            } else {
                self.fail_background_task(
                    task_id,
                    format!(
                        "{} AI-mask refresh item(s) failed: {}",
                        state.failures.len(),
                        state.failures.join(" · ")
                    ),
                );
            }
        }
        self.egui_ctx.request_repaint();
    }
}
