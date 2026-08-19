use super::*;

impl AurawApp {
    pub(in crate::app) fn advance_preview_detail(&mut self, frame: &eframe::Frame) {
        let idle_delay = zoom_detail_idle_delay();
        if self.preview.zoom <= DETAIL_ZOOM_START {
            if frame.wgpu_render_state().is_some() {
                if let Some(old) = self.preview.detail.take() {
                    if let Some(texture_id) = old.pipeline.egui_texture_id {
                        self.retire_egui_texture(texture_id);
                    }
                }
            }
            self.preview.motion_at = None;
            self.preview.detail_pending_stage = None;
            self.preview.detail_urgent = false;
            return;
        }
        // Building and installing a detail crop is intentionally deferred until
        // both fingers are lifted. A stationary pause during a pinch must not run
        // CPU proxy preparation and GPU pipeline setup on the UI thread.
        if self.preview.touch_navigation_active {
            return;
        }
        if self.ui.active_tab != AppTab::Develop
            || self.preview.quality_dirty
            || self.develop.lens_correction_dirty
            || self.lens_correction_busy()
            || self.develop.load_receiver.is_some()
            || self.foreground_operation_is(ForegroundOperationKind::AiDenoise)
        {
            return;
        }

        let detail_is_current = self.preview.detail_is_current();
        if detail_is_current {
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

        let Some(full_raw) = self.develop.loaded_raw.as_ref().map(Arc::clone) else {
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.egui_ctx.request_repaint();
            return;
        };

        self.preview.motion_at = None;
        self.preview.detail_urgent = false;
        self.preview.detail_pending_stage = None;
        let visible = self.preview.visible_uv;

        let cfa_period = match full_raw.cfa_kind {
            crate::pipeline::CfaKind::Bayer => 2,
            crate::pipeline::CfaKind::XTrans => 6,
        };
        let (x0, x1) = aligned_detail_axis(
            visible.min[0],
            visible.max[0],
            full_raw.width,
            cfa_period,
            self.preview.viewport_pixels[0],
            self.preview.quality.detail_pixel_scale(),
        );
        let (y0, y1) = aligned_detail_axis(
            visible.min[1],
            visible.max[1],
            full_raw.height,
            cfa_period,
            self.preview.viewport_pixels[1],
            self.preview.quality.detail_pixel_scale(),
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
                self.preview.quality,
                self.preview.viewport_pixels,
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
        // Keep mask atlases in full-image coordinates to avoid double-applying crop offsets.
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
        let mask_region = detail_mask_source_region(
            &self.masks.stack,
            [x0, y0],
            [crop_width, crop_height],
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
        // Prefer the normal proxy whenever its tone statistics are still current.
        let normal_tone_is_current = !matches!(
            self.preview.pending_stage,
            Some(ProcessingStage::Raw | ProcessingStage::Tone)
        );
        let full_frame_tone_pipeline = if normal_tone_is_current {
            self.preview.gpu_pipeline.as_ref().or_else(|| {
                self.preview.navigation
                    .as_ref()
                    .map(|preview| &preview.pipeline)
            })
        } else {
            self.preview.navigation
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
            // The atlas is viewport-local. A moved crop therefore needs fresh
            // mask pixels even when the underlying geometry did not change.
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
            detail.revision = self.preview.revision;
            detail.raw = Arc::clone(&detail_raw);
            detail.source_origin = [x0, y0];
            detail.source_size = [crop_width, crop_height];
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
                "preview pipeline display-profile install failed: {error:#}"
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
            revision: self.preview.revision,
            raw: detail_raw,
            source_origin: [x0, y0],
            source_size: [crop_width, crop_height],
            mask_source_region: mask_region,
            virtual_origin: [virtual_origin_x, virtual_origin_y],
            virtual_full_size: [virtual_full_width, virtual_full_height],
        });
        self.masks.detail_dirty_layers.fill(false);
        self.egui_ctx.request_repaint();
    }
}
