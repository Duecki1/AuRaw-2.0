impl AurawApp {
    pub(crate) fn inpaint_busy(&self) -> bool {
        self.inpaint_task_id.is_some()
            || self.inpaint_receiver.is_some()
            || self.inpaint_consent_open
    }

    pub(crate) fn inpaint_progress(&self) -> Option<(u64, u64)> {
        self.inpaint_download_progress
    }

    pub(crate) fn inpaint_inferencing(&self) -> bool {
        self.inpaint_inferencing
    }

    pub(crate) fn prepare_live_retouch_preview(&mut self, frame: &eframe::Frame) {
        self.inpaint_source_cache = None;
        if !self.inpaint_tool.requires_source() {
            return;
        }
        let result = (|| -> Result<MaskRgbImage, String> {
            let render_state = frame
                .wgpu_render_state()
                .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
            let raw = self
                .preview_raw
                .as_ref()
                .ok_or_else(|| "Open an image before using Heal or Clone.".to_owned())?;
            let pipeline = self
                .gpu_pipeline
                .as_ref()
                .ok_or_else(|| "Open an image before using Heal or Clone.".to_owned())?;

            // A completed retouch stroke is queued for the normal incremental
            // preview scheduler. A user can start the next stroke before that
            // scheduler reaches its Output stage, so render the current CPU
            // inpaint layer here before taking the live brush snapshot. The
            // blocking readback below also guarantees these queued GPU commands
            // have completed in submission order.
            pipeline
                .update_inpaint_layer(
                    &render_state.queue,
                    self.inpaint_layer.as_ref(),
                    0,
                    0,
                    raw.width,
                    raw.height,
                )
                .map_err(|error| {
                    format!("Could not update the live retouch source: {error:#}")
                })?;
            let params = GpuParams::new(&self.target_exposure, &self.masks, raw)
                .with_vignette_geometry(self.geometry);
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
            let rgba = pipeline
                .read_output_region_blocking(
                    &render_state.device,
                    &render_state.queue,
                    0,
                    0,
                    pipeline.width,
                    pipeline.height,
                )
                .map_err(|error| format!("Could not capture the live retouch preview: {error:#}"))?;
            MaskRgbImage::new(pipeline.width, pipeline.height, rgba)
                .ok_or_else(|| "The live retouch preview has invalid dimensions.".to_owned())
        })();
        match result {
            Ok(source) => self.inpaint_source_cache = Some(source),
            Err(error) => self.notice = Some(error),
        }
    }

    pub(crate) fn live_retouch_preview(&self) -> Option<&MaskRgbImage> {
        self.inpaint_source_cache.as_ref()
    }

    pub(crate) fn clear_inpainting_tool(&mut self, kind: InpaintStrokeKind) {
        if self.inpaint_receiver.is_some() {
            return;
        }
        self.inpaint_stroke.clear();
        let previous_len = self.inpaint_strokes.len();
        self.inpaint_strokes.retain(|stroke| stroke.kind != kind);
        if self.inpaint_strokes.len() == previous_len {
            return;
        }
        self.inpaint_replace_index = None;
        self.note_inpainting_edit_changed();
        self.last_inpaint_brush_point = None;
        self.rebuild_inpaint_layer();
        self.inpaint_stroke_texture = None;
        self.inpaint_stroke_texture_key = None;
        self.inpaint_hovered_stroke = None;
        self.inpaint_selected_stroke = None;
        self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
        self.note_inpainting_changed_for_ai_masks();
        self.queue_preview_processing(ProcessingStage::Tone);
        self.notice = Some(format!("All {} strokes cleared.", kind.label()));
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn reset_inpainting_state(&mut self) {
        // A native inference call may only notice cancellation between phases. Keep its
        // receiver connected until the terminal event arrives, while invalidating its
        // document generation so the result can never be installed into the new image.
        if let Some(task_id) = self.inpaint_task_id {
            self.cancel_background_task(task_id);
        }
        let worker_still_running = self.inpaint_receiver.is_some();
        self.inpaint_stroke.clear();
        self.inpaint_strokes.clear();
        self.last_inpaint_brush_point = None;
        self.inpaint_source_anchor = None;
        self.inpaint_source_offset = None;
        self.inpaint_source_pick_active = self.inpaint_tool.requires_source();
        self.inpaint_layer = None;
        self.inpaint_texture = None;
        self.inpaint_texture_key = None;
        self.inpaint_stroke_texture = None;
        self.inpaint_stroke_texture_key = None;
        self.inpaint_hovered_stroke = None;
        self.inpaint_selected_stroke = None;
        self.inpaint_focus_texture = None;
        self.inpaint_focus_texture_key = None;
        self.inpaint_source_cache = None;
        self.inpaint_pending_source = None;
        self.inpaint_replace_index = None;
        self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
        self.inpaint_consent_open = false;
        if !worker_still_running {
            self.inpaint_task_id = None;
            self.inpaint_receiver = None;
            self.inpaint_active_dabs = None;
            self.inpaint_download_progress = None;
            self.inpaint_inferencing = false;
        }
        self.inpaint_texture_revision = self.inpaint_texture_revision.wrapping_add(1);
    }

    pub(crate) fn request_inpaint(&mut self, frame: &eframe::Frame) {
        if self.inpaint_stroke.is_empty() || self.inpaint_busy() {
            return;
        }
        self.inpaint_selected_stroke = None;
        self.inpaint_focus_texture = None;
        self.inpaint_focus_texture_key = None;
        if self.inpaint_tool.requires_source() {
            self.request_source_retouch(frame, self.inpaint_tool, None);
            return;
        }
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
            self.inpaint_stroke.clear();
            self.last_inpaint_brush_point = None;
            return;
        }

        self.inpaint_replace_index = None;

        // Capture only the full-resolution RAW region needed by this stroke.
        // This avoids the old preview-proxy source while keeping brush release
        // fast: shader programs are reused and only a small local crop is
        // allocated/read back.
        let source = match self.capture_inpaint_source(frame, &self.inpaint_stroke, None) {
            Ok(source) => source,
            Err(error) => {
                self.inpaint_source_cache = None;
                self.notice = Some(error);
                self.inpaint_stroke.clear();
                self.last_inpaint_brush_point = None;
                return;
            }
        };
        self.inpaint_pending_source = Some(source);
        let model_path = self.lama_model_path();
        if model_path.exists() {
            self.start_inpaint_worker(model_path);
        } else {
            self.inpaint_consent_open = true;
            self.egui_ctx.request_repaint();
        }
    }

    pub(crate) fn regenerate_inpaint_stroke(&mut self, frame: &eframe::Frame, index: usize) {
        if self.inpaint_busy() || index >= self.inpaint_strokes.len() {
            return;
        }
        self.inpaint_selected_stroke = None;
        self.inpaint_focus_texture = None;
        self.inpaint_focus_texture_key = None;
        let existing = self.inpaint_strokes[index].clone();
        if existing.kind.requires_source() {
            self.inpaint_stroke = existing.dabs;
            self.last_inpaint_brush_point = None;
            self.request_source_retouch(frame, existing.kind, Some(index));
            return;
        }
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
            return;
        }

        let dabs = existing.dabs;
        let source = match self.capture_inpaint_source(frame, &dabs, Some(index)) {
            Ok(source) => source,
            Err(error) => {
                self.notice = Some(error);
                return;
            }
        };

        self.inpaint_stroke = dabs;
        self.last_inpaint_brush_point = None;
        self.inpaint_pending_source = Some(source);
        self.inpaint_replace_index = Some(index);
        let model_path = self.lama_model_path();
        if model_path.exists() {
            self.start_inpaint_worker(model_path);
        } else {
            self.inpaint_consent_open = true;
            self.egui_ctx.request_repaint();
        }
    }

    fn capture_inpaint_source(
        &self,
        frame: &eframe::Frame,
        dabs: &[BrushDab],
        excluded_stroke: Option<usize>,
    ) -> Result<PreparedInpaintSource, String> {
        let full_raw = self
            .loaded_raw
            .as_ref()
            .ok_or_else(|| "Open an image before using Inpainting.".to_owned())?;
        let patch = inpaint_patch_rect(dabs, full_raw.width, full_raw.height)
            .ok_or_else(|| "The erase stroke does not cover the image.".to_owned())?;
        let rgb_rec2020 = self.capture_inpaint_scene_square(frame, patch, excluded_stroke)?;

        Ok(PreparedInpaintSource {
            rgb_rec2020,
            width: patch.size,
            height: patch.size,
            origin_x: patch.x,
            origin_y: patch.y,
            full_width: full_raw.width,
            full_height: full_raw.height,
        })
    }

    fn capture_inpaint_scene_square(
        &self,
        frame: &eframe::Frame,
        patch: InpaintPatchRect,
        excluded_stroke: Option<usize>,
    ) -> Result<Vec<f32>, String> {
        let render_state = frame
            .wgpu_render_state()
            .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
        let full_raw = self
            .loaded_raw
            .as_ref()
            .ok_or_else(|| "Open an image before using Inpainting.".to_owned())?;
        let template = self
            .gpu_pipeline
            .as_ref()
            .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
        if patch.size == 0
            || patch.x.checked_add(patch.size).is_none_or(|right| right > full_raw.width)
            || patch.y.checked_add(patch.size).is_none_or(|bottom| bottom > full_raw.height)
        {
            return Err("The inpainting source region falls outside the image.".to_owned());
        }
        let halo = 32u32;
        let capture_x = patch.x.saturating_sub(halo);
        let capture_y = patch.y.saturating_sub(halo);
        let capture_right = patch
            .x
            .saturating_add(patch.size)
            .saturating_add(halo)
            .min(full_raw.width);
        let capture_bottom = patch
            .y
            .saturating_add(patch.size)
            .saturating_add(halo)
            .min(full_raw.height);
        let capture_width = capture_right.saturating_sub(capture_x);
        let capture_height = capture_bottom.saturating_sub(capture_y);
        if capture_width == 0 || capture_height == 0 {
            return Err("The inpainting source region is empty.".to_owned());
        }
        let local_raw = crop_raw(
            full_raw,
            capture_x,
            capture_y,
            capture_width,
            capture_height,
        );
        let empty_masks = MaskStack::default();
        let mut neutral_exposure = self.exposure;
        neutral_exposure.temperature = 0.0;
        neutral_exposure.tint = 0.0;
        let params = GpuParams::new_for_tile(
            &neutral_exposure,
            &empty_masks,
            &local_raw,
            capture_x as i32,
            capture_y as i32,
            full_raw.width,
            full_raw.height,
        );
        let pipeline = RawGpuPipeline::new_headless_reusing_programs(
            &render_state.device,
            &render_state.queue,
            &local_raw,
            &params,
            ProcessingQuality::Preview,
            template,
        )
        .map_err(|error| {
            format!("Could not prepare the full-resolution inpainting crop: {error:#}")
        })?;
        let patch_local_x = patch.x.saturating_sub(capture_x);
        let patch_local_y = patch.y.saturating_sub(capture_y);
        let scene = pipeline
            .render_inpaint_working_scene_region_resized_blocking(
                &render_state.device,
                &render_state.queue,
                &params,
                patch_local_x,
                patch_local_y,
                patch.size,
                patch.size,
                LAMA_EDGE,
                LAMA_EDGE,
            )
            .map_err(|error| format!("Could not read the inpainting crop: {error:#}"))?;
        let expected = LAMA_EDGE as usize * LAMA_EDGE as usize * 3;
        if scene.len() != expected || scene.iter().any(|value| !value.is_finite()) {
            return Err("The inpainting crop has an invalid Rec.2020 working buffer.".to_owned());
        }
        let replacement_base = excluded_stroke.map(|excluded| {
            compose_inpaint_strokes(
                &self
                    .inpaint_strokes
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| *index != excluded)
                    .map(|(_, stroke)| stroke.clone())
                    .collect::<Vec<_>>(),
            )
        });
        let source_layer = replacement_base
            .as_ref()
            .and_then(|layer| layer.as_ref())
            .or(self.inpaint_layer.as_ref().filter(|_| excluded_stroke.is_none()));
        let rgb_rec2020 = if let Some(layer) = source_layer {
            flatten_inpaint_source_model_region(
                scene,
                layer,
                [patch.x, patch.y],
                patch.size,
                [full_raw.width, full_raw.height],
                full_raw.cam_to_srgb,
            )?
        } else {
            scene
        };

        Ok(rgb_rec2020)
    }

    fn request_source_retouch(
        &mut self,
        frame: &eframe::Frame,
        kind: InpaintStrokeKind,
        replace_index: Option<usize>,
    ) {
        let dabs = std::mem::take(&mut self.inpaint_stroke);
        self.last_inpaint_brush_point = None;
        self.inpaint_stroke_texture = None;
        self.inpaint_stroke_texture_key = None;
        let result = (|| -> Result<InpaintStroke, String> {
            if !kind.requires_source() || dabs.is_empty() {
                return Err("The retouch stroke is empty.".to_owned());
            }
            let full_raw = self
                .loaded_raw
                .as_ref()
                .ok_or_else(|| "Open an image before using Heal or Clone.".to_owned())?;
            let source_offset = if let Some(index) = replace_index {
                self.inpaint_strokes
                    .get(index)
                    .and_then(|stroke| stroke.source_offset)
                    .ok_or_else(|| "The retouch stroke has no saved source point.".to_owned())?
            } else {
                let source = self.inpaint_source_anchor.ok_or_else(|| {
                    "Set a source point before painting with Heal or Clone.".to_owned()
                })?;
                self.inpaint_source_offset.unwrap_or([
                    source[0] - dabs[0].center[0],
                    source[1] - dabs[0].center[1],
                ])
            };
            if replace_index.is_none() {
                self.inpaint_source_offset = Some(source_offset);
            }
            let full_min = full_raw.width.min(full_raw.height).max(1) as f32;
            let source_is_valid = dabs.iter().all(|dab| {
                let radius = dab.size.max(0.0) * full_min + 4.0;
                let x = (dab.center[0] + source_offset[0]) * full_raw.width as f32;
                let y = (dab.center[1] + source_offset[1]) * full_raw.height as f32;
                x - radius >= 0.0
                    && y - radius >= 0.0
                    && x + radius < full_raw.width as f32
                    && y + radius < full_raw.height as f32
            });
            if !source_is_valid {
                return Err(
                    "The Heal/Clone source crosses the image edge. Choose a source farther inside the image."
                        .to_owned(),
                );
            }
            let destination_patch = inpaint_patch_rect(&dabs, full_raw.width, full_raw.height)
                .ok_or_else(|| "The retouch stroke does not cover the image.".to_owned())?;
            let offset_pixels = [
                source_offset[0] * full_raw.width as f32,
                source_offset[1] * full_raw.height as f32,
            ];
            let max_source_x = full_raw.width.saturating_sub(destination_patch.size);
            let max_source_y = full_raw.height.saturating_sub(destination_patch.size);
            let source_patch = InpaintPatchRect {
                x: (destination_patch.x as f32 + offset_pixels[0])
                    .round()
                    .clamp(0.0, max_source_x as f32) as u32,
                y: (destination_patch.y as f32 + offset_pixels[1])
                    .round()
                    .clamp(0.0, max_source_y as f32) as u32,
                size: destination_patch.size,
            };
            let destination_rgb =
                self.capture_inpaint_scene_square(frame, destination_patch, replace_index)?;
            let source_rgb = if source_patch == destination_patch {
                destination_rgb.clone()
            } else {
                self.capture_inpaint_scene_square(frame, source_patch, replace_index)?
            };
            let patch = build_retouch_patch(
                kind,
                [full_raw.width, full_raw.height],
                [destination_patch.x, destination_patch.y],
                [destination_patch.size, destination_patch.size],
                &destination_rgb,
                [source_patch.x, source_patch.y],
                [source_patch.size, source_patch.size],
                &source_rgb,
                [LAMA_EDGE, LAMA_EDGE],
                source_offset,
                &dabs,
            )
            .ok_or_else(|| format!("Could not build the {} result.", kind.label()))?;
            InpaintStroke::from_tool_result(kind, Some(source_offset), dabs.clone(), patch)
                .ok_or_else(|| format!("The {} result is invalid.", kind.label()))
        })();

        let stroke = match result {
            Ok(stroke) => stroke,
            Err(error) => {
                self.notice = Some(error);
                if self.inpaint_source_anchor.is_none() {
                    self.inpaint_source_pick_active = true;
                }
                self.egui_ctx.request_repaint();
                return;
            }
        };
        let preflight = if let Some(index) = replace_index {
            if index >= self.inpaint_strokes.len() {
                Err(crate::sidecar::SidecarError::Invalid(
                    "retouch replacement target no longer exists".to_owned(),
                ))
            } else {
                let previous = std::mem::replace(&mut self.inpaint_strokes[index], stroke);
                let result =
                    crate::sidecar::preflight_mask_change(&self.masks, &self.inpaint_strokes);
                if result.is_err() {
                    self.inpaint_strokes[index] = previous;
                }
                result
            }
        } else {
            crate::sidecar::preflight_inpaint_addition(&self.masks, &self.inpaint_strokes, &stroke)
                .map(|_| self.inpaint_strokes.push(stroke))
        };
        match preflight {
            Ok(()) => {
                self.note_inpainting_edit_changed();
                self.rebuild_inpaint_layer();
                self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
                self.note_inpainting_changed_for_ai_masks();
                self.queue_preview_processing(ProcessingStage::Tone);
                self.notice = Some(if replace_index.is_some() {
                    format!("{} stroke regenerated.", kind.label())
                } else {
                    format!("{} stroke applied.", kind.label())
                });
            }
            Err(error) => {
                self.notice = Some(format!(
                    "{} result was not applied because the edit cannot fit in the platform sidecar: {error}. Delete an existing mask or retouch result and try again.",
                    kind.label()
                ));
            }
        }
        self.inpaint_source_cache = None;
        self.egui_ctx.request_repaint();
    }

    fn start_inpaint_worker(&mut self, model_path: PathBuf) {
        if self.inpaint_task_id.is_some() || self.inpaint_receiver.is_some() {
            return;
        }
        let Some(source) = self.inpaint_pending_source.take() else {
            self.inpaint_replace_index = None;
            self.notice = Some("The image could not be prepared for inpainting.".to_owned());
            return;
        };
        if self.inpaint_stroke.is_empty() {
            self.inpaint_replace_index = None;
            return;
        }
        let dabs = std::mem::take(&mut self.inpaint_stroke);
        self.last_inpaint_brush_point = None;
        self.inpaint_stroke_texture = None;
        self.inpaint_stroke_texture_key = None;
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.onnx_runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.onnx_runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;
        let generation = self.inpaint_revision;
        let request = InpaintTaskRequest {
            document_id: self.sidecar_generation,
            generation,
            model_path,
            runtime_path,
            runtime_sha256,
            request: InpaintRequest {
                source,
                dabs: dabs.clone(),
            },
            dabs,
        };
        let needs_download = !request.model_path.exists();
        if needs_download {
            let task_id = self.enqueue_background_action(
                TaskKind::Inpainting {
                    document_id: request.document_id,
                    generation,
                },
                "Downloading inpainting model",
                TaskProgress::indeterminate("Waiting for earlier background work…"),
                true,
                BackgroundAction::Inpainting(request),
            );
            self.inpaint_task_id = Some(task_id);
        } else {
            let task_id = self.background_tasks.start_nonblocking(
                TaskKind::Inpainting {
                    document_id: request.document_id,
                    generation,
                },
                "Erasing selection",
                TaskProgress::indeterminate("Running local LaMa inpainting…"),
                true,
            );
            self.start_inpaint_task(task_id, request);
        }
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn poll_inpaint_worker(&mut self) {
        let Some(task_id) = self.inpaint_task_id else {
            return;
        };
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.inpaint_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(event) => {
                        let finished = matches!(event, InpaintEvent::Finished(_));
                        events.push(event);
                        if finished {
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let mut finished = None;
        for event in events {
            match event {
                InpaintEvent::DownloadProgress { downloaded, total } => {
                    self.inpaint_download_progress = Some((downloaded, total));
                    self.inpaint_inferencing = false;
                    self.background_tasks.set_global_visible(task_id, true);
                    self.background_tasks
                        .rename(task_id, "Downloading inpainting model");
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::units(
                            downloaded,
                            total,
                            Some("bytes".to_owned()),
                            "Downloading inpainting model",
                        )
                        .with_detail(format!(
                            "{:.1} / {:.1} MB",
                            downloaded as f64 / 1_000_000.0,
                            total as f64 / 1_000_000.0
                        )),
                    );
                }
                InpaintEvent::Inferencing => {
                    self.inpaint_download_progress = None;
                    self.inpaint_inferencing = true;
                    self.background_tasks.set_global_visible(task_id, false);
                    self.background_tasks.release_current(task_id);
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::indeterminate("Running local LaMa inpainting…"),
                    );
                }
                InpaintEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err("The inpainting worker stopped unexpectedly.".to_owned()));
        }
        let Some(result) = finished else {
            return;
        };

        let cancelled = self.background_task_cancelled(task_id);
        let stale = self.inpaint_job_document_id != self.sidecar_generation
            || self.inpaint_job_generation != self.inpaint_revision;
        let failed_during_inference = self.inpaint_inferencing;
        self.inpaint_receiver = None;
        self.inpaint_task_id = None;
        self.inpaint_download_progress = None;
        self.inpaint_inferencing = false;

        if cancelled || stale {
            self.inpaint_active_dabs = None;
            self.inpaint_replace_index = None;
            self.finish_background_task(task_id);
            self.egui_ctx.request_repaint();
            return;
        }

        match result {
            Ok(result) => {
                let dabs = self.inpaint_active_dabs.take().unwrap_or_default();
                if let Some(stroke) = InpaintStroke::from_result(dabs, result) {
                    let replace_index = self.inpaint_replace_index.take();
                    let mut pending_stroke = Some(stroke);
                    let preflight = if let Some(index) = replace_index {
                        if index >= self.inpaint_strokes.len() {
                            Err(crate::sidecar::SidecarError::Invalid(
                                "inpainting replacement target no longer exists".to_owned(),
                            ))
                        } else {
                            let replacement = pending_stroke.take().expect("inpaint result exists");
                            let previous =
                                std::mem::replace(&mut self.inpaint_strokes[index], replacement);
                            let result = crate::sidecar::preflight_mask_change(
                                &self.masks,
                                &self.inpaint_strokes,
                            );
                            if result.is_err() {
                                self.inpaint_strokes[index] = previous;
                            }
                            result
                        }
                    } else {
                        crate::sidecar::preflight_inpaint_addition(
                            &self.masks,
                            &self.inpaint_strokes,
                            pending_stroke.as_ref().expect("inpaint result exists"),
                        )
                    };

                    match preflight {
                        Ok(()) => {
                            if let Some(stroke) = pending_stroke.take() {
                                self.inpaint_strokes.push(stroke);
                            }
                            self.note_inpainting_edit_changed();
                            self.rebuild_inpaint_layer();
                            self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
                            self.note_inpainting_changed_for_ai_masks();
                            self.queue_preview_processing(ProcessingStage::Tone);
                            self.notice = Some(if replace_index.is_some() {
                                "Inpainting stroke regenerated.".to_owned()
                            } else {
                                "Erase complete.".to_owned()
                            });
                            self.finish_background_task(task_id);
                        }
                        Err(error) => {
                            let message = format!(
                                "Erase result was not applied because the edit cannot fit in the platform sidecar: {error}. Delete an existing mask or erase result and try again."
                            );
                            self.notice = Some(message.clone());
                            if failed_during_inference {
                                self.finish_background_task(task_id);
                            } else {
                                self.fail_background_task(task_id, message);
                            }
                        }
                    }
                } else {
                    self.inpaint_replace_index = None;
                    let message = "Inpainting returned an empty result.".to_owned();
                    self.notice = Some(message.clone());
                    if failed_during_inference {
                        self.finish_background_task(task_id);
                    } else {
                        self.fail_background_task(task_id, message);
                    }
                }
            }
            Err(error) => {
                log::error!("Inpainting failed: {error}");
                self.inpaint_active_dabs = None;
                self.inpaint_replace_index = None;
                let message = format!("Inpainting failed: {error}");
                self.notice = Some(message.clone());
                if failed_during_inference {
                    self.finish_background_task(task_id);
                } else {
                    self.fail_background_task(task_id, message);
                }
            }
        }
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn delete_inpaint_stroke(&mut self, index: usize) {
        if self.inpaint_busy() || index >= self.inpaint_strokes.len() {
            return;
        }
        self.inpaint_strokes.remove(index);
        self.inpaint_hovered_stroke = None;
        self.inpaint_selected_stroke = None;
        self.inpaint_focus_texture = None;
        self.inpaint_focus_texture_key = None;
        self.note_inpainting_edit_changed();
        self.rebuild_inpaint_layer();
        self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
        self.note_inpainting_changed_for_ai_masks();
        self.queue_preview_processing(ProcessingStage::Tone);
        self.notice = Some("Inpainting stroke deleted.".to_owned());
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn rebuild_inpaint_layer(&mut self) {
        if self
            .inpaint_selected_stroke
            .is_some_and(|index| index >= self.inpaint_strokes.len())
        {
            self.inpaint_selected_stroke = None;
        }
        self.inpaint_hovered_stroke = None;
        self.inpaint_focus_texture = None;
        self.inpaint_focus_texture_key = None;
        self.inpaint_layer = compose_inpaint_strokes(&self.inpaint_strokes);
        self.inpaint_texture = None;
        self.inpaint_texture_key = None;
        self.inpaint_texture_revision = self.inpaint_texture_revision.wrapping_add(1);
    }

    #[cfg(not(target_os = "android"))]
    fn lama_model_path(&self) -> PathBuf {
        let root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("auraw/models/lama_fp32.onnx")
    }

    #[cfg(target_os = "android")]
    fn lama_model_path(&self) -> PathBuf {
        self.android_app
            .internal_data_path()
            .unwrap_or_else(std::env::temp_dir)
            .join("models/lama_fp32.onnx")
    }

    pub(crate) fn show_inpainting_dialogs(&mut self, ctx: &egui::Context) {
        if self.inpaint_consent_open {
            crate::ui::responsive_popup(egui::Window::new("Download inpainting model?"), ctx, 520.0)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Inpainting uses the LaMa ONNX model to remove painted content.");
                    ui.label(format!(
                        "The first use downloads {:.0} MB and stores the model in AuRaw's cache.",
                        LAMA_MODEL_BYTES as f64 / 1_000_000.0
                    ));
                    ui.label("Model license: Apache-2.0. The model is optional and can be used only after this download.");
                    ui.label("Inference is local. No photograph or brush stroke is uploaded.");
                    ui.label("When you continue, your device connects directly to Hugging Face. Hugging Face receives connection data such as your IP address and request time under its own privacy policy. AuRaw sends no account identifier or telemetry.");
                    ui.horizontal_wrapped(|ui| {
                        ui.hyperlink_to(
                            "Hugging Face privacy policy",
                            "https://huggingface.co/privacy",
                        );
                        ui.separator();
                        ui.hyperlink_to(
                            "Apache-2.0 model page",
                            "https://huggingface.co/Carve/LaMa-ONNX",
                        );
                    });
                    #[cfg(not(target_os = "android"))]
                    if self.onnx_runtime_path.is_none() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Select a trusted local ONNX Runtime library in Settings before continuing.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Consent, download and continue").clicked() {
                            self.inpaint_consent_open = false;
                            self.start_inpaint_worker(self.lama_model_path());
                        }
                        if ui.button("Cancel").clicked() {
                            self.inpaint_consent_open = false;
                            self.inpaint_pending_source = None;
                            self.inpaint_active_dabs = None;
                            self.inpaint_replace_index = None;
                            self.inpaint_stroke.clear();
                            self.last_inpaint_brush_point = None;
                        }
                    });
                });
        }
        if self.inpaint_receiver.is_some()
            && self
                .inpaint_task_id
                .is_some_and(|id| self.background_task_details_open(id))
        {
            #[cfg(not(target_os = "android"))]
            let mut minimize = false;
            let mut cancel = false;
            crate::ui::responsive_popup(egui::Window::new("Erasing selection"), ctx, 420.0)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    if let Some((downloaded, total)) = self.inpaint_download_progress {
                        let fraction = downloaded as f32 / total.max(1) as f32;
                        ui.label("Downloading lama_fp32.onnx…");
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .show_percentage()
                                .text(format!(
                                    "{:.1} / {:.1} MB",
                                    downloaded as f64 / 1_000_000.0,
                                    total as f64 / 1_000_000.0
                                )),
                        );
                    } else if self.inpaint_inferencing {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Running local LaMa inpainting…");
                        });
                    } else {
                        ui.spinner();
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        #[cfg(not(target_os = "android"))]
                        {
                            minimize = ui.button("Minimize").clicked();
                        }
                        cancel = ui.button("Cancel").clicked();
                    });
                });
            if let Some(task_id) = self.inpaint_task_id {
                #[cfg(not(target_os = "android"))]
                if minimize {
                    self.set_background_task_details_open(task_id, false);
                }
                if cancel {
                    self.cancel_background_task(task_id);
                }
            }
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

fn flatten_inpaint_source_model_region(
    mut rgb_rec2020: Vec<f32>,
    layer: &InpaintLayer,
    origin: [u32; 2],
    size: u32,
    full_dimensions: [u32; 2],
    legacy_camera_to_working: [[f32; 4]; 3],
) -> Result<Vec<f32>, String> {
    let [origin_x, origin_y] = origin;
    let [full_width, full_height] = full_dimensions;
    if size == 0 || full_width == 0 || full_height == 0 {
        return Err("The inpainting source has invalid dimensions.".to_owned());
    }
    let expected = LAMA_EDGE as usize * LAMA_EDGE as usize * 3;
    if rgb_rec2020.len() != expected {
        return Err("The inpainting source is incomplete.".to_owned());
    }
    for patch in layer.patches.iter() {
        if !patch.is_valid() {
            continue;
        }
        for y in 0..LAMA_EDGE {
            let global_y =
                origin_y as f32 + ((y as f32 + 0.5) * size as f32 / LAMA_EDGE as f32) - 0.5;
            for x in 0..LAMA_EDGE {
                let global_x =
                    origin_x as f32 + ((x as f32 + 0.5) * size as f32 / LAMA_EDGE as f32) - 0.5;
                let source_x =
                    (global_x + 0.5) * patch.source_width as f32 / full_width as f32 - 0.5;
                let source_y =
                    (global_y + 0.5) * patch.source_height as f32 / full_height as f32 - 0.5;
                let Some((mut replacement, alpha)) =
                    patch.sample_linear_rec2020_bilinear(source_x, source_y)
                else {
                    continue;
                };
                if alpha <= 1e-6 {
                    continue;
                }
                replacement =
                    patch.resolve_neutral_working_rgb(replacement, legacy_camera_to_working);
                let destination = (y as usize * LAMA_EDGE as usize + x as usize) * 3;
                for channel in 0..3 {
                    rgb_rec2020[destination + channel] = rgb_rec2020[destination + channel]
                        + (replacement[channel] - rgb_rec2020[destination + channel]) * alpha;
                }
            }
        }
    }
    Ok(rgb_rec2020)
}

#[cfg(test)]
mod tests {
    use super::flatten_inpaint_source_model_region;
    use crate::inpainting::LAMA_EDGE;
    use crate::pipeline::{InpaintLayer, InpaintPatch};
    use half::f16;

    #[test]
    fn later_stroke_source_flattens_a_resampled_existing_patch() {
        let rgba16f = [0.5, 0.25, 0.75, 1.0]
            .map(|value| f16::from_f32(value).to_bits())
            .to_vec();
        let patch = InpaintPatch::new_linear_resampled(
            [4, 4],
            [1, 1],
            [2, 2],
            [1, 1],
            rgba16f,
            vec![255],
        )
        .unwrap();
        let layer = InpaintLayer::new(vec![patch]).unwrap();
        let source = vec![0.1; (LAMA_EDGE * LAMA_EDGE * 3) as usize];
        let flattened = flatten_inpaint_source_model_region(
            source,
            &layer,
            [0, 0],
            4,
            [4, 4],
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
        )
        .unwrap();

        assert!((flattened[0] - 0.1).abs() < 1e-6);
        let center = ((256 * LAMA_EDGE + 256) * 3) as usize;
        assert!((flattened[center] - 0.5).abs() < 1e-3);
        assert!((flattened[center + 1] - 0.25).abs() < 1e-3);
        assert!((flattened[center + 2] - 0.75).abs() < 1e-3);
    }
}
