const SIDECAR_AUTOSAVE_INTERVAL: Duration = Duration::from_millis(900);
const SIDECAR_AUTOSAVE_ACTIVE_POLL: Duration = Duration::from_millis(100);

fn autosave_deadline(
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

fn sidecar_interaction_active(ctx: &egui::Context) -> bool {
    ctx.input(|input| input.pointer.any_down()) || ctx.egui_wants_keyboard_input()
}

impl AurawApp {
    pub(super) fn capture_sidecar_edit_state(&self) -> SidecarEditState {
        let camera_profile = self
            .selected_camera_profile
            .as_ref()
            .zip(self.camera_profile_folder.as_ref())
            .and_then(|(selected, root)| selected.strip_prefix(root).ok())
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf);
        SidecarEditState {
            exposure: self.exposure,
            geometry: self.geometry.sanitized(),
            camera_profile,
            masks: self.committed_mask_state_for_persistence(),
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
    fn begin_sidecar_open(&mut self) -> u64 {
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

    fn install_sidecar_target(
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

    fn queue_current_sidecar_save(&mut self, explicit: bool) {
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
                || self
                    .sidecar_pending
                    .iter()
                    .any(|request| request.generation == generation && request.revision == revision))
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

    fn schedule_sidecar_autosave(&mut self, ctx: &egui::Context, interaction_active: bool) {
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

        let stale_pending = self.sidecar_pending.iter().any(|request| {
            request.generation == generation && request.revision != revision
        });
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

    #[cfg(not(target_os = "android"))]
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
    fn detach_current_android_document_for_library_action(
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
        let result = (|| {
            let Some(mut loaded) = crate::sidecar::load_android(
                &self.android_app,
                raw_uri,
                display_name,
            )
            .map_err(|error| error.to_string())?
            else {
                return Ok(());
            };
            crate::sidecar::reset_adjustments_preserving_mask_properties(&mut loaded.edits);
            crate::sidecar::save_android(
                &self.android_app,
                raw_uri,
                display_name,
                loaded.edits,
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
        })();
        if was_current {
            self.open_android_library_document(raw_uri, display_name);
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
    pub(crate) fn delete_android_library_item(
        &mut self,
        raw_uri: &str,
        display_name: &str,
    ) -> Result<(), String> {
        let was_current =
            self.detach_current_android_document_for_library_action(raw_uri, display_name);
        let result = crate::android::delete_library_document(
            &self.android_app,
            raw_uri,
            display_name,
        );
        if result.is_err() && was_current {
            self.open_android_library_document(raw_uri, display_name);
        }
        result
    }

    fn detach_current_sidecar_target_for_library_action(&mut self) -> bool {
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

    pub(crate) fn save_edits_now(&mut self) {
        if !self.can_save_edits() {
            return;
        }
        self.commit_edit_history_now();
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

    fn start_next_sidecar_save(&mut self) {
        if self.sidecar_in_flight.is_some() {
            return;
        }
        if self
            .sidecar_pending
            .front()
            .is_some_and(|request| {
                !request.explicit
                    && request.generation == self.sidecar_generation
                    && sidecar_interaction_active(&self.egui_ctx)
            })
        {
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
                let _ = sender.send(SidecarSaveEvent { job, result });
                repaint.request_repaint();
            });

        match spawn {
            Ok(_) => {
                self.sidecar_in_flight = Some(job);
                self.sidecar_receiver = Some(receiver);
            }
            Err(error) => {
                if job.generation == self.sidecar_generation {
                    self.sidecar_failed_revision = Some(job.revision);
                    self.notice = Some(format!("Could not start edit save worker: {error}"));
                }
                self.egui_ctx.request_repaint();
            }
        }
    }

    fn poll_sidecar_save(&mut self) {
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
                    self.sidecar_failed_revision = Some(job.revision);
                    self.notice = Some("Edit save worker stopped unexpectedly.".to_owned());
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

    fn finish_sidecar_save(&mut self, event: SidecarSaveEvent) {
        self.sidecar_receiver = None;
        self.sidecar_in_flight = None;
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
                }
                Err(error) => {
                    self.sidecar_failed_revision = Some(event.job.revision);
                    self.notice = Some(format!("Could not save edits: {error}"));
                }
            }
        } else if let Err(error) = event.result {
            log::warn!("sidecar save for an old RAW failed: {error}");
        }
    }

    fn install_developed_thumbnail_result(
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

    fn load_developed_thumbnail_for_target(
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

    fn queue_developed_thumbnail_refresh(&mut self, generation: u64, revision: u64) {
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
                self.install_developed_thumbnail_result(
                    &job.target,
                    thumbnail,
                    revision,
                );
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

    fn poll_developed_thumbnail(&mut self, frame: &eframe::Frame) {
        let received = self
            .developed_thumbnail_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match received {
            Some(Ok(event)) => {
                self.developed_thumbnail_receiver = None;
                self.developed_thumbnail_in_flight = None;
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
        if self.preview_quality_dirty || self.lens_correction_dirty {
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
                    let thumbnail = crate::pipeline::transform_thumbnail_geometry(&thumbnail, geometry);
                    match &worker_target {
                        #[cfg(not(target_os = "android"))]
                        crate::sidecar::SidecarTarget::Desktop { raw_path } => {
                            let fingerprint =
                                crate::sidecar::desktop_sidecar_fingerprint(raw_path)?.ok_or_else(
                                    || {
                                        "edit sidecar disappeared before thumbnail capture"
                                            .to_owned()
                                    },
                                )?;
                            crate::sidecar::save_developed_thumbnail_cache(
                                raw_path,
                                &thumbnail,
                                fingerprint,
                            )?;
                        }
                        #[cfg(target_os = "android")]
                        crate::sidecar::SidecarTarget::Desktop { .. } => {
                            return Err(
                                "desktop sidecar target is unavailable on Android".to_owned(),
                            );
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

    fn flush_sidecar_on_exit(&mut self) {
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
                    self.sidecar_receiver = None;
                    self.sidecar_in_flight = None;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
        if self.sidecar_in_flight.is_some() || !self.sidecar_pending.is_empty() {
            log::warn!("timed out while flushing the latest edit sidecar during shutdown");
        }
    }
}

fn save_sidecar_request(
    request: SidecarSaveRequest,
    #[cfg(target_os = "android")] android_app: &android_activity::AndroidApp,
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
