use super::*;

impl AurawApp {
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

    pub(crate) fn apply_white_balance_area(&mut self, area: [[f32; 2]; 2]) -> bool {
        let result = self.loaded_raw.as_ref().and_then(|raw| {
            raw.white_balance_offsets_from_area(
                area[0],
                area[1],
                self.exposure.black_point,
            )
        });
        self.white_balance_picker_active = false;
        self.white_balance_picker_drag = None;
        let Some((temperature, tint)) = result else {
            self.notice = Some(
                "Could not estimate white balance there. Choose a brighter, unclipped neutral area."
                    .to_owned(),
            );
            self.egui_ctx.request_repaint();
            return false;
        };
        self.exposure.temperature = temperature;
        self.exposure.tint = tint;
        self.notice = Some("White balance sampled from the selected image area.".to_owned());
        self.mark_pipeline_dirty();
        true
    }

    pub(in crate::app) fn advance_zoomed_processing(&mut self, frame: &eframe::Frame) {
        let Some(stage) = self.preview_detail_pending_stage else {
            return;
        };
        let Some(detail) = self
            .preview_detail
            .as_ref()
            .filter(|detail| detail.revision == self.preview_revision)
        else {
            return;
        };
        if detail.pipeline.mask_layer_capacity() < self.masks.masks.len().max(1) {
            // Explicit-edge detail atlases allocate only active layers. Adding
            // another mask invalidates that small texture and rebuilds just the
            // detail pipeline on the next frame.
            if let Some(detail) = self.preview_detail.as_mut() {
                detail.revision = self.preview_revision.wrapping_sub(1);
            }
            self.preview_detail_pending_stage = None;
            self.preview_detail_urgent = true;
            self.preview_motion_at = Some(Instant::now());
            self.egui_ctx.request_repaint();
            return;
        }
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
        let mask_region = detail_mask_source_region(
            &self.masks,
            detail.source_origin,
            detail.source_size,
            full_raw.width,
            full_raw.height,
        );
        let params = GpuParams::new_for_tile(
            &self.target_exposure,
            &self.masks,
            &detail_raw,
            virtual_origin[0],
            virtual_origin[1],
            virtual_full_size[0],
            virtual_full_size[1],
        )
        .with_vignette_geometry(self.geometry)
        .with_mask_uv_rect_and_extent(
            mask_source_region_uv(mask_region, full_raw.width, full_raw.height),
            mask_region_texture_extent(mask_region, detail.pipeline.mask_atlas_edge()),
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
            let region_changed = detail.mask_source_region != mask_region;
            if let Err(error) = Self::upload_detail_masks(
                &detail.pipeline,
                &render_state.queue,
                &self.masks,
                full_raw,
                mask_region,
                (!region_changed).then_some(&self.detail_dirty_mask_layers),
            ) {
                self.notice = Some(error);
                self.preview_detail_pending_stage = None;
                return;
            }
            detail.mask_source_region = mask_region;
            self.detail_dirty_mask_layers.fill(false);
        }

        if stage == ProcessingStage::Output {
            if let Err(error) = detail.pipeline.update_inpaint_layer(
                &render_state.queue,
                self.inpaint_layer.as_ref(),
                virtual_origin[0],
                virtual_origin[1],
                virtual_full_size[0],
                virtual_full_size[1],
            ) {
                self.notice = Some(format!("Could not update zoomed inpainting: {error:#}"));
                self.preview_detail_pending_stage = None;
                return;
            }
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

    pub(in crate::app) fn advance_processing(&mut self, frame: &eframe::Frame) {
        if self.preview_zoom > DETAIL_ZOOM_START {
            self.advance_zoomed_processing(frame);
            if self.preview_detail_is_current() {
                return;
            }
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
                    .rasterize_layer_f16(layer, edge, edge, raw.width, raw.height);
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
            if let Err(error) = pipeline.update_light_rays_mask_layers(
                &render_state.queue,
                &self.masks,
                raw.width,
                raw.height,
            ) {
                self.notice = Some(format!("Could not update Light Rays mask: {error:#}"));
                return;
            }
        }

        if stage == ProcessingStage::Output {
            if let Err(error) = pipeline.update_inpaint_layer(
                &render_state.queue,
                self.inpaint_layer.as_ref(),
                0,
                0,
                raw.width,
                raw.height,
            ) {
                self.notice = Some(format!("Could not update preview inpainting: {error:#}"));
                return;
            }
        }
        let params = GpuParams::new(&self.target_exposure, &self.masks, raw)
            .with_vignette_geometry(self.geometry);
        pipeline.dispatch_stage(&render_state.queue, &render_state.device, &params, stage);
        self.pending_stage = match stage {
            ProcessingStage::Raw => Some(ProcessingStage::Tone),
            ProcessingStage::Tone => Some(ProcessingStage::Output),
            ProcessingStage::Output => None,
        };
    }
}
