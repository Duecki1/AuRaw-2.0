impl AurawApp {
    pub(crate) fn mark_lens_correction_dirty(&mut self) {
        if self.original_raw.is_some() {
            self.lens_correction_dirty = true;
            self.notice = None;
        }
    }

    fn apply_pending_lens_correction(&mut self, frame: &eframe::Frame) {
        if !self.lens_correction_dirty {
            return;
        }
        self.lens_correction_dirty = false;

        let Some(original_raw) = self.original_raw.as_ref().map(Arc::clone) else {
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            return;
        };

        let mut correction_notice = None;
        let (full_raw, applied_label) = if self.lens_correction.enabled {
            let Some(selection) = self.lens_correction.selected_lens() else {
                self.lens_correction.enabled = false;
                self.lens_correction.applied = false;
                self.lens_correction.catalog.status =
                    "Select a lens profile before enabling correction.".to_owned();
                return;
            };
            match apply_lensfun_correction(&original_raw, &selection) {
                Ok(corrected) => (Arc::new(corrected), Some(selection.label())),
                Err(error) => {
                    self.lens_correction.enabled = false;
                    self.lens_correction.applied = false;
                    self.lens_correction.catalog.status =
                        format!("Could not apply {}: {error:#}", selection.label());
                    correction_notice = Some("Lens correction failed; restored the original RAW geometry.".to_owned());
                    (Arc::clone(&original_raw), None)
                }
            }
        } else {
            (Arc::clone(&original_raw), None)
        };

        let preview_spec = ProxySpec {
            max_edge: self.preview_quality.proxy_edge(),
        };
        let preview_raw = if full_raw.width.max(full_raw.height) <= preview_spec.max_edge {
            Arc::clone(&full_raw)
        } else {
            Arc::new(build_proxy(&full_raw, preview_spec))
        };
        let params = GpuParams::new(&self.exposure, &MaskStack::default(), &preview_raw);
        let mut pipeline = match RawGpuPipeline::new_headless_with_quality(
            &render_state.device,
            &render_state.queue,
            &preview_raw,
            &params,
            ProcessingQuality::Preview,
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                self.notice =
                    Some(format!("Could not rebuild the corrected GPU preview: {error:#}"));
                return;
            }
        };
        pipeline.recompute(&render_state.queue, &render_state.device, &params);

        let mut renderer = render_state.renderer.write();
        if let Some(old) = self.gpu_pipeline.take() {
            if let Some(texture_id) = old.egui_texture_id {
                renderer.free_texture(&texture_id);
            }
        }
        if let Some(old) = self.preview_detail.take() {
            if let Some(texture_id) = old.pipeline.egui_texture_id {
                renderer.free_texture(&texture_id);
            }
        }
        pipeline.register_egui_texture(&render_state.device, &mut renderer);
        drop(renderer);

        // Existing local masks are tied to the previous image geometry. Clear
        // them rather than silently applying them to shifted content.
        self.masks.clear();
        self.active_mask_tool = None;
        self.brush_mode = BrushMode::Paint;
        self.mask_drag = None;
        self.last_brush_point = None;
        self.mask_interaction_dirty_layer = None;
        self.mask_interaction_frame_count = 0;
        self.mask_interaction_has_uncommitted_change = false;
        self.mask_overlay_revision = self.mask_overlay_revision.wrapping_add(1);
        self.mask_overlay_texture = None;
        self.mask_overlay_texture_key = None;
        self.mask_overlay_blink = None;
        self.mask_thumbnail_group_textures.clear();
        self.mask_thumbnail_component_mask = None;
        self.mask_thumbnail_component_textures.clear();
        self.mask_thumbnail_revision = self.mask_overlay_revision;
        self.mask_source_cache = None;
        self.subject_mask_cache = None;
        self.subject_consent_open = false;
        self.subject_receiver = None;
        self.subject_download_progress = None;
        self.subject_inferencing = false;
        self.dirty_mask_layers = [false; MAX_LOCAL_MASKS];

        self.loaded_raw = Some(full_raw);
        self.preview_raw = Some(preview_raw);
        self.gpu_pipeline = Some(pipeline);
        self.preview_zoom = 1.0;
        self.preview_center = [0.5, 0.5];
        self.preview_visible_uv = PreviewUvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        };
        self.preview_motion_at = None;
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.target_exposure = self.exposure;
        self.pending_stage = None;
        self.lens_correction.applied = applied_label.is_some();
        if let Some(label) = applied_label {
            self.lens_correction.catalog.status = format!("Applied {label}");
        } else if correction_notice.is_none() {
            self.lens_correction.catalog.status =
                "Lens correction disabled; using the original RAW geometry.".to_owned();
        }
        self.notice = correction_notice;
    }

    pub(crate) fn note_preview_motion(&mut self) {
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.preview_motion_at = Some(Instant::now());
        self.egui_ctx
            .request_repaint_after(Duration::from_millis(1_000));
    }

    pub(crate) fn preview_quality_changed(&mut self) {
        if self.loaded_raw.is_some() || self.load_receiver.is_some() {
            self.preview_quality_dirty = true;
            self.note_preview_motion();
        }
    }

    fn upload_preview_masks(
        pipeline: &RawGpuPipeline,
        queue: &wgpu::Queue,
        masks: &MaskStack,
        raw: &LoadedRaw,
    ) -> Result<(), String> {
        let edge = pipeline.mask_atlas_edge();
        for layer in 0..masks.masks.len().min(MAX_LOCAL_MASKS) {
            let bytes = masks.rasterize_layer(layer, edge, edge, raw.width, raw.height);
            pipeline
                .update_mask_layer(queue, layer, &bytes)
                .map_err(|error| format!("Could not update preview mask: {error:#}"))?;
        }
        Ok(())
    }

    fn apply_pending_preview_quality(&mut self, frame: &eframe::Frame) {
        if !self.preview_quality_dirty || self.load_receiver.is_some() {
            return;
        }
        let Some(full_raw) = self.loaded_raw.as_ref().map(Arc::clone) else {
            self.preview_quality_dirty = false;
            return;
        };
        self.preview_quality_dirty = false;
        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            return;
        };

        let spec = ProxySpec {
            max_edge: self.preview_quality.proxy_edge(),
        };
        let preview_raw = if full_raw.width.max(full_raw.height) <= spec.max_edge {
            Arc::clone(&full_raw)
        } else {
            Arc::new(build_proxy(&full_raw, spec))
        };
        let params = GpuParams::new(&self.exposure, &self.masks, &preview_raw);
        let mut pipeline = match RawGpuPipeline::new_headless_with_quality(
            &render_state.device,
            &render_state.queue,
            &preview_raw,
            &params,
            ProcessingQuality::Preview,
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                self.notice = Some(format!("Could not rebuild the GPU preview: {error:#}"));
                return;
            }
        };
        if let Err(error) = Self::upload_preview_masks(
            &pipeline,
            &render_state.queue,
            &self.masks,
            &preview_raw,
        ) {
            self.notice = Some(error);
            return;
        }
        pipeline.recompute(&render_state.queue, &render_state.device, &params);

        let mut renderer = render_state.renderer.write();
        if let Some(old) = self.gpu_pipeline.take() {
            if let Some(texture_id) = old.egui_texture_id {
                renderer.free_texture(&texture_id);
            }
        }
        if let Some(old) = self.preview_detail.take() {
            if let Some(texture_id) = old.pipeline.egui_texture_id {
                renderer.free_texture(&texture_id);
            }
        }
        pipeline.register_egui_texture(&render_state.device, &mut renderer);
        drop(renderer);

        self.preview_raw = Some(preview_raw);
        self.gpu_pipeline = Some(pipeline);
        self.target_exposure = self.exposure;
        self.pending_stage = None;
        self.dirty_mask_layers.fill(false);
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.preview_motion_at = (self.preview_zoom > 1.01).then(Instant::now);
        if self.preview_motion_at.is_some() {
            self.egui_ctx
                .request_repaint_after(Duration::from_millis(1_000));
        }
        if let Some(raw) = &self.preview_raw {
            if let Some(full) = &self.loaded_raw {
                self.image_status = format!(
                    "{} {} — full {}×{}, preview {}×{} ({})",
                    full.camera_make,
                    full.camera_model,
                    full.width,
                    full.height,
                    raw.width,
                    raw.height,
                    self.preview_quality.label(),
                );
            }
        }
    }

    fn advance_preview_detail(&mut self, frame: &eframe::Frame) {
        const IDLE_DELAY: Duration = Duration::from_millis(1_000);
        if self.preview_zoom <= 1.01 {
            if let Some(render_state) = frame.wgpu_render_state() {
                if let Some(old) = self.preview_detail.take() {
                    if let Some(texture_id) = old.pipeline.egui_texture_id {
                        render_state.renderer.write().free_texture(&texture_id);
                    }
                }
            }
            self.preview_motion_at = None;
            return;
        }
        if self.active_tab != AppTab::Develop
            || self.pending_stage.is_some()
            || self.preview_quality_dirty
            || self.lens_correction_dirty
            || self.load_receiver.is_some()
        {
            return;
        }
        if self
            .preview_detail
            .as_ref()
            .is_some_and(|detail| detail.revision == self.preview_revision)
        {
            return;
        }
        let Some(motion_at) = self.preview_motion_at else {
            return;
        };
        let elapsed = motion_at.elapsed();
        if elapsed < IDLE_DELAY {
            self.egui_ctx.request_repaint_after(IDLE_DELAY - elapsed);
            return;
        }
        // Avoid retrying every frame if allocation fails. A later zoom, edit,
        // or quality change schedules a fresh attempt.
        self.preview_motion_at = None;

        let Some(full_raw) = self.loaded_raw.as_ref().map(Arc::clone) else {
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let visible = self.preview_visible_uv;
        let visible_width = (visible.max[0] - visible.min[0]).max(1.0 / full_raw.width as f32);
        let visible_height = (visible.max[1] - visible.min[1]).max(1.0 / full_raw.height as f32);
        let pad_u = (visible_width * 0.04).max(32.0 / full_raw.width.max(1) as f32);
        let pad_v = (visible_height * 0.04).max(32.0 / full_raw.height.max(1) as f32);
        let min_u = (visible.min[0] - pad_u).clamp(0.0, 1.0);
        let min_v = (visible.min[1] - pad_v).clamp(0.0, 1.0);
        let max_u = (visible.max[0] + pad_u).clamp(0.0, 1.0);
        let max_v = (visible.max[1] + pad_v).clamp(0.0, 1.0);

        let x0 = ((min_u * full_raw.width as f32).floor() as u32)
            .min(full_raw.width.saturating_sub(1));
        let y0 = ((min_v * full_raw.height as f32).floor() as u32)
            .min(full_raw.height.saturating_sub(1));
        let x1 = ((max_u * full_raw.width as f32).ceil() as u32)
            .clamp(x0 + 1, full_raw.width);
        let y1 = ((max_v * full_raw.height as f32).ceil() as u32)
            .clamp(y0 + 1, full_raw.height);
        let crop_width = x1 - x0;
        let crop_height = y1 - y0;
        let crop = crop_raw(&full_raw, x0, y0, crop_width, crop_height);
        let detail_spec = ProxySpec {
            max_edge: self.preview_quality.detail_edge(),
        };
        let detail_raw = if crop.width.max(crop.height) <= detail_spec.max_edge {
            Arc::new(crop)
        } else {
            Arc::new(build_proxy(&crop, detail_spec))
        };
        let detail_masks = self.masks.cropped_for_region(
            x0,
            y0,
            crop_width,
            crop_height,
            full_raw.width,
            full_raw.height,
        );
        let virtual_full_width = ((detail_raw.width as f64 * full_raw.width as f64
            / crop_width as f64)
            .round() as u32)
            .max(detail_raw.width);
        let virtual_full_height = ((detail_raw.height as f64 * full_raw.height as f64
            / crop_height as f64)
            .round() as u32)
            .max(detail_raw.height);
        let virtual_origin_x = (x0 as f64 / full_raw.width as f64
            * virtual_full_width as f64)
            .round() as i32;
        let virtual_origin_y = (y0 as f64 / full_raw.height as f64
            * virtual_full_height as f64)
            .round() as i32;
        let params = GpuParams::new_for_tile(
            &self.exposure,
            &detail_masks,
            &detail_raw,
            virtual_origin_x,
            virtual_origin_y,
            virtual_full_width,
            virtual_full_height,
        );
        let mut pipeline = match RawGpuPipeline::new_headless_with_quality(
            &render_state.device,
            &render_state.queue,
            &detail_raw,
            &params,
            ProcessingQuality::Preview,
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                self.notice = Some(format!("Could not render the zoomed preview: {error:#}"));
                return;
            }
        };
        if let Err(error) = Self::upload_preview_masks(
            &pipeline,
            &render_state.queue,
            &detail_masks,
            &detail_raw,
        ) {
            self.notice = Some(error);
            return;
        }
        pipeline.recompute(&render_state.queue, &render_state.device, &params);

        let mut renderer = render_state.renderer.write();
        if let Some(old) = self.preview_detail.take() {
            if let Some(texture_id) = old.pipeline.egui_texture_id {
                renderer.free_texture(&texture_id);
            }
        }
        pipeline.register_egui_texture(&render_state.device, &mut renderer);
        drop(renderer);

        self.preview_detail = Some(PreviewDetail {
            pipeline,
            uv_rect: PreviewUvRect {
                min: [
                    x0 as f32 / full_raw.width as f32,
                    y0 as f32 / full_raw.height as f32,
                ],
                max: [
                    x1 as f32 / full_raw.width as f32,
                    y1 as f32 / full_raw.height as f32,
                ],
            },
            revision: self.preview_revision,
        });
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn mark_pipeline_dirty(&mut self) {
        if self.gpu_pipeline.is_none() {
            self.target_exposure = self.exposure;
            return;
        }

        if let Some(stage) = affected_stage(&self.target_exposure, &self.exposure) {
            self.pending_stage = Some(match self.pending_stage {
                Some(existing) => existing.min(stage),
                None => stage,
            });
            self.target_exposure = self.exposure;
            self.notice = None;
            self.note_preview_motion();
        }
    }

    fn advance_processing(&mut self, frame: &eframe::Frame) {
        let Some(stage) = self.pending_stage else {
            return;
        };
        let (Some(raw), Some(pipeline)) = (&self.preview_raw, &self.gpu_pipeline) else {
            self.pending_stage = None;
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        if stage == ProcessingStage::Output && self.dirty_mask_layers.iter().any(|dirty| *dirty) {
            let edge = pipeline.mask_atlas_edge();
            let mut upload_error = None;
            for layer in 0..MAX_LOCAL_MASKS {
                if !self.dirty_mask_layers[layer] {
                    continue;
                }
                let bytes = self
                    .masks
                    .rasterize_layer(layer, edge, edge, raw.width, raw.height);
                if let Err(error) = pipeline.update_mask_layer(&render_state.queue, layer, &bytes) {
                    upload_error = Some(format!("Could not update local mask: {error:#}"));
                    break;
                }
                self.dirty_mask_layers[layer] = false;
            }
            if let Some(error) = upload_error {
                self.notice = Some(error);
                return;
            }
        }

        let params = GpuParams::new(&self.target_exposure, &self.masks, raw);
        pipeline.dispatch_stage(&render_state.queue, &render_state.device, &params, stage);
        self.pending_stage = match stage {
            ProcessingStage::Raw => Some(ProcessingStage::Tone),
            ProcessingStage::Tone => Some(ProcessingStage::Output),
            ProcessingStage::Output => None,
        };
    }

    pub(crate) fn can_export(&self) -> bool {
        self.loaded_raw.is_some()
            && self.preview_raw.is_some()
            && self.export_receiver.is_none()
            && !self.export_publish_pending
            && self.load_receiver.is_none()
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn export_png(&mut self, frame: &eframe::Frame) {
        if !self.can_export() {
            return;
        }

        let default_name = self
            .current_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}-auraw.png"))
            .unwrap_or_else(|| "auraw-export.png".to_owned());
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("PNG image", &["png"])
            .set_file_name(default_name)
            .save_file()
        else {
            return;
        };
        let has_png_extension = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some(extension) if extension.eq_ignore_ascii_case("png")
        );
        if !has_png_extension {
            path.set_extension("png");
        }

        self.start_export(path, frame);
    }

    #[cfg(target_os = "android")]
    pub(crate) fn export_png(&mut self, frame: &eframe::Frame) {
        if !self.can_export() {
            return;
        }

        let Some(data_dir) = self.android_app.internal_data_path() else {
            self.notice = Some("Android did not provide an app data directory.".to_owned());
            return;
        };
        let export_dir = data_dir.join("cache").join("exports");
        if let Err(error) = std::fs::create_dir_all(&export_dir) {
            self.notice = Some(format!("Could not prepare Android export cache: {error}"));
            return;
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = export_dir.join(format!("AuRaw-{timestamp}.png"));
        self.start_export(path, frame);
    }

    fn start_export(&mut self, path: PathBuf, frame: &eframe::Frame) {
        if !self.can_export() {
            return;
        }

        let Some(raw) = &self.loaded_raw else {
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            return;
        };

        let source_file_name = self
            .current_path
            .as_ref()
            .and_then(|source| source.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .or_else(|| self.current_label.clone());
        let metadata = ExportMetadata::from_raw(raw, source_file_name);
        self.export_receiver = Some(spawn_tiled_png_export(
            render_state.device.clone(),
            render_state.queue.clone(),
            Arc::clone(raw),
            self.exposure,
            self.masks.clone(),
            path,
            TileSpec::default(),
            self.export_settings,
            metadata,
        ));
        self.export_progress = Some((0, 0));
        self.notice = None;
    }

    #[cfg(target_os = "android")]
    fn poll_android_export_publish(&mut self) {
        while let Some(result) = crate::android::take_export_publish_result() {
            self.export_publish_pending = false;
            match result {
                crate::android::ExportPublishResult::Published(location) => {
                    self.notice = Some(format!("Exported to {location}"));
                }
                crate::android::ExportPublishResult::Failed(error) => {
                    self.notice = Some(format!("Export failed: {error}"));
                    log::error!("Android export publish failed: {error}");
                }
            }
        }
    }

    fn poll_export_worker(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.export_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let mut finished = false;
        for event in events {
            match event {
                ExportEvent::Progress {
                    completed_tiles,
                    total_tiles,
                } => self.export_progress = Some((completed_tiles, total_tiles)),
                ExportEvent::Finished(result) => {
                    finished = true;
                    self.export_progress = None;
                    match result {
                        Ok(path) => {
                            #[cfg(not(target_os = "android"))]
                            {
                                self.notice = Some(format!("Exported {}", path.display()));
                            }

                            #[cfg(target_os = "android")]
                            {
                                let display_name = path
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("AuRaw-export.png")
                                    .to_owned();
                                match crate::android::publish_png(
                                    &self.android_app,
                                    &path,
                                    &display_name,
                                ) {
                                    Ok(()) => {
                                        self.export_publish_pending = true;
                                        self.notice = Some("Saving to Pictures/AuRaw…".to_owned());
                                    }
                                    Err(error) => {
                                        let _ = std::fs::remove_file(&path);
                                        self.notice = Some(format!("Export failed: {error}"));
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            self.notice = Some(format!("Export failed: {error}"));
                            log::error!("export failed: {error}");
                        }
                    }
                }
            }
        }

        if finished || disconnected {
            self.export_receiver = None;
            if disconnected && self.notice.is_none() {
                self.export_progress = None;
                self.notice = Some("Export worker stopped unexpectedly.".to_owned());
            }
        }
    }

    fn refresh_status(&mut self) {
        self.status = if let Some(label) = &self.loading_label {
            format!("Decoding and preparing proxy for {label}…")
        } else if let Some((completed, total)) = self.export_progress {
            if total == 0 {
                "Preparing tiled export…".to_owned()
            } else {
                format!("Exporting PNG — tile {completed}/{total}")
            }
        } else if self.export_publish_pending {
            "Saving to Pictures/AuRaw…".to_owned()
        } else if let Some(stage) = self.pending_stage {
            format!("Updating preview — {}…", stage.label())
        } else if let Some(notice) = &self.notice {
            notice.clone()
        } else {
            self.image_status.clone()
        };
    }

    pub(crate) fn reset_develop_adjustments(&mut self) {
        let previous = self.exposure;
        self.exposure = ExposureParams::scene_referred_default();

        // Highlight reconstruction is an application-level processing preference,
        // not one of the Lightroom-style Develop adjustments.
        self.exposure.highlight_method = previous.highlight_method;
        self.exposure.highlight_clip = previous.highlight_clip;
        self.exposure.highlight_reconstruction = previous.highlight_reconstruction;
        self.exposure.highlight_iterations = previous.highlight_iterations;
        self.exposure.highlight_color_adaptation = previous.highlight_color_adaptation;

        // Demosaic selection is likewise a raw-processing preference rather
        // than a Develop adjustment. Resetting exposure/tone controls must not
        // silently change the reconstruction algorithm.
        self.exposure.demosaic_mode = previous.demosaic_mode;
        self.exposure.dual_threshold = previous.dual_threshold;
        self.exposure.frequency_chroma = previous.frequency_chroma;

        self.mark_pipeline_dirty();
    }

    pub(crate) fn reset_highlight_reconstruction_settings(&mut self) {
        let defaults = ExposureParams::default();
        self.exposure.highlight_method = defaults.highlight_method;
        self.exposure.highlight_clip = defaults.highlight_clip;
        self.exposure.highlight_reconstruction = defaults.highlight_reconstruction;
        self.exposure.highlight_iterations = defaults.highlight_iterations;
        self.exposure.highlight_color_adaptation = defaults.highlight_color_adaptation;
        self.mark_pipeline_dirty();
    }
}
