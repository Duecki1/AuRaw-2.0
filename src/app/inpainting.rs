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
        self.note_inpainting_edit_changed();
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
        self.inpaint_source_cache = None;
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

        // Capture only the full-resolution RAW region needed by this stroke.
        // This avoids the old preview-proxy source while keeping brush release
        // fast: shader programs are reused and only a small local crop is
        // allocated/read back.
        let source = match self.capture_inpaint_source(frame, &self.inpaint_stroke) {
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

    fn capture_inpaint_source(
        &self,
        frame: &eframe::Frame,
        dabs: &[BrushDab],
    ) -> Result<PreparedInpaintSource, String> {
        let render_state = frame
            .wgpu_render_state()
            .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
        let full_raw = self
            .loaded_raw
            .as_ref()
            .ok_or_else(|| "Open an image before using Inpainting.".to_owned())?;
        let template = self
            .gpu_pipeline
            .as_ref()
            .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
        let patch = inpaint_patch_rect(dabs, full_raw.width, full_raw.height)
            .ok_or_else(|| "The erase stroke does not cover the image.".to_owned())?;
        let rect = inpaint_capture_rect(dabs, full_raw.width, full_raw.height)
            .ok_or_else(|| "The erase stroke does not cover the image.".to_owned())?;

        let local_raw = crop_raw(full_raw, rect.x, rect.y, rect.width, rect.height);
        let empty_masks = MaskStack::default();
        let mut neutral_exposure = self.exposure;
        neutral_exposure.temperature = 0.0;
        neutral_exposure.tint = 0.0;
        let params = GpuParams::new_for_tile(
            &neutral_exposure,
            &empty_masks,
            &local_raw,
            rect.x as i32,
            rect.y as i32,
            full_raw.width,
            full_raw.height,
        );
        let pipeline = RawGpuPipeline::new_headless_reusing_programs(
            &render_state.device,
            &render_state.queue,
            &local_raw,
            &params,
            ProcessingQuality::Preview,
            template,
        )
        .map_err(|error| {
            format!("Could not prepare the full-resolution inpainting crop: {error:#}")
        })?;
        let patch_local_x = patch.x.saturating_sub(rect.x);
        let patch_local_y = patch.y.saturating_sub(rect.y);
        let scene = pipeline
            .render_inpaint_working_scene_region_resized_blocking(
                &render_state.device,
                &render_state.queue,
                &params,
                patch_local_x,
                patch_local_y,
                patch.size,
                patch.size,
                LAMA_EDGE,
                LAMA_EDGE,
            )
            .map_err(|error| format!("Could not read the inpainting crop: {error:#}"))?;
        let expected = LAMA_EDGE as usize * LAMA_EDGE as usize * 3;
        if scene.len() != expected || scene.iter().any(|value| !value.is_finite()) {
            return Err("The inpainting crop has an invalid Rec.2020 working buffer.".to_owned());
        }
        let rgb_rec2020 = if let Some(layer) = &self.inpaint_layer {
            flatten_inpaint_source_model_region(
                scene,
                layer,
                [patch.x, patch.y],
                patch.size,
                [full_raw.width, full_raw.height],
                full_raw.cam_to_srgb,
            )?
        } else {
            scene
        };

        Ok(PreparedInpaintSource {
            rgb_rec2020,
            width: patch.size,
            height: patch.size,
            origin_x: patch.x,
            origin_y: patch.y,
            full_width: full_raw.width,
            full_height: full_raw.height,
        })
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
                    if let Some(stroke) = InpaintStroke::from_result(dabs, result) {
                        match crate::sidecar::preflight_inpaint_addition(
                            &self.masks,
                            &self.inpaint_strokes,
                            &stroke,
                        ) {
                            Ok(()) => {
                                self.inpaint_strokes.push(stroke);
                                self.note_inpainting_edit_changed();
                                self.rebuild_inpaint_layer();
                                self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
                                self.note_inpainting_changed_for_ai_masks();
                                self.queue_preview_processing(ProcessingStage::Output);
                                self.notice = Some("Erase complete.".to_owned());
                            }
                            Err(error) => {
                                self.notice = Some(format!(
                                    "Erase result was not applied because the edit cannot fit in the platform sidecar: {error}. Delete an existing mask or erase result and try again."
                                ));
                            }
                        }
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
        self.note_inpainting_edit_changed();
        self.rebuild_inpaint_layer();
        self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
        self.note_inpainting_changed_for_ai_masks();
        self.queue_preview_processing(ProcessingStage::Output);
        self.notice = Some("Inpainting stroke deleted.".to_owned());
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn rebuild_inpaint_layer(&mut self) {
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
                    ui.set_max_width(520.0);
                    ui.label("Inpainting uses the LaMa ONNX model to remove painted content.");
                    ui.label(format!(
                        "The first use downloads {:.0} MB and stores the model in AuRaw's cache.",
                        LAMA_MODEL_BYTES as f64 / 1_000_000.0
                    ));
                    ui.label("Model license: Apache-2.0. The model is optional and can be used only after this download.");
                    ui.label("Inference is local. No photograph or brush stroke is uploaded.");
                    ui.label("When you continue, your device connects directly to Hugging Face. Hugging Face receives connection data such as your IP address and request time under its own privacy policy. AuRaw sends no account identifier or telemetry.");
                    ui.horizontal_wrapped(|ui| {
                        ui.hyperlink_to(
                            "Hugging Face privacy policy",
                            "https://huggingface.co/privacy",
                        );
                        ui.separator();
                        ui.hyperlink_to(
                            "Apache-2.0 model page",
                            "https://huggingface.co/Carve/LaMa-ONNX",
                        );
                    });
                    #[cfg(not(target_os = "android"))]
                    if self.onnx_runtime_path.is_none() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Select a trusted local ONNX Runtime library in Settings before continuing.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Consent, download and continue").clicked() {
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

fn flatten_inpaint_source_model_region(
    mut rgb_rec2020: Vec<f32>,
    layer: &InpaintLayer,
    origin: [u32; 2],
    size: u32,
    full_dimensions: [u32; 2],
    legacy_camera_to_working: [[f32; 4]; 3],
) -> Result<Vec<f32>, String> {
    let [origin_x, origin_y] = origin;
    let [full_width, full_height] = full_dimensions;
    if size == 0 || full_width == 0 || full_height == 0 {
        return Err("The inpainting source has invalid dimensions.".to_owned());
    }
    let expected = LAMA_EDGE as usize * LAMA_EDGE as usize * 3;
    if rgb_rec2020.len() != expected {
        return Err("The inpainting source is incomplete.".to_owned());
    }
    for patch in layer.patches.iter() {
        if !patch.is_valid() {
            continue;
        }
        for y in 0..LAMA_EDGE {
            let global_y =
                origin_y as f32 + ((y as f32 + 0.5) * size as f32 / LAMA_EDGE as f32) - 0.5;
            for x in 0..LAMA_EDGE {
                let global_x =
                    origin_x as f32 + ((x as f32 + 0.5) * size as f32 / LAMA_EDGE as f32) - 0.5;
                let source_x =
                    (global_x + 0.5) * patch.source_width as f32 / full_width as f32 - 0.5;
                let source_y =
                    (global_y + 0.5) * patch.source_height as f32 / full_height as f32 - 0.5;
                let Some((mut replacement, alpha)) =
                    patch.sample_linear_rec2020_bilinear(source_x, source_y)
                else {
                    continue;
                };
                if alpha <= 1e-6 {
                    continue;
                }
                replacement =
                    patch.resolve_neutral_working_rgb(replacement, legacy_camera_to_working);
                let destination = (y as usize * LAMA_EDGE as usize + x as usize) * 3;
                for channel in 0..3 {
                    rgb_rec2020[destination + channel] = rgb_rec2020[destination + channel]
                        + (replacement[channel] - rgb_rec2020[destination + channel]) * alpha;
                }
            }
        }
    }
    Ok(rgb_rec2020)
}

#[cfg(test)]
mod tests {
    use super::flatten_inpaint_source_model_region;
    use crate::inpainting::LAMA_EDGE;
    use crate::pipeline::{InpaintLayer, InpaintPatch};
    use half::f16;

    #[test]
    fn later_stroke_source_flattens_a_resampled_existing_patch() {
        let rgba16f = [0.5, 0.25, 0.75, 1.0]
            .map(|value| f16::from_f32(value).to_bits())
            .to_vec();
        let patch = InpaintPatch::new_linear_resampled(
            [4, 4],
            [1, 1],
            [2, 2],
            [1, 1],
            rgba16f,
            vec![255],
        )
        .unwrap();
        let layer = InpaintLayer::new(vec![patch]).unwrap();
        let source = vec![0.1; (LAMA_EDGE * LAMA_EDGE * 3) as usize];
        let flattened = flatten_inpaint_source_model_region(
            source,
            &layer,
            [0, 0],
            4,
            [4, 4],
            [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
        )
        .unwrap();

        assert!((flattened[0] - 0.1).abs() < 1e-6);
        let center = ((256 * LAMA_EDGE + 256) * 3) as usize;
        assert!((flattened[center] - 0.5).abs() < 1e-3);
        assert!((flattened[center + 1] - 0.25).abs() < 1e-3);
        assert!((flattened[center + 2] - 0.75).abs() < 1e-3);
    }
}
