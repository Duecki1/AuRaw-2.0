use super::*;

impl AurawApp {
    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_birefnet_quality(&mut self, quality: BiRefNetQuality) {
        if self.birefnet_quality == quality {
            return;
        }
        self.birefnet_quality = quality;
        // A cached alpha belongs to the checkpoint that produced it. Keep the
        // currently displayed mask until its replacement succeeds, but never
        // let a request at the new tier reuse the previous tier's result.
        self.subject_mask_cache = None;
        self.subject_generation = self.subject_generation.wrapping_add(1);
        self.persist_performance_settings();
    }

    pub(crate) fn birefnet_quality_change_enabled(&self) -> bool {
        self.subject_task_id.is_none() && self.subject_receiver.is_none()
    }

    pub(crate) fn request_subject_mask(&mut self, frame: &eframe::Frame) {
        self.object_error_dialog = None;
        self.recover_terminal_ai_mask_task_owners();
        if let Some(mask) = self.subject_mask_cache.clone() {
            self.apply_subject_mask(mask);
            return;
        }
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
            return;
        }
        if let Err(error) = self.capture_mask_source(frame) {
            self.report_ai_mask_error(error);
            return;
        }
        let path = self.birefnet_model_path();
        if path.is_file() {
            self.start_subject_worker(path);
        } else {
            self.subject_consent_open = true;
        }
    }

    pub(in crate::app) fn start_subject_worker(&mut self, model_path: PathBuf) {
        if self.subject_task_id.is_some() || self.subject_receiver.is_some() {
            return;
        }
        let Some(source) = self.mask_source_cache.clone() else {
            self.notice =
                Some("The preview could not be prepared for subject selection.".to_owned());
            return;
        };
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.onnx_runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.onnx_runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;

        self.subject_generation = self.subject_generation.wrapping_add(1);
        let generation = self.subject_generation;
        let request = SubjectMaskTaskRequest {
            document_id: self.sidecar_generation,
            generation,
            quality: self.birefnet_quality,
            source,
            model_path,
            runtime_path,
            runtime_sha256,
        };

        if let Some(task_id) = self.library_ai_mask_refresh_task_id.filter(|task_id| {
            self.background_tasks.current_id() == Some(*task_id) && self.ai_mask_update_active
        }) {
            self.start_subject_mask_task(task_id, request);
        } else {
            // Full SHA-256 verification belongs in the worker. Hashing the
            // large subject model here blocks Android's UI thread long
            // enough to look like a hung mask operation.
            let needs_download = !request.model_path.is_file();
            if needs_download {
                let task_id = self.enqueue_background_action(
                    TaskKind::SubjectMask {
                        document_id: request.document_id,
                        generation,
                    },
                    "Downloading subject-mask model",
                    TaskProgress::indeterminate("Waiting for earlier background work…"),
                    true,
                    BackgroundAction::SubjectMask(request),
                );
                self.subject_task_id = Some(task_id);
            } else {
                let task_id = self.background_tasks.start_nonblocking(
                    TaskKind::SubjectMask {
                        document_id: request.document_id,
                        generation,
                    },
                    "Generating subject mask",
                    TaskProgress::indeterminate("Running local subject-mask inference…"),
                    true,
                );
                self.start_subject_mask_task(task_id, request);
            }
        }
        self.egui_ctx.request_repaint();
    }

    pub(in crate::app) fn apply_subject_mask(&mut self, mask: MaskImage) {
        // Keep BiRefNet output raw. The shared SubjectRefinement is composited
        // while each dirty atlas layer is rasterized, so a regenerated mask or
        // a newly added Subject/Background component inherits it automatically.
        self.subject_mask_cache = Some(mask.clone());
        for local_mask in &mut self.masks.masks {
            for component in &mut local_mask.components {
                if matches!(component.kind, MaskKind::Subject | MaskKind::Background) {
                    if let crate::pipeline::MaskGeometry::Ai { mask: target, .. } =
                        &mut component.geometry
                    {
                        *target = Some(mask.clone());
                    }
                }
            }
        }
        self.mark_all_mask_layers_dirty();
        self.blink_selected_mask();
    }

    pub(in crate::app) fn poll_subject_worker(&mut self) {
        let Some(task_id) = self.subject_task_id else {
            return;
        };
        let (events, disconnected) = drain_worker_events(
            self.subject_receiver.as_ref(),
            |event| matches!(event, SubjectMaskEvent::Finished(_)),
        );

        let mut finished = None;
        for event in events {
            match event {
                SubjectMaskEvent::DownloadProgress {
                    label,
                    downloaded,
                    total,
                } => {
                    self.subject_download_progress = Some((label, downloaded, total));
                    self.subject_inferencing = false;
                    self.background_tasks.set_global_visible(task_id, true);
                    self.background_tasks
                        .rename(task_id, "Downloading subject-mask model");
                    let progress = TaskProgress::units(
                        downloaded,
                        total,
                        Some("bytes".to_owned()),
                        format!("Downloading {label}"),
                    )
                    .with_detail(format!(
                        "{:.1} / {:.1} MB",
                        downloaded as f64 / 1_000_000.0,
                        total as f64 / 1_000_000.0
                    ));
                    self.update_background_progress(Some(task_id), progress);
                }
                SubjectMaskEvent::Inferencing => {
                    self.subject_download_progress = None;
                    self.subject_inferencing = true;
                    self.background_tasks.set_global_visible(task_id, false);
                    if matches!(
                        self.background_tasks.snapshot(task_id).map(|task| task.kind),
                        Some(TaskKind::SubjectMask { .. })
                    ) {
                        self.background_tasks.release_current(task_id);
                    }
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::indeterminate(format!(
                            "Running {} quality locally with {}…",
                            self.birefnet_quality.label(),
                            self.birefnet_quality.model().checkpoint
                        )),
                    );
                }
                SubjectMaskEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err("The subject-mask worker stopped unexpectedly.".to_owned()));
        }
        let Some(result) = finished else {
            return;
        };

        let updating_all = self.ai_mask_update_active && self.ai_mask_update_subject_pending;
        let library_task = self.library_ai_mask_refresh_task_id == Some(task_id);
        let cancelled = self.background_task_cancelled(task_id);
        let stale = self.subject_job_document_id != self.sidecar_generation
            || self.subject_job_generation != self.subject_generation;
        self.subject_receiver = None;
        self.subject_task_id = None;
        self.subject_download_progress = None;
        self.subject_inferencing = false;

        let mut succeeded = false;
        let mut error_message = None;
        if !cancelled && !stale {
            match result {
                Ok(result) => {
                    if let Some(mask) = result.into_probability_mask() {
                        self.apply_subject_mask(mask);
                        succeeded = true;
                    } else {
                        error_message =
                            Some("Subject selection returned an invalid mask image.".to_owned());
                    }
                }
                Err(error) => {
                    error_message = Some(format!("Subject selection failed: {error}"));
                }
            }
        }

        if library_task {
            if cancelled {
                self.cancel_ai_mask_update();
            } else if updating_all {
                self.ai_mask_update_subject_pending = false;
                self.ai_mask_update_failed |= !succeeded;
                if let Some(message) = error_message.clone() {
                    self.notice = Some(message);
                }
                self.continue_ai_mask_update();
            }
        } else if cancelled || succeeded {
            self.finish_background_task(task_id);
        } else {
            let message = error_message
                .unwrap_or_else(|| {
                    if stale {
                        "Subject selection became stale before inference completed.".to_owned()
                    } else {
                        "Subject selection did not produce a mask.".to_owned()
                    }
                });
            self.notice = Some(message.clone());
            self.fail_background_task(task_id, message);
        }
        self.egui_ctx.request_repaint();
    }
}
