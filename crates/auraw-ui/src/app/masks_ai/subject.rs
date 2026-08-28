use super::*;

impl AurawApp {
    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_birefnet_quality(&mut self, quality: BiRefNetQuality) {
        if self.ai.birefnet_quality == quality {
            return;
        }
        self.ai.birefnet_quality = quality;
        self.masks.subject_cache = None;
        self.persist_performance_settings();
    }

    pub(crate) fn birefnet_quality_change_enabled(&self) -> bool {
        !self.foreground_operation_is(ForegroundOperationKind::SubjectMask)
    }

    pub(crate) fn request_subject_mask(&mut self, frame: &eframe::Frame) {
        self.ai.object_error_dialog = None;
        if self.foreground_operation_active() {
            self.ui.notice =
                Some("Finish or cancel the current editing operation first.".to_owned());
            return;
        }
        if let Some(mask) = self.masks.subject_cache.clone() {
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
        if crate::ai_masks::birefnet_model_is_verified(self.ai.birefnet_quality, &path) {
            self.start_subject_worker(path, false);
        } else {
            self.ai.subject_consent_open = true;
        }
    }

    pub(in crate::app) fn start_subject_worker(
        &mut self,
        model_path: PathBuf,
        allow_download: bool,
    ) {
        if self.foreground_operation_active() {
            return;
        }
        let Some(source) = self.masks.source_cache.clone() else {
            self.ui.notice =
                Some("The preview could not be prepared for subject selection.".to_owned());
            return;
        };
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.ai.runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.ai.runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;

        let model_present =
            crate::ai_masks::birefnet_model_is_verified(self.ai.birefnet_quality, &model_path);
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let receiver = spawn_subject_mask(
            SubjectMaskWorkerRequest {
                quality: self.ai.birefnet_quality,
                model_path,
                allow_download,
                runtime_path,
                runtime_sha256,
                width: source.width,
                height: source.height,
                rgba: source.rgba.to_vec(),
            },
            Arc::clone(&cancellation),
        );
        let progress = ForegroundProgress::indeterminate(if model_present {
            format!(
                "Running {} quality locally with {}…",
                self.ai.birefnet_quality.label(),
                self.ai.birefnet_quality.model().checkpoint
            )
        } else {
            format!(
                "Preparing {} download…",
                self.ai.birefnet_quality.model().download_label
            )
        });
        self.begin_foreground_operation(ForegroundOperation {
            kind: ForegroundOperationKind::SubjectMask,
            document_id: self.persistence.sidecar_generation,
            cancellation,
            progress,
            cancelling: false,
            receiver: ForegroundOperationReceiver::Subject(receiver),
            context: ForegroundOperationContext::Subject,
        });
    }

    pub(in crate::app) fn apply_subject_mask(&mut self, mask: MaskImage) {
        self.masks.subject_cache = Some(mask.clone());
        for local_mask in &mut self.masks.stack.masks {
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
        if !self.foreground_operation_is(ForegroundOperationKind::SubjectMask) {
            return;
        }
        let Some(mut operation) = self.foreground_operation.take() else {
            return;
        };
        let ForegroundOperationReceiver::Subject(receiver) = &operation.receiver else {
            self.foreground_operation = Some(operation);
            return;
        };
        let (events, disconnected) = drain_worker_events(Some(receiver), |event| {
            matches!(event, SubjectMaskEvent::Finished(_))
        });

        let mut finished = None;
        for event in events {
            match event {
                SubjectMaskEvent::DownloadProgress(progress) => {
                    operation.progress = ForegroundProgress::units(
                        progress.downloaded,
                        progress.total,
                        Some("bytes".to_owned()),
                        format!("Downloading {}", progress.label),
                    )
                    .with_detail(format!(
                        "{:.1} / {:.1} MB",
                        progress.downloaded as f64 / 1_000_000.0,
                        progress.total as f64 / 1_000_000.0
                    ));
                }
                SubjectMaskEvent::Inferencing => {
                    operation.progress = ForegroundProgress::indeterminate(format!(
                        "Running {} quality locally with {}…",
                        self.ai.birefnet_quality.label(),
                        self.ai.birefnet_quality.model().checkpoint
                    ));
                }
                SubjectMaskEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err(
                "The subject-mask worker stopped unexpectedly.".to_owned()
            ));
        }
        let Some(result) = finished else {
            self.foreground_operation = Some(operation);
            return;
        };

        let updating_all = self.ai.mask_update_active && self.ai.mask_update_subject_pending;
        let library_refresh = self.ai.library_mask_refresh.is_some();
        let cancelled = operation.is_cancelled();
        let stale = operation.document_id != self.persistence.sidecar_generation;

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
                Err(error) => error_message = Some(format!("Subject selection failed: {error}")),
            }
        }

        if library_refresh {
            if cancelled {
                self.cancel_ai_mask_update();
            } else if updating_all {
                self.ai.mask_update_subject_pending = false;
                self.ai.mask_update_failed |= !succeeded;
                if let Some(message) = error_message {
                    self.ui.notice = Some(message);
                }
                self.continue_ai_mask_update();
            }
        } else if !cancelled && !succeeded {
            let message = error_message.unwrap_or_else(|| {
                if stale {
                    "Subject selection became stale before inference completed.".to_owned()
                } else {
                    "Subject selection did not produce a mask.".to_owned()
                }
            });
            self.ui.notice = Some(message);
        }
        self.egui_ctx.request_repaint();
    }
}
