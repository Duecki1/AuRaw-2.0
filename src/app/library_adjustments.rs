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
    ) -> Result<(), String> {
        if self.loaded_raw.is_none() || self.sidecar_target.is_none() {
            return Err("The destination image is not loaded.".to_owned());
        }

        self.finish_mask_geometry_interaction();
        self.commit_edit_history_now();
        let mut merged = self.capture_sidecar_edit_state();
        crate::sidecar::apply_copied_adjustments(
            &mut merged,
            &clipboard.edits,
            clipboard.settings,
        );

        let adjustments_changed = clipboard.settings.adjustments;
        let masks_changed = clipboard.settings.masks;
        let inpainting_changed = clipboard.settings.inpainting;
        let lens_changed = clipboard.settings.lens_correction
            && (self.lens_correction.enabled != merged.lens.enabled
                || self.lens_correction.selected_maker != merged.lens.maker
                || self.lens_correction.selected_model != merged.lens.model);

        if adjustments_changed {
            self.exposure = merged.exposure;
            self.geometry = merged.geometry.sanitized();
            self.crop_constraint_reference = None;
            self.note_geometry_changed();
            self.exposure.sanitize_tone_curves();
            self.note_edit_changed();
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
            self.note_inpainting_changed_for_ai_masks();
            self.ai_masks_need_update |= merged.ai_masks_need_update;
            self.queue_preview_processing(crate::pipeline::ProcessingStage::Tone);
        }

        if clipboard.settings.lens_correction {
            self.lens_correction.enabled = merged.lens.enabled;
            self.lens_correction.selected_maker = merged.lens.maker;
            self.lens_correction.selected_model = merged.lens.model;
            if lens_changed {
                self.note_lens_correction_changed_for_masks();
                self.ai_masks_need_update |= merged.ai_masks_need_update;
                self.mark_lens_correction_dirty();
            }
        }

        if adjustments_changed && !lens_changed {
            self.mark_pipeline_dirty();
        }

        self.commit_edit_history_now();
        self.queue_explicit_sidecar_save();
        Ok(())
    }

    #[cfg(not(target_os = "android"))]
    fn desktop_library_edit_state(
        &mut self,
        raw_path: &std::path::Path,
    ) -> Result<SidecarEditState, String> {
        if self.current_path.as_deref() == Some(raw_path) && self.loaded_raw.is_some() {
            self.finish_mask_geometry_interaction();
            self.commit_edit_history_now();
            return Ok(self.capture_sidecar_edit_state());
        }
        crate::sidecar::load_desktop(raw_path)
            .map_err(|error| error.to_string())
            .map(|loaded| {
                loaded
                    .map(|loaded| loaded.edits)
                    .unwrap_or_else(crate::sidecar::default_edit_state)
            })
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
    ) -> (usize, Vec<String>) {
        let Some(clipboard) = self.adjustment_clipboard.clone() else {
            return (0, vec!["Copy adjustments from an image first.".to_owned()]);
        };
        let mut completed = 0usize;
        let mut failures = Vec::new();

        for raw_path in raw_paths {
            let result = if self.current_path.as_deref() == Some(raw_path.as_path())
                && self.loaded_raw.is_some()
            {
                self.apply_adjustment_clipboard_to_current(&clipboard)
            } else {
                (|| {
                    let mut destination = crate::sidecar::load_desktop(raw_path)
                        .map_err(|error| error.to_string())?
                        .map(|loaded| loaded.edits)
                        .unwrap_or_else(crate::sidecar::default_edit_state);
                    crate::sidecar::apply_copied_adjustments(
                        &mut destination,
                        &clipboard.edits,
                        clipboard.settings,
                    );
                    crate::sidecar::save_desktop(raw_path, destination)
                        .map_err(|error| error.to_string())?;
                    crate::sidecar::invalidate_developed_thumbnail_cache(raw_path)?;
                    Ok(())
                })()
            };

            match result {
                Ok(()) => completed += 1,
                Err(error) => failures.push(format!("{}: {error}", raw_path.display())),
            }
        }
        (completed, failures)
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
        if self.current_android_library_target_matches(raw_uri, display_name) {
            self.finish_mask_geometry_interaction();
            self.commit_edit_history_now();
            return Ok(self.capture_sidecar_edit_state());
        }
        crate::sidecar::load_android(&self.android_app, raw_uri, display_name)
            .map_err(|error| error.to_string())
            .map(|loaded| {
                loaded
                    .map(|loaded| loaded.edits)
                    .unwrap_or_else(crate::sidecar::default_edit_state)
            })
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
    ) -> (usize, Vec<String>) {
        let Some(clipboard) = self.adjustment_clipboard.clone() else {
            return (0, vec!["Copy adjustments from an image first.".to_owned()]);
        };
        let mut completed = 0usize;
        let mut failures = Vec::new();

        for (raw_uri, display_name) in targets {
            let result = if self.current_android_library_target_matches(raw_uri, display_name) {
                self.apply_adjustment_clipboard_to_current(&clipboard)
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
                    crate::sidecar::apply_copied_adjustments(
                        &mut destination,
                        &clipboard.edits,
                        clipboard.settings,
                    );
                    crate::sidecar::save_android(
                        &self.android_app,
                        raw_uri,
                        display_name,
                        destination,
                    )
                    .map_err(|error| error.to_string())?;
                    Ok(())
                })()
            };

            match result {
                Ok(()) => completed += 1,
                Err(error) => failures.push(format!("{display_name}: {error}")),
            }
        }
        (completed, failures)
    }
}
