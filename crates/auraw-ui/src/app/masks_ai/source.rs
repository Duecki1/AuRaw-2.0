use super::*;

impl AurawApp {
    pub(in crate::app) fn capture_mask_source_from_active_preview(
        &self,
        frame: &eframe::Frame,
    ) -> Result<MaskRgbImage, String> {
        let render_state = frame
            .wgpu_render_state()
            .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
        let raw = self.develop.preview_raw
            .as_ref()
            .ok_or_else(|| "The preview image is not available yet.".to_owned())?;
        let pipeline = self.preview.gpu_pipeline
            .as_ref()
            .ok_or_else(|| "Open an image before creating this mask.".to_owned())?;

        // The expanded processing graph does not always leave enough GPU budget
        // for a second full RawGpuPipeline. Render the canonical unedited source
        // through the already-resident preview graph, read it back, then restore
        // the current edited preview before this UI frame is painted. This is
        // also the path used while reconstructing saved range-mask sources
        // during image load.
        //
        // Desktop previews may use a monitor ICC transform, while mask models
        // require the canonical sRGB rendition produced by a fresh pipeline.
        // Install sRGB before the reference render and restore the monitor
        // transform before recomputing the visible edited preview.
        #[cfg(not(target_os = "android"))]
        pipeline
            .reset_display_to_srgb(&render_state.queue)
            .map_err(|error| format!("Could not prepare mask-source color output: {error:#}"))?;
        let reference_exposure = ExposureParams::scene_referred_default();
        let reference_masks = MaskStack::default();
        let reference_params = GpuParams::new(&reference_exposure, &reference_masks, raw);
        pipeline.recompute(&render_state.queue, &render_state.device, &reference_params);

        let readback = pipeline.read_output_region_blocking(
            &render_state.device,
            &render_state.queue,
            0,
            0,
            pipeline.width,
            pipeline.height,
        );

        // `recompute` updates the complete graph, so restore exactly the state
        // the user should see even when the readback failed. A failure must not
        // leave the preview displaying the neutral AI-mask rendition.
        let restore_params = if self.preview.original_requested {
            GpuParams::new(&self.preview.original_exposure, &reference_masks, raw)
                .with_vignette_geometry(self.develop.geometry)
        } else {
            GpuParams::new(&self.develop.target_exposure, &self.masks.stack, raw)
                .with_vignette_geometry(self.develop.geometry)
        };
        #[cfg(not(target_os = "android"))]
        let display_restore = pipeline
            .write_output_transform(&render_state.queue, &self.preferences.display_output_transform)
            .map_err(|error| format!("Could not restore the preview color output: {error:#}"));
        pipeline.recompute(&render_state.queue, &render_state.device, &restore_params);

        #[cfg(not(target_os = "android"))]
        display_restore?;
        let rgba = readback
            .map_err(|error| format!("Could not read the original RAW for masking: {error:#}"))?;
        MaskRgbImage::new(pipeline.width, pipeline.height, rgba)
            .ok_or_else(|| "The canonical mask source has invalid dimensions.".to_owned())
    }

    pub(crate) fn capture_mask_source(&mut self, frame: &eframe::Frame) -> Result<(), String> {
        if self.masks.source_cache.is_some() {
            return Ok(());
        }

        #[cfg(target_os = "android")]
        {
            let source = self.capture_mask_source_from_active_preview(frame)?;
            self.masks.source_cache = Some(source);
            Ok(())
        }

        #[cfg(not(target_os = "android"))]
        {
            let render_state = frame
                .wgpu_render_state()
                .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
            let program_template = self.preview.gpu_pipeline
                .as_ref()
                .map(RawGpuPipeline::program_template)
                .or_else(|| self.preview.program_template.clone())
                .ok_or_else(|| "Open an image before creating this mask.".to_owned())?;
            let full_raw = self.develop.loaded_raw
                .as_ref()
                .ok_or_else(|| "The original RAW is not available.".to_owned())?;
            let source_edge = ai_mask_source_proxy_edge(full_raw.width, full_raw.height);
            let raw = if full_raw.width.max(full_raw.height) <= source_edge {
                Arc::clone(full_raw)
            } else {
                Arc::new(build_proxy(
                    full_raw,
                    ProxySpec {
                        max_edge: source_edge,
                    },
                ))
            };

            let reference_exposure = ExposureParams::scene_referred_default();
            let reference_masks = MaskStack::default();
            let params = GpuParams::new(&reference_exposure, &reference_masks, &raw);
            // This graph renders an unmasked reference image for inference;
            // reserving the normal 32-layer 2048px editing atlas wastes 256
            // MiB and can reject an otherwise valid 4K capture on a 1.5 GiB
            // GPU budget. An explicit atlas allocates one tiny unused layer.
            let reference_pipeline_result =
                RawGpuPipeline::new_headless_reusing_program_template_with_mask_edge(
                &render_state.device,
                &render_state.queue,
                &raw,
                &params,
                ProcessingQuality::Preview,
                &program_template,
                64,
            );
            let reference_pipeline = match reference_pipeline_result {
                Ok(pipeline) => pipeline,
                Err(error)
                    if error
                        .to_string()
                        .contains("GPU pipelines already reserve") =>
                {
                    crate::diagnostics::record(format!(
                        "Dedicated AI mask-source graph exceeded the coexistence budget; using the active preview graph: {error:#}"
                    ));
                    let source = self.capture_mask_source_from_active_preview(frame)?;
                    self.masks.source_cache = Some(source);
                    return Ok(());
                }
                Err(error) => {
                    return Err(format!(
                        "Could not prepare the original RAW for masking: {error:#}"
                    ));
                }
            };
            reference_pipeline.recompute(&render_state.queue, &render_state.device, &params);
            let rgba = reference_pipeline
                .read_output_region_blocking(
                    &render_state.device,
                    &render_state.queue,
                    0,
                    0,
                    reference_pipeline.width,
                    reference_pipeline.height,
                )
                .map_err(|error| {
                    format!("Could not read the original RAW for masking: {error:#}")
                })?;
            let source = MaskRgbImage::new(
                reference_pipeline.width,
                reference_pipeline.height,
                rgba,
            )
            .ok_or_else(|| "The canonical mask source has invalid dimensions.".to_owned())?;
            self.masks.source_cache = Some(source);
            Ok(())
        }
    }

    pub(crate) fn report_ai_mask_error(&mut self, error: String) {
        self.ui.notice = Some(error.clone());
        self.ai.object_error_dialog = Some(error);
        self.egui_ctx.request_repaint();
    }
}
