use super::*;

impl AurawApp {
    pub(crate) fn restart_refined_object_mask_for_stroke(
        &mut self,
        mask_index: usize,
        component_index: usize,
    ) -> bool {
        let target = (mask_index, component_index);
        let cleared = self
            .masks
            .masks
            .get_mut(mask_index)
            .and_then(|mask| mask.components.get_mut(component_index))
            .is_some_and(|component| {
                let crate::pipeline::MaskGeometry::Object { mask, strokes, .. } =
                    &mut component.geometry
                else {
                    return false;
                };
                if mask.is_none() {
                    return false;
                }
                *mask = None;
                strokes.clear();
                true
            });
        if !cleared {
            return false;
        }

        // Any in-flight result or cached logits belong to the replaced mask.
        // Incrementing the generation makes that result stale without trying
        // to interrupt ONNX Runtime halfway through a session run.
        self.object_generation = self.object_generation.wrapping_add(1);
        if self.object_pending_target == Some(target) {
            self.object_pending_target = None;
        }
        if self
            .object_cache
            .as_ref()
            .is_some_and(|(cached_target, _)| *cached_target == target)
        {
            self.object_cache = None;
        }
        self.mask_overlay_blink = None;
        self.brush_mode = BrushMode::Paint;
        true
    }

    pub(crate) fn request_object_mask(&mut self, mask_index: usize, component_index: usize) {
        self.object_error_dialog = None;
        self.recover_terminal_ai_mask_task_owners();
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
            return;
        }
        let Some(component) = self
            .masks
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
        else {
            return;
        };
        let crate::pipeline::MaskGeometry::Object { strokes, .. } = &component.geometry else {
            return;
        };
        if !strokes
            .iter()
            .any(|stroke| stroke.positive && !stroke.points.is_empty())
        {
            self.notice = Some("Paint inside an object before generating its mask.".to_owned());
            return;
        }
        if self.mask_source_cache.is_none() {
            self.report_ai_mask_error(
                "The original image source is not ready for object selection. Re-open the Object mask or create it again."
                    .to_owned(),
            );
            return;
        }
        let (encoder, decoder) = self.sam21_model_paths();
        if encoder.is_file() && decoder.is_file() && self.vitmatte_model_path().is_file() {
            self.start_object_worker(mask_index, component_index, encoder, decoder);
        } else {
            self.object_pending_target = Some((mask_index, component_index));
            self.object_consent_open = true;
        }
    }

    pub(in crate::app) fn start_object_worker(
        &mut self,
        mask_index: usize,
        component_index: usize,
        encoder_path: PathBuf,
        decoder_path: PathBuf,
    ) {
        let library_task = self.library_ai_mask_refresh_task_id.filter(|task_id| {
            self.background_tasks.current_id() == Some(*task_id) && self.ai_mask_update_active
        });
        if library_task.is_none() {
            if let Some(existing) = self.object_task_id {
                self.cancel_background_task(existing);
                if self.object_receiver.is_some() {
                    // Native inference may only stop between phases. Keep the
                    // receiver paired with its stable task ID and retain only
                    // the newest requested target for the follow-up run.
                    self.object_pending_target = Some((mask_index, component_index));
                    self.object_generation = self.object_generation.wrapping_add(1);
                    return;
                }
            }
        } else if self.object_receiver.is_some() {
            self.object_pending_target = Some((mask_index, component_index));
            self.object_generation = self.object_generation.wrapping_add(1);
            return;
        }

        let Some(source) = self.mask_source_cache.clone() else {
            self.notice =
                Some("The original image source is unavailable for object selection.".to_owned());
            return;
        };
        let (strokes, brush_size, edge_refine) = {
            let Some(component) = self
                .masks
                .masks
                .get(mask_index)
                .and_then(|mask| mask.components.get(component_index))
            else {
                return;
            };
            let crate::pipeline::MaskGeometry::Object {
                strokes,
                brush_size,
                edge_refine,
                ..
            } = &component.geometry
            else {
                return;
            };
            let captured_brush_size = strokes
                .iter()
                .filter_map(|stroke| (stroke.brush_size > 0.0).then_some(stroke.brush_size))
                .fold(0.0f32, f32::max);
            (
                strokes.clone(),
                if captured_brush_size > 0.0 {
                    captured_brush_size
                } else {
                    *brush_size
                },
                *edge_refine,
            )
        };
        let cache = self
            .object_cache
            .as_ref()
            .filter(|(target, _)| *target == (mask_index, component_index))
            .map(|(_, cache)| cache.clone());
        let object_request = ObjectMaskRequest {
            source_width: source.width,
            source_height: source.height,
            source_rgba: source.rgba.to_vec(),
            strokes,
            brush_size,
            edge_refine,
            cache,
        };
        self.object_generation = self.object_generation.wrapping_add(1);
        let generation = self.object_generation;
        self.object_pending_target = None;
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.onnx_runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.onnx_runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;
        let Some(target) = self.capture_ai_mask_target(mask_index, component_index) else {
            self.notice = Some("The selected object mask is no longer available.".to_owned());
            return;
        };
        let request = ObjectMaskTaskRequest {
            document_id: self.sidecar_generation,
            generation,
            target,
            encoder_path,
            decoder_path,
            vitmatte_path: self.vitmatte_model_path(),
            runtime_path,
            runtime_sha256,
            request: object_request,
        };

        if let Some(task_id) = library_task {
            self.start_object_mask_task(task_id, request);
        } else {
            // The object worker verifies every model before loading it.
            // Keep this UI-thread decision metadata-only on Android/mobile
            // storage instead of synchronously hashing the complete model set.
            let needs_download = !request.encoder_path.is_file()
                || !request.decoder_path.is_file()
                || !request.vitmatte_path.is_file();
            if needs_download {
                let task_id = self.enqueue_background_action(
                    TaskKind::ObjectMask {
                        document_id: request.document_id,
                        generation,
                    },
                    "Downloading object-mask model",
                    TaskProgress::indeterminate("Waiting for earlier background work…"),
                    true,
                    BackgroundAction::ObjectMask(Box::new(request)),
                );
                self.object_task_id = Some(task_id);
            } else {
                let task_id = self.background_tasks.start_nonblocking(
                    TaskKind::ObjectMask {
                        document_id: request.document_id,
                        generation,
                    },
                    "Generating object mask",
                    TaskProgress::indeterminate(
                        "Encoding the image and generating the object mask…",
                    ),
                    true,
                );
                self.start_object_mask_task(task_id, request);
            }
        }
        self.egui_ctx.request_repaint();
    }

    pub(in crate::app) fn poll_object_worker(&mut self) {
        let Some(task_id) = self.object_task_id else {
            return;
        };
        let (events, disconnected) = drain_worker_events(
            self.object_receiver.as_ref(),
            |event| matches!(event, ObjectMaskEvent::Finished(_)),
        );

        let mut finished = None;
        for event in events {
            match event {
                ObjectMaskEvent::DownloadProgress {
                    label,
                    downloaded,
                    total,
                } => {
                    self.object_download_progress = Some((label, downloaded, total));
                    self.object_inferencing = false;
                    self.background_tasks.set_global_visible(task_id, true);
                    self.background_tasks
                        .rename(task_id, "Downloading object-mask model");
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
                ObjectMaskEvent::Inferencing { decoder_only } => {
                    self.object_download_progress = None;
                    self.object_inferencing = true;
                    self.object_decoder_only = decoder_only;
                    self.background_tasks.set_global_visible(task_id, false);
                    if matches!(
                        self.background_tasks.snapshot(task_id).map(|task| task.kind),
                        Some(TaskKind::ObjectMask { .. })
                    ) {
                        self.background_tasks.release_current(task_id);
                    }
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::indeterminate(if decoder_only {
                            "Updating the object mask…"
                        } else {
                            "Encoding the image and generating the object mask…"
                        }),
                    );
                }
                ObjectMaskEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err("The object-mask worker stopped unexpectedly.".to_owned()));
        }
        let Some(result) = finished else {
            return;
        };

        let target = self.object_job_target.take();
        let generation = self.object_job_generation;
        let document_id = self.object_job_document_id;
        let updating_all = self.ai_mask_update_active && target.is_some();
        let library_task = self.library_ai_mask_refresh_task_id == Some(task_id);
        let cancelled = self.background_task_cancelled(task_id);
        let stale = generation != self.object_generation
            || document_id != self.sidecar_generation;
        let failed_during_inference = self.object_inferencing;
        self.object_receiver = None;
        self.object_task_id = None;
        self.object_download_progress = None;
        self.object_inferencing = false;

        let mut succeeded = false;
        let mut error_message = None;
        if !cancelled && !stale {
            match (target, result) {
                (Some(target), Ok(result)) => {
                    let crate::ai_masks::ObjectMaskResult {
                        width,
                        height,
                        mask: pixels,
                        cache,
                    } = result;
                    let location = self.resolve_ai_mask_target(&target);
                    let mask = MaskImage::new(width, height, pixels);
                    match (location, mask) {
                        (_, None) => {
                            error_message = Some(
                                "Object selection returned malformed dimensions or pixel data."
                                    .to_owned(),
                            );
                        }
                        (Err(error), Some(_)) => error_message = Some(error),
                        (Ok((mask_index, component_index)), Some(mask)) => {
                            let applied = self
                                .masks
                                .masks
                                .get_mut(mask_index)
                                .and_then(|local| local.components.get_mut(component_index))
                                .is_some_and(|component| {
                                    if let MaskGeometry::Object {
                                        mask: generated_mask,
                                        ..
                                    } = &mut component.geometry
                                    {
                                        *generated_mask = Some(mask);
                                        true
                                    } else {
                                        false
                                    }
                                });
                            if applied {
                                self.object_cache = Some(((mask_index, component_index), cache));
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
                (_, Ok(_)) => {}
                (_, Err(error)) => {
                    error_message = Some(format!("Object selection failed: {error}"));
                }
            }
        }

        if library_task {
            if cancelled {
                self.cancel_ai_mask_update();
            } else if let Some((mask_index, component_index)) = self.object_pending_target.take() {
                let (encoder, decoder) = self.sam21_model_paths();
                self.start_object_worker(mask_index, component_index, encoder, decoder);
            } else if updating_all {
                self.ai_mask_update_failed |= !succeeded;
                if !succeeded {
                    self.ai_mask_update_object_queue.clear();
                }
                if let Some(message) = error_message.clone() {
                    self.notice = Some(message);
                }
                self.continue_ai_mask_update();
            }
        } else if cancelled || stale {
            self.finish_background_task(task_id);
            if let Some((mask_index, component_index)) = self.object_pending_target.take() {
                let (encoder, decoder) = self.sam21_model_paths();
                self.start_object_worker(mask_index, component_index, encoder, decoder);
            }
        } else if succeeded {
            self.finish_background_task(task_id);
        } else {
            let message = error_message
                .unwrap_or_else(|| {
                    if stale {
                        "Object selection became stale before inference completed.".to_owned()
                    } else {
                        "Object selection did not produce a mask.".to_owned()
                    }
                });
            self.notice = Some(message.clone());
            if failed_during_inference {
                self.object_error_dialog = Some(message);
                self.finish_background_task(task_id);
            } else {
                self.fail_background_task(task_id, message);
            }
        }
        self.egui_ctx.request_repaint();
    }
}
