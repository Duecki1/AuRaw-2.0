use super::*;

impl AurawApp {
    pub(crate) fn mark_lens_correction_dirty(&mut self) {
        self.note_edit_changed();
        if self.develop.original_raw.is_some() {
            self.develop.lens_correction_dirty = true;
            self.cancel_foreground_operation_if(ForegroundOperationKind::LensCorrection);
            self.ui.notice = None;
            self.egui_ctx.request_repaint();
        }
    }

    pub(in crate::app) fn apply_pending_lens_correction(&mut self, frame: &eframe::Frame) {
        self.queue_pending_lens_correction(frame);
    }

    pub(in crate::app) fn queue_pending_lens_correction(&mut self, frame: &eframe::Frame) {
        self.poll_lens_correction_worker(frame);
        if !self.develop.lens_correction_dirty || self.foreground_operation_active() {
            return;
        }

        let Some(original_raw) = self.develop.original_raw.as_ref().map(Arc::clone) else {
            self.develop.lens_correction_dirty = false;
            return;
        };
        let selection = if self.develop.lens_correction.enabled {
            let Some(selection) = self.develop.lens_correction.selected_lens() else {
                self.develop.lens_correction_dirty = false;
                self.develop.lens_correction.enabled = false;
                self.develop.lens_correction.applied = false;
                self.develop.lens_correction.catalog.status =
                    "Select a lens profile before enabling correction.".to_owned();
                return;
            };
            Some(selection)
        } else {
            None
        };

        if let Some(restored_masks) = self.persistence.lens_restore_masks.take() {
            self.masks.stack = restored_masks;
            self.rehydrate_restored_mask_state();
        }

        #[cfg(target_os = "android")]
        let cached_raws = match selection.as_ref() {
            Some(requested) => self.preview.lens_corrected_cache
                .as_ref()
                .filter(|(cached, quality, _, _)| {
                    cached == requested && *quality == self.preview.quality
                })
                .map(|(_, _, full_raw, preview_raw)| {
                    (Arc::clone(full_raw), Arc::clone(preview_raw))
                }),
            None => self.preview.lens_original_cache
                .as_ref()
                .filter(|(quality, _)| *quality == self.preview.quality)
                .map(|(_, preview_raw)| (Arc::clone(&original_raw), Arc::clone(preview_raw))),
        };
        #[cfg(not(target_os = "android"))]
        let cached_raws = None;

        let preview_proxy_edge = self.preview.quality.proxy_edge_for_fitted_source(
            self.preview.viewport_pixels,
            original_raw.width,
            original_raw.height,
            self.develop.geometry,
        );
        self.develop.lens_correction_dirty = false;
        self.start_lens_correction_task(LensCorrectionTaskRequest {
            original_raw,
            selection,
            #[cfg(target_os = "android")]
            preview_quality: self.preview.quality,
            preview_proxy_edge,
            cached_raws,
        });
    }

    pub(in crate::app) fn start_lens_correction_task(&mut self, request: LensCorrectionTaskRequest) {
        if self.foreground_operation_active() {
            self.develop.lens_correction_dirty = true;
            return;
        }
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancellation = Arc::clone(&cancellation);
        let status_label = request
            .selection
            .as_ref()
            .map(LensfunLens::label)
            .unwrap_or_else(|| "original RAW geometry".to_owned());
        self.develop.lens_correction.catalog.status = if request.selection.is_some() {
            format!("Applying {status_label}…")
        } else {
            "Disabling lens correction…".to_owned()
        };
        let progress = ForegroundProgress::indeterminate(if request.selection.is_some() {
            "Applying lens profile…"
        } else {
            "Restoring original RAW geometry…"
        });

        let repaint = self.egui_ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let spawn_result = std::thread::Builder::new()
            .name("auraw-lens-correction".to_owned())
            .spawn(move || {
                use std::sync::atomic::Ordering;
                let result = (|| -> Result<PreparedLensCorrection, String> {
                    if worker_cancellation.load(Ordering::Acquire) {
                        return Err("foreground operation cancelled".to_owned());
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
                        if worker_cancellation.load(Ordering::Acquire) {
                            return Err("foreground operation cancelled".to_owned());
                        }
                        let _ = sender.send(LensCorrectionEvent::Progress(
                            "Building preview proxy…".to_owned(),
                        ));
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
                    if worker_cancellation.load(Ordering::Acquire) {
                        return Err("foreground operation cancelled".to_owned());
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
                let _ = sender.send(LensCorrectionEvent::Finished(result));
                repaint.request_repaint();
            });
        match spawn_result {
            Ok(_) => {
                self.begin_foreground_operation(ForegroundOperation {
                    kind: ForegroundOperationKind::LensCorrection,
                    document_id: self.persistence.sidecar_generation,
                    cancellation,
                    progress,
                    cancelling: false,
                    receiver: ForegroundOperationReceiver::LensCorrection(receiver),
                    context: ForegroundOperationContext::LensCorrection,
                });
                self.egui_ctx.request_repaint_after(Duration::from_millis(50));
            }
            Err(error) => {
                self.develop.lens_correction.enabled = self.develop.lens_correction.applied;
                self.develop.lens_correction.catalog.status =
                    format!("Could not start lens correction: {error}");
                self.ui.notice = Some(self.develop.lens_correction.catalog.status.clone());
            }
        }
    }

    pub(crate) fn lens_correction_busy(&self) -> bool {
        self.develop.lens_correction_dirty
            || self.foreground_operation_is(ForegroundOperationKind::LensCorrection)
    }

    pub(in crate::app) fn poll_lens_correction_worker(&mut self, frame: &eframe::Frame) {
        if !self.foreground_operation_is(ForegroundOperationKind::LensCorrection) {
            return;
        }
        let Some(mut operation) = self.foreground_operation.take() else {
            return;
        };
        let ForegroundOperationReceiver::LensCorrection(receiver) = &operation.receiver else {
            self.foreground_operation = Some(operation);
            return;
        };
        let (events, disconnected) = drain_worker_events(Some(receiver), |event| {
            matches!(event, LensCorrectionEvent::Finished(_))
        });

        let mut finished = None;
        for event in events {
            match event {
                LensCorrectionEvent::Progress(phase) => {
                    operation.progress = ForegroundProgress::indeterminate(phase);
                }
                LensCorrectionEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err("Lens-correction worker stopped unexpectedly.".to_owned()));
        }
        let Some(result) = finished else {
            self.foreground_operation = Some(operation);
            return;
        };

        let stale = !operation.accepts_result(self.persistence.sidecar_generation);
        if stale {
            return;
        }
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                if !error.contains("cancelled") {
                    self.develop.lens_correction.enabled = self.develop.lens_correction.applied;
                    self.develop.lens_correction.catalog.status = error.clone();
                    self.ui.notice =
                        Some("Lens correction failed; restored the previous preview.".to_owned());
                }
                return;
            }
        };
        operation.progress = ForegroundProgress::indeterminate("Preparing GPU preview…");

        let Some(render_state) = frame.wgpu_render_state() else {
            self.ui.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            return;
        };

        #[cfg(target_os = "android")]
        {
            let Some(pipeline) = self.preview.gpu_pipeline.as_ref() else {
                self.ui.notice = Some("The preview pipeline is unavailable.".to_owned());
                return;
            };
            if let Err(error) =
                pipeline.upload_raw_tile(&render_state.queue, &prepared.preview_raw)
            {
                self.ui.notice = Some(format!(
                    "Could not update the lens-corrected preview pixels: {error:#}"
                ));
                return;
            }
            let params = GpuParams::new(&self.develop.exposure, &self.masks.stack, &prepared.preview_raw)
                .with_vignette_geometry(self.develop.geometry);
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
            if let Some(selection) = prepared.selection.clone() {
                self.preview.lens_corrected_cache = Some((
                    selection,
                    prepared.preview_quality,
                    Arc::clone(&prepared.full_raw),
                    Arc::clone(&prepared.preview_raw),
                ));
            } else {
                self.preview.lens_original_cache = Some((
                    prepared.preview_quality,
                    Arc::clone(&prepared.preview_raw),
                ));
            }
        }

        #[cfg(not(target_os = "android"))]
        {
            let preview_masks = self.masks.stack.clone();
            let params = GpuParams::new(&self.develop.exposure, &preview_masks, &prepared.preview_raw)
                .with_vignette_geometry(self.develop.geometry);
            let mut pipeline = match RawGpuPipeline::new_headless_with_quality(
                &render_state.device,
                &render_state.queue,
                &prepared.preview_raw,
                &params,
                ProcessingQuality::Preview,
            ) {
                Ok(pipeline) => pipeline,
                Err(error) => {
                    self.ui.notice =
                        Some(format!("Could not rebuild the corrected GPU preview: {error:#}"));
                    return;
                }
            };
            if let Err(error) = self.apply_display_output_transform(&render_state.queue, &pipeline) {
                self.ui.notice = Some(format!("Could not prepare the preview color profile: {error:#}"));
                return;
            }
            if let Err(error) = Self::upload_preview_masks(
                &pipeline,
                &render_state.queue,
                &preview_masks,
                &prepared.preview_raw,
            ) {
                self.ui.notice = Some(error);
                return;
            }
            pipeline.recompute(&render_state.queue, &render_state.device, &params);

            if !operation.accepts_result(self.persistence.sidecar_generation) {
                return;
            }
            let mut renderer = render_state.renderer.write();
            self.take_preview_pipeline_and_release_textures(&mut renderer);
            pipeline.register_egui_texture(&render_state.device, &mut renderer);
            drop(renderer);
            self.preview.gpu_pipeline = Some(pipeline);
        }

        if !operation.accepts_result(self.persistence.sidecar_generation)
        {
            return;
        }

        self.rehydrate_restored_mask_state();
        self.note_lens_correction_changed_for_masks();
        self.masks.dirty_layers = [false; MAX_LOCAL_MASKS];
        self.masks.detail_dirty_layers = [false; MAX_LOCAL_MASKS];
        self.masks.navigation_dirty_layers = [false; MAX_LOCAL_MASKS];
        self.develop.loaded_raw = Some(prepared.full_raw);
        self.develop.preview_raw = Some(prepared.preview_raw);
        self.preview.zoom = 1.0;
        self.preview.center = [0.5, 0.5];
        self.preview.visible_uv = PreviewUvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        };
        self.preview.motion_at = None;
        self.preview.touch_navigation_active = false;
        self.preview.revision = self.preview.revision.wrapping_add(1);
        self.preview.detail_pending_stage = None;
        self.preview.navigation_pending_stage = None;
        self.preview.detail_urgent = false;
        self.develop.target_exposure = self.develop.exposure;
        self.preview.pending_stage = None;
        self.develop.lens_correction.applied = prepared.applied_label.is_some();
        self.develop.lens_correction.catalog.status = prepared.applied_label.map_or_else(
            || "Lens correction disabled; using the original RAW geometry.".to_owned(),
            |label| format!("Applied {label}"),
        );
        self.ui.notice = None;
        self.resume_persisted_ai_denoise(frame);
        self.egui_ctx.request_repaint();
    }
}
