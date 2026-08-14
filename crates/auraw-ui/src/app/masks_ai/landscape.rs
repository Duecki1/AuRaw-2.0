use super::*;

impl AurawApp {
    pub(crate) fn request_landscape_mask(
        &mut self,
        frame: &eframe::Frame,
        mask_index: usize,
        component_index: usize,
    ) {
        self.object_error_dialog = None;
        self.recover_terminal_ai_mask_task_owners();
        if self.landscape_task_id.is_some() || self.landscape_receiver.is_some() {
            self.notice = Some("Wait for the current landscape mask to finish.".to_owned());
            return;
        }
        let valid = self
            .masks
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
            self.notice = Some("The selected landscape mask is no longer available.".to_owned());
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
            self.landscape_pending_target = Some((mask_index, component_index));
            self.landscape_consent_open = true;
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
        if self.landscape_task_id.is_some() || self.landscape_receiver.is_some() {
            return;
        }
        let Some(source) = self.mask_source_cache.clone() else {
            self.notice =
                Some("The preview could not be prepared for landscape selection.".to_owned());
            return;
        };
        let vitmatte_path = self.vitmatte_model_path();
        let needs_download = !crate::ai_masks::landscape_model_is_verified(&model_path)
            || !crate::ai_masks::vitmatte_model_is_verified(&vitmatte_path);
        if needs_download && !allow_download {
            self.landscape_pending_target = Some((mask_index, component_index));
            self.landscape_consent_open = true;
            self.egui_ctx.request_repaint();
            return;
        }
        let Some(category) = self
            .masks
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .and_then(|component| match &component.geometry {
                MaskGeometry::Landscape { category, .. } => Some(*category),
                _ => None,
            })
        else {
            self.notice = Some("The selected landscape mask is no longer available.".to_owned());
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

        self.landscape_generation = self.landscape_generation.wrapping_add(1);
        let generation = self.landscape_generation;
        let Some(target) = self.capture_ai_mask_target(mask_index, component_index) else {
            self.notice = Some("The selected landscape mask is no longer available.".to_owned());
            return;
        };
        let request = LandscapeMaskTaskRequest {
            document_id: self.sidecar_generation,
            generation,
            target,
            source,
            model_path,
            vitmatte_path,
            allow_download,
            runtime_path,
            runtime_sha256,
            category,
        };

        if let Some(task_id) = self.library_ai_mask_refresh_task_id.filter(|task_id| {
            self.background_tasks.current_id() == Some(*task_id) && self.ai_mask_update_active
        }) {
            self.start_landscape_mask_task(task_id, request);
        } else if needs_download {
            let task_id = self.enqueue_background_action(
                TaskKind::LandscapeMask {
                    document_id: request.document_id,
                    generation,
                },
                "Downloading landscape-mask model",
                TaskProgress::indeterminate("Waiting for earlier background work…"),
                true,
                BackgroundAction::LandscapeMask(request),
            );
            self.landscape_task_id = Some(task_id);
        } else {
            let task_id = self.background_tasks.start_nonblocking(
                TaskKind::LandscapeMask {
                    document_id: request.document_id,
                    generation,
                },
                "Generating landscape mask",
                TaskProgress::indeterminate("Running local landscape-mask inference…"),
                true,
            );
            self.start_landscape_mask_task(task_id, request);
        }
        self.egui_ctx.request_repaint();
    }

    pub(in crate::app) fn poll_landscape_worker(&mut self) {
        let Some(task_id) = self.landscape_task_id else {
            return;
        };
        let (events, disconnected) = drain_worker_events(
            self.landscape_receiver.as_ref(),
            |event| matches!(event, LandscapeMaskEvent::Finished(_)),
        );
        let mut finished = None;
        for event in events {
            match event {
                LandscapeMaskEvent::DownloadProgress {
                    label,
                    downloaded,
                    total,
                } => {
                    self.landscape_download_progress = Some((downloaded, total));
                    self.landscape_inferencing = false;
                    self.background_tasks.set_global_visible(task_id, true);
                    self.background_tasks
                        .rename(task_id, format!("Downloading {label}"));
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::units(
                            downloaded,
                            total,
                            Some("bytes".to_owned()),
                            format!("Downloading {label}"),
                        )
                        .with_detail(format!(
                            "{:.1} / {:.1} MB",
                            downloaded as f64 / 1_000_000.0,
                            total as f64 / 1_000_000.0
                        )),
                    );
                }
                LandscapeMaskEvent::Inferencing => {
                    self.landscape_download_progress = None;
                    self.landscape_inferencing = true;
                    self.background_tasks.set_global_visible(task_id, false);
                    if matches!(
                        self.background_tasks.snapshot(task_id).map(|task| task.kind),
                        Some(TaskKind::LandscapeMask { .. })
                    ) {
                        self.background_tasks.release_current(task_id);
                    }
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::indeterminate("Running local landscape-mask inference…"),
                    );
                }
                LandscapeMaskEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err(
                "The landscape-mask worker stopped unexpectedly.".to_owned(),
            ));
        }
        let Some(result) = finished else {
            return;
        };

        let target = self.landscape_job_target.clone();
        let job_category = self.landscape_job_category;
        let updating_all = self.ai_mask_update_active && target.is_some();
        let library_task = self.library_ai_mask_refresh_task_id == Some(task_id);
        let cancelled = self.background_task_cancelled(task_id);
        let stale = self.landscape_job_document_id != self.sidecar_generation
            || self.landscape_job_generation != self.landscape_generation;
        self.landscape_receiver = None;
        self.landscape_task_id = None;
        self.landscape_download_progress = None;
        self.landscape_inferencing = false;
        self.landscape_job_target = None;
        self.landscape_job_category = None;

        let mut succeeded = false;
        let mut error_message = None;
        if !cancelled && !stale {
            match (target, job_category, result) {
                (Some(target), Some(expected_category), Ok(result)) => {
                    let location = self.resolve_ai_mask_target(&target);
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

        if library_task {
            if cancelled {
                self.cancel_ai_mask_update();
            } else if updating_all {
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
                        "Landscape selection became stale before inference completed.".to_owned()
                    } else {
                        "Landscape selection did not produce a mask.".to_owned()
                    }
                });
            self.notice = Some(message.clone());
            self.fail_background_task(task_id, message);
        }
        self.egui_ctx.request_repaint();
    }
}
