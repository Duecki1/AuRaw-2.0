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
        self.ui.notice = Some(message.clone());
        crate::diagnostics::record(&message);
        log::warn!("{message}");
        self.egui_ctx.request_repaint();
    }

    pub(super) fn report_sidecar_save_failure(&mut self, revision: Option<u64>, detail: impl AsRef<str>) {
        self.persistence.sidecar_save_feedback_until = None;
        if let Some(revision) = revision {
            self.persistence.sidecar_failed_revision = Some(revision);
        }

        let message = format!("Could not save edits: {}", detail.as_ref());
        self.ui.notice = Some(message.clone());
        self.persistence.sidecar_save_error_dialog = Some(message.clone());
        crate::diagnostics::record(format!("Edit save failed: {}", detail.as_ref()));
        log::error!("{message}");
        self.egui_ctx.request_repaint();
    }

    pub(super) fn show_sidecar_save_error_dialog(&mut self, ctx: &egui::Context) {
        let Some(message) = self.persistence.sidecar_save_error_dialog.clone() else {
            return;
        };
        let can_retry = self.can_save_edits()
            && !self.sidecar_save_in_progress()
            && self.persistence.sidecar_failed_revision == Some(self.edit_commit_revision());
        let mut retry = false;
        let mut close = false;
        crate::ui::responsive_popup(egui::Window::new("Could not save edits"), ctx, 460.0)
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("AuRaw was unable to write the edit sidecar.");
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(&message).monospace())
                        .wrap()
                        .selectable(true),
                );
                ui.add_space(6.0);
                ui.small("This error was added to the log in Settings → Diagnostics.");
                ui.add_space(8.0);
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
            });
        if retry {
            self.persistence.sidecar_save_error_dialog = None;
            self.save_edits_now();
        } else if close {
            self.persistence.sidecar_save_error_dialog = None;
        }
    }

    pub(super) fn capture_sidecar_edit_state(&self) -> SidecarEditState {
        let masks = self.committed_mask_state_for_persistence();
        let camera_profile = self.develop.selected_camera_profile.as_ref().and_then(|selected| {
            let root = self.preferences.camera_profile_folder.as_ref()?;
            if selected == root {
                return Some(std::path::PathBuf::from("."));
            }
            let relative = selected.strip_prefix(root).ok()?;
            (!relative.as_os_str().is_empty()).then(|| relative.to_path_buf())
        });
        SidecarEditState {
            exposure: self.develop.exposure,
            geometry: self.develop.geometry.sanitized(),
            camera_profile,
            subject_refinement: (!masks.subject_refinement.is_empty())
                .then(|| masks.subject_refinement.clone()),
            masks,
            lens: SidecarLensEditState {
                enabled: self.develop.lens_correction.enabled,
                maker: self.develop.lens_correction.selected_maker.clone(),
                model: self.develop.lens_correction.selected_model.clone(),
            },
            ai_masks_need_update: self.ai.masks_need_update,
        }
    }

    /// Finalize and enqueue the old image before any per-image state is reset.
    /// Requests own both their target and edit snapshot, so a slow completion
    /// can never be redirected to the next RAW.
    pub(super) fn begin_sidecar_open(&mut self) -> u64 {
        self.commit_edit_history_now();
        let revision = self.edit_commit_revision();
        let pending_latest = self.persistence.sidecar_pending
            .iter_mut()
            .find(|request| {
                request.generation == self.persistence.sidecar_generation && request.revision == revision
            })
            .map(|request| request.explicit = true)
            .is_some();
        let already_queued = self.persistence.sidecar_in_flight.is_some_and(|job| {
            job.generation == self.persistence.sidecar_generation && job.revision == revision
        }) || pending_latest;
        if self.persistence.sidecar_saved_revision != Some(revision) && !already_queued {
            // Switching images is the one automatic retry for a previously
            // failed latest revision. Once captured, the immutable request is
            // safe even after the active target changes.
            self.queue_current_sidecar_save(true);
        }
        self.start_next_sidecar_save();

        self.persistence.sidecar_generation = self.persistence.sidecar_generation.wrapping_add(1);
        self.persistence.sidecar_target = None;
        self.persistence.sidecar_saved_revision = None;
        self.persistence.sidecar_failed_revision = None;
        self.persistence.sidecar_autosave_deadline = None;
        self.persistence.sidecar_generation
    }

    pub(super) fn install_sidecar_target(
        &mut self,
        target: crate::sidecar::SidecarTarget,
        generation: u64,
        needs_rewrite: bool,
    ) {
        if generation != self.persistence.sidecar_generation {
            return;
        }
        self.persistence.sidecar_target = Some(target);
        self.persistence.sidecar_failed_revision = None;
        self.persistence.sidecar_saved_revision = (!needs_rewrite).then(|| self.edit_commit_revision());
        if needs_rewrite {
            self.queue_current_sidecar_save(false);
            self.start_next_sidecar_save();
        } else {
            #[cfg(not(target_os = "android"))]
            if self.persistence.sidecar_target
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
        let Some(target) = self.persistence.sidecar_target.clone() else {
            return;
        };
        let generation = self.persistence.sidecar_generation;
        let revision = self.edit_commit_revision();

        if !explicit
            && (self.persistence.sidecar_saved_revision == Some(revision)
                || self.persistence.sidecar_failed_revision == Some(revision)
                || self.persistence.sidecar_in_flight
                    .is_some_and(|job| job.generation == generation && job.revision == revision)
                || self.persistence.sidecar_pending.iter().any(|request| {
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
        if let Some(index) = self.persistence.sidecar_pending
            .iter()
            .position(|pending| pending.generation == generation)
        {
            request.explicit |= self.persistence.sidecar_pending[index].explicit;
            self.persistence.sidecar_pending[index] = request;
        } else {
            self.persistence.sidecar_pending.push_back(request);
        }
    }

    pub(super) fn schedule_sidecar_autosave(&mut self, ctx: &egui::Context, interaction_active: bool) {
        if self.develop.loaded_raw.is_none() || self.persistence.sidecar_target.is_none() {
            self.persistence.sidecar_autosave_deadline = None;
            return;
        }

        let generation = self.persistence.sidecar_generation;
        let revision = self.edit_commit_revision();
        let revision_is_covered = self.persistence.sidecar_saved_revision == Some(revision)
            || self.persistence.sidecar_failed_revision == Some(revision)
            || self.persistence.sidecar_in_flight
                .is_some_and(|job| job.generation == generation && job.revision == revision)
            || self.persistence.sidecar_pending
                .iter()
                .any(|request| request.generation == generation && request.revision == revision);
        if revision_is_covered {
            self.persistence.sidecar_autosave_deadline = None;
            self.start_next_sidecar_save();
            return;
        }

        let stale_pending = self.persistence.sidecar_pending
            .iter()
            .any(|request| request.generation == generation && request.revision != revision);
        if stale_pending && !interaction_active {
            // This generation already reached an earlier deadline while a
            // worker or interaction kept it queued. Replace that snapshot
            // with the newest committed value and preserve any explicit bit.
            self.persistence.sidecar_autosave_deadline = None;
            self.queue_current_sidecar_save(false);
            self.start_next_sidecar_save();
            return;
        }

        let now = Instant::now();
        let deadline = autosave_deadline(self.persistence.sidecar_autosave_deadline, generation, now);
        self.persistence.sidecar_autosave_deadline = Some(deadline);
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

        self.persistence.sidecar_autosave_deadline = None;
        self.queue_current_sidecar_save(false);
        self.start_next_sidecar_save();
    }

    pub(crate) fn detach_current_file_for_library_action(
        &mut self,
        raw_path: &std::path::Path,
    ) -> bool {
        if self.develop.current_path.as_deref() != Some(raw_path) {
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
            self.persistence.sidecar_target.as_ref(),
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
            &self.android.android_app,
            raw_uri,
            display_name,
        );
        if was_current && result.is_ok() {
            self.reload_android_library_document_after_reset(raw_uri, display_name);
        }
        result
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
            &self.android.android_app,
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
            crate::android::delete_library_document(&self.android.android_app, raw_uri, display_name);
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
        let detached_generation = self.persistence.sidecar_generation;
        self.persistence.sidecar_generation = self.persistence.sidecar_generation.wrapping_add(1);
        self.persistence.sidecar_target = None;
        self.persistence.sidecar_saved_revision = None;
        self.persistence.sidecar_failed_revision = None;
        self.persistence.sidecar_autosave_deadline = None;
        self.persistence.sidecar_pending
            .retain(|request| request.generation != detached_generation);
        true
    }

    pub(crate) fn can_save_edits(&self) -> bool {
        self.develop.loaded_raw.is_some() && self.persistence.sidecar_target.is_some()
    }

    pub(crate) fn sidecar_save_in_progress(&self) -> bool {
        self.persistence.sidecar_in_flight
            .is_some_and(|job| job.generation == self.persistence.sidecar_generation)
            || self.persistence.sidecar_pending
                .iter()
                .any(|request| request.generation == self.persistence.sidecar_generation)
    }

    pub(crate) fn sidecar_save_succeeded_recently(&self) -> bool {
        self.persistence.sidecar_save_feedback_until
            .is_some_and(|until| Instant::now() < until)
    }

    pub(crate) fn save_edits_now(&mut self) {
        if !self.can_save_edits() {
            return;
        }
        self.commit_edit_history_now();
        self.persistence.sidecar_save_feedback_until = None;
        self.persistence.sidecar_failed_revision = None;
        self.queue_current_sidecar_save(true);
        self.start_next_sidecar_save();
        self.ui.notice = Some("Saving edits…".to_owned());
    }

    pub(crate) fn handle_sidecar_shortcut(&mut self, ctx: &egui::Context) {
        let save = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::S);
        if self.can_save_edits() && ctx.input_mut(|input| input.consume_shortcut(&save)) {
            self.save_edits_now();
        }
    }

    pub(super) fn start_next_sidecar_save(&mut self) {
        if self.persistence.sidecar_in_flight.is_some() {
            return;
        }
        if self.persistence.sidecar_pending.front().is_some_and(|request| {
            !request.explicit
                && request.generation == self.persistence.sidecar_generation
                && sidecar_interaction_active(&self.egui_ctx)
        }) {
            self.egui_ctx
                .request_repaint_after(SIDECAR_AUTOSAVE_ACTIVE_POLL);
            return;
        }
        let Some(request) = self.persistence.sidecar_pending.pop_front() else {
            return;
        };
        let job = SidecarSaveJob {
            generation: request.generation,
            revision: request.revision,
            explicit: request.explicit,
        };
        let repaint = self.egui_ctx.clone();
        let (sender, receiver) = mpsc::channel();
        #[cfg(target_os = "android")]
        let android_app = self.android.android_app.clone();

        let spawn = std::thread::Builder::new()
            .name("auraw-sidecar-save".to_owned())
            .spawn(move || {
                let result = save_sidecar_request(
                    request,
                    #[cfg(target_os = "android")]
                    &android_app,
                );
                let _ = sender.send(SidecarSaveEvent { job, result });
                repaint.request_repaint();
            });

        match spawn {
            Ok(_) => {
                self.persistence.sidecar_in_flight = Some(job);
                self.persistence.sidecar_receiver = Some(receiver);
            }
            Err(error) => {
                if job.generation == self.persistence.sidecar_generation {
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
        let received = self.persistence.sidecar_receiver
            .as_ref()
            .map(|receiver| receiver.try_recv());
        let event = match received {
            Some(Ok(event)) => Some(event),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                let job = self.persistence.sidecar_in_flight.take();
                self.persistence.sidecar_receiver = None;
                if let Some(job) = job.filter(|job| job.generation == self.persistence.sidecar_generation) {
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
        self.persistence.sidecar_receiver = None;
        self.persistence.sidecar_in_flight = None;
        if event.job.generation == self.persistence.sidecar_generation {
            match event.result {
                Ok(location) => {
                    let recovered_from_failure = self.persistence.sidecar_failed_revision.take().is_some();
                    self.persistence.sidecar_saved_revision = Some(event.job.revision);
                    self.queue_developed_thumbnail_refresh(
                        event.job.generation,
                        event.job.revision,
                    );
                    if event.job.explicit || recovered_from_failure {
                        self.ui.notice = Some(format!("Edits saved to {location}."));
                    }
                    if event.job.explicit {
                        self.persistence.sidecar_save_feedback_until =
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
                &self.android.android_app,
                raw_uri,
                display_name,
                512,
            ),
        }
    }

    pub(super) fn queue_developed_thumbnail_refresh(&mut self, generation: u64, revision: u64) {
        if generation != self.persistence.sidecar_generation {
            return;
        }
        let Some(target) = self.persistence.sidecar_target.clone() else {
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
                if self.persistence.developed_thumbnail_pending.as_ref() == Some(&job) {
                    self.persistence.developed_thumbnail_pending = None;
                }
                return;
            }
            Ok(None) => {}
            Err(error) => {
                log::warn!("could not validate developed thumbnail cache: {error}");
            }
        }

        if self.persistence.developed_thumbnail_in_flight.as_ref() == Some(&job)
            || self.persistence.developed_thumbnail_pending.as_ref() == Some(&job)
        {
            return;
        }
        self.persistence.developed_thumbnail_pending = Some(job);
        self.egui_ctx.request_repaint();
    }

    pub(super) fn poll_developed_thumbnail(&mut self, frame: &eframe::Frame) {
        let received = self.persistence.developed_thumbnail_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match received {
            Some(Ok(event)) => {
                self.persistence.developed_thumbnail_receiver = None;
                self.persistence.developed_thumbnail_in_flight = None;
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
                self.persistence.developed_thumbnail_receiver = None;
                self.persistence.developed_thumbnail_in_flight = None;
                log::warn!("developed-thumbnail worker stopped unexpectedly");
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }

        if self.persistence.developed_thumbnail_in_flight.is_some() {
            return;
        }
        let Some(job) = self.persistence.developed_thumbnail_pending.clone() else {
            return;
        };
        if job.generation != self.persistence.sidecar_generation
            || self.persistence.sidecar_target.as_ref() != Some(&job.target)
        {
            self.persistence.developed_thumbnail_pending = None;
            return;
        }
        let current_revision = self.edit_commit_revision();
        if current_revision != job.revision || self.persistence.sidecar_saved_revision != Some(job.revision) {
            self.persistence.developed_thumbnail_pending = None;
            return;
        }
        if self.preview.quality_dirty
            || self.develop.lens_correction_dirty
            || self.lens_correction_busy()
        {
            self.egui_ctx.request_repaint();
            return;
        }

        // Prefer the normal full-frame preview. While zoomed, edits deliberately
        // leave that proxy pending; the current tiny navigation pipeline is still
        // a complete adjusted full-frame image and is sufficient for a 512px card.
        let snapshot = if self.preview.pending_stage.is_none() {
            self.preview.gpu_pipeline
                .as_ref()
                .map(RawGpuPipeline::output_snapshot)
        } else if self.preview.navigation_pending_stage.is_none() {
            self.preview.navigation
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
            self.persistence.developed_thumbnail_pending = None;
            log::warn!("cannot cache developed thumbnail without the wgpu backend");
            return;
        };
        let device = render_state.device.clone();
        let queue = render_state.queue.clone();
        let repaint = self.egui_ctx.clone();
        let geometry = self.develop.geometry;
        let worker_job = job.clone();
        let worker_target = job.target.clone();
        #[cfg(target_os = "android")]
        let android_app = self.android.android_app.clone();
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
                            let fingerprint = crate::sidecar::desktop_sidecar_fingerprint(raw_path)?
                                .ok_or_else(|| {
                                    "edit sidecar disappeared before thumbnail capture".to_owned()
                                })?;
                            crate::sidecar::save_developed_thumbnail_cache(
                                raw_path,
                                &thumbnail,
                                fingerprint,
                            )?;
                        }
                        #[cfg(target_os = "android")]
                        crate::sidecar::SidecarTarget::Desktop { .. } => {},
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
                self.persistence.developed_thumbnail_pending = None;
                self.persistence.developed_thumbnail_in_flight = Some(job);
                self.persistence.developed_thumbnail_receiver = Some(receiver);
            }
            Err(error) => {
                self.persistence.developed_thumbnail_pending = None;
                log::warn!("could not start developed-thumbnail worker: {error}");
            }
        }
    }

    pub(super) fn flush_sidecar_on_exit(&mut self) {
        self.commit_edit_history_now();
        let revision = self.edit_commit_revision();
        for request in &mut self.persistence.sidecar_pending {
            if request.generation == self.persistence.sidecar_generation && request.revision == revision {
                request.explicit = true;
            }
        }
        if self.persistence.sidecar_saved_revision != Some(revision)
            && !self.persistence.sidecar_in_flight.is_some_and(|job| {
                job.generation == self.persistence.sidecar_generation && job.revision == revision
            })
            && !self.persistence.sidecar_pending.iter().any(|request| {
                request.generation == self.persistence.sidecar_generation && request.revision == revision
            })
        {
            self.queue_current_sidecar_save(true);
        }

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            self.start_next_sidecar_save();
            if self.persistence.sidecar_in_flight.is_none() && self.persistence.sidecar_pending.is_empty() {
                break;
            }
            let Some(receiver) = self.persistence.sidecar_receiver.as_ref() else {
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
                    let job = self.persistence.sidecar_in_flight.take();
                    self.persistence.sidecar_receiver = None;
                    let revision = job
                        .filter(|job| job.generation == self.persistence.sidecar_generation)
                        .map(|job| job.revision);
                    self.report_sidecar_save_failure(
                        revision,
                        "the edit-save worker stopped unexpectedly while finishing the save",
                    );
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        if self.persistence.sidecar_in_flight.is_some() || !self.persistence.sidecar_pending.is_empty() {
            let revision = self.persistence.sidecar_in_flight
                .filter(|job| job.generation == self.persistence.sidecar_generation)
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
            crate::sidecar::save_desktop(&raw_path, request.edits)
                .map(|path| path.display().to_string())
                .map_err(|error| error.to_string())
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
