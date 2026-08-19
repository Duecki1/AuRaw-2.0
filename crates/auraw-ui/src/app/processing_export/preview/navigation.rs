use super::*;

impl AurawApp {
    pub(in crate::app) fn advance_navigation_preview(&mut self, frame: &eframe::Frame) {
        if self.foreground_operation_is(ForegroundOperationKind::AiDenoise) {
            return;
        }
        let zoomed = self.preview.zoom > DETAIL_ZOOM_START;
        let should_update = self.preview.navigation_pending_stage.is_some();
        // The normal preview is already a complete full-frame fallback while the
        // user only zooms or pans. Create the tiny navigation pipeline lazily when
        // an actual edit needs its fast full-frame update, then retain it until fit.
        // Eager creation here caused a visible hitch on the first pinch frame.
        let should_exist = zoomed && (self.preview.navigation.is_some() || should_update);
        if !should_exist && !should_update {
            // Release the navigation proxy when fit view is stable.
            if frame.wgpu_render_state().is_some() {
                if let Some(old) = self.preview.navigation.take() {
                    if let Some(texture_id) = old.pipeline.egui_texture_id {
                        self.retire_egui_texture(texture_id);
                    }
                }
            } else {
                self.preview.navigation = None;
            }
            return;
        }
        let Some(full_raw) = self.develop.loaded_raw.as_ref().map(Arc::clone) else {
            self.preview.navigation_pending_stage = None;
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        let navigation_capacity_stale = self.preview.navigation.as_ref().is_some_and(|preview| {
            preview.pipeline.mask_layer_capacity() < self.masks.stack.masks.len().max(1)
        });
        if navigation_capacity_stale {
            if let Some(old) = self.preview.navigation.take() {
                if let Some(texture_id) = old.pipeline.egui_texture_id {
                    self.retire_egui_texture(texture_id);
                }
            }
        }

        if self.preview.navigation.is_none() {
            if !should_exist {
                self.preview.navigation_pending_stage = None;
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
            let params = GpuParams::new(&self.develop.target_exposure, &self.masks.stack, &raw)
                .with_vignette_geometry(self.develop.geometry);
            let Some(template) = self.preview.gpu_pipeline.as_ref() else {
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
                    self.ui.notice = Some(format!(
                        "Could not prepare the adjusted navigation preview: {error:#}"
                    ));
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
            if let Err(error) =
                Self::upload_preview_masks(&pipeline, &render_state.queue, &self.masks.stack, &raw)
            {
                self.ui.notice = Some(error);
                return;
            }
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
            let mut renderer = render_state.renderer.write();
            pipeline.register_egui_texture(&render_state.device, &mut renderer);
            drop(renderer);
            self.preview.navigation = Some(PreviewNavigation { pipeline, raw });
            self.preview.navigation_pending_stage = None;
            self.masks.navigation_dirty_layers.fill(false);
            self.egui_ctx.request_repaint();
            return;
        }

        let Some(stage) = self.preview.navigation_pending_stage else {
            return;
        };
        let Some(preview) = self.preview.navigation.as_mut() else {
            return;
        };
        if self.masks.navigation_dirty_layers.iter().any(|dirty| *dirty) {
            let edge = preview.pipeline.mask_atlas_edge();
            for layer in 0..MAX_LOCAL_MASKS {
                if !self.masks.navigation_dirty_layers[layer] {
                    continue;
                }
                let bytes = self.masks.stack.rasterize_layer_f16(
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
                    self.ui.notice = Some(format!(
                        "Could not update the navigation local mask: {error:#}"
                    ));
                    return;
                }
                self.masks.navigation_dirty_layers[layer] = false;
            }
            if let Err(error) = preview.pipeline.update_light_rays_mask_layers(
                &render_state.queue,
                &self.masks.stack,
                preview.raw.width,
                preview.raw.height,
            ) {
                self.ui.notice = Some(format!(
                    "Could not update the navigation Light Rays mask: {error:#}"
                ));
                return;
            }
        }

        let params = GpuParams::new(&self.develop.target_exposure, &self.masks.stack, &preview.raw)
            .with_vignette_geometry(self.develop.geometry);
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
        self.preview.navigation_pending_stage = None;
        self.egui_ctx.request_repaint();
    }
}
