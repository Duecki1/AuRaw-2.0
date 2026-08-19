use super::*;

impl PreviewState {
    pub(in crate::app) fn detail_is_current(&self) -> bool {
        self.detail
            .as_ref()
            .is_some_and(|detail| detail.revision == self.revision)
    }

    pub(crate) fn processing_pending(&self) -> bool {
        self.detail_pending_stage.is_some()
            || self.navigation_pending_stage.is_some()
            || (self.pending_stage.is_some()
                && (self.zoom <= DETAIL_ZOOM_START || !self.detail_is_current()))
    }

    pub(crate) fn original_visible(&self) -> bool {
        self.original_requested
    }
}

impl AurawApp {
    pub(crate) fn note_preview_motion(&mut self) {
        let edit_was_pending = self.preview.detail_pending_stage.is_some();
        let rendered_content_was_current = self.preview.original_rendered_state
            == Some((self.preview.original_requested, self.preview.revision));
        self.preview.revision = self.preview.revision.wrapping_add(1);
        // `preview_revision` also invalidates the viewport-specific detail crop,
        // but panning and pinching do not change developed pixels. Carry the
        // rendered-content marker forward so `sync_original_preview` does not
        // dispatch the full RAW compute graph for every navigation sample.
        if rendered_content_was_current {
            self.preview.original_rendered_state =
                Some((self.preview.original_requested, self.preview.revision));
        }
        self.preview.detail_urgent = edit_was_pending;
        self.preview.motion_at = Some(Instant::now());
        if edit_was_pending {
            self.egui_ctx.request_repaint();
        } else {
            self.egui_ctx
                .request_repaint_after(zoom_detail_idle_delay());
        }
    }

    pub(crate) fn queue_preview_processing(&mut self, stage: ProcessingStage) {
        self.preview.pending_stage = Some(match self.preview.pending_stage {
            Some(existing) => existing.min(stage),
            None => stage,
        });

        if self.preview.zoom > DETAIL_ZOOM_START {
            self.preview.detail_pending_stage = Some(match self.preview.detail_pending_stage {
                Some(existing) => existing.min(stage),
                None => stage,
            });
            self.preview.detail_urgent = true;
        }

        if self.preview.zoom > DETAIL_ZOOM_START {
            self.preview.navigation_pending_stage =
                Some(match self.preview.navigation_pending_stage {
                    Some(existing) => existing.min(stage),
                    None => stage,
                });
        }

        self.ui.notice = None;
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn set_original_preview_requested(&mut self, requested: bool) {
        if self.preview.original_requested == requested {
            return;
        }
        self.preview.original_requested = requested;
        self.preview.original_rendered_state = None;
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn toggle_original_preview(&mut self) {
        self.set_original_preview_requested(!self.preview.original_requested);
    }

    pub(crate) fn sync_original_preview(&mut self, frame: &eframe::Frame) {
        let requested_state = (self.preview.original_requested, self.preview.revision);
        if self.preview.original_rendered_state == Some(requested_state) {
            return;
        }

        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let empty_masks = MaskStack::default();
        let exposure = if self.preview.original_requested {
            &self.preview.original_exposure
        } else {
            &self.develop.target_exposure
        };
        let masks = if self.preview.original_requested {
            &empty_masks
        } else {
            &self.masks.stack
        };
        if let (Some(raw), Some(pipeline)) = (&self.develop.preview_raw, &self.preview.gpu_pipeline) {
            let params = GpuParams::new(exposure, masks, raw)
                .with_vignette_geometry(self.develop.geometry);
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
        }
        if let Some(navigation) = self.preview.navigation.as_ref() {
            let params = GpuParams::new(exposure, masks, &navigation.raw)
                .with_vignette_geometry(self.develop.geometry);
            navigation
                .pipeline
                .recompute(&render_state.queue, &render_state.device, &params);
        }
        if let Some(detail) = self.preview.detail
            .as_ref()
            .filter(|detail| detail.revision == self.preview.revision)
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
            .with_vignette_geometry(self.develop.geometry);
            if let Some(full_raw) = self.develop.loaded_raw.as_ref() {
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
        self.preview.original_rendered_state = Some(requested_state);
        self.egui_ctx.request_repaint();
    }
}
