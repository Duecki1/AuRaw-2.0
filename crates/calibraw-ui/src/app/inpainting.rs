use super::*;

impl InpaintState {
    pub(crate) fn reset_for_document(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.store(true, std::sync::atomic::Ordering::Release);
        }
        self.edits = Arc::new(RemoveEditState::default());
        self.source_point = None;
        self.source_pick_active = false;
        self.aligned_offset = None;
        self.active_points.clear();
        self.last_brush_uv = None;
        self.pending_brush = None;
        self.pending_retouch = None;
        self.model_consent_open = false;
        self.receiver = None;
        self.processing_label = None;
        self.hovered_stroke = None;
        self.selected_stroke = None;
        self.stroke_opacity_edit_pending = false;
    }

    pub(crate) fn processing(&self) -> bool {
        self.receiver.is_some() || self.model_consent_open
    }
}

impl CalibRawApp {
    pub(crate) fn reset_inpainting_state(&mut self) {
        self.inpaint.reset_for_document();
    }

    pub(crate) fn install_remove_edits(&mut self, edits: Arc<RemoveEditState>) {
        if let Some(cancellation) = self.inpaint.cancellation.take() {
            cancellation.store(true, std::sync::atomic::Ordering::Release);
        }
        self.inpaint.receiver = None;
        self.inpaint.pending_brush = None;
        self.inpaint.pending_retouch = None;
        self.inpaint.model_consent_open = false;
        self.inpaint.active_points.clear();
        self.inpaint.source_pick_active = false;
        self.inpaint.last_brush_uv = None;
        self.inpaint.processing_label = None;
        self.inpaint.edits = edits;
        self.inpaint.hovered_stroke = None;
        self.inpaint.selected_stroke = None;
        self.inpaint.stroke_opacity_edit_pending = false;
    }

    pub(crate) fn cancel_remove_processing(&mut self) {
        if let Some(cancellation) = self.inpaint.cancellation.take() {
            cancellation.store(true, std::sync::atomic::Ordering::Release);
        }
        self.inpaint.receiver = None;
        self.inpaint.pending_brush = None;
        self.inpaint.pending_retouch = None;
        self.inpaint.model_consent_open = false;
        self.inpaint.processing_label = None;
        self.inpaint.active_points.clear();
        self.inpaint.source_pick_active = false;
        self.inpaint.last_brush_uv = None;
    }

    pub(crate) fn clear_inpainting_tool(&mut self) {
        self.finish_inpaint_stroke_opacity_edit();
        self.cancel_remove_processing();
        let tool = self.inpaint.tool;
        if !self
            .inpaint
            .edits
            .strokes
            .iter()
            .any(|stroke| tool.matches_stroke_tool(stroke.retouch.map(|retouch| retouch.tool)))
        {
            return;
        }
        Arc::make_mut(&mut self.inpaint.edits)
            .strokes
            .retain(|stroke| !tool.matches_stroke_tool(stroke.retouch.map(|retouch| retouch.tool)));
        self.inpaint.hovered_stroke = None;
        self.inpaint.selected_stroke = None;
        self.note_remove_edit_changed();
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn delete_inpaint_stroke(&mut self, index: usize) {
        self.finish_inpaint_stroke_opacity_edit();
        self.cancel_remove_processing();
        if index >= self.inpaint.edits.strokes.len() {
            return;
        }
        Arc::make_mut(&mut self.inpaint.edits).strokes.remove(index);
        self.inpaint.hovered_stroke = None;
        self.inpaint.selected_stroke = None;
        self.note_remove_edit_changed();
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn set_inpaint_stroke_opacity(&mut self, index: usize, opacity: f32) {
        if self.inpaint.processing() || !opacity.is_finite() {
            return;
        }
        let opacity = opacity.clamp(0.0, 1.0);
        let Some(current) = self.inpaint.edits.strokes.get(index) else {
            return;
        };
        if (current.opacity - opacity).abs() <= f32::EPSILON {
            return;
        }
        let Some(stroke) = Arc::make_mut(&mut self.inpaint.edits)
            .strokes
            .get_mut(index)
        else {
            return;
        };
        stroke.opacity = opacity;
        self.inpaint.stroke_opacity_edit_pending = true;
        self.queue_preview_processing(ProcessingStage::Raw);
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn finish_inpaint_stroke_opacity_edit(&mut self) {
        if !std::mem::take(&mut self.inpaint.stroke_opacity_edit_pending) {
            return;
        }
        self.note_remove_edit_changed();
    }

    fn start_remove_request(
        &mut self,
        frame: &eframe::Frame,
        existing: RemoveEditState,
        brush: RemoveBrushStroke,
        allow_download: bool,
    ) -> bool {
        let Some(raw) = self.develop.loaded_raw.as_ref().cloned() else {
            self.ui.notice = Some("Open a RAW image before using Remove.".to_owned());
            return false;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.ui.notice = Some("GPU rendering is unavailable for Remove.".to_owned());
            return false;
        };
        #[cfg(not(target_os = "android"))]
        let (runtime_path, runtime_sha256) = self.onnx_runtime_for_ai();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;
        let cancellation = Arc::new(AtomicBool::new(false));
        let request = RemoveRequest {
            device: render_state.device.clone(),
            queue: render_state.queue.clone(),
            raw,
            geometry: self.develop.geometry.sanitized(),
            exposure: self.develop.exposure,
            masks: self.masks.stack.clone(),
            existing,
            brush: brush.clone(),
            opacity: self.inpaint.brush_opacity,
            model_path: self.big_lama_model_path(),
            allow_download,
            runtime_path,
            runtime_sha256,
            program_prewarm: self.export.gpu_prewarm.clone(),
            cancellation: Arc::clone(&cancellation),
        };
        self.inpaint.pending_brush = Some(brush);
        self.inpaint.pending_retouch = None;
        self.inpaint.processing_label = Some("Preparing local context…".to_owned());
        self.inpaint.cancellation = Some(cancellation);
        self.inpaint.receiver = Some(spawn_remove(request));
        self.egui_ctx
            .request_repaint_after(Duration::from_millis(30));
        true
    }

    pub(crate) fn start_remove_worker(&mut self, frame: &eframe::Frame, brush: RemoveBrushStroke) {
        if self.inpaint.processing() || brush.points.is_empty() {
            return;
        }
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
            return;
        }
        let model_path = self.big_lama_model_path();
        let runtime_download_needed = self.automatic_onnx_runtime_download_needed();
        if crate::remove::big_lama_model_is_verified(&model_path) && !runtime_download_needed {
            self.ai.runtime_download_consent_pending = false;
            let existing = self.inpaint.edits.as_ref().clone();
            let _ = self.start_remove_request(frame, existing, brush, false);
        } else {
            self.ai.runtime_download_consent_pending = runtime_download_needed;
            self.inpaint.pending_brush = Some(brush);
            self.inpaint.pending_retouch = None;
            self.inpaint.model_consent_open = true;
            self.egui_ctx.request_repaint();
        }
    }

    pub(crate) fn start_retouch_worker(
        &mut self,
        frame: &eframe::Frame,
        brush: RemoveBrushStroke,
        retouch: RetouchStroke,
    ) {
        if self.inpaint.processing() || brush.points.is_empty() {
            return;
        }
        let Some(raw) = self.develop.loaded_raw.as_ref().cloned() else {
            self.ui.notice = Some("Open a RAW image before using retouch brushes.".to_owned());
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.ui.notice = Some("GPU rendering is unavailable for retouch brushes.".to_owned());
            return;
        };
        let cancellation = Arc::new(AtomicBool::new(false));
        let request = RetouchRequest {
            device: render_state.device.clone(),
            queue: render_state.queue.clone(),
            raw,
            geometry: self.develop.geometry.sanitized(),
            exposure: self.develop.exposure,
            masks: self.masks.stack.clone(),
            existing: self.inpaint.edits.as_ref().clone(),
            brush: brush.clone(),
            retouch,
            program_prewarm: self.export.gpu_prewarm.clone(),
            cancellation: Arc::clone(&cancellation),
        };
        self.inpaint.pending_brush = Some(brush);
        self.inpaint.pending_retouch = Some(retouch);
        self.inpaint.processing_label = Some(format!("Applying {} locally…", retouch.tool.label()));
        self.inpaint.cancellation = Some(cancellation);
        self.inpaint.receiver = Some(spawn_retouch(request));
        self.egui_ctx
            .request_repaint_after(Duration::from_millis(16));
    }

    pub(crate) fn show_remove_model_dialog(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.inpaint.model_consent_open {
            return;
        }
        let model_download_needed =
            !crate::remove::big_lama_model_is_verified(&self.big_lama_model_path());
        let runtime_download_needed = self.ai.runtime_download_consent_pending;
        let title = match (model_download_needed, runtime_download_needed) {
            (true, true) => "Download Remove model and ONNX Runtime?",
            (true, false) => "Download Remove model?",
            (false, true) => "Download ONNX Runtime?",
            (false, false) => "Prepare Remove?",
        };
        crate::ui::responsive_popup(
            egui::Window::new(title),
            ctx,
            520.0,
        )
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label("Remove uses the Big-LaMa Places2 ONNX inpainting model for local context repair.");
            if model_download_needed {
                ui.strong("Remove model");
                ui.label(format!(
                    "Big-LaMa Places2 ONNX: about {:.0} MB download. Model license: {}.",
                    crate::remove::BIG_LAMA_MODEL_BYTES as f64 / 1_000_000.0,
                    crate::remove::BIG_LAMA_MODEL_LICENSE
                ));
                ui.label(format!(
                    "Provenance: {}.",
                    crate::remove::BIG_LAMA_MODEL_PROVENANCE
                ));
                ui.label(format!(
                    "CalibRaw accepts only the pinned model after exact size and SHA-256 verification ({}).",
                    &crate::remove::BIG_LAMA_MODEL_SHA256_HEX[..12]
                ));
            }
            #[cfg(not(target_os = "android"))]
            if runtime_download_needed {
                Self::show_automatic_onnx_runtime_download_details(ui);
            }
            if model_download_needed && runtime_download_needed {
                ui.separator();
                ui.label("CalibRaw downloads and verifies the model first, followed by ONNX Runtime. Both are cached locally.");
            }
            ui.label("Inference is local. No photograph or Remove stroke is uploaded.");
            ui.label(concat!(
                "When you continue, your device connects directly to Hugging Face. Hugging Face ",
                "receives connection data such as your IP address and request time under its privacy ",
                "policy. CalibRaw sends no account identifier or telemetry."
            ));
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("Hugging Face privacy policy", "https://huggingface.co/privacy");
                if model_download_needed {
                    ui.separator();
                    ui.hyperlink_to("Big-LaMa ONNX model card", "https://huggingface.co/Carve/LaMa-ONNX");
                }
            });
            #[cfg(not(target_os = "android"))]
            if self.ai.runtime_mode == OnnxRuntimeMode::Manual && self.ai.runtime_path.is_none() {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Manual runtime mode needs a trusted local ONNX Runtime library. Select one in Settings or switch to Automatic.",
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Consent, download and continue").clicked()
                    && self.ai_runtime_ready()
                {
                    self.ai.runtime_download_consent_pending = false;
                    self.inpaint.model_consent_open = false;
                    if let Some(brush) = self.inpaint.pending_brush.take() {
                        let existing = self.inpaint.edits.as_ref().clone();
                        let _ = self.start_remove_request(
                            frame,
                            existing,
                            brush,
                            model_download_needed,
                        );
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.ai.runtime_download_consent_pending = false;
                    self.inpaint.model_consent_open = false;
                    self.inpaint.pending_brush = None;
                    self.inpaint.pending_retouch = None;
                    self.inpaint.last_brush_uv = None;
                }
            });
        });
    }

    pub(crate) fn advance_remove_worker(&mut self, _frame: &eframe::Frame) {
        let mut events = Vec::new();
        if let Some(receiver) = self.inpaint.receiver.as_ref() {
            while let Ok(event) = receiver.try_recv() {
                events.push(event);
            }
        }
        if events.is_empty() {
            if self.inpaint.receiver.is_some() {
                self.egui_ctx
                    .request_repaint_after(Duration::from_millis(50));
            }
            return;
        }
        for event in events {
            match event {
                RemoveEvent::DownloadProgress(progress) => {
                    let fraction = if progress.total == 0 {
                        0.0
                    } else {
                        progress.downloaded as f64 / progress.total as f64
                    };
                    self.inpaint.processing_label = Some(format!(
                        "Downloading {}… {:.0}%",
                        progress.label,
                        (fraction * 100.0).clamp(0.0, 100.0)
                    ));
                }
                RemoveEvent::Processing { .. } => {
                    self.inpaint.processing_label = Some("Applying Big-LaMa…".to_owned());
                }
                RemoveEvent::Finished(result) => {
                    self.inpaint.receiver = None;
                    self.inpaint.cancellation = None;
                    let pending_brush = self.inpaint.pending_brush.take();
                    let pending_retouch = self.inpaint.pending_retouch.take();
                    self.inpaint.processing_label = None;
                    match result {
                        Ok(stroke) => {
                            let applied_tool = stroke
                                .retouch
                                .map(|retouch| retouch.tool.label())
                                .unwrap_or("Remove");
                            Arc::make_mut(&mut self.inpaint.edits).strokes.push(stroke);
                            self.inpaint.selected_stroke = None;
                            self.inpaint.hovered_stroke = None;
                            self.note_remove_edit_changed();
                            self.ui.notice = Some(format!("{applied_tool} applied."));
                        }
                        Err(error) => {
                            if error.contains("consent to its download again") {
                                self.inpaint.pending_brush = pending_brush;
                                self.ai.runtime_download_consent_pending =
                                    self.automatic_onnx_runtime_download_needed();
                                self.inpaint.model_consent_open = true;
                                self.ui.notice = Some(
                                    "Big-LaMa needs to be installed or re-verified before Remove can continue."
                                        .to_owned(),
                                );
                            } else if !error.contains("cancelled") {
                                let tool = pending_retouch
                                    .map(|retouch| retouch.tool.label())
                                    .unwrap_or("Remove");
                                self.ui.notice = Some(format!("{tool} failed: {error}"));
                                crate::diagnostics::record(format!("{tool} failed: {error}"));
                                log::error!("{tool} failed: {error}");
                            }
                        }
                    }
                }
            }
        }
        self.egui_ctx.request_repaint();
    }
}
