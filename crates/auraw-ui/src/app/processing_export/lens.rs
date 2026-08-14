use super::*;

impl AurawApp {
    pub(crate) fn mark_lens_correction_dirty(&mut self) {
        self.note_edit_changed();
        if self.original_raw.is_some() {
            self.lens_correction_dirty = true;
            self.lens_correction_generation = self.lens_correction_generation.wrapping_add(1);
            self.notice = None;
            self.egui_ctx.request_repaint();
        }
    }

    pub(in crate::app) fn apply_pending_lens_correction(&mut self, frame: &eframe::Frame) {
        self.queue_pending_lens_correction(frame);
    }

    pub(in crate::app) fn queue_pending_lens_correction(&mut self, frame: &eframe::Frame) {
        self.poll_lens_correction_worker(frame);
        if !self.lens_correction_dirty {
            return;
        }
        self.lens_correction_dirty = false;

        let Some(original_raw) = self.original_raw.as_ref().map(Arc::clone) else {
            return;
        };
        let selection = if self.lens_correction.enabled {
            let Some(selection) = self.lens_correction.selected_lens() else {
                self.lens_correction.enabled = false;
                self.lens_correction.applied = false;
                self.lens_correction.catalog.status =
                    "Select a lens profile before enabling correction.".to_owned();
                return;
            };
            Some(selection)
        } else {
            None
        };

        if let Some(restored_masks) = self.history_lens_restore_masks.take() {
            self.masks = restored_masks;
            self.rehydrate_restored_mask_state();
        }

        #[cfg(target_os = "android")]
        let cached_raws = match selection.as_ref() {
            Some(requested) => self
                .lens_corrected_preview_cache
                .as_ref()
                .filter(|(cached, quality, _, _)| {
                    cached == requested && *quality == self.preview_quality
                })
                .map(|(_, _, full_raw, preview_raw)| {
                    (Arc::clone(full_raw), Arc::clone(preview_raw))
                }),
            None => self
                .lens_original_preview_cache
                .as_ref()
                .filter(|(quality, _)| *quality == self.preview_quality)
                .map(|(_, preview_raw)| (Arc::clone(&original_raw), Arc::clone(preview_raw))),
        };
        #[cfg(not(target_os = "android"))]
        let cached_raws = None;

        let generation = self.lens_correction_generation;
        let document_id = self.sidecar_generation;
        let name = selection
            .as_ref()
            .map(|lens| format!("Applying {}", lens.label()))
            .unwrap_or_else(|| "Disabling lens correction".to_owned());
        let preview_proxy_edge = self.preview_quality.proxy_edge_for_fitted_source(
            self.preview_viewport_pixels,
            original_raw.width,
            original_raw.height,
            self.geometry,
        );
        self.enqueue_lens_background_action(
            LensCorrectionTaskRequest {
                document_id,
                generation,
                original_raw,
                selection,
                #[cfg(target_os = "android")]
                preview_quality: self.preview_quality,
                preview_proxy_edge,
                cached_raws,
            },
            name,
        );
    }

    pub(in crate::app) fn start_lens_correction_task(&mut self, id: TaskId, request: LensCorrectionTaskRequest) {
        let Some(cancellation) = self.background_tasks.cancellation_token(id) else {
            self.fail_background_task(id, "Lens correction lost its cancellation state.");
            return;
        };
        self.lens_correction_task_id = Some(id);
        let status_label = request
            .selection
            .as_ref()
            .map(LensfunLens::label)
            .unwrap_or_else(|| "original RAW geometry".to_owned());
        self.lens_correction.catalog.status = if request.selection.is_some() {
            format!("Applying {status_label} in the background…")
        } else {
            "Disabling lens correction in the background…".to_owned()
        };
        self.background_tasks.update_progress(
            id,
            TaskProgress::indeterminate(if request.selection.is_some() {
                "Applying profile…"
            } else {
                "Restoring original RAW geometry…"
            }),
        );

        let repaint = self.egui_ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let document_id = request.document_id;
        let generation = request.generation;
        let spawn_result = std::thread::Builder::new()
            .name("auraw-lens-correction".to_owned())
            .spawn(move || {
                use std::sync::atomic::Ordering;
                let result = (|| -> Result<PreparedLensCorrection, String> {
                    if cancellation.load(Ordering::Acquire) {
                        return Err("background task cancelled".to_owned());
                    }
                    let applied_label = request.selection.as_ref().map(LensfunLens::label);
                    let (full_raw, preview_raw) = if let Some(cached_raws) = request.cached_raws {
                        cached_raws
                    } else {
                        let original_raw = Arc::clone(&request.original_raw);
                        let full_raw = if let Some(selection) = request.selection.as_ref() {
                            Arc::new(
                                apply_lensfun_correction(&original_raw, selection).map_err(
                                    |error| {
                                        format!(
                                            "Could not apply {}: {error:#}",
                                            selection.label()
                                        )
                                    },
                                )?,
                            )
                        } else {
                            original_raw
                        };
                        if cancellation.load(Ordering::Acquire) {
                            return Err("background task cancelled".to_owned());
                        }
                        let _ = sender.send(LensCorrectionEvent::Progress {
                            task_id: id,
                            document_id,
                            generation,
                            phase: "Building preview proxy…".to_owned(),
                        });
                        repaint.request_repaint();
                        let preview_spec = ProxySpec {
                            max_edge: request.preview_proxy_edge,
                        };
                        let preview_raw =
                            if full_raw.width.max(full_raw.height) <= preview_spec.max_edge {
                                Arc::clone(&full_raw)
                            } else {
                                Arc::new(build_proxy(&full_raw, preview_spec))
                            };
                        (full_raw, preview_raw)
                    };
                    if cancellation.load(Ordering::Acquire) {
                        return Err("background task cancelled".to_owned());
                    }
                    Ok(PreparedLensCorrection {
                        full_raw,
                        preview_raw,
                        applied_label,
                        #[cfg(target_os = "android")]
                        selection: request.selection,
                        #[cfg(target_os = "android")]
                        preview_quality: request.preview_quality,
                    })
                })();
                let _ = sender.send(LensCorrectionEvent::Finished {
                    task_id: id,
                    document_id,
                    generation,
                    result,
                });
                repaint.request_repaint();
            });
        match spawn_result {
            Ok(_) => {
                self.lens_correction_receiver = Some(receiver);
                self.egui_ctx.request_repaint_after(Duration::from_millis(50));
            }
            Err(error) => {
                self.lens_correction_task_id = None;
                self.fail_background_task(id, format!("Could not start lens correction: {error}"));
            }
        }
    }

    pub(crate) fn lens_correction_busy(&self) -> bool {
        self.lens_correction_receiver.is_some()
            || self.background_task_snapshots().iter().any(|task| {
                matches!(task.kind, TaskKind::LensCorrection { .. })
                    && task.status != TaskStatus::Failed
            })
    }

    pub(in crate::app) fn poll_lens_correction_worker(&mut self, frame: &eframe::Frame) {
        let (events, disconnected) = drain_worker_events(
            self.lens_correction_receiver.as_ref(),
            |event| matches!(event, LensCorrectionEvent::Finished { .. }),
        );

        let mut finished = None;
        for event in events {
            match event {
                LensCorrectionEvent::Progress {
                    task_id,
                    document_id,
                    generation,
                    phase,
                } => {
                    if document_id == self.sidecar_generation
                        && generation == self.lens_correction_generation
                        && !self.background_task_cancelled(task_id)
                    {
                        self.update_background_progress(
                            Some(task_id),
                            TaskProgress::indeterminate(phase),
                        );
                    }
                }
                LensCorrectionEvent::Finished {
                    task_id,
                    document_id,
                    generation,
                    result,
                } => finished = Some((task_id, document_id, generation, result)),
            }
        }

        if finished.is_none() && disconnected {
            self.lens_correction_receiver = None;
            if let Some(id) = self.lens_correction_task_id.take() {
                self.fail_background_task(id, "Lens-correction worker stopped unexpectedly.");
            }
            self.lens_correction.enabled = self.lens_correction.applied;
            self.lens_correction.catalog.status =
                "Lens-correction worker stopped unexpectedly.".to_owned();
            self.notice = Some(self.lens_correction.catalog.status.clone());
            return;
        }
        let Some((task_id, document_id, generation, result)) = finished else {
            return;
        };
        self.lens_correction_receiver = None;
        if self.lens_correction_task_id == Some(task_id) {
            self.lens_correction_task_id = None;
        }

        let stale = document_id != self.sidecar_generation
            || generation != self.lens_correction_generation
            || self.background_task_cancelled(task_id);
        if stale {
            self.finish_background_task(task_id);
            return;
        }

        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                if self.background_task_cancelled(task_id) {
                    self.finish_background_task(task_id);
                } else {
                    self.lens_correction.enabled = self.lens_correction.applied;
                    self.lens_correction.catalog.status = error.clone();
                    self.notice =
                        Some("Lens correction failed; restored the previous preview.".to_owned());
                    self.fail_background_task(task_id, error);
                }
                return;
            }
        };
        self.update_background_progress(
            Some(task_id),
            TaskProgress::indeterminate("Preparing GPU preview…"),
        );

        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            self.fail_background_task(task_id, self.notice.clone().unwrap_or_default());
            return;
        };

        #[cfg(target_os = "android")]
        {
            let Some(pipeline) = self.gpu_pipeline.as_ref() else {
                self.fail_background_task(task_id, "The preview pipeline is unavailable.");
                return;
            };
            if let Err(error) =
                pipeline.upload_raw_tile(&render_state.queue, &prepared.preview_raw)
            {
                self.notice = Some(format!(
                    "Could not update the lens-corrected preview pixels: {error:#}"
                ));
                self.fail_background_task(task_id, self.notice.clone().unwrap_or_default());
                return;
            }
            let params = GpuParams::new(&self.exposure, &self.masks, &prepared.preview_raw)
                .with_vignette_geometry(self.geometry);
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
            if let Some(selection) = prepared.selection.clone() {
                self.lens_corrected_preview_cache = Some((
                    selection,
                    prepared.preview_quality,
                    Arc::clone(&prepared.full_raw),
                    Arc::clone(&prepared.preview_raw),
                ));
            } else {
                self.lens_original_preview_cache = Some((
                    prepared.preview_quality,
                    Arc::clone(&prepared.preview_raw),
                ));
            }
        }

        #[cfg(not(target_os = "android"))]
        {
            let preview_masks = self.masks.clone();
            let params = GpuParams::new(&self.exposure, &preview_masks, &prepared.preview_raw)
                .with_vignette_geometry(self.geometry);
            let mut pipeline = match RawGpuPipeline::new_headless_with_quality(
                &render_state.device,
                &render_state.queue,
                &prepared.preview_raw,
                &params,
                ProcessingQuality::Preview,
            ) {
                Ok(pipeline) => pipeline,
                Err(error) => {
                    let message =
                        format!("Could not rebuild the corrected GPU preview: {error:#}");
                    self.notice = Some(message.clone());
                    self.fail_background_task(task_id, message);
                    return;
                }
            };
            if let Err(error) = self.apply_display_output_transform(&render_state.queue, &pipeline) {
                let message = format!("Could not prepare the preview color profile: {error:#}");
                self.notice = Some(message.clone());
                self.fail_background_task(task_id, message);
                return;
            }
            if let Err(error) = Self::upload_preview_masks(
                &pipeline,
                &render_state.queue,
                &preview_masks,
                &prepared.preview_raw,
            ) {
                self.notice = Some(error.clone());
                self.fail_background_task(task_id, error);
                return;
            }
            if let Err(error) = pipeline.update_inpaint_layer(
                &render_state.queue,
                self.inpaint_layer.as_ref(),
                0,
                0,
                prepared.preview_raw.width,
                prepared.preview_raw.height,
            ) {
                let message =
                    format!("Could not rebuild lens-corrected preview inpainting: {error:#}");
                self.notice = Some(message.clone());
                self.fail_background_task(task_id, message);
                return;
            }
            pipeline.recompute(&render_state.queue, &render_state.device, &params);

            if document_id != self.sidecar_generation
                || generation != self.lens_correction_generation
                || self.background_task_cancelled(task_id)
            {
                self.finish_background_task(task_id);
                return;
            }
            let mut renderer = render_state.renderer.write();
            self.take_preview_pipeline_and_release_textures(&mut renderer);
            pipeline.register_egui_texture(&render_state.device, &mut renderer);
            drop(renderer);
            self.gpu_pipeline = Some(pipeline);
        }

        if document_id != self.sidecar_generation
            || generation != self.lens_correction_generation
            || self.background_task_cancelled(task_id)
        {
            self.finish_background_task(task_id);
            return;
        }

        self.rehydrate_restored_mask_state();
        self.note_lens_correction_changed_for_masks();
        self.dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.detail_dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.navigation_dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.loaded_raw = Some(prepared.full_raw);
        self.preview_raw = Some(prepared.preview_raw);
        self.inpaint_source_cache = None;
        self.preview_zoom = 1.0;
        self.preview_center = [0.5, 0.5];
        self.preview_visible_uv = PreviewUvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        };
        self.preview_motion_at = None;
        self.preview_touch_navigation_active = false;
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.preview_detail_pending_stage = None;
        self.navigation_pending_stage = None;
        self.preview_detail_urgent = false;
        self.target_exposure = self.exposure;
        self.pending_stage = None;
        self.lens_correction.applied = prepared.applied_label.is_some();
        self.lens_correction.catalog.status = prepared.applied_label.map_or_else(
            || "Lens correction disabled; using the original RAW geometry.".to_owned(),
            |label| format!("Applied {label}"),
        );
        self.notice = None;
        self.finish_background_task(task_id);
        self.resume_persisted_ai_denoise(frame);
        self.egui_ctx.request_repaint();
    }

}
