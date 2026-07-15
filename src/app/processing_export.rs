fn aligned_detail_axis(
    min_uv: f32,
    max_uv: f32,
    extent: u32,
    cfa_period: u32,
    viewport_pixels: u32,
    detail_pixel_scale: f32,
) -> (u32, u32) {
    let extent = extent.max(1);
    let period = cfa_period.max(1);
    let visible_start =
        ((min_uv.clamp(0.0, 1.0) * extent as f32).floor() as u32).min(extent.saturating_sub(1));
    let visible_end =
        ((max_uv.clamp(0.0, 1.0) * extent as f32).ceil() as u32).clamp(visible_start + 1, extent);
    let visible_len = visible_end - visible_start;

    // Demosaic, chroma cleanup, clarity, and glow all sample neighbouring
    // pixels. Keep a generous source-space halo, then never display that halo.
    // This prevents the straight crop-edge and coloured zipper artifacts that
    // otherwise become obvious at high zoom.
    let visible_detail_pixels =
        (viewport_pixels.max(1) as f32 * detail_pixel_scale.max(0.1)).max(1.0);
    let support_padding =
        (visible_len as f32 * EXPORT_TILE_HALO as f32 / visible_detail_pixels).ceil() as u32;
    let padding = ((visible_len as f32 * 0.06).ceil() as u32)
        .max(EXPORT_TILE_HALO)
        .max(support_padding);
    let padded_start = visible_start.saturating_sub(padding);
    let padded_end = visible_end.saturating_add(padding).min(extent);
    let aligned_start = (padded_start / period) * period;
    let aligned_end = padded_end
        .div_ceil(period)
        .saturating_mul(period)
        .min(extent)
        .max(aligned_start + 1);
    (aligned_start, aligned_end)
}

fn detail_texture_uv(visible: PreviewUvRect, crop: PreviewUvRect) -> PreviewUvRect {
    let crop_width = (crop.max[0] - crop.min[0]).max(f32::EPSILON);
    let crop_height = (crop.max[1] - crop.min[1]).max(f32::EPSILON);
    PreviewUvRect {
        min: [
            ((visible.min[0] - crop.min[0]) / crop_width).clamp(0.0, 1.0),
            ((visible.min[1] - crop.min[1]) / crop_height).clamp(0.0, 1.0),
        ],
        max: [
            ((visible.max[0] - crop.min[0]) / crop_width).clamp(0.0, 1.0),
            ((visible.max[1] - crop.min[1]) / crop_height).clamp(0.0, 1.0),
        ],
    }
}

fn requested_detail_edge(
    quality: PreviewQuality,
    viewport_pixels: [u32; 2],
    visible: PreviewUvRect,
    crop_width: u32,
    crop_height: u32,
    full_width: u32,
    full_height: u32,
) -> u32 {
    let visible_source_width =
        ((visible.max[0] - visible.min[0]).max(1.0 / full_width.max(1) as f32) * full_width as f32)
            .max(1.0);
    let visible_source_height = ((visible.max[1] - visible.min[1])
        .max(1.0 / full_height.max(1) as f32)
        * full_height as f32)
        .max(1.0);
    let padded_width_pixels =
        viewport_pixels[0].max(1) as f32 * crop_width as f32 / visible_source_width;
    let padded_height_pixels =
        viewport_pixels[1].max(1) as f32 * crop_height as f32 / visible_source_height;
    (padded_width_pixels.max(padded_height_pixels) * quality.detail_pixel_scale())
        .ceil()
        .clamp(256.0, quality.detail_edge() as f32) as u32
}

fn navigation_proxy_edge() -> u32 {
    if cfg!(target_os = "android") { 384 } else { 512 }
}

fn navigation_mask_edge() -> u32 {
    if cfg!(target_os = "android") { 256 } else { 384 }
}

/// Start a detailed crop for every real zoom level above fit. The previous
/// 1.01 cutoff excluded an exact 101% zoom and, together with the former
/// proxy-texel shortcut, kept the tiny navigation image visible until much deeper
/// zoom levels.
const DETAIL_ZOOM_START: f32 = 1.0005;

fn zoom_detail_idle_delay() -> Duration {
    // Wait only long enough to coalesce wheel/pinch events. A full second made
    // the navigation proxy look like the final preview after zooming stopped.
    Duration::from_millis(if cfg!(target_os = "android") { 220 } else { 140 })
}

impl AurawApp {
    pub(crate) fn mark_lens_correction_dirty(&mut self) {
        self.note_edit_changed();
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
                    correction_notice = Some(
                        "Lens correction failed; restored the original RAW geometry.".to_owned(),
                    );
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
        let restored_history_masks = self.history_lens_restore_masks.take();
        let empty_masks = MaskStack::default();
        let preview_masks = restored_history_masks.as_ref().unwrap_or(&empty_masks);
        let params = GpuParams::new(&self.exposure, preview_masks, &preview_raw);
        let mut pipeline = match RawGpuPipeline::new_headless_with_quality(
            &render_state.device,
            &render_state.queue,
            &preview_raw,
            &params,
            ProcessingQuality::Preview,
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                self.notice = Some(format!(
                    "Could not rebuild the corrected GPU preview: {error:#}"
                ));
                return;
            }
        };
        if let Err(error) =
            Self::upload_preview_masks(&pipeline, &render_state.queue, preview_masks, &preview_raw)
        {
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
        if let Some(old) = self.preview_navigation.take() {
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
        self.mask_interaction_last_upload = None;
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
        self.object_consent_open = false;
        self.object_pending_target = None;
        self.object_receiver = None;
        self.object_download_progress = None;
        self.object_inferencing = false;
        self.object_decoder_only = false;
        self.object_generation = self.object_generation.wrapping_add(1);
        self.object_job_generation = 0;
        self.object_job_target = None;
        self.object_cache = None;
        self.dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.detail_dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.navigation_dirty_mask_layers = [false; MAX_LOCAL_MASKS];

        if let Some(restored_masks) = restored_history_masks {
            self.masks = restored_masks;
            self.rehydrate_restored_mask_state();
        }

        self.loaded_raw = Some(full_raw);
        self.preview_raw = Some(preview_raw);
        self.gpu_pipeline = Some(pipeline);
        self.preview_zoom = 1.0;
        self.preview_center = [0.5, 0.5];
        self.preview_visible_uv = PreviewUvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        };
        self.preview_viewport_pixels = [1, 1];
        self.preview_motion_at = None;
        self.preview_touch_navigation_active = false;
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.preview_detail_pending_stage = None;
        self.navigation_pending_stage = None;
        self.preview_detail_urgent = false;
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
        self.preview_detail_pending_stage = None;
        self.preview_detail_urgent = false;
        self.preview_motion_at = Some(Instant::now());
        self.egui_ctx
            .request_repaint_after(zoom_detail_idle_delay());
    }

    /// Queue processing for the full proxy and, while zoomed, both the visible
    /// high-resolution crop and the tiny adjusted full-frame navigation proxy.
    /// The normal full-frame proxy is still deferred until fit view, but zoom
    /// and pan never fall back to an unedited/stale RAW rendition.
    pub(crate) fn queue_preview_processing(&mut self, stage: ProcessingStage) {
        self.pending_stage = Some(match self.pending_stage {
            Some(existing) => existing.min(stage),
            None => stage,
        });

        if self.preview_zoom > DETAIL_ZOOM_START {
            self.preview_detail_pending_stage = Some(match self.preview_detail_pending_stage {
                Some(existing) => existing.min(stage),
                None => stage,
            });
            self.preview_detail_urgent = true;
        }

        if self.preview_zoom > DETAIL_ZOOM_START || self.preview_navigation.is_some() {
            self.navigation_pending_stage = Some(match self.navigation_pending_stage {
                Some(existing) => existing.min(stage),
                None => stage,
            });
        }

        self.notice = None;
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn preview_base_pipeline(&self) -> Option<&RawGpuPipeline> {
        // Keep the normal adjusted full-frame proxy as the zoom backing while
        // it is current. The tiny navigation proxy is only needed after an edit
        // makes that normal proxy stale; otherwise selecting it here needlessly
        // downgrades 101-150% zoom even before the detail crop is ready.
        let use_navigation = self.preview_navigation.is_some() && self.pending_stage.is_some();
        if use_navigation {
            self.preview_navigation
                .as_ref()
                .map(|preview| &preview.pipeline)
        } else {
            self.gpu_pipeline.as_ref()
        }
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
        if let Err(error) =
            Self::upload_preview_masks(&pipeline, &render_state.queue, &self.masks, &preview_raw)
        {
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
        if let Some(old) = self.preview_navigation.take() {
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
        self.preview_detail_pending_stage = None;
        self.navigation_pending_stage = None;
        self.preview_detail_urgent = false;
        self.dirty_mask_layers.fill(false);
        self.detail_dirty_mask_layers.fill(false);
        self.navigation_dirty_mask_layers.fill(false);
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.preview_motion_at = (self.preview_zoom > DETAIL_ZOOM_START).then(Instant::now);
        if self.preview_motion_at.is_some() {
            self.egui_ctx
                .request_repaint_after(zoom_detail_idle_delay());
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
        let idle_delay = zoom_detail_idle_delay();
        if self.preview_zoom <= DETAIL_ZOOM_START {
            if let Some(render_state) = frame.wgpu_render_state() {
                if let Some(old) = self.preview_detail.take() {
                    if let Some(texture_id) = old.pipeline.egui_texture_id {
                        render_state.renderer.write().free_texture(&texture_id);
                    }
                }
            }
            self.preview_motion_at = None;
            self.preview_detail_pending_stage = None;
            self.preview_detail_urgent = false;
            return;
        }
        if self.active_tab != AppTab::Develop
            || self.preview_quality_dirty
            || self.lens_correction_dirty
            || self.load_receiver.is_some()
        {
            return;
        }

        let detail_is_current = self
            .preview_detail
            .as_ref()
            .is_some_and(|detail| detail.revision == self.preview_revision);
        if detail_is_current {
            // Parameter edits are dispatched directly into this current crop by
            // advance_zoomed_processing; rebuilding the RAW crop would waste CPU.
            return;
        }

        let urgent = self.preview_detail_urgent;
        if !urgent {
            let Some(motion_at) = self.preview_motion_at else {
                return;
            };
            let elapsed = motion_at.elapsed();
            if elapsed < idle_delay {
                self.egui_ctx.request_repaint_after(idle_delay - elapsed);
                return;
            }
        }

        // Avoid retrying every frame if allocation fails. A later zoom, edit,
        // or quality change schedules a fresh attempt.
        self.preview_motion_at = None;
        self.preview_detail_urgent = false;
        self.preview_detail_pending_stage = None;

        let Some(full_raw) = self.loaded_raw.as_ref().map(Arc::clone) else {
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let visible = self.preview_visible_uv;
        // Always build the visible detail crop above fit view. The old
        // "full proxy has enough texels" shortcut compared against preview_raw,
        // but the UI may actually be displaying the 384/512-pixel adjusted
        // navigation proxy. That mismatch is what delayed high-quality output
        // until roughly 140-160% zoom.

        let cfa_period = match full_raw.cfa_kind {
            crate::pipeline::CfaKind::Bayer => 2,
            crate::pipeline::CfaKind::XTrans => 6,
        };
        let (x0, x1) = aligned_detail_axis(
            visible.min[0],
            visible.max[0],
            full_raw.width,
            cfa_period,
            self.preview_viewport_pixels[0],
            self.preview_quality.detail_pixel_scale(),
        );
        let (y0, y1) = aligned_detail_axis(
            visible.min[1],
            visible.max[1],
            full_raw.height,
            cfa_period,
            self.preview_viewport_pixels[1],
            self.preview_quality.detail_pixel_scale(),
        );
        let crop_width = x1 - x0;
        let crop_height = y1 - y0;
        let crop_uv = PreviewUvRect {
            min: [
                x0 as f32 / full_raw.width as f32,
                y0 as f32 / full_raw.height as f32,
            ],
            max: [
                x1 as f32 / full_raw.width as f32,
                y1 as f32 / full_raw.height as f32,
            ],
        };
        let texture_uv_rect = detail_texture_uv(visible, crop_uv);
        let detail_spec = ProxySpec {
            max_edge: requested_detail_edge(
                self.preview_quality,
                self.preview_viewport_pixels,
                visible,
                crop_width,
                crop_height,
                full_raw.width,
                full_raw.height,
            ),
        };
        let detail_raw = Arc::new(build_region_proxy(
            &full_raw,
            x0,
            y0,
            crop_width,
            crop_height,
            detail_spec,
        ));
        // The adjustment shader samples the mask atlas with normalized
        // full-image coordinates (tile origin + local pixel divided by the
        // virtual full size). Keep the atlas in that same coordinate space.
        // Cropping/remapping the mask stack here double-transforms every mask
        // once the crop moves away from the image origin.
        let virtual_full_width =
            ((detail_raw.width as f64 * full_raw.width as f64 / crop_width as f64).round() as u32)
                .max(detail_raw.width);
        let virtual_full_height = ((detail_raw.height as f64 * full_raw.height as f64
            / crop_height as f64)
            .round() as u32)
            .max(detail_raw.height);
        let virtual_origin_x =
            (x0 as f64 / full_raw.width as f64 * virtual_full_width as f64).round() as i32;
        let virtual_origin_y =
            (y0 as f64 / full_raw.height as f64 * virtual_full_height as f64).round() as i32;
        let params = GpuParams::new_for_tile(
            &self.target_exposure,
            &self.masks,
            &detail_raw,
            virtual_origin_x,
            virtual_origin_y,
            virtual_full_width,
            virtual_full_height,
        );
        // Prefer the higher-resolution normal proxy whenever its histogram is
        // still valid. Output-only edits do not invalidate ToneStats; RAW/WB
        // edits do, so their freshly updated navigation proxy is the anchor
        // until the deferred normal proxy reaches its Tone stage again.
        let normal_tone_is_current = !matches!(
            self.pending_stage,
            Some(ProcessingStage::Raw | ProcessingStage::Tone)
        );
        let full_frame_tone_pipeline = if normal_tone_is_current {
            self.gpu_pipeline.as_ref().or_else(|| {
                self.preview_navigation
                    .as_ref()
                    .map(|preview| &preview.pipeline)
            })
        } else {
            self.preview_navigation
                .as_ref()
                .map(|preview| &preview.pipeline)
                .or(self.gpu_pipeline.as_ref())
        };
        if let Some(detail) = self.preview_detail.as_mut().filter(|detail| {
            detail.pipeline.width == detail_raw.width && detail.pipeline.height == detail_raw.height
        }) {
            if let Err(error) = detail
                .pipeline
                .upload_raw_tile(&render_state.queue, &detail_raw)
            {
                self.notice = Some(format!(
                    "Could not update the zoomed preview crop: {error:#}"
                ));
                return;
            }
            // A reused detail pipeline already owns the invariant full-frame
            // mask atlas. Panning only replaces the RAW crop, so do not
            // rerasterize every AI/brush mask unless its geometry changed.
            if self.detail_dirty_mask_layers.iter().any(|dirty| *dirty) {
                let edge = detail.pipeline.mask_atlas_edge();
                for layer in 0..MAX_LOCAL_MASKS {
                    if !self.detail_dirty_mask_layers[layer] {
                        continue;
                    }
                    let bytes = self.masks.rasterize_layer(
                        layer,
                        edge,
                        edge,
                        full_raw.width,
                        full_raw.height,
                    );
                    if let Err(error) =
                        detail
                            .pipeline
                            .update_mask_layer(&render_state.queue, layer, &bytes)
                    {
                        self.notice =
                            Some(format!("Could not update the zoomed local mask: {error:#}"));
                        return;
                    }
                    self.detail_dirty_mask_layers[layer] = false;
                }
            }
            detail.pipeline.dispatch_stage(
                &render_state.queue,
                &render_state.device,
                &params,
                ProcessingStage::Raw,
            );
            detail.pipeline.dispatch_stage(
                &render_state.queue,
                &render_state.device,
                &params,
                ProcessingStage::Tone,
            );
            if let Some(full_frame) = full_frame_tone_pipeline {
                detail.pipeline.inherit_tone_statistics(
                    &render_state.queue,
                    &render_state.device,
                    full_frame,
                );
            }
            detail.pipeline.dispatch_stage(
                &render_state.queue,
                &render_state.device,
                &params,
                ProcessingStage::Output,
            );
            detail.uv_rect = visible;
            detail.texture_uv_rect = texture_uv_rect;
            detail.revision = self.preview_revision;
            detail.raw = Arc::clone(&detail_raw);
            detail.source_origin = [x0, y0];
            detail.source_size = [crop_width, crop_height];
            detail.virtual_origin = [virtual_origin_x, virtual_origin_y];
            detail.virtual_full_size = [virtual_full_width, virtual_full_height];
            self.detail_dirty_mask_layers.fill(false);
            self.egui_ctx.request_repaint();
            return;
        }

        let Some(program_template) = self.gpu_pipeline.as_ref() else {
            return;
        };
        let mut pipeline = match RawGpuPipeline::new_headless_reusing_programs(
            &render_state.device,
            &render_state.queue,
            &detail_raw,
            &params,
            ProcessingQuality::Preview,
            program_template,
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
            &self.masks,
            &full_raw,
        ) {
            self.notice = Some(error);
            return;
        }
        pipeline.dispatch_stage(
            &render_state.queue,
            &render_state.device,
            &params,
            ProcessingStage::Raw,
        );
        pipeline.dispatch_stage(
            &render_state.queue,
            &render_state.device,
            &params,
            ProcessingStage::Tone,
        );
        if let Some(full_frame) = full_frame_tone_pipeline {
            pipeline.inherit_tone_statistics(&render_state.queue, &render_state.device, full_frame);
        }
        pipeline.dispatch_stage(
            &render_state.queue,
            &render_state.device,
            &params,
            ProcessingStage::Output,
        );

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
            uv_rect: visible,
            texture_uv_rect,
            revision: self.preview_revision,
            raw: detail_raw,
            source_origin: [x0, y0],
            source_size: [crop_width, crop_height],
            virtual_origin: [virtual_origin_x, virtual_origin_y],
            virtual_full_size: [virtual_full_width, virtual_full_height],
        });
        self.detail_dirty_mask_layers.fill(false);
        self.egui_ctx.request_repaint();
    }

    fn advance_navigation_preview(&mut self, frame: &eframe::Frame) {
        let should_exist = self.preview_zoom > DETAIL_ZOOM_START;
        let should_update = self.navigation_pending_stage.is_some();
        if !should_exist && !should_update {
            return;
        }
        let Some(full_raw) = self.loaded_raw.as_ref().map(Arc::clone) else {
            self.navigation_pending_stage = None;
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        if self.preview_navigation.is_none() {
            if !should_exist {
                self.navigation_pending_stage = None;
                return;
            }
            let raw = if full_raw.width.max(full_raw.height) <= navigation_proxy_edge() {
                Arc::clone(&full_raw)
            } else {
                Arc::new(build_proxy(
                    &full_raw,
                    ProxySpec {
                        max_edge: navigation_proxy_edge(),
                    },
                ))
            };
            let params = GpuParams::new(&self.target_exposure, &self.masks, &raw);
            let Some(template) = self.gpu_pipeline.as_ref() else {
                return;
            };
            let mut pipeline = match RawGpuPipeline::new_headless_reusing_programs_with_mask_edge(
                &render_state.device,
                &render_state.queue,
                &raw,
                &params,
                ProcessingQuality::Preview,
                template,
                navigation_mask_edge(),
            ) {
                Ok(pipeline) => pipeline,
                Err(error) => {
                    self.notice = Some(format!(
                        "Could not prepare the adjusted navigation preview: {error:#}"
                    ));
                    return;
                }
            };
            if let Err(error) =
                Self::upload_preview_masks(&pipeline, &render_state.queue, &self.masks, &raw)
            {
                self.notice = Some(error);
                return;
            }
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
            let mut renderer = render_state.renderer.write();
            pipeline.register_egui_texture(&render_state.device, &mut renderer);
            drop(renderer);
            self.preview_navigation = Some(PreviewNavigation { pipeline, raw });
            self.navigation_pending_stage = None;
            self.navigation_dirty_mask_layers.fill(false);
            self.egui_ctx.request_repaint();
            return;
        }

        let Some(stage) = self.navigation_pending_stage else {
            return;
        };
        let Some(preview) = self.preview_navigation.as_mut() else {
            return;
        };
        if self.navigation_dirty_mask_layers.iter().any(|dirty| *dirty) {
            let edge = preview.pipeline.mask_atlas_edge();
            for layer in 0..MAX_LOCAL_MASKS {
                if !self.navigation_dirty_mask_layers[layer] {
                    continue;
                }
                let bytes = self.masks.rasterize_layer(
                    layer,
                    edge,
                    edge,
                    preview.raw.width,
                    preview.raw.height,
                );
                if let Err(error) =
                    preview
                        .pipeline
                        .update_mask_layer(&render_state.queue, layer, &bytes)
                {
                    self.notice = Some(format!(
                        "Could not update the navigation local mask: {error:#}"
                    ));
                    return;
                }
                self.navigation_dirty_mask_layers[layer] = false;
            }
        }

        let params = GpuParams::new(&self.target_exposure, &self.masks, &preview.raw);
        let stages = match stage {
            ProcessingStage::Raw => &[
                ProcessingStage::Raw,
                ProcessingStage::Tone,
                ProcessingStage::Output,
            ][..],
            ProcessingStage::Tone => &[ProcessingStage::Tone, ProcessingStage::Output][..],
            ProcessingStage::Output => &[ProcessingStage::Output][..],
        };
        for stage in stages {
            preview.pipeline.dispatch_stage(
                &render_state.queue,
                &render_state.device,
                &params,
                *stage,
            );
        }
        self.navigation_pending_stage = None;
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn mark_pipeline_dirty(&mut self) {
        self.note_edit_changed();
        if self.gpu_pipeline.is_none() {
            self.target_exposure = self.exposure;
            return;
        }

        if let Some(stage) = affected_stage(&self.target_exposure, &self.exposure) {
            self.target_exposure = self.exposure;
            self.queue_preview_processing(stage);
        }
    }

    fn advance_zoomed_processing(&mut self, frame: &eframe::Frame) {
        let Some(stage) = self.preview_detail_pending_stage else {
            return;
        };
        let Some(detail) = self
            .preview_detail
            .as_ref()
            .filter(|detail| detail.revision == self.preview_revision)
        else {
            // advance_preview_detail will construct the current visible crop,
            // immediately for edits and after the idle delay for navigation.
            return;
        };
        let Some(full_raw) = self.loaded_raw.as_ref() else {
            self.preview_detail_pending_stage = None;
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        let detail_raw = Arc::clone(&detail.raw);
        let virtual_origin = detail.virtual_origin;
        let virtual_full_size = detail.virtual_full_size;
        let params = GpuParams::new_for_tile(
            &self.target_exposure,
            &self.masks,
            &detail_raw,
            virtual_origin[0],
            virtual_origin[1],
            virtual_full_size[0],
            virtual_full_size[1],
        );

        let normal_tone_is_current = !matches!(
            self.pending_stage,
            Some(ProcessingStage::Raw | ProcessingStage::Tone)
        );
        let full_frame_tone_pipeline = if normal_tone_is_current {
            self.gpu_pipeline.as_ref().or_else(|| {
                self.preview_navigation
                    .as_ref()
                    .map(|preview| &preview.pipeline)
            })
        } else {
            self.preview_navigation
                .as_ref()
                .map(|preview| &preview.pipeline)
                .or(self.gpu_pipeline.as_ref())
        };
        let Some(detail) = self.preview_detail.as_mut() else {
            return;
        };
        if stage == ProcessingStage::Output
            && self.detail_dirty_mask_layers.iter().any(|dirty| *dirty)
        {
            let edge = detail.pipeline.mask_atlas_edge();
            for layer in 0..MAX_LOCAL_MASKS {
                if !self.detail_dirty_mask_layers[layer] {
                    continue;
                }
                // The detail shader addresses this atlas in full-image UVs,
                // so dirty layers must stay full-frame as well. Rasterizing a
                // crop-local atlas makes masks slide, repeat, or disappear
                // while panning.
                let bytes = self.masks.rasterize_layer(
                    layer,
                    edge,
                    edge,
                    full_raw.width,
                    full_raw.height,
                );
                if let Err(error) =
                    detail.pipeline.update_mask_layer(&render_state.queue, layer, &bytes)
                {
                    self.notice = Some(format!(
                        "Could not update the zoomed local mask: {error:#}"
                    ));
                    self.preview_detail_pending_stage = None;
                    return;
                }
                self.detail_dirty_mask_layers[layer] = false;
            }
        }

        if stage == ProcessingStage::Output {
            if let Some(full_frame) = full_frame_tone_pipeline {
                detail.pipeline.inherit_tone_statistics(
                    &render_state.queue,
                    &render_state.device,
                    full_frame,
                );
            }
        }
        detail
            .pipeline
            .dispatch_stage(&render_state.queue, &render_state.device, &params, stage);
        self.preview_detail_pending_stage = match stage {
            ProcessingStage::Raw => Some(ProcessingStage::Tone),
            ProcessingStage::Tone => Some(ProcessingStage::Output),
            ProcessingStage::Output => None,
        };
        if self.preview_detail_pending_stage.is_none() {
            detail.revision = self.preview_revision;
            self.preview_detail_urgent = false;
        }
        self.egui_ctx.request_repaint();
    }

    fn advance_processing(&mut self, frame: &eframe::Frame) {
        if self.preview_zoom > DETAIL_ZOOM_START {
            self.advance_zoomed_processing(frame);
            return;
        }

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
        } else if self.preview_zoom > DETAIL_ZOOM_START {
            if let Some(stage) = self.preview_detail_pending_stage {
                format!("Updating visible zoom crop — {}…", stage.label())
            } else if let Some(notice) = &self.notice {
                notice.clone()
            } else {
                self.image_status.clone()
            }
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
