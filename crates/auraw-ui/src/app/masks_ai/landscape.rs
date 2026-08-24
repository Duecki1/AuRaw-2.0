use super::*;

impl AurawApp {
    pub(crate) fn request_landscape_mask(
        &mut self,
        frame: &eframe::Frame,
        mask_index: usize,
        component_index: usize,
    ) {
        self.ai.object_error_dialog = None;
        if self.foreground_operation_active() {
            self.ui.notice =
                Some("Finish or cancel the current editing operation first.".to_owned());
            return;
        }
        let valid = self
            .masks
            .stack
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .is_some_and(|component| {
                matches!(
                    (component.kind, &component.geometry),
                    (MaskKind::Landscape, MaskGeometry::Landscape { .. })
                )
            });
        if !valid {
            self.ui.notice = Some("The selected landscape mask is no longer available.".to_owned());
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
        let path = self.landscape_model_path();
        if crate::ai_masks::landscape_model_is_verified(&path)
            && crate::ai_masks::vitmatte_model_is_verified(&self.vitmatte_model_path())
        {
            self.start_landscape_worker(mask_index, component_index, path, false);
        } else {
            self.ai.landscape_pending_target = Some((mask_index, component_index));
            self.ai.landscape_consent_open = true;
            self.egui_ctx.request_repaint();
        }
    }

    pub(in crate::app) fn start_landscape_worker(
        &mut self,
        mask_index: usize,
        component_index: usize,
        model_path: PathBuf,
        allow_download: bool,
    ) {
        if self.foreground_operation_active() {
            return;
        }
        let Some(source) = self.masks.source_cache.clone() else {
            self.ui.notice =
                Some("The preview could not be prepared for landscape selection.".to_owned());
            return;
        };
        let vitmatte_path = self.vitmatte_model_path();
        let needs_download = !crate::ai_masks::landscape_model_is_verified(&model_path)
            || !crate::ai_masks::vitmatte_model_is_verified(&vitmatte_path);
        if needs_download && !allow_download {
            self.ai.landscape_pending_target = Some((mask_index, component_index));
            self.ai.landscape_consent_open = true;
            self.egui_ctx.request_repaint();
            return;
        }
        let Some(category) = self
            .masks
            .stack
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .and_then(|component| match &component.geometry {
                MaskGeometry::Landscape { category, .. } => Some(*category),
                _ => None,
            })
        else {
            self.ui.notice = Some("The selected landscape mask is no longer available.".to_owned());
            return;
        };
        let Some(target) = self.masks.capture_ai_target(mask_index, component_index) else {
            self.ui.notice = Some("The selected landscape mask is no longer available.".to_owned());
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

        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let receiver = spawn_landscape_mask(
            LandscapeMaskWorkerRequest {
                model_path,
                vitmatte_path,
                allow_download,
                runtime_path,
                runtime_sha256,
                width: source.width,
                height: source.height,
                rgba: source.rgba.to_vec(),
                category,
            },
            Arc::clone(&cancellation),
        );
        let progress = ForegroundProgress::indeterminate(if needs_download {
            "Preparing landscape-mask models…"
        } else {
            "Running local landscape-mask inference…"
        });
        self.begin_foreground_operation(ForegroundOperation {
            kind: ForegroundOperationKind::LandscapeMask,
            document_id: self.persistence.sidecar_generation,
            cancellation,
            progress,
            cancelling: false,
            receiver: ForegroundOperationReceiver::Landscape(receiver),
            context: ForegroundOperationContext::Landscape { target, category },
        });
        self.egui_ctx.request_repaint();
    }

    pub(in crate::app) fn poll_landscape_worker(&mut self) {
        if !self.foreground_operation_is(ForegroundOperationKind::LandscapeMask) {
            return;
        }
        let Some(mut operation) = self.foreground_operation.take() else {
            return;
        };
        let ForegroundOperationReceiver::Landscape(receiver) = &operation.receiver else {
            self.foreground_operation = Some(operation);
            return;
        };
        let (events, disconnected) = drain_worker_events(Some(receiver), |event| {
            matches!(event, LandscapeMaskEvent::Finished(_))
        });
        let mut finished = None;
        for event in events {
            match event {
                LandscapeMaskEvent::DownloadProgress(progress) => {
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
                LandscapeMaskEvent::Inferencing => {
                    operation.progress = ForegroundProgress::indeterminate(
                        "Running local landscape-mask inference…",
                    );
                }
                LandscapeMaskEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err(
                "The landscape-mask worker stopped unexpectedly.".to_owned()
            ));
        }
        let Some(result) = finished else {
            self.foreground_operation = Some(operation);
            return;
        };

        let (target, job_category) = match &operation.context {
            ForegroundOperationContext::Landscape { target, category } => {
                (Some(target.clone()), Some(*category))
            }
            _ => (None, None),
        };
        let updating_all = self.ai.mask_update_active && target.is_some();
        let library_refresh = self.ai.library_mask_refresh.is_some();
        let cancelled = operation.is_cancelled();
        let stale = operation.document_id != self.persistence.sidecar_generation;

        let mut succeeded = false;
        let mut error_message = None;
        if !cancelled && !stale {
            match (target, job_category, result) {
                (Some(target), Some(expected_category), Ok(result)) => {
                    let location = self.masks.resolve_ai_target(&target);
                    let mask_image = MaskImage::new(result.width, result.height, result.mask);
                    match (location, mask_image) {
                        (_, None) => {
                            error_message = Some(
                                "Landscape selection returned malformed dimensions or pixel data."
                                    .to_owned(),
                            );
                        }
                        (Err(error), Some(_)) => error_message = Some(error),
                        (Ok((mask_index, component_index)), Some(mask_image)) => {
                            let applied = self
                                .masks
                                .stack
                                .masks
                                .get_mut(mask_index)
                                .and_then(|mask| mask.components.get_mut(component_index))
                                .is_some_and(|component| {
                                    if let MaskGeometry::Landscape { mask, category, .. } =
                                        &mut component.geometry
                                    {
                                        if *category == expected_category {
                                            *mask = Some(mask_image);
                                            return true;
                                        }
                                    }
                                    false
                                });
                            if applied {
                                self.mark_mask_geometry_dirty(mask_index);
                                self.blink_selected_component();
                                succeeded = true;
                            } else {
                                error_message = Some(
                                    "The target component changed type before inference completed."
                                        .to_owned(),
                                );
                            }
                        }
                    }
                }
                (_, _, Ok(_)) => {}
                (_, _, Err(error)) => {
                    error_message = Some(format!("Landscape selection failed: {error}"));
                }
            }
        }

        if library_refresh {
            if cancelled {
                self.cancel_ai_mask_update();
            } else if updating_all {
                self.ai.mask_update_failed |= !succeeded;
                if let Some(message) = error_message.clone() {
                    self.ui.notice = Some(message);
                }
                self.continue_ai_mask_update();
            }
        } else if !cancelled && !succeeded {
            let message = error_message.unwrap_or_else(|| {
                if stale {
                    "Landscape selection became stale before inference completed.".to_owned()
                } else {
                    "Landscape selection did not produce a mask.".to_owned()
                }
            });
            self.ui.notice = Some(message);
        }
        self.egui_ctx.request_repaint();
    }
}
