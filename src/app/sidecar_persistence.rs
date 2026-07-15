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
    fn capture_sidecar_edit_state(&self) -> SidecarEditState {
        SidecarEditState {
            exposure: self.exposure,
            masks: self.committed_mask_state_for_persistence(),
            lens: SidecarLensEditState {
                enabled: self.lens_correction.enabled,
                maker: self.lens_correction.selected_maker.clone(),
                model: self.lens_correction.selected_model.clone(),
            },
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
        }
    }

    fn queue_current_sidecar_save(&mut self, explicit: bool) {
        let Some(target) = self.sidecar_target.clone() else {
            return;
        };
        let generation = self.sidecar_generation;
        let revision = self.edit_commit_revision();

        if !explicit {
            if self.sidecar_saved_revision == Some(revision)
                || self.sidecar_failed_revision == Some(revision)
                || self
                    .sidecar_in_flight
                    .is_some_and(|job| job.generation == generation && job.revision == revision)
                || self
                    .sidecar_pending
                    .iter()
                    .any(|request| request.generation == generation && request.revision == revision)
            {
                return;
            }
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
