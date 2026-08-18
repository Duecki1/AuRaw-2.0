use super::*;

impl AurawApp {
    pub(crate) fn preview_base_pipeline(&self) -> Option<&RawGpuPipeline> {
        self.preview.gpu_pipeline.as_ref()
    }

    pub(crate) fn preview_is_preparing(&self) -> bool {
        self.develop.load_receiver.is_some()
            || self.foreground_operation_is(ForegroundOperationKind::AiDenoise)
            || self.preview.rebuild_receiver.is_some()
            || self.preview.quality_dirty
    }

    pub(crate) fn preview_quality_changed(&mut self) {
        self.persist_performance_settings();
        if self.develop.loaded_raw.is_some() || self.develop.load_receiver.is_some() {
            self.preview.quality_dirty = true;
            self.note_preview_motion();
        }
    }

    pub(in crate::app) fn requested_preview_edge_for_source(&self, full_raw: &LoadedRaw) -> u32 {
        let fallback = self.preview.quality
            .proxy_edge_for_viewport(self.preview.viewport_pixels);
        if self.preview.zoom > DETAIL_ZOOM_START {
            return self.develop.preview_raw
                .as_ref()
                .map(|raw| raw.width.max(raw.height))
                .unwrap_or(fallback)
                .min(full_raw.width.max(full_raw.height));
        }

        let span_x = (self.preview.visible_uv.max[0] - self.preview.visible_uv.min[0])
            .abs()
            .clamp(1.0 / full_raw.width.max(1) as f32, 1.0);
        let span_y = (self.preview.visible_uv.max[1] - self.preview.visible_uv.min[1])
            .abs()
            .clamp(1.0 / full_raw.height.max(1) as f32, 1.0);
        let [viewport_width, viewport_height] = self.preview.viewport_pixels.map(|value| value.max(1));
        let quarter_turn = self.develop.geometry.quarter_turns % 2 == 1;
        let (display_for_source_x, display_for_source_y) = if quarter_turn {
            (viewport_height, viewport_width)
        } else {
            (viewport_width, viewport_height)
        };
        let source_x = full_raw.width.max(1) as f64 * f64::from(span_x);
        let source_y = full_raw.height.max(1) as f64 * f64::from(span_y);
        let source_to_display = (f64::from(display_for_source_x) / source_x)
            .max(f64::from(display_for_source_y) / source_y);
        let requested = (full_raw.width.max(full_raw.height) as f64
            * source_to_display
            * f64::from(self.preview.quality.pixel_scale()))
        .ceil() as u32;
        fallback
            .max(requested.saturating_add(6))
            .min(full_raw.width.max(full_raw.height))
    }

    pub(crate) fn preview_source_region_changed(&mut self) {
        if self.preview.zoom > DETAIL_ZOOM_START {
            return;
        }
        if let (Some(full_raw), Some(preview_raw)) =
            (self.develop.loaded_raw.as_ref(), self.develop.preview_raw.as_ref())
        {
            let target = self.requested_preview_edge_for_source(full_raw);
            if preview_raw.width.max(preview_raw.height).saturating_add(5) < target {
                self.preview.quality_dirty = true;
            }
        }
    }

    pub(crate) fn set_preview_viewport_pixels(&mut self, viewport_pixels: [u32; 2]) -> bool {
        if self.preview.viewport_pixels == viewport_pixels {
            return false;
        }
        self.preview.viewport_pixels = viewport_pixels;

        if self.develop.load_receiver.is_some() {
            self.preview.quality_dirty = true;
        }

        if let (Some(full_raw), Some(preview_raw)) =
            (self.develop.loaded_raw.as_ref(), self.develop.preview_raw.as_ref())
        {
            let target_edge = self.requested_preview_edge_for_source(full_raw);
            let current_edge = preview_raw.width.max(preview_raw.height);
            if current_edge.saturating_add(5) < target_edge {
                self.preview.quality_dirty = true;
            }
        }
        true
    }

    pub(in crate::app) fn upload_preview_masks(
        pipeline: &RawGpuPipeline,
        queue: &wgpu::Queue,
        masks: &MaskStack,
        raw: &LoadedRaw,
    ) -> Result<(), String> {
        let edge = pipeline.mask_atlas_edge();
        for layer in 0..masks.masks.len().min(MAX_LOCAL_MASKS) {
            let layer_started = std::time::Instant::now();
            let bytes = masks.rasterize_layer_f16(layer, edge, edge, raw.width, raw.height);
            let raster_elapsed = layer_started.elapsed();
            pipeline
                .update_mask_layer(queue, layer, &bytes)
                .map_err(|error| format!("Could not update preview mask: {error:#}"))?;
            crate::diagnostics::record(format!(
                "Preview mask layer {} rasterized/uploaded in {:.3}s (raster {:.3}s)",
                layer + 1,
                layer_started.elapsed().as_secs_f64(),
                raster_elapsed.as_secs_f64()
            ));
        }
        pipeline
            .update_light_rays_mask_layers(queue, masks, raw.width, raw.height)
            .map_err(|error| format!("Could not update Light Rays mask: {error:#}"))?;
        Ok(())
    }

    pub(in crate::app) fn upload_detail_masks(
        pipeline: &RawGpuPipeline,
        queue: &wgpu::Queue,
        masks: &MaskStack,
        full_raw: &LoadedRaw,
        region: [u32; 4],
        dirty_layers: Option<&[bool; MAX_LOCAL_MASKS]>,
    ) -> Result<(), String> {
        let cropped = masks.cropped_for_region(
            region[0],
            region[1],
            region[2],
            region[3],
            full_raw.width,
            full_raw.height,
        );
        let edge = pipeline.mask_atlas_edge();
        let extent = mask_region_texture_extent(region, edge);
        for layer in 0..masks.masks.len().min(MAX_LOCAL_MASKS) {
            if dirty_layers.is_some_and(|dirty| !dirty[layer]) {
                continue;
            }
            let bytes = cropped.rasterize_layer_f16(
                layer,
                extent[0],
                extent[1],
                region[2],
                region[3],
            );
            pipeline
                .update_mask_layer_region(queue, layer, extent[0], extent[1], &bytes)
                .map_err(|error| format!("Could not update zoomed local mask: {error:#}"))?;
        }
        pipeline
            .update_light_rays_mask_layers(
                queue,
                masks,
                full_raw.width,
                full_raw.height,
            )
            .map_err(|error| format!("Could not update zoomed Light Rays mask: {error:#}"))?;
        Ok(())
    }

    pub(in crate::app) fn poll_preview_rebuild_worker(&mut self, frame: &eframe::Frame) {
        let received = self.preview.rebuild_receiver
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv);
        let event = match received {
            Some(Ok(event)) => Some(event),
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.preview.rebuild_receiver = None;
                self.preview.quality_dirty = false;
                self.ui.notice = Some("Preview rebuild worker stopped unexpectedly.".to_owned());
                None
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => None,
        };
        let Some(PreviewRebuildEvent::Finished(result)) = event else {
            return;
        };
        self.preview.rebuild_receiver = None;
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.preview.quality_dirty = false;
                self.ui.notice = Some(format!("Could not prepare the preview proxy: {error}"));
                return;
            }
        };
        let source_is_current = self.develop.loaded_raw
            .as_ref()
            .is_some_and(|raw| Arc::ptr_eq(raw, &prepared.source_raw));
        if !source_is_current
            || prepared.ai_enabled != self.develop.exposure.ai_denoise_enabled
        {
            self.preview.quality_dirty |= source_is_current;
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            self.preview.quality_dirty = true;
            return;
        };
        let params = GpuParams::new(&self.develop.exposure, &self.masks.stack, &prepared.preview_raw)
            .with_vignette_geometry(self.develop.geometry);
        let program_template = self.preview.gpu_pipeline
            .as_ref()
            .map(RawGpuPipeline::program_template)
            .or_else(|| self.preview.program_template.clone());
        if let Some(template) = program_template.as_ref() {
            self.preview.program_template = Some(template.clone());
        }

        // GPU objects are deliberately created here, after the CPU-only worker
        // result has been accepted for the current document. An abandoned
        // worker can therefore never contend with the next RAW open. Keep the
        // current preview resident for a seamless swap whenever the budget
        // permits it; compiled programs are reused in either path.
        let build_pipeline = || {
            if let Some(template) = program_template.as_ref() {
                RawGpuPipeline::new_headless_reusing_program_template(
                    &render_state.device,
                    &render_state.queue,
                    &prepared.preview_raw,
                    &params,
                    ProcessingQuality::Preview,
                    template,
                )
            } else {
                RawGpuPipeline::new_headless_with_quality(
                    &render_state.device,
                    &render_state.queue,
                    &prepared.preview_raw,
                    &params,
                    ProcessingQuality::Preview,
                )
            }
        };
        let pipeline_started = Instant::now();
        let mut pipeline_result = build_pipeline();
        let needs_in_place_replacement = self.preview.gpu_pipeline.is_some()
            && pipeline_result.as_ref().err().is_some_and(|error| {
                error
                    .to_string()
                    .contains("GPU pipelines already reserve")
            });
        if needs_in_place_replacement {
            crate::diagnostics::record(
                "DPI preview replacement exceeded coexistence budget; released old graph and reused its compiled programs",
            );
            let previous = {
                let mut renderer = render_state.renderer.write();
                self.take_preview_pipeline_and_release_textures(&mut renderer)
            };
            drop(previous);
            pipeline_result = build_pipeline();
        }
        let mut pipeline = match pipeline_result {
            Ok(pipeline) => pipeline,
            Err(error) => {
                self.preview.quality_dirty = false;
                self.ui.notice = Some(format!("Could not rebuild the GPU preview: {error:#}"));
                return;
            }
        };
        crate::diagnostics::record(format!(
            "DPI preview GPU graph prepared on the UI thread in {:.3}s",
            pipeline_started.elapsed().as_secs_f64()
        ));
        #[cfg(not(target_os = "android"))]
        if let Err(error) = self.apply_display_output_transform(&render_state.queue, &pipeline)
        {
            self.ui.notice = Some(format!("Could not prepare the preview color profile: {error:#}"));
            self.preview.quality_dirty = false;
            return;
        }
        if let Err(error) = Self::upload_preview_masks(
            &pipeline,
            &render_state.queue,
            &self.masks.stack,
            &prepared.preview_raw,
        ) {
            self.ui.notice = Some(error);
            self.preview.quality_dirty = false;
            return;
        }
        if let Err(error) = pipeline.update_inpaint_layer(
            &render_state.queue,
            self.inpaint.layer.as_ref(),
            0,
            0,
            prepared.preview_raw.width,
            prepared.preview_raw.height,
        ) {
            self.ui.notice = Some(format!("Could not rebuild preview inpainting: {error:#}"));
            self.preview.quality_dirty = false;
            return;
        }
        pipeline.recompute(&render_state.queue, &render_state.device, &params);
        let previous = {
            let mut renderer = render_state.renderer.write();
            let previous = self.take_preview_pipeline_and_release_textures(&mut renderer);
            pipeline.register_egui_texture(&render_state.device, &mut renderer);
            previous
        };
        drop(previous);

        self.preview.program_template = Some(pipeline.program_template());
        self.develop.preview_raw = Some(prepared.preview_raw);
        self.preview.gpu_pipeline = Some(pipeline);
        #[cfg(target_os = "android")]
        {
            // Keep the mobile lens toggle cache coherent with the newly
            // selected DPI proxy. Without this, the next lens toggle can
            // restore the lower-resolution proxy that preceded this rebuild.
            if self.develop.lens_correction.applied {
                if let (Some(selection), Some(full_raw), Some(preview_raw)) = (
                    self.develop.lens_correction.selected_lens(),
                    self.develop.loaded_raw.as_ref(),
                    self.develop.preview_raw.as_ref(),
                ) {
                    self.preview.lens_corrected_cache = Some((
                        selection,
                        prepared.quality,
                        Arc::clone(full_raw),
                        Arc::clone(preview_raw),
                    ));
                }
            } else if let Some(preview_raw) = self.develop.preview_raw.as_ref() {
                self.preview.lens_original_cache =
                    Some((prepared.quality, Arc::clone(preview_raw)));
            }
        }
        self.inpaint.source_cache = None;
        self.develop.target_exposure = self.develop.exposure;
        self.preview.pending_stage = None;
        self.preview.detail_pending_stage = None;
        self.preview.navigation_pending_stage = None;
        self.preview.detail_urgent = false;
        self.masks.dirty_layers.fill(false);
        self.masks.detail_dirty_layers.fill(false);
        self.masks.navigation_dirty_layers.fill(false);
        self.preview.revision = self.preview.revision.wrapping_add(1);
        self.preview.motion_at = (self.preview.zoom > DETAIL_ZOOM_START).then(Instant::now);
        if let (Some(full), Some(preview)) = (&self.develop.loaded_raw, &self.develop.preview_raw) {
            self.develop.image_status = format!(
                "{} {} — full {}×{}, preview {}×{} ({})",
                full.camera_make,
                full.camera_model,
                full.width,
                full.height,
                preview.width,
                preview.height,
                prepared.quality.label(),
            );
            let latest_edge = self.requested_preview_edge_for_source(full);
            if prepared.quality != self.preview.quality
                || preview.width.max(preview.height).saturating_add(5) < latest_edge
            {
                self.preview.quality_dirty = true;
            }
        }
        crate::diagnostics::record(format!(
            "DPI preview rebuild installed: edge {} -> {}x{} ({})",
            prepared.requested_edge,
            self.develop.preview_raw.as_ref().map_or(0, |raw| raw.width),
            self.develop.preview_raw.as_ref().map_or(0, |raw| raw.height),
            prepared.quality.label(),
        ));
    }

    pub(in crate::app) fn apply_pending_preview_quality(&mut self, _frame: &eframe::Frame) {
        if self.preview.rebuild_receiver.is_some() {
            return;
        }
        if !self.preview.quality_dirty
            || self.develop.load_receiver.is_some()
            || self.lens_correction_busy()
        {
            return;
        }
        #[cfg(target_os = "android")]
        if self.foreground_operation_is(ForegroundOperationKind::AiDenoise) {
            return;
        }
        let Some(source_raw) = self.develop.loaded_raw.as_ref().map(Arc::clone) else {
            self.preview.quality_dirty = false;
            return;
        };
        let requested_edge = self.requested_preview_edge_for_source(&source_raw);
        let quality = self.preview.quality;
        let ai_enabled = self.develop.exposure.ai_denoise_enabled;

        // Viewport measurements often settle during the same frame in which a
        // RAW is installed. Do not rebuild an already-sharp proxy merely because
        // that measurement set the dirty bit. This also makes fit -> zoom -> fit
        // a no-op for the main surface.
        let current_is_sufficient = self.develop.preview_raw
            .as_ref()
            .zip(self.preview.gpu_pipeline.as_ref())
            .is_some_and(|(raw, pipeline)| {
                raw.width.max(raw.height).saturating_add(5) >= requested_edge
                    && pipeline.immutable_ai_source_matches(source_raw.cfa_kind, ai_enabled)
            });
        if current_is_sufficient {
            self.preview.quality_dirty = false;
            return;
        }

        for texture_id in [
            self.preview.detail
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
            self.preview.navigation
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
        ]
        .into_iter()
        .flatten()
        {
            self.retire_egui_texture(texture_id);
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let context = self.egui_ctx.clone();
        let worker_source = Arc::clone(&source_raw);
        let spawn = std::thread::Builder::new()
            .name("auraw-preview-rebuild".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let preview_raw = if worker_source.width.max(worker_source.height)
                        <= requested_edge
                    {
                        Arc::clone(&worker_source)
                    } else {
                        Arc::new(build_proxy(
                            &worker_source,
                            ProxySpec {
                                max_edge: requested_edge,
                            },
                        ))
                    };
                    Ok::<_, anyhow::Error>(PreparedPreviewRebuild {
                        source_raw: worker_source,
                        preview_raw,
                        quality,
                        requested_edge,
                        ai_enabled,
                    })
                }))
                .unwrap_or_else(|panic| {
                    let message = panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("unknown panic");
                    Err(anyhow::anyhow!("preview rebuild panicked: {message}"))
                })
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(PreviewRebuildEvent::Finished(result));
                context.request_repaint();
            });
        match spawn {
            Ok(_) => {
                self.preview.quality_dirty = false;
                self.preview.rebuild_receiver = Some(receiver);
                self.egui_ctx.request_repaint_after(Duration::from_millis(50));
            }
            Err(error) => {
                self.ui.notice = Some(format!("Could not start preview rebuild: {error}"));
            }
        }
    }
}
