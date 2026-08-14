use super::*;

const SIDECAR_AUTOSAVE_INTERVAL: Duration = Duration::from_millis(900);
const SIDECAR_AUTOSAVE_ACTIVE_POLL: Duration = Duration::from_millis(100);

pub(super) fn autosave_deadline(
    existing: Option<SidecarAutosaveDeadline>,
    generation: u64,
    now: Instant,
) -> SidecarAutosaveDeadline {
    existing
        .filter(|deadline| deadline.generation == generation)
        .unwrap_or(SidecarAutosaveDeadline {
            generation,
            due_at: now + SIDECAR_AUTOSAVE_INTERVAL,
        })
}

pub(super) fn sidecar_interaction_active(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.pointer.any_down()) || ctx.egui_wants_keyboard_input()
}

impl AurawApp {
    pub(crate) fn report_mask_persistence_limit(
        &mut self,
        action: &str,
        error: &crate::sidecar::SidecarError,
    ) {
        let message = format!(
            "{action} was not applied because the resulting edit could not be saved: {error}"
        );
        self.notice = Some(message.clone());
        crate::diagnostics::record(&message);
        log::warn!("{message}");
        self.egui_ctx.request_repaint();
    }

    pub(super) fn report_sidecar_save_failure(&mut self, revision: Option<u64>, detail: impl AsRef<str>) {
        self.sidecar_save_feedback_until = None;
        self.sidecar_conflict_resolution_error = None;
        if let Some(revision) = revision {
            self.sidecar_failed_revision = Some(revision);
        }

        let message = format!("Could not save edits: {}", detail.as_ref());
        self.notice = Some(message.clone());
        self.sidecar_save_error_dialog = Some(message.clone());
        crate::diagnostics::record(format!("Edit save failed: {}", detail.as_ref()));
        log::error!("{message}");
        self.egui_ctx.request_repaint();
    }

    pub(super) fn show_sidecar_save_error_dialog(&mut self, ctx: &egui::Context) {
        let Some(message) = self.sidecar_save_error_dialog.clone() else {
            return;
        };

        let cloud_conflict = message
            .strip_prefix("Could not save edits: ")
            .is_some_and(crate::cloud::is_sidecar_conflict_message);
        let conflict_raw_path = cloud_conflict
            .then(|| self.current_path.clone())
            .flatten()
            .filter(|path| crate::cloud::cached_asset_id_for_raw(path).is_some());
        let resolving_conflict = self.sidecar_conflict_receiver.is_some();
        let can_retry = self.can_save_edits()
            && !self.sidecar_save_in_progress()
            && self.sidecar_failed_revision == Some(self.edit_commit_revision());
        let mut retry = false;
        let mut close = false;
        let mut resolution = None;
        crate::ui::responsive_popup(egui::Window::new("Could not save edits"), ctx, 460.0)
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(if cloud_conflict {
                    "The server and this device both have newer edits for this RAW."
                } else {
                    "AuRaw was unable to write the edit sidecar."
                });
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(&message).monospace())
                        .wrap()
                        .selectable(true),
                );
                ui.add_space(6.0);
                ui.small("This error was added to the log in Settings → Diagnostics.");
                ui.add_space(8.0);
                if let Some(error) = self.sidecar_conflict_resolution_error.as_deref() {
                    ui.label(
                        egui::RichText::new(error)
                            .small()
                            .color(ui.visuals().error_fg_color),
                    );
                    ui.add_space(6.0);
                }
                if let Some(raw_path) = conflict_raw_path.as_ref() {
                    ui.label("Choose which edit sidecar should become authoritative:");
                    ui.add_space(4.0);
                    if resolving_conflict {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Resolving cloud edit conflict…");
                        });
                    } else {
                        if ui
                            .add_enabled(
                                can_retry,
                                egui::Button::new("Overwrite server with local edits"),
                            )
                            .on_hover_text(
                                "Keep the edits currently open on this device and replace the server sidecar.",
                            )
                            .clicked()
                        {
                            resolution = Some((
                                raw_path.clone(),
                                CloudSidecarConflictResolution::OverwriteServer,
                            ));
                        }
                        if ui
                            .add_enabled(
                                can_retry,
                                egui::Button::new("Overwrite local edits with server"),
                            )
                            .on_hover_text(
                                "Discard this device's conflicting edits, install the latest server sidecar, and reload the RAW.",
                            )
                            .clicked()
                        {
                            resolution = Some((
                                raw_path.clone(),
                                CloudSidecarConflictResolution::OverwriteLocal,
                            ));
                        }
                        ui.label(
                            egui::RichText::new(
                                "Overwriting local edits discards the preserved conflicting sidecar on this device.",
                            )
                            .small()
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                    if ui
                        .add_enabled(!resolving_conflict, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        close = true;
                    }
                } else {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(can_retry, egui::Button::new("Try again"))
                            .clicked()
                        {
                            retry = true;
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                }
            });

        if let Some((raw_path, resolution)) = resolution {
            self.start_cloud_sidecar_conflict_resolution(raw_path, resolution);
        } else if retry {
            self.sidecar_save_error_dialog = None;
            self.save_edits_now();
        } else if close {
            self.sidecar_save_error_dialog = None;
            self.sidecar_conflict_resolution_error = None;
        }
    }

    pub(super) fn start_cloud_sidecar_conflict_resolution(
        &mut self,
        raw_path: PathBuf,
        resolution: CloudSidecarConflictResolution,
    ) {
        if self.sidecar_conflict_receiver.is_some() {
            return;
        }
        let generation = self.sidecar_generation;
        let revision = self.edit_commit_revision();
        let repaint = self.egui_ctx.clone();
        let (sender, receiver) = mpsc::channel();
        self.sidecar_conflict_resolution_error = None;
        let worker_path = raw_path.clone();
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-conflict-resolution".to_owned())
            .spawn(move || {
                let result = match resolution {
                    CloudSidecarConflictResolution::OverwriteServer => {
                        crate::cloud::overwrite_server_sidecar_with_local(&worker_path)
                    }
                    CloudSidecarConflictResolution::OverwriteLocal => {
                        crate::cloud::overwrite_local_sidecar_with_server(&worker_path)
                    }
                };
                let _ = sender.send(CloudSidecarConflictEvent {
                    raw_path: worker_path,
                    generation,
                    revision,
                    resolution,
                    result,
                });
                repaint.request_repaint();
            });
        match spawn {
            Ok(_) => {
                self.sidecar_conflict_receiver = Some(receiver);
                self.notice = Some("Resolving cloud edit conflict…".to_owned());
            }
            Err(error) => {
                self.sidecar_conflict_resolution_error = Some(format!(
                    "Could not start cloud conflict resolution: {error}"
                ));
            }
        }
    }

    pub(super) fn poll_cloud_sidecar_conflict_resolution(&mut self, frame: &eframe::Frame) {
        let received = self
            .sidecar_conflict_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        let event = match received {
            Some(Ok(event)) => event,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.sidecar_conflict_receiver = None;
                self.sidecar_conflict_resolution_error =
                    Some("The cloud conflict-resolution worker stopped unexpectedly.".to_owned());
                return;
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => return,
        };
        self.sidecar_conflict_receiver = None;
        self.library.update_cloud_sync_state_for_cached_raw(
            &event.raw_path,
            &self.egui_ctx,
        );

        let location = match event.result {
            Ok(location) => location,
            Err(error) => {
                self.sidecar_conflict_resolution_error = Some(error.clone());
                self.notice = Some(format!("Could not resolve cloud edit conflict: {error}"));
                return;
            }
        };

        let still_current = event.generation == self.sidecar_generation
            && self.current_path.as_ref() == Some(&event.raw_path);
        self.sidecar_save_error_dialog = None;
        self.sidecar_conflict_resolution_error = None;
        if !still_current {
            self.notice = Some(match event.resolution {
                CloudSidecarConflictResolution::OverwriteServer => {
                    format!("The cached edits were saved over the server copy at {location}.")
                }
                CloudSidecarConflictResolution::OverwriteLocal => {
                    format!("The cached sidecar was replaced with the server copy from {location}.")
                }
            });
            return;
        }

        match event.resolution {
            CloudSidecarConflictResolution::OverwriteServer => {
                self.sidecar_failed_revision = None;
                if event.revision == self.edit_commit_revision() {
                    self.sidecar_saved_revision = Some(event.revision);
                    self.queue_developed_thumbnail_refresh(event.generation, event.revision);
                } else {
                    self.sidecar_saved_revision = None;
                }
                self.sidecar_save_feedback_until =
                    Some(Instant::now() + Duration::from_millis(1_200));
                self.notice = Some(format!(
                    "Local edits replaced the server sidecar at {location}."
                ));
            }
            CloudSidecarConflictResolution::OverwriteLocal => {
                let label = self
                    .current_label
                    .clone()
                    .unwrap_or_else(|| event.raw_path.display().to_string());
                self.abandon_current_sidecar_for_cloud_conflict();
                self.open_path_labeled(
                    event.raw_path.clone(),
                    label,
                    false,
                    crate::sidecar::SidecarTarget::Desktop {
                        raw_path: event.raw_path,
                    },
                    frame,
                    None,
                );
                self.notice = Some(format!(
                    "Local edits were replaced with the server sidecar from {location}; reloading the RAW."
                ));
            }
        }
    }

    pub(super) fn abandon_current_sidecar_for_cloud_conflict(&mut self) {
        let generation = self.sidecar_generation;
        self.sidecar_target = None;
        self.sidecar_saved_revision = None;
        self.sidecar_failed_revision = None;
        self.sidecar_autosave_deadline = None;
        self.sidecar_pending
            .retain(|request| request.generation != generation);
    }

    pub(super) fn capture_sidecar_edit_state(&self) -> SidecarEditState {
        let masks = self.committed_mask_state_for_persistence();
        let camera_profile = self.selected_camera_profile.as_ref().and_then(|selected| {
            let root = self.camera_profile_folder.as_ref()?;
            if selected == root {
                return Some(std::path::PathBuf::from("."));
            }
            let relative = selected.strip_prefix(root).ok()?;
            (!relative.as_os_str().is_empty()).then(|| relative.to_path_buf())
        });
        SidecarEditState {
            exposure: self.exposure,
            geometry: self.geometry.sanitized(),
            camera_profile,
            subject_refinement: (!masks.subject_refinement.is_empty())
                .then(|| masks.subject_refinement.clone()),
            masks,
            inpainting: Arc::new(self.inpaint_strokes.clone()),
            lens: SidecarLensEditState {
                enabled: self.lens_correction.enabled,
                maker: self.lens_correction.selected_maker.clone(),
                model: self.lens_correction.selected_model.clone(),
            },
            ai_masks_need_update: self.ai_masks_need_update,
        }
    }

    /// Finalize and enqueue the old image before any per-image state is reset.
    /// Requests own both their target and edit snapshot, so a slow completion
    /// can never be redirected to the next RAW.
    pub(super) fn begin_sidecar_open(&mut self) -> u64 {
        self.commit_edit_history_now();
        let revision = self.edit_commit_revision();
        let pending_latest = self
            .sidecar_pending
            .iter_mut()
            .find(|request| {
                request.generation == self.sidecar_generation && request.revision == revision
            })
            .map(|request| request.explicit = true)
            .is_some();
        let already_queued = self.sidecar_in_flight.is_some_and(|job| {
            job.generation == self.sidecar_generation && job.revision == revision
        }) || pending_latest;
        if self.sidecar_saved_revision != Some(revision) && !already_queued {
            // Switching images is the one automatic retry for a previously
            // failed latest revision. Once captured, the immutable request is
            // safe even after the active target changes.
            self.queue_current_sidecar_save(true);
        }
        self.start_next_sidecar_save();

        self.sidecar_generation = self.sidecar_generation.wrapping_add(1);
        self.sidecar_target = None;
        self.sidecar_saved_revision = None;
        self.sidecar_failed_revision = None;
        self.sidecar_autosave_deadline = None;
        self.sidecar_generation
    }

    pub(super) fn install_sidecar_target(
        &mut self,
        target: crate::sidecar::SidecarTarget,
        generation: u64,
        needs_rewrite: bool,
    ) {
        if generation != self.sidecar_generation {
            return;
        }
        self.sidecar_target = Some(target);
        self.sidecar_failed_revision = None;
        self.sidecar_saved_revision = (!needs_rewrite).then(|| self.edit_commit_revision());
        if needs_rewrite {
            self.queue_current_sidecar_save(false);
            self.start_next_sidecar_save();
        } else {
            #[cfg(not(target_os = "android"))]
            if self
                .sidecar_target
                .as_ref()
                .is_some_and(|target| match target {
                    crate::sidecar::SidecarTarget::Desktop { raw_path } => {
                        crate::sidecar::sidecar_path_for_raw(raw_path).is_file()
                    }
                })
            {
                self.queue_developed_thumbnail_refresh(generation, self.edit_commit_revision());
            }
        }
    }

    pub(super) fn queue_explicit_sidecar_save(&mut self) {
        self.commit_edit_history_now();
        self.queue_current_sidecar_save(true);
        self.start_next_sidecar_save();
    }

    pub(super) fn queue_current_sidecar_save(&mut self, explicit: bool) {
        let Some(target) = self.sidecar_target.clone() else {
            return;
        };
        let generation = self.sidecar_generation;
        let revision = self.edit_commit_revision();

        if !explicit
            && (self.sidecar_saved_revision == Some(revision)
                || self.sidecar_failed_revision == Some(revision)
                || self
                    .sidecar_in_flight
                    .is_some_and(|job| job.generation == generation && job.revision == revision)
                || self.sidecar_pending.iter().any(|request| {
                    request.generation == generation && request.revision == revision
                }))
        {
            return;
        }

        let mut request = SidecarSaveRequest {
            target,
            generation,
            revision,
            explicit,
            edits: self.capture_sidecar_edit_state(),
        };
        if let Some(index) = self
            .sidecar_pending
            .iter()
            .position(|pending| pending.generation == generation)
        {
            request.explicit |= self.sidecar_pending[index].explicit;
            self.sidecar_pending[index] = request;
        } else {
            self.sidecar_pending.push_back(request);
        }
    }

    pub(super) fn schedule_sidecar_autosave(&mut self, ctx: &egui::Context, interaction_active: bool) {
        if self.loaded_raw.is_none() || self.sidecar_target.is_none() {
            self.sidecar_autosave_deadline = None;
            return;
        }

        let generation = self.sidecar_generation;
        let revision = self.edit_commit_revision();
        let revision_is_covered = self.sidecar_saved_revision == Some(revision)
            || self.sidecar_failed_revision == Some(revision)
            || self
                .sidecar_in_flight
                .is_some_and(|job| job.generation == generation && job.revision == revision)
            || self
                .sidecar_pending
                .iter()
                .any(|request| request.generation == generation && request.revision == revision);
        if revision_is_covered {
            self.sidecar_autosave_deadline = None;
            self.start_next_sidecar_save();
            return;
        }

        let stale_pending = self
            .sidecar_pending
            .iter()
            .any(|request| request.generation == generation && request.revision != revision);
        if stale_pending && !interaction_active {
            // This generation already reached an earlier deadline while a
            // worker or interaction kept it queued. Replace that snapshot
            // with the newest committed value and preserve any explicit bit.
            self.sidecar_autosave_deadline = None;
            self.queue_current_sidecar_save(false);
            self.start_next_sidecar_save();
            return;
        }

        let now = Instant::now();
        let deadline = autosave_deadline(self.sidecar_autosave_deadline, generation, now);
        self.sidecar_autosave_deadline = Some(deadline);
        if now < deadline.due_at {
            ctx.request_repaint_after(deadline.due_at.duration_since(now));
            return;
        }
        if interaction_active {
            // Keep the original deadline: a continuous sequence of edits gets
            // persisted promptly after it becomes idle instead of restarting
            // the full delay after every committed value.
            ctx.request_repaint_after(SIDECAR_AUTOSAVE_ACTIVE_POLL);
            return;
        }

        self.sidecar_autosave_deadline = None;
        self.queue_current_sidecar_save(false);
        self.start_next_sidecar_save();
    }

    pub(crate) fn detach_current_file_for_library_action(
        &mut self,
        raw_path: &std::path::Path,
    ) -> bool {
        if self.current_path.as_deref() != Some(raw_path) {
            return false;
        }
        self.detach_current_sidecar_target_for_library_action()
    }

    #[cfg(target_os = "android")]
    pub(crate) fn detach_current_android_document_for_library_action(
        &mut self,
        raw_uri: &str,
        display_name: &str,
    ) -> bool {
        let is_current = matches!(
            self.sidecar_target.as_ref(),
            Some(crate::sidecar::SidecarTarget::Android {
                raw_uri: current_uri,
                display_name: current_name,
            }) if current_uri == raw_uri && current_name == display_name
        );
        if !is_current {
            return false;
        }
        self.detach_current_sidecar_target_for_library_action()
    }

    #[cfg(target_os = "android")]
    pub(crate) fn reset_android_library_adjustments(
        &mut self,
        raw_uri: &str,
        display_name: &str,
    ) -> Result<(), String> {
        let was_current =
            self.detach_current_android_document_for_library_action(raw_uri, display_name);
        // The Android sidecar contains the entire edit document, including
        // generated AI-mask data. Deleting it (rather than rewriting neutral
        // values) makes Reset All equivalent to opening an untouched RAW.
        let result = crate::android::remove_raw_sidecar(
            &self.android_app,
            raw_uri,
            display_name,
        );
        if was_current && result.is_ok() {
            self.reload_android_library_document_after_reset(raw_uri, display_name);
        }
        result
    }

    #[cfg(target_os = "android")]
    pub(crate) fn duplicate_android_library_item(
        &mut self,
        raw_uri: &str,
        display_name: &str,
    ) -> Result<String, String> {
        crate::android::duplicate_library_document(&self.android_app, raw_uri, display_name)
    }

    #[cfg(target_os = "android")]
    pub(crate) fn rename_android_library_item(
        &mut self,
        raw_uri: &str,
        display_name: &str,
        requested_name: &str,
    ) -> Result<String, String> {
        let was_current =
            self.detach_current_android_document_for_library_action(raw_uri, display_name);
        let result = crate::android::rename_library_document(
            &self.android_app,
            raw_uri,
            display_name,
            requested_name,
        );
        match result {
            Ok(renamed_uri) => {
                if was_current {
                    self.open_android_library_document(&renamed_uri, requested_name);
                }
                Ok(renamed_uri)
            }
            Err(error) => {
                if was_current {
                    self.open_android_library_document(raw_uri, display_name);
                }
                Err(error)
            }
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn delete_android_library_item(
        &mut self,
        raw_uri: &str,
        display_name: &str,
    ) -> Result<(), String> {
        let was_current =
            self.detach_current_android_document_for_library_action(raw_uri, display_name);
        let result =
            crate::android::delete_library_document(&self.android_app, raw_uri, display_name);
        if result.is_err() && was_current {
            self.open_android_library_document(raw_uri, display_name);
        }
        result
    }

    pub(super) fn detach_current_sidecar_target_for_library_action(&mut self) -> bool {
        // Finish any immutable save request before the caller removes or
        // replaces the files. Detaching the target then prevents autosave from
        // recreating the deleted sidecar while the Library tab remains open.
        self.flush_sidecar_on_exit();
        let detached_generation = self.sidecar_generation;
        self.sidecar_generation = self.sidecar_generation.wrapping_add(1);
        self.sidecar_target = None;
        self.sidecar_saved_revision = None;
        self.sidecar_failed_revision = None;
        self.sidecar_autosave_deadline = None;
        self.sidecar_pending
            .retain(|request| request.generation != detached_generation);
        true
    }

    pub(crate) fn can_save_edits(&self) -> bool {
        self.loaded_raw.is_some() && self.sidecar_target.is_some()
    }

    pub(crate) fn sidecar_save_in_progress(&self) -> bool {
        self.sidecar_in_flight
            .is_some_and(|job| job.generation == self.sidecar_generation)
            || self
                .sidecar_pending
                .iter()
                .any(|request| request.generation == self.sidecar_generation)
    }

    pub(crate) fn sidecar_save_succeeded_recently(&self) -> bool {
        self.sidecar_save_feedback_until
            .is_some_and(|until| Instant::now() < until)
    }

    pub(crate) fn save_edits_now(&mut self) {
        if !self.can_save_edits() {
            return;
        }
        self.commit_edit_history_now();
        self.sidecar_save_feedback_until = None;
        self.sidecar_failed_revision = None;
        self.queue_current_sidecar_save(true);
        self.start_next_sidecar_save();
        self.notice = Some("Saving edits…".to_owned());
    }

    pub(crate) fn handle_sidecar_shortcut(&mut self, ctx: &egui::Context) {
        let save = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S);
        if self.can_save_edits() && ctx.input_mut(|input| input.consume_shortcut(&save)) {
            self.save_edits_now();
        }
    }

    pub(super) fn start_next_sidecar_save(&mut self) {
        if self.sidecar_in_flight.is_some() {
            return;
        }
        if self.sidecar_pending.front().is_some_and(|request| {
            !request.explicit
                && request.generation == self.sidecar_generation
                && sidecar_interaction_active(&self.egui_ctx)
        }) {
            self.egui_ctx
                .request_repaint_after(SIDECAR_AUTOSAVE_ACTIVE_POLL);
            return;
        }
        let Some(request) = self.sidecar_pending.pop_front() else {
            return;
        };
        let job = SidecarSaveJob {
            generation: request.generation,
            revision: request.revision,
            explicit: request.explicit,
        };
        let repaint = self.egui_ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let raw_path = match &request.target {
            crate::sidecar::SidecarTarget::Desktop { raw_path } => Some(raw_path.clone()),
            #[cfg(target_os = "android")]
            crate::sidecar::SidecarTarget::Android { .. } => None,
        };
        #[cfg(target_os = "android")]
        let android_app = self.android_app.clone();

        let spawn = std::thread::Builder::new()
            .name("auraw-sidecar-save".to_owned())
            .spawn(move || {
                let result = save_sidecar_request(
                    request,
                    #[cfg(target_os = "android")]
                    &android_app,
                );
                let _ = sender.send(SidecarSaveEvent {
                    job,
                    raw_path,
                    result,
                });
                repaint.request_repaint();
            });

        match spawn {
            Ok(_) => {
                self.sidecar_in_flight = Some(job);
                self.sidecar_receiver = Some(receiver);
            }
            Err(error) => {
                if job.generation == self.sidecar_generation {
                    self.report_sidecar_save_failure(
                        Some(job.revision),
                        format!("could not start the edit-save worker: {error}"),
                    );
                } else {
                    self.report_sidecar_save_failure(
                        None,
                        format!(
                            "could not start the edit-save worker for a previously opened RAW: {error}"
                        ),
                    );
                }
            }
        }
    }

    pub(super) fn poll_sidecar_save(&mut self) {
        let received = self
            .sidecar_receiver
            .as_ref()
            .map(|receiver| receiver.try_recv());
        let event = match received {
            Some(Ok(event)) => Some(event),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                let job = self.sidecar_in_flight.take();
                self.sidecar_receiver = None;
                if let Some(job) = job.filter(|job| job.generation == self.sidecar_generation) {
                    self.report_sidecar_save_failure(
                        Some(job.revision),
                        "the edit-save worker stopped unexpectedly",
                    );
                } else if job.is_some() {
                    self.report_sidecar_save_failure(
                        None,
                        "the edit-save worker for a previously opened RAW stopped unexpectedly",
                    );
                }
                None
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => None,
        };

        if let Some(event) = event {
            self.finish_sidecar_save(event);
        }

        self.start_next_sidecar_save();
    }

    pub(super) fn finish_sidecar_save(&mut self, event: SidecarSaveEvent) {
        self.sidecar_receiver = None;
        self.sidecar_in_flight = None;
        if let Some(raw_path) = event.raw_path.as_deref() {
            self.library
                .update_cloud_sync_state_for_cached_raw(raw_path, &self.egui_ctx);
        }
        if event.job.generation == self.sidecar_generation {
            match event.result {
                Ok(location) => {
                    let recovered_from_failure = self.sidecar_failed_revision.take().is_some();
                    self.sidecar_saved_revision = Some(event.job.revision);
                    self.queue_developed_thumbnail_refresh(
                        event.job.generation,
                        event.job.revision,
                    );
                    if event.job.explicit || recovered_from_failure {
                        self.notice = Some(format!("Edits saved to {location}."));
                    }
                    if event.job.explicit {
                        self.sidecar_save_feedback_until =
                            Some(Instant::now() + Duration::from_millis(1_200));
                        self.egui_ctx
                            .request_repaint_after(Duration::from_millis(1_200));
                    }
                }
                Err(error) => {
                    self.report_sidecar_save_failure(Some(event.job.revision), error);
                }
            }
        } else if let Err(error) = event.result {
            self.report_sidecar_save_failure(
                None,
                format!("saving a previously opened RAW failed: {error}"),
            );
        }
    }

    pub(super) fn install_developed_thumbnail_result(
        &mut self,
        target: &crate::sidecar::SidecarTarget,
        thumbnail: crate::pipeline::RawThumbnail,
        revision: u64,
    ) {
        match target {
            #[cfg(not(target_os = "android"))]
            crate::sidecar::SidecarTarget::Desktop { raw_path } => {
                self.library.install_developed_thumbnail(
                    raw_path,
                    thumbnail,
                    &self.egui_ctx,
                    revision,
                );
            }
            #[cfg(target_os = "android")]
            crate::sidecar::SidecarTarget::Desktop { .. } => {}
            #[cfg(target_os = "android")]
            crate::sidecar::SidecarTarget::Android { raw_uri, .. } => {
                self.library.install_android_developed_thumbnail(
                    raw_uri,
                    thumbnail,
                    &self.egui_ctx,
                    revision,
                );
            }
        }
    }

    pub(super) fn load_developed_thumbnail_for_target(
        &self,
        target: &crate::sidecar::SidecarTarget,
    ) -> Result<Option<crate::pipeline::RawThumbnail>, String> {
        match target {
            #[cfg(not(target_os = "android"))]
            crate::sidecar::SidecarTarget::Desktop { raw_path } => {
                crate::sidecar::load_developed_thumbnail_cache(raw_path, 512)
            }
            #[cfg(target_os = "android")]
            crate::sidecar::SidecarTarget::Desktop { .. } => Ok(None),
            #[cfg(target_os = "android")]
            crate::sidecar::SidecarTarget::Android {
                raw_uri,
                display_name,
            } => crate::android::load_developed_thumbnail_cache(
                &self.android_app,
                raw_uri,
                display_name,
                512,
            ),
        }
    }

    pub(super) fn queue_developed_thumbnail_refresh(&mut self, generation: u64, revision: u64) {
        if generation != self.sidecar_generation {
            return;
        }
        let Some(target) = self.sidecar_target.clone() else {
            return;
        };
        let job = DevelopedThumbnailJob {
            target,
            generation,
            revision,
        };

        // Explicitly saving an unchanged revision must not perform another GPU
        // readback. The exact sidecar fingerprint makes this reuse safe even on
        // filesystems whose modification timestamps have coarse resolution.
        match self.load_developed_thumbnail_for_target(&job.target) {
            Ok(Some(thumbnail)) => {
                self.install_developed_thumbnail_result(&job.target, thumbnail, revision);
                if self.developed_thumbnail_pending.as_ref() == Some(&job) {
                    self.developed_thumbnail_pending = None;
                }
                return;
            }
            Ok(None) => {}
            Err(error) => {
                log::warn!("could not validate developed thumbnail cache: {error}");
            }
        }

        if self.developed_thumbnail_in_flight.as_ref() == Some(&job)
            || self.developed_thumbnail_pending.as_ref() == Some(&job)
        {
            return;
        }
        self.developed_thumbnail_pending = Some(job);
        self.egui_ctx.request_repaint();
    }

    pub(super) fn poll_developed_thumbnail(&mut self, frame: &eframe::Frame) {
        let received = self
            .developed_thumbnail_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match received {
            Some(Ok(event)) => {
                self.developed_thumbnail_receiver = None;
                self.developed_thumbnail_in_flight = None;
                match &event.job.target {
                    crate::sidecar::SidecarTarget::Desktop { raw_path } => self
                        .library
                        .update_cloud_sync_state_for_cached_raw(raw_path, &self.egui_ctx),
                    #[cfg(target_os = "android")]
                    crate::sidecar::SidecarTarget::Android { .. } => {}
                }
                match event.result {
                    Ok(thumbnail) => self.install_developed_thumbnail_result(
                        &event.job.target,
                        thumbnail,
                        event.job.revision,
                    ),
                    Err(error) => {
                        // A changed sidecar is an expected race: the newer save
                        // queues another capture. Other failures are still useful
                        // diagnostics but should not interrupt editing.
                        if error.contains("sidecar changed") {
                            log::debug!("discarded stale developed thumbnail: {error}");
                        } else {
                            log::warn!("could not refresh developed thumbnail: {error}");
                        }
                    }
                }
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.developed_thumbnail_receiver = None;
                self.developed_thumbnail_in_flight = None;
                log::warn!("developed-thumbnail worker stopped unexpectedly");
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }

        if self.developed_thumbnail_in_flight.is_some() {
            return;
        }
        let Some(job) = self.developed_thumbnail_pending.clone() else {
            return;
        };
        if job.generation != self.sidecar_generation
            || self.sidecar_target.as_ref() != Some(&job.target)
        {
            self.developed_thumbnail_pending = None;
            return;
        }
        let current_revision = self.edit_commit_revision();
        if current_revision != job.revision || self.sidecar_saved_revision != Some(job.revision) {
            self.developed_thumbnail_pending = None;
            return;
        }
        if self.preview_quality_dirty
            || self.lens_correction_dirty
            || self.lens_correction_busy()
        {
            self.egui_ctx.request_repaint();
            return;
        }

        // Prefer the normal full-frame preview. While zoomed, edits deliberately
        // leave that proxy pending; the current tiny navigation pipeline is still
        // a complete adjusted full-frame image and is sufficient for a 512px card.
        let snapshot = if self.pending_stage.is_none() {
            self.gpu_pipeline
                .as_ref()
                .map(RawGpuPipeline::output_snapshot)
        } else if self.navigation_pending_stage.is_none() {
            self.preview_navigation
                .as_ref()
                .map(|preview| preview.pipeline.output_snapshot())
        } else {
            None
        };
        let Some(snapshot) = snapshot else {
            self.egui_ctx.request_repaint();
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.developed_thumbnail_pending = None;
            log::warn!("cannot cache developed thumbnail without the wgpu backend");
            return;
        };
        let device = render_state.device.clone();
        let queue = render_state.queue.clone();
        let repaint = self.egui_ctx.clone();
        let geometry = self.geometry;
        let worker_job = job.clone();
        let worker_target = job.target.clone();
        #[cfg(target_os = "android")]
        let android_app = self.android_app.clone();
        let (sender, receiver) = mpsc::channel();
        let spawn = std::thread::Builder::new()
            .name("auraw-developed-thumbnail".to_owned())
            .spawn(move || {
                let result = (|| {
                    let thumbnail = snapshot
                        .read_thumbnail_blocking(&device, &queue, 512)
                        .map_err(|error| format!("GPU thumbnail readback failed: {error:#}"))?;
                    let thumbnail =
                        crate::pipeline::transform_thumbnail_geometry(&thumbnail, geometry);
                    match &worker_target {
                        #[cfg(not(target_os = "android"))]
                        crate::sidecar::SidecarTarget::Desktop { raw_path } => {
                            let fingerprint = crate::sidecar::desktop_sidecar_fingerprint(
                                raw_path,
                            )?
                            .ok_or_else(|| {
                                "edit sidecar disappeared before thumbnail capture".to_owned()
                            })?;
                            crate::sidecar::save_developed_thumbnail_cache(
                                raw_path,
                                &thumbnail,
                                fingerprint,
                            )?;
                            if let Err(error) =
                                crate::cloud::upload_developed_thumbnail_if_cloud_cached(
                                    raw_path,
                                    &thumbnail,
                                )
                            {
                                log::warn!("could not sync cloud developed thumbnail: {error}");
                            }
                        }
                        #[cfg(target_os = "android")]
                        crate::sidecar::SidecarTarget::Desktop { raw_path } => {
                            let allow_network = crate::android::network_available(&android_app)
                                .unwrap_or_else(|error| {
                                    log::warn!(
                                        "could not inspect Android network state before cloud thumbnail sync: {error}"
                                    );
                                    true
                                });
                            if allow_network {
                                crate::cloud::upload_developed_thumbnail_if_cloud_cached(
                                    raw_path,
                                    &thumbnail,
                                )?;
                            }
                        }
                        #[cfg(target_os = "android")]
                        crate::sidecar::SidecarTarget::Android {
                            raw_uri,
                            display_name,
                        } => crate::android::save_developed_thumbnail_cache(
                            &android_app,
                            raw_uri,
                            display_name,
                            &thumbnail,
                        )?,
                    }
                    Ok(thumbnail)
                })();
                let _ = sender.send(DevelopedThumbnailEvent {
                    job: worker_job,
                    result,
                });
                repaint.request_repaint();
            });
        match spawn {
            Ok(_) => {
                self.developed_thumbnail_pending = None;
                self.developed_thumbnail_in_flight = Some(job);
                self.developed_thumbnail_receiver = Some(receiver);
            }
            Err(error) => {
                self.developed_thumbnail_pending = None;
                log::warn!("could not start developed-thumbnail worker: {error}");
            }
        }
    }

    pub(super) fn flush_sidecar_on_exit(&mut self) {
        self.commit_edit_history_now();
        let revision = self.edit_commit_revision();
        for request in &mut self.sidecar_pending {
            if request.generation == self.sidecar_generation && request.revision == revision {
                request.explicit = true;
            }
        }
        if self.sidecar_saved_revision != Some(revision)
            && !self.sidecar_in_flight.is_some_and(|job| {
                job.generation == self.sidecar_generation && job.revision == revision
            })
            && !self.sidecar_pending.iter().any(|request| {
                request.generation == self.sidecar_generation && request.revision == revision
            })
        {
            self.queue_current_sidecar_save(true);
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            self.start_next_sidecar_save();
            if self.sidecar_in_flight.is_none() && self.sidecar_pending.is_empty() {
                break;
            }
            let Some(receiver) = self.sidecar_receiver.as_ref() else {
                if Instant::now() >= deadline {
                    break;
                }
                continue;
            };
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            match receiver.recv_timeout(remaining) {
                Ok(event) => self.finish_sidecar_save(event),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let job = self.sidecar_in_flight.take();
                    self.sidecar_receiver = None;
                    let revision = job
                        .filter(|job| job.generation == self.sidecar_generation)
                        .map(|job| job.revision);
                    self.report_sidecar_save_failure(
                        revision,
                        "the edit-save worker stopped unexpectedly while finishing the save",
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        if self.sidecar_in_flight.is_some() || !self.sidecar_pending.is_empty() {
            let revision = self
                .sidecar_in_flight
                .filter(|job| job.generation == self.sidecar_generation)
                .map(|job| job.revision);
            self.report_sidecar_save_failure(
                revision,
                "timed out while finishing the edit save during shutdown",
            );
        }
    }
}

pub(super) fn save_sidecar_request(
    request: SidecarSaveRequest,
    #[cfg(target_os = "android")] android_app: &auraw_ffi::AndroidApp,
) -> Result<String, String> {
    match request.target {
        crate::sidecar::SidecarTarget::Desktop { raw_path } => {
            let path = crate::sidecar::save_desktop(&raw_path, request.edits)
                .map_err(|error| error.to_string())?;
            #[cfg(target_os = "android")]
            let allow_network =
                crate::android::network_available(android_app).unwrap_or_else(|error| {
                    log::warn!(
                        "could not inspect Android network state before cloud save: {error}"
                    );
                    true
                });
            #[cfg(not(target_os = "android"))]
            let allow_network = true;
            crate::cloud::sync_sidecar_if_cloud_cached(&raw_path, allow_network)
                .map(|location| location.unwrap_or_else(|| path.display().to_string()))
        }
        #[cfg(target_os = "android")]
        crate::sidecar::SidecarTarget::Android {
            raw_uri,
            display_name,
        } => crate::sidecar::save_android(android_app, &raw_uri, &display_name, request.edits)
            .map_err(|error| error.to_string()),
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod sidecar_persistence_tests {
    use super::*;

    #[test]
    fn autosave_deadline_does_not_slide_for_continuous_commits() {
        let started = Instant::now();
        let first = autosave_deadline(None, 7, started);
        let later = autosave_deadline(Some(first), 7, started + Duration::from_millis(500));

        assert_eq!(later.generation, 7);
        assert_eq!(later.due_at, first.due_at);
    }

    #[test]
    fn autosave_deadline_is_scoped_to_the_open_image() {
        let started = Instant::now();
        let old = autosave_deadline(None, 2, started);
        let switched_at = started + Duration::from_millis(100);
        let new = autosave_deadline(Some(old), 3, switched_at);

        assert_eq!(new.generation, 3);
        assert_eq!(new.due_at, switched_at + SIDECAR_AUTOSAVE_INTERVAL);
    }
}
