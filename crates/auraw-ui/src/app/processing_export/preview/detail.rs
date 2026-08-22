use super::*;

impl AurawApp {
    /// Schedules the high-density source crop. The base preview stays visible
    /// while the CPU reduces the RAW region, so interaction never waits for a
    /// sensor-sized proxy build.
    pub(in crate::app) fn advance_preview_detail(&mut self, _frame: &eframe::Frame) {
        let idle_delay = zoom_detail_idle_delay();
        if self.preview.zoom <= DETAIL_ZOOM_START {
            if let Some(old) = self.preview.detail.take() {
                if let Some(texture_id) = old.pipeline.egui_texture_id {
                    self.retire_egui_texture(texture_id);
                }
            }
            // Dropping the receiver invalidates an in-flight CPU result. The
            // worker owns its inputs and can safely finish in the background.
            self.preview.detail_rebuild_receiver = None;
            self.preview.motion_at = None;
            self.preview.detail_pending_stage = None;
            self.preview.detail_urgent = false;
            return;
        }
        if self.preview.touch_navigation_active
            || self.ui.active_tab != AppTab::Develop
            || self.preview.quality_dirty
            || self.develop.lens_correction_dirty
            || self.lens_correction_busy()
            || self.develop.load_receiver.is_some()
            || self.foreground_operation_is(ForegroundOperationKind::AiDenoise)
            || self.preview.detail_rebuild_receiver.is_some()
        {
            return;
        }
        if self.preview.detail_is_current() {
            return;
        }

        let urgent = self.preview.detail_urgent;
        if !urgent {
            let Some(motion_at) = self.preview.motion_at else {
                return;
            };
            let elapsed = motion_at.elapsed();
            if elapsed < idle_delay {
                self.egui_ctx.request_repaint_after(idle_delay - elapsed);
                return;
            }
        }

        let Some(source_raw) = self.develop.loaded_raw.as_ref().map(Arc::clone) else {
            return;
        };
        let request = PreviewDetailRequest {
            source_raw,
            revision: self.preview.revision,
            visible: self.preview.visible_uv,
            viewport_pixels: self.preview.viewport_pixels,
            quality: self.preview.quality,
        };
        self.preview.motion_at = None;
        self.preview.detail_urgent = false;
        self.preview.detail_pending_stage = None;

        let (sender, receiver) = std::sync::mpsc::channel();
        let context = self.egui_ctx.clone();
        match std::thread::Builder::new()
            .name("auraw-preview-detail".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    prepare_preview_detail(request)
                }))
                .unwrap_or_else(|panic| {
                    let message = panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("unknown panic");
                    Err(anyhow::anyhow!(
                        "zoom preview preparation panicked: {message}"
                    ))
                })
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(PreviewDetailRebuildEvent::Finished(result));
                context.request_repaint();
            }) {
            Ok(_) => {
                self.preview.detail_rebuild_receiver = Some(receiver);
                self.egui_ctx
                    .request_repaint_after(Duration::from_millis(50));
            }
            Err(error) => {
                self.preview.detail_urgent = true;
                self.preview.motion_at = Some(Instant::now());
                self.ui.notice = Some(format!("Could not start zoom-preview preparation: {error}"));
            }
        }
    }

    pub(in crate::app) fn poll_preview_detail_rebuild_worker(&mut self, frame: &eframe::Frame) {
        let received = self
            .preview
            .detail_rebuild_receiver
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv);
        let event = match received {
            Some(Ok(event)) => Some(event),
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.preview.detail_rebuild_receiver = None;
                self.preview.detail_urgent = self.preview.zoom > DETAIL_ZOOM_START;
                self.preview.motion_at = self.preview.detail_urgent.then(Instant::now);
                self.ui.notice = Some("Zoom-preview worker stopped unexpectedly.".to_owned());
                None
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => None,
        };
        let Some(PreviewDetailRebuildEvent::Finished(result)) = event else {
            return;
        };
        self.preview.detail_rebuild_receiver = None;
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.preview.detail_urgent = self.preview.zoom > DETAIL_ZOOM_START;
                self.preview.motion_at = self.preview.detail_urgent.then(Instant::now);
                self.ui.notice = Some(format!("Could not prepare the zoomed preview: {error}"));
                return;
            }
        };
        let source_is_current = self
            .develop
            .loaded_raw
            .as_ref()
            .is_some_and(|raw| Arc::ptr_eq(raw, &prepared.source_raw));
        if !source_is_current
            || prepared.revision != self.preview.revision
            || prepared.quality != self.preview.quality
            || self.preview.quality_dirty
        {
            if source_is_current && self.preview.zoom > DETAIL_ZOOM_START {
                self.preview.detail_urgent = true;
                self.preview.motion_at = Some(Instant::now());
            }
            return;
        }
        self.install_prepared_preview_detail(frame, prepared);
    }

    fn install_prepared_preview_detail(
        &mut self,
        frame: &eframe::Frame,
        prepared: PreparedPreviewDetail,
    ) {
        let Some(full_raw) = self.develop.loaded_raw.as_ref().map(Arc::clone) else {
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.preview.detail_urgent = true;
            self.preview.motion_at = Some(Instant::now());
            return;
        };
        let PreparedPreviewDetail {
            revision,
            visible,
            texture_uv_rect,
            source_origin,
            source_size,
            raw: detail_raw,
            ..
        } = prepared;
        let [x0, y0] = source_origin;
        let [crop_width, crop_height] = source_size;
        // Keep mask atlases in full-image coordinates to avoid double-applying
        // crop offsets when a tile becomes the display texture.
        let virtual_full_width = ((detail_raw.width as f64 * full_raw.width as f64
            / crop_width.max(1) as f64)
            .round() as u32)
            .max(detail_raw.width);
        let virtual_full_height = ((detail_raw.height as f64 * full_raw.height as f64
            / crop_height.max(1) as f64)
            .round() as u32)
            .max(detail_raw.height);
        let virtual_origin_x =
            (x0 as f64 / full_raw.width.max(1) as f64 * virtual_full_width as f64).round() as i32;
        let virtual_origin_y =
            (y0 as f64 / full_raw.height.max(1) as f64 * virtual_full_height as f64).round() as i32;
        let mask_region = detail_mask_source_region(
            &self.masks.stack,
            source_origin,
            source_size,
            full_raw.width,
            full_raw.height,
        );
        let params = GpuParams::new_for_tile(
            &self.develop.target_exposure,
            &self.masks.stack,
            &detail_raw,
            virtual_origin_x,
            virtual_origin_y,
            virtual_full_width,
            virtual_full_height,
        )
        .with_vignette_geometry(self.develop.geometry)
        .with_mask_uv_rect_and_extent(
            mask_source_region_uv(mask_region, full_raw.width, full_raw.height),
            mask_region_texture_extent(mask_region, detail_mask_edge()),
        );
        let normal_tone_is_current = !matches!(
            self.preview.pending_stage,
            Some(ProcessingStage::Raw | ProcessingStage::Tone)
        );
        let full_frame_tone_pipeline = if normal_tone_is_current {
            self.preview.gpu_pipeline.as_ref().or_else(|| {
                self.preview
                    .navigation
                    .as_ref()
                    .map(|preview| &preview.pipeline)
            })
        } else {
            self.preview
                .navigation
                .as_ref()
                .map(|preview| &preview.pipeline)
                .or(self.preview.gpu_pipeline.as_ref())
        };
        let required_mask_layers = self.masks.stack.masks.len().max(1);
        if let Some(detail) = self.preview.detail.as_mut().filter(|detail| {
            detail.pipeline.width == detail_raw.width
                && detail.pipeline.height == detail_raw.height
                && detail.pipeline.mask_layer_capacity() >= required_mask_layers
        }) {
            if let Err(error) = detail
                .pipeline
                .upload_raw_tile(&render_state.queue, &detail_raw)
            {
                self.ui.notice = Some(format!(
                    "Could not update the zoomed preview crop: {error:#}"
                ));
                return;
            }
            if let Err(error) = Self::upload_detail_masks(
                &detail.pipeline,
                &render_state.queue,
                &self.masks.stack,
                &full_raw,
                mask_region,
                None,
            ) {
                self.ui.notice = Some(error);
                return;
            }
            if let Err(error) = detail.pipeline.dispatch_stage_with_remove(
                &render_state.queue,
                &render_state.device,
                &params,
                ProcessingStage::Raw,
                &self.inpaint.edits,
                &full_raw,
                &self.develop.target_exposure,
                [x0 as f32, y0 as f32],
                [crop_width as f32, crop_height as f32],
            ) {
                self.ui.notice = Some(format!(
                    "Could not apply Remove to zoomed preview: {error:#}"
                ));
                return;
            }
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
            detail.revision = revision;
            detail.raw = detail_raw;
            detail.source_origin = source_origin;
            detail.source_size = source_size;
            detail.mask_source_region = mask_region;
            detail.virtual_origin = [virtual_origin_x, virtual_origin_y];
            detail.virtual_full_size = [virtual_full_width, virtual_full_height];
            self.masks.detail_dirty_layers.fill(false);
            self.egui_ctx.request_repaint();
            return;
        }

        let Some(program_template) = self.preview.gpu_pipeline.as_ref() else {
            return;
        };
        let mut pipeline = match RawGpuPipeline::new_headless_reusing_programs_with_mask_edge(
            &render_state.device,
            &render_state.queue,
            &detail_raw,
            &params,
            ProcessingQuality::Preview,
            program_template,
            detail_mask_edge(),
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                self.ui.notice = Some(format!("Could not render the zoomed preview: {error:#}"));
                return;
            }
        };
        #[cfg(not(target_os = "android"))]
        if let Err(error) = self.apply_display_output_transform(&render_state.queue, &pipeline) {
            self.ui.notice = Some(
                "Could not prepare the preview color profile. The previous complete preview remains available."
                    .to_owned(),
            );
            crate::diagnostics::record(format!(
                "zoom-preview pipeline display-profile install failed: {error:#}"
            ));
            return;
        }
        if let Err(error) = Self::upload_detail_masks(
            &pipeline,
            &render_state.queue,
            &self.masks.stack,
            &full_raw,
            mask_region,
            None,
        ) {
            self.ui.notice = Some(error);
            return;
        }
        if let Err(error) = pipeline.dispatch_stage_with_remove(
            &render_state.queue,
            &render_state.device,
            &params,
            ProcessingStage::Raw,
            &self.inpaint.edits,
            &full_raw,
            &self.develop.target_exposure,
            [x0 as f32, y0 as f32],
            [crop_width as f32, crop_height as f32],
        ) {
            self.ui.notice = Some(format!(
                "Could not apply Remove to zoomed preview: {error:#}"
            ));
            return;
        }
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
        if let Some(old) = self.preview.detail.take() {
            if let Some(texture_id) = old.pipeline.egui_texture_id {
                self.retire_egui_texture(texture_id);
            }
        }
        pipeline.register_egui_texture(&render_state.device, &mut renderer);
        drop(renderer);

        self.preview.detail = Some(PreviewDetail {
            pipeline,
            uv_rect: visible,
            texture_uv_rect,
            revision,
            raw: detail_raw,
            source_origin,
            source_size,
            mask_source_region: mask_region,
            virtual_origin: [virtual_origin_x, virtual_origin_y],
            virtual_full_size: [virtual_full_width, virtual_full_height],
        });
        self.masks.detail_dirty_layers.fill(false);
        self.egui_ctx.request_repaint();
    }
}

struct PreviewDetailRequest {
    source_raw: Arc<LoadedRaw>,
    revision: u64,
    visible: PreviewUvRect,
    viewport_pixels: [u32; 2],
    quality: PreviewQuality,
}

fn prepare_preview_detail(request: PreviewDetailRequest) -> anyhow::Result<PreparedPreviewDetail> {
    let PreviewDetailRequest {
        source_raw,
        revision,
        visible,
        viewport_pixels,
        quality,
    } = request;
    let cfa_period = match source_raw.cfa_kind {
        crate::pipeline::CfaKind::Bayer => 2,
        crate::pipeline::CfaKind::XTrans => 6,
    };
    let (x0, x1) = aligned_detail_axis(
        visible.min[0],
        visible.max[0],
        source_raw.width,
        cfa_period,
        viewport_pixels[0],
        quality.detail_pixel_scale(),
    );
    let (y0, y1) = aligned_detail_axis(
        visible.min[1],
        visible.max[1],
        source_raw.height,
        cfa_period,
        viewport_pixels[1],
        quality.detail_pixel_scale(),
    );
    let source_size = [x1 - x0, y1 - y0];
    let crop_uv = PreviewUvRect {
        min: [
            x0 as f32 / source_raw.width.max(1) as f32,
            y0 as f32 / source_raw.height.max(1) as f32,
        ],
        max: [
            x1 as f32 / source_raw.width.max(1) as f32,
            y1 as f32 / source_raw.height.max(1) as f32,
        ],
    };
    let raw = Arc::new(build_region_proxy(
        &source_raw,
        x0,
        y0,
        source_size[0],
        source_size[1],
        ProxySpec {
            max_edge: requested_detail_edge(
                quality,
                viewport_pixels,
                visible,
                source_size[0],
                source_size[1],
                source_raw.width,
                source_raw.height,
            ),
        },
    ));
    Ok(PreparedPreviewDetail {
        source_raw,
        revision,
        quality,
        visible,
        texture_uv_rect: detail_texture_uv(visible, crop_uv),
        source_origin: [x0, y0],
        source_size,
        raw,
    })
}
