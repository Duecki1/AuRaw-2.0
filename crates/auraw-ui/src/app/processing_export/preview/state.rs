use super::*;

impl AurawApp {
    pub(in crate::app) fn preview_detail_is_current(&self) -> bool {
        self.preview_detail
            .as_ref()
            .is_some_and(|detail| detail.revision == self.preview_revision)
    }

    pub(crate) fn preview_processing_pending(&self) -> bool {
        self.preview_detail_pending_stage.is_some()
            || self.navigation_pending_stage.is_some()
            || (self.pending_stage.is_some()
                && (self.preview_zoom <= DETAIL_ZOOM_START
                    || !self.preview_detail_is_current()))
    }

    pub(crate) fn note_preview_motion(&mut self) {
        let edit_was_pending = self.preview_detail_pending_stage.is_some();
        let rendered_content_was_current = self.original_preview_rendered_state
            == Some((self.original_preview_requested, self.preview_revision));
        self.preview_revision = self.preview_revision.wrapping_add(1);
        // `preview_revision` also invalidates the viewport-specific detail crop,
        // but panning and pinching do not change developed pixels. Carry the
        // rendered-content marker forward so `sync_original_preview` does not
        // dispatch the full RAW compute graph for every navigation sample.
        if rendered_content_was_current {
            self.original_preview_rendered_state =
                Some((self.original_preview_requested, self.preview_revision));
        }
        self.preview_detail_urgent = edit_was_pending;
        self.preview_motion_at = Some(Instant::now());
        if edit_was_pending {
            self.egui_ctx.request_repaint();
        } else {
            self.egui_ctx
                .request_repaint_after(zoom_detail_idle_delay());
        }
    }

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

        if self.preview_zoom > DETAIL_ZOOM_START {
            self.navigation_pending_stage = Some(match self.navigation_pending_stage {
                Some(existing) => existing.min(stage),
                None => stage,
            });
        }

        self.notice = None;
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn original_preview_visible(&self) -> bool {
        self.original_preview_requested
    }

    pub(crate) fn set_original_preview_requested(&mut self, requested: bool) {
        if self.original_preview_requested == requested {
            return;
        }
        self.original_preview_requested = requested;
        self.original_preview_rendered_state = None;
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn toggle_original_preview(&mut self) {
        self.set_original_preview_requested(!self.original_preview_requested);
    }

    pub(crate) fn sync_original_preview(&mut self, frame: &eframe::Frame) {
        let requested_state = (self.original_preview_requested, self.preview_revision);
        if self.original_preview_rendered_state == Some(requested_state) {
            return;
        }

        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let empty_masks = MaskStack::default();
        let exposure = if self.original_preview_requested {
            &self.original_preview_exposure
        } else {
            &self.target_exposure
        };
        let masks = if self.original_preview_requested {
            &empty_masks
        } else {
            &self.masks
        };
        let inpaint = if self.original_preview_requested {
            None
        } else {
            self.inpaint_layer.as_ref()
        };
        let mut textures_to_retire = Vec::new();

        // The main preview is the durable interactive surface. Optional zoom
        // pipelines are caches: a failed cache upload must not make inpainting or
        // original-preview toggling fail globally. Drop only the failed optional
        // cache and let its normal scheduler rebuild it.
        if let (Some(raw), Some(pipeline)) = (&self.preview_raw, &self.gpu_pipeline) {
            if let Err(error) = pipeline.update_inpaint_layer(
                &render_state.queue,
                inpaint,
                0,
                0,
                raw.width,
                raw.height,
            ) {
                self.original_preview_rendered_state = None;
                self.pending_stage = Some(ProcessingStage::Output);
                self.notice = Some(
                    "Could not update preview inpainting. The last complete preview is still shown."
                        .to_owned(),
                );
                crate::diagnostics::record(format!(
                    "main preview inpaint upload failed; rendered revision remains dirty: {error:#}"
                ));
                self.egui_ctx.request_repaint();
                return;
            }
        }

        let navigation_upload_error = self.preview_navigation.as_ref().and_then(|navigation| {
            navigation
                .pipeline
                .update_inpaint_layer(
                    &render_state.queue,
                    inpaint,
                    0,
                    0,
                    navigation.raw.width,
                    navigation.raw.height,
                )
                .err()
        });
        if let Some(error) = navigation_upload_error {
            crate::diagnostics::record(format!(
                "discarding navigation preview after inpaint upload failure: {error:#}"
            ));
            if let Some(old) = self.preview_navigation.take() {
                if let Some(texture_id) = old.pipeline.egui_texture_id {
                    textures_to_retire.push(texture_id);
                }
            }
            self.navigation_pending_stage = Some(ProcessingStage::Output);
        }

        let detail_upload_error = self
            .preview_detail
            .as_ref()
            .filter(|detail| detail.revision == self.preview_revision)
            .and_then(|detail| {
                detail
                    .pipeline
                    .update_inpaint_layer(
                        &render_state.queue,
                        inpaint,
                        detail.virtual_origin[0],
                        detail.virtual_origin[1],
                        detail.virtual_full_size[0],
                        detail.virtual_full_size[1],
                    )
                    .err()
            });
        if let Some(error) = detail_upload_error {
            crate::diagnostics::record(format!(
                "discarding zoom detail after inpaint upload failure: {error:#}"
            ));
            if let Some(old) = self.preview_detail.take() {
                if let Some(texture_id) = old.pipeline.egui_texture_id {
                    textures_to_retire.push(texture_id);
                }
            }
            self.preview_motion_at = Some(Instant::now());
            self.preview_detail_pending_stage = Some(ProcessingStage::Output);
            self.preview_detail_urgent = true;
        }

        if let (Some(raw), Some(pipeline)) = (&self.preview_raw, &self.gpu_pipeline) {
            let params = GpuParams::new(exposure, masks, raw).with_vignette_geometry(self.geometry);
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
        }
        if let Some(navigation) = self.preview_navigation.as_ref() {
            let params = GpuParams::new(exposure, masks, &navigation.raw)
                .with_vignette_geometry(self.geometry);
            navigation
                .pipeline
                .recompute(&render_state.queue, &render_state.device, &params);
        }
        if let Some(detail) = self
            .preview_detail
            .as_ref()
            .filter(|detail| detail.revision == self.preview_revision)
        {
            let mut params = GpuParams::new_for_tile(
                exposure,
                masks,
                &detail.raw,
                detail.virtual_origin[0],
                detail.virtual_origin[1],
                detail.virtual_full_size[0],
                detail.virtual_full_size[1],
            )
            .with_vignette_geometry(self.geometry);
            if let Some(full_raw) = self.loaded_raw.as_ref() {
                let mask_region = detail_mask_source_region(
                    masks,
                    detail.source_origin,
                    detail.source_size,
                    full_raw.width,
                    full_raw.height,
                );
                params = params.with_mask_uv_rect_and_extent(
                    mask_source_region_uv(mask_region, full_raw.width, full_raw.height),
                    mask_region_texture_extent(mask_region, detail.pipeline.mask_atlas_edge()),
                );
            }
            detail
                .pipeline
                .recompute(&render_state.queue, &render_state.device, &params);
        }
        for texture_id in textures_to_retire {
            self.retire_egui_texture(texture_id);
        }

        self.original_preview_rendered_state = Some(requested_state);
        self.egui_ctx.request_repaint();
    }
}
