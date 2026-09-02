use super::*;

impl CalibRawApp {
    pub(crate) fn restart_refined_object_mask_for_stroke(
        &mut self,
        mask_index: usize,
        component_index: usize,
    ) -> bool {
        let target = (mask_index, component_index);
        let cleared = self
            .masks
            .stack
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

        if self.foreground_operation_is(ForegroundOperationKind::ObjectMask) {
            self.cancel_foreground_operation();
        }
        if self.ai.object_pending_target == Some(target) {
            self.ai.object_pending_target = None;
        }
        if self
            .ai
            .object_cache
            .as_ref()
            .is_some_and(|(cached_target, _)| *cached_target == target)
        {
            self.ai.object_cache = None;
        }
        self.masks.overlay_blink = None;
        self.masks.brush_mode = BrushMode::Paint;
        true
    }

    pub(crate) fn request_object_mask(&mut self, mask_index: usize, component_index: usize) {
        self.ai.object_error_dialog = None;
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
            return;
        }
        let Some(component) = self
            .masks
            .stack
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
            self.ui.notice = Some("Paint inside an object before generating its mask.".to_owned());
            return;
        }
        if self.masks.source_cache.is_none() {
            self.report_ai_mask_error(
                "The original image source is not ready for object selection. Re-open the Object mask or create it again."
                    .to_owned(),
            );
            return;
        }
        if self.foreground_operation_active() {
            if self.foreground_operation_is(ForegroundOperationKind::ObjectMask) {
                self.ai.object_pending_target = Some((mask_index, component_index));
                self.cancel_foreground_operation();
            } else {
                self.ui.notice =
                    Some("Finish or cancel the current editing operation first.".to_owned());
            }
            return;
        }

        let (encoder, decoder) = self.sam21_model_paths();
        if crate::ai_masks::object_models_are_verified(&encoder, &decoder) {
            self.start_object_worker(mask_index, component_index, encoder, decoder, false);
        } else {
            self.ai.object_pending_target = Some((mask_index, component_index));
            self.ai.object_consent_open = true;
        }
    }

    pub(in crate::app) fn start_object_worker(
        &mut self,
        mask_index: usize,
        component_index: usize,
        encoder_path: PathBuf,
        decoder_path: PathBuf,
        allow_download: bool,
    ) {
        if self.foreground_operation_active() {
            if self.foreground_operation_is(ForegroundOperationKind::ObjectMask) {
                self.ai.object_pending_target = Some((mask_index, component_index));
                self.cancel_foreground_operation();
            }
            return;
        }

        let Some(source) = self.masks.source_cache.clone() else {
            self.ui.notice =
                Some("The original image source is unavailable for object selection.".to_owned());
            return;
        };
        let (strokes, brush_size, edge_refine) = {
            let Some(component) = self
                .masks
                .stack
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
            .ai
            .object_cache
            .as_ref()
            .filter(|(target, _)| *target == (mask_index, component_index))
            .map(|(_, cache)| cache.clone());
        let request = ObjectMaskRequest {
            source_width: source.width,
            source_height: source.height,
            source_rgba: source.rgba.to_vec(),
            strokes,
            brush_size,
            edge_refine,
            cache,
        };
        let Some(target) = self.masks.capture_ai_target(mask_index, component_index) else {
            self.ui.notice = Some("The selected object mask is no longer available.".to_owned());
            return;
        };
        self.ai.object_pending_target = None;
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.ai.runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.ai.runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;

        let needs_download =
            !crate::ai_masks::object_models_are_verified(&encoder_path, &decoder_path);
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let receiver = spawn_object_mask(ObjectMaskWorkerRequest {
            encoder_path,
            decoder_path,
            allow_download,
            runtime_path,
            runtime_sha256,
            inference: request,
            cancellation: Arc::clone(&cancellation),
        });
        let progress = ForegroundProgress::indeterminate(if needs_download {
            "Preparing object-mask models…"
        } else {
            "Encoding the image and generating the object mask…"
        });
        self.begin_foreground_operation(ForegroundOperation {
            kind: ForegroundOperationKind::ObjectMask,
            document_id: self.persistence.sidecar_generation,
            cancellation,
            progress,
            cancelling: false,
            receiver: ForegroundOperationReceiver::Object(receiver),
            context: ForegroundOperationContext::Object {
                target,
                inference_started: false,
            },
        });
    }

    pub(in crate::app) fn poll_object_worker(&mut self) {
        if !self.foreground_operation_is(ForegroundOperationKind::ObjectMask) {
            return;
        }
        let Some(mut operation) = self.foreground_operation.take() else {
            return;
        };
        let ForegroundOperationReceiver::Object(receiver) = &operation.receiver else {
            self.foreground_operation = Some(operation);
            return;
        };
        let (events, disconnected) = drain_worker_events(Some(receiver), |event| {
            matches!(event, ObjectMaskEvent::Finished(_))
        });

        let mut finished = None;
        for event in events {
            match event {
                ObjectMaskEvent::DownloadProgress(progress) => {
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
                ObjectMaskEvent::Inferencing { decoder_only } => {
                    if let ForegroundOperationContext::Object {
                        inference_started, ..
                    } = &mut operation.context
                    {
                        *inference_started = true;
                    }
                    operation.progress = ForegroundProgress::indeterminate(if decoder_only {
                        "Updating the object mask…"
                    } else {
                        "Encoding the image and generating the object mask…"
                    });
                }
                ObjectMaskEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err(
                "The object-mask worker stopped unexpectedly.".to_owned()
            ));
        }
        let Some(result) = finished else {
            self.foreground_operation = Some(operation);
            return;
        };

        let (target, failed_during_inference) = match &operation.context {
            ForegroundOperationContext::Object {
                target,
                inference_started,
            } => (Some(target.clone()), *inference_started),
            _ => (None, false),
        };
        let updating_all = self.ai.mask_update_active && target.is_some();
        let cancelled = operation.is_cancelled();
        let stale = operation.document_id != self.persistence.sidecar_generation;

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
                    let location = self.masks.resolve_ai_target(&target);
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
                                .stack
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
                                self.ai.object_cache = Some(((mask_index, component_index), cache));
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

        if updating_all {
            if cancelled || stale {
                let pending_target = self.ai.object_pending_target.take();
                self.cancel_ai_mask_update();
                if !stale {
                    if let Some((mask_index, component_index)) = pending_target {
                        self.request_object_mask(mask_index, component_index);
                    }
                }
            } else {
                self.ai.mask_update_failed |= !succeeded;
                if !succeeded {
                    self.ai.mask_update_object_queue.clear();
                }
                if let Some(message) = error_message {
                    self.ui.notice = Some(message);
                }
                self.continue_ai_mask_update();
            }
        } else if cancelled || stale {
            if let Some((mask_index, component_index)) = self.ai.object_pending_target.take() {
                self.request_object_mask(mask_index, component_index);
            }
        } else if !succeeded {
            let message = error_message.unwrap_or_else(|| {
                if stale {
                    "Object selection became stale before inference completed.".to_owned()
                } else {
                    "Object selection did not produce a mask.".to_owned()
                }
            });
            self.ui.notice = Some(message.clone());
            if failed_during_inference {
                self.ai.object_error_dialog = Some(message);
            }
        }
        self.egui_ctx.request_repaint();
    }
}
