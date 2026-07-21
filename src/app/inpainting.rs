impl AurawApp {
    pub(crate) fn inpaint_busy(&self) -> bool {
        self.inpaint_receiver.is_some() || self.inpaint_consent_open
    }

    pub(crate) fn inpaint_progress(&self) -> Option<(u64, u64)> {
        self.inpaint_download_progress
    }

    pub(crate) fn inpaint_inferencing(&self) -> bool {
        self.inpaint_inferencing
    }

    pub(crate) fn clear_inpainting(&mut self) {
        if self.inpaint_receiver.is_some() {
            return;
        }
        self.inpaint_stroke.clear();
        self.inpaint_strokes.clear();
        self.last_inpaint_brush_point = None;
        self.inpaint_layer = None;
        self.inpaint_texture = None;
        self.inpaint_texture_key = None;
        self.inpaint_stroke_texture = None;
        self.inpaint_stroke_texture_key = None;
        self.inpaint_texture_revision = self.inpaint_texture_revision.wrapping_add(1);
        self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
        self.note_inpainting_changed_for_ai_masks();
        self.queue_preview_processing(ProcessingStage::Output);
        self.notice = Some("Inpainting cleared.".to_owned());
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn reset_inpainting_state(&mut self) {
        self.inpaint_stroke.clear();
        self.inpaint_strokes.clear();
        self.last_inpaint_brush_point = None;
        self.inpaint_layer = None;
        self.inpaint_texture = None;
        self.inpaint_texture_key = None;
        self.inpaint_stroke_texture = None;
        self.inpaint_stroke_texture_key = None;
        self.inpaint_pending_source = None;
        self.inpaint_active_dabs = None;
        self.inpaint_revision = 0;
        self.inpaint_consent_open = false;
        self.inpaint_receiver = None;
        self.inpaint_download_progress = None;
        self.inpaint_inferencing = false;
        self.inpaint_texture_revision = self.inpaint_texture_revision.wrapping_add(1);
    }

    pub(crate) fn request_inpaint(&mut self, frame: &eframe::Frame) {
        if self.inpaint_stroke.is_empty() || self.inpaint_busy() {
            return;
        }
        #[cfg(not(target_os = "android"))]
        if self.onnx_runtime_path.is_none() || self.onnx_runtime_sha256.is_none() {
            self.notice = Some(
                "Choose a trusted ONNX Runtime library under Settings before using Inpainting."
                    .to_owned(),
            );
            self.inpaint_stroke.clear();
            self.last_inpaint_brush_point = None;
            return;
        }

        let source = match self.capture_inpaint_source(frame).and_then(|source| {
            if let Some(layer) = &self.inpaint_layer {
                flatten_inpaint_source(source, layer)
            } else {
                Ok(source)
            }
        }) {
            Ok(source) => source,
            Err(error) => {
                self.notice = Some(error);
                self.inpaint_stroke.clear();
                self.last_inpaint_brush_point = None;
                return;
            }
        };
        self.inpaint_pending_source = Some(source);
        let model_path = self.lama_model_path();
        if model_path.exists() {
            self.start_inpaint_worker(model_path);
        } else {
            self.inpaint_consent_open = true;
            self.egui_ctx.request_repaint();
        }
    }

    fn capture_inpaint_source(&self, frame: &eframe::Frame) -> Result<MaskRgbImage, String> {
        let render_state = frame
            .wgpu_render_state()
            .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
        let raw = self
            .preview_raw
            .as_ref()
            .ok_or_else(|| "Open an image before using Inpainting.".to_owned())?;

        // Run LaMa on the pre-adjustment scene rather than the live edited
        // output. The generated replacement can then be inserted back before
        // Develop processing, allowing later global and mask adjustments to
        // affect it exactly like the surrounding photo instead of baking the
        // sliders into the patch at erase time.
        let empty_masks = MaskStack::default();
        // Persist inpainting in the RAW's neutral scene-working basis. At
        // display/export time the GPU maps that neutral basis through the live
        // camera white-balance matrix before applying the current DCP/profile
        // and all later global/local edits. This keeps temperature/tint editable
        // instead of baking the values that happened to be active when erased.
        let mut neutral_exposure = self.exposure;
        neutral_exposure.temperature = 0.0;
        neutral_exposure.tint = 0.0;
        let params = GpuParams::new(&neutral_exposure, &empty_masks, raw);
        let pipeline = RawGpuPipeline::new_headless_with_quality(
            &render_state.device,
            &render_state.queue,
            raw,
            &params,
            ProcessingQuality::High,
        )
        .map_err(|error| format!("Could not prepare the inpainting scene: {error:#}"))?;
        let scene = pipeline
            .render_inpaint_working_scene_blocking(
                &render_state.device,
                &render_state.queue,
                &params,
            )
            .map_err(|error| format!("Could not read the inpainting scene: {error:#}"))?;
        let rgba = scene_rec2020_to_srgb8(&scene)
            .map_err(|error| format!("Could not encode the inpainting scene: {error}"))?;
        MaskRgbImage::new(pipeline.width, pipeline.height, rgba)
            .ok_or_else(|| "The inpainting source has invalid dimensions.".to_owned())
    }

    fn start_inpaint_worker(&mut self, model_path: PathBuf) {
        if self.inpaint_receiver.is_some() {
            return;
        }
        let Some(source) = self.inpaint_pending_source.take() else {
            self.notice = Some("The image could not be prepared for inpainting.".to_owned());
            return;
        };
        if self.inpaint_stroke.is_empty() {
            return;
        }
        let dabs = std::mem::take(&mut self.inpaint_stroke);
        self.inpaint_active_dabs = Some(dabs.clone());
        self.last_inpaint_brush_point = None;
        self.inpaint_stroke_texture = None;
        self.inpaint_stroke_texture_key = None;
        self.inpaint_download_progress = None;
        self.inpaint_inferencing = model_path.exists();
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.onnx_runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.onnx_runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;
        self.inpaint_receiver = Some(spawn_inpaint(
            model_path,
            runtime_path,
            runtime_sha256,
            InpaintRequest { source, dabs },
        ));
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn poll_inpaint_worker(&mut self) {
        let mut finished = None;
        if let Some(receiver) = &self.inpaint_receiver {
            while let Ok(event) = receiver.try_recv() {
                match event {
                    InpaintEvent::DownloadProgress { downloaded, total } => {
                        self.inpaint_download_progress = Some((downloaded, total));
                        self.inpaint_inferencing = false;
                    }
                    InpaintEvent::Inferencing => {
                        self.inpaint_download_progress = None;
                        self.inpaint_inferencing = true;
                    }
                    InpaintEvent::Finished(result) => finished = Some(result),
                }
            }
        }
        if let Some(result) = finished {
            self.inpaint_receiver = None;
            self.inpaint_download_progress = None;
            self.inpaint_inferencing = false;
            match result {
                Ok(result) => {
                    let dabs = self.inpaint_active_dabs.take().unwrap_or_default();
                    if let Some(stroke) = InpaintStroke::from_result(dabs, &result) {
                        self.inpaint_strokes.push(stroke);
                        self.rebuild_inpaint_layer();
                        self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
                        self.note_inpainting_changed_for_ai_masks();
                        self.queue_preview_processing(ProcessingStage::Output);
                        self.notice = Some("Erase complete.".to_owned());
                    } else {
                        self.notice = Some("Inpainting returned an empty result.".to_owned());
                    }
                }
                Err(error) => {
                    log::error!("Inpainting failed: {error}");
                    self.inpaint_active_dabs = None;
                    self.notice = Some(format!("Inpainting failed: {error}"));
                }
            }
            self.egui_ctx.request_repaint();
        }
    }

    pub(crate) fn delete_inpaint_stroke(&mut self, index: usize) {
        if self.inpaint_busy() || index >= self.inpaint_strokes.len() {
            return;
        }
        self.inpaint_strokes.remove(index);
        self.rebuild_inpaint_layer();
        self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
        self.note_inpainting_changed_for_ai_masks();
        self.queue_preview_processing(ProcessingStage::Output);
        self.notice = Some("Inpainting stroke deleted.".to_owned());
        self.egui_ctx.request_repaint();
    }

    fn rebuild_inpaint_layer(&mut self) {
        self.inpaint_layer = compose_inpaint_strokes(&self.inpaint_strokes);
        self.inpaint_texture = None;
        self.inpaint_texture_key = None;
        self.inpaint_texture_revision = self.inpaint_texture_revision.wrapping_add(1);
    }

    #[cfg(not(target_os = "android"))]
    fn lama_model_path(&self) -> PathBuf {
        let root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("auraw/models/lama_fp32.onnx")
    }

    #[cfg(target_os = "android")]
    fn lama_model_path(&self) -> PathBuf {
        self.android_app
            .internal_data_path()
            .unwrap_or_else(std::env::temp_dir)
            .join("models/lama_fp32.onnx")
    }

    pub(crate) fn show_inpainting_dialogs(&mut self, ctx: &egui::Context) {
        if self.inpaint_consent_open {
            egui::Window::new("Download inpainting model?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Inpainting uses the LaMa ONNX model to remove painted content.");
                    ui.label(format!(
                        "The first use downloads {:.0} MB and stores the model in AuRaw's cache.",
                        LAMA_MODEL_BYTES as f64 / 1_000_000.0
                    ));
                    ui.label("Inference is local. No photograph or brush stroke is uploaded.");
                    #[cfg(not(target_os = "android"))]
                    if self.onnx_runtime_path.is_none() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Select a trusted local ONNX Runtime library in Settings before continuing.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Download and continue").clicked() {
                            self.inpaint_consent_open = false;
                            self.start_inpaint_worker(self.lama_model_path());
                        }
                        if ui.button("Cancel").clicked() {
                            self.inpaint_consent_open = false;
                            self.inpaint_pending_source = None;
                            self.inpaint_active_dabs = None;
                            self.inpaint_stroke.clear();
                            self.last_inpaint_brush_point = None;
                        }
                    });
                });
        }
        if self.inpaint_receiver.is_some() {
            egui::Window::new("Erasing selection")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    if let Some((downloaded, total)) = self.inpaint_download_progress {
                        let fraction = downloaded as f32 / total.max(1) as f32;
                        ui.label("Downloading lama_fp32.onnx…");
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .show_percentage()
                                .text(format!(
                                    "{:.1} / {:.1} MB",
                                    downloaded as f64 / 1_000_000.0,
                                    total as f64 / 1_000_000.0
                                )),
                        );
                    } else if self.inpaint_inferencing {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Running local LaMa inpainting…");
                        });
                    } else {
                        ui.spinner();
                    }
                });
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}


fn scene_rec2020_to_srgb8(scene: &[f32]) -> Result<Vec<u8>, String> {
    if scene.len() % 3 != 0 {
        return Err("scene RGB buffer has an invalid length".to_owned());
    }
    let mut rgba = Vec::with_capacity(scene.len() / 3 * 4);
    for rgb in scene.chunks_exact(3) {
        let r = 1.660_491_0 * rgb[0] - 0.587_641_1 * rgb[1] - 0.072_849_9 * rgb[2];
        let g = -0.124_550_5 * rgb[0] + 1.132_899_9 * rgb[1] - 0.008_349_4 * rgb[2];
        let b = -0.018_150_8 * rgb[0] - 0.100_578_9 * rgb[1] + 1.118_729_7 * rgb[2];
        for value in [r, g, b] {
            let linear = value.max(0.0);
            let encoded = if linear <= 0.003_130_8 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            rgba.push((encoded.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        rgba.push(255);
    }
    Ok(rgba)
}

fn flatten_inpaint_source(
    source: MaskRgbImage,
    layer: &InpaintLayer,
) -> Result<MaskRgbImage, String> {
    if source.width == 0 || source.height == 0 || layer.width == 0 || layer.height == 0 {
        return Err("The inpainting layer has invalid dimensions.".to_owned());
    }
    let expected_source_pixels = usize::try_from(source.width)
        .ok()
        .and_then(|width| {
            usize::try_from(source.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| "The inpainting source dimensions are too large.".to_owned())?;
    let expected_source_bytes = expected_source_pixels
        .checked_mul(4)
        .ok_or_else(|| "The inpainting source dimensions are too large.".to_owned())?;
    if source.rgba.len() != expected_source_bytes {
        return Err("The inpainting source is incomplete.".to_owned());
    }
    let expected_layer_pixels = usize::try_from(layer.width)
        .ok()
        .and_then(|width| {
            usize::try_from(layer.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or_else(|| "The inpainting layer dimensions are too large.".to_owned())?;
    let expected_layer_bytes = expected_layer_pixels
        .checked_mul(4)
        .ok_or_else(|| "The inpainting layer dimensions are too large.".to_owned())?;
    if layer.rgba.len() != expected_layer_bytes || layer.mask.len() != expected_layer_pixels {
        return Err("The inpainting layer is incomplete.".to_owned());
    }
    let mut rgba = source.rgba.to_vec();
    for y in 0..source.height {
        let layer_y = ((y as f32 + 0.5) * layer.height as f32 / source.height as f32)
            .floor()
            .clamp(0.0, layer.height.saturating_sub(1) as f32) as u32;
        for x in 0..source.width {
            let layer_x = ((x as f32 + 0.5) * layer.width as f32 / source.width as f32)
                .floor()
                .clamp(0.0, layer.width.saturating_sub(1) as f32) as u32;
            let layer_index = (layer_y as usize * layer.width as usize + layer_x as usize) as usize;
            if layer.mask[layer_index] == 0 {
                continue;
            }
            let source_index = (y as usize * source.width as usize + x as usize) * 4;
            let layer_rgba = layer_index * 4;
            rgba[source_index..source_index + 4]
                .copy_from_slice(&layer.rgba[layer_rgba..layer_rgba + 4]);
        }
    }
    MaskRgbImage::new(source.width, source.height, rgba)
        .ok_or_else(|| "The flattened inpainting source has invalid dimensions.".to_owned())
}
