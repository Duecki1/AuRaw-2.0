use super::*;

impl InpaintState {
    pub(crate) fn reset_for_document(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.store(true, std::sync::atomic::Ordering::Release);
        }
        self.edits = Arc::new(RemoveEditState::default());
        self.active_points.clear();
        self.last_brush_uv = None;
        self.pending_brush = None;
        self.model_consent_open = false;
        self.receiver = None;
        self.processing_label = None;
        self.hovered_stroke = None;
        self.selected_stroke = None;
    }

    pub(crate) fn processing(&self) -> bool {
        self.receiver.is_some() || self.model_consent_open
    }
}

impl AurawApp {
    pub(crate) fn reset_inpainting_state(&mut self) {
        self.inpaint.reset_for_document();
    }

    pub(crate) fn install_remove_edits(&mut self, edits: Arc<RemoveEditState>) {
        if let Some(cancellation) = self.inpaint.cancellation.take() {
            cancellation.store(true, std::sync::atomic::Ordering::Release);
        }
        self.inpaint.receiver = None;
        self.inpaint.pending_brush = None;
        self.inpaint.model_consent_open = false;
        self.inpaint.active_points.clear();
        self.inpaint.last_brush_uv = None;
        self.inpaint.processing_label = None;
        self.inpaint.edits = edits;
        self.inpaint.hovered_stroke = None;
        self.inpaint.selected_stroke = None;
    }

    pub(crate) fn cancel_remove_processing(&mut self) {
        if let Some(cancellation) = self.inpaint.cancellation.take() {
            cancellation.store(true, std::sync::atomic::Ordering::Release);
        }
        self.inpaint.receiver = None;
        self.inpaint.pending_brush = None;
        self.inpaint.model_consent_open = false;
        self.inpaint.processing_label = None;
        self.inpaint.active_points.clear();
        self.inpaint.last_brush_uv = None;
    }

    pub(crate) fn clear_inpainting_tool(&mut self) {
        self.cancel_remove_processing();
        if self.inpaint.edits.strokes.is_empty() {
            return;
        }
        Arc::make_mut(&mut self.inpaint.edits).strokes.clear();
        self.inpaint.hovered_stroke = None;
        self.inpaint.selected_stroke = None;
        self.note_remove_edit_changed();
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn delete_inpaint_stroke(&mut self, index: usize) {
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
        let runtime_path = self.ai.runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.ai.runtime_sha256.clone();
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
            model_path: self.big_lama_model_path(),
            allow_download,
            runtime_path,
            runtime_sha256,
            tone_statistics: None,
            program_prewarm: self.export.gpu_prewarm.clone(),
            cancellation: Arc::clone(&cancellation),
        };
        self.inpaint.pending_brush = Some(brush);
        self.inpaint.processing_label = Some("Preparing local context…".to_owned());
        self.inpaint.cancellation = Some(cancellation);
        self.inpaint.receiver = Some(spawn_remove(request));
        self.egui_ctx.request_repaint_after(Duration::from_millis(30));
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
        if crate::remove::big_lama_model_is_verified(&model_path) {
            let existing = self.inpaint.edits.as_ref().clone();
            let _ = self.start_remove_request(frame, existing, brush, false);
        } else {
            self.inpaint.pending_brush = Some(brush);
            self.inpaint.model_consent_open = true;
            self.egui_ctx.request_repaint();
        }
    }

    pub(crate) fn show_remove_model_dialog(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        if !self.inpaint.model_consent_open {
            return;
        }
        crate::ui::responsive_popup(
            egui::Window::new("Download Remove model?"),
            ctx,
            520.0,
        )
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label("Remove uses the Big-LaMa Places2 ONNX inpainting model for local context repair.");
            ui.label(format!(
                concat!(
                    "The first use downloads about {:.0} MB and stores the verified ONNX ",
                    "file in AuRaw's shared model cache."
                ),
                crate::remove::BIG_LAMA_MODEL_BYTES as f64 / 1_000_000.0
            ));
            ui.label(format!(
                "Model license: {}. Provenance: {}.",
                crate::remove::BIG_LAMA_MODEL_LICENSE,
                crate::remove::BIG_LAMA_MODEL_PROVENANCE
            ));
            ui.label("Inference is local. No photograph or Remove stroke is uploaded.");
            ui.label(concat!(
                "When you continue, your device connects directly to Hugging Face. Hugging Face ",
                "receives connection data such as your IP address and request time under its privacy ",
                "policy. AuRaw sends no account identifier or telemetry."
            ));
            ui.label(format!(
                "AuRaw accepts only the pinned model after exact size and SHA-256 verification ({}).",
                &crate::remove::BIG_LAMA_MODEL_SHA256_HEX[..12]
            ));
            ui.horizontal_wrapped(|ui| {
                ui.hyperlink_to("Hugging Face privacy policy", "https://huggingface.co/privacy");
                ui.separator();
                ui.hyperlink_to("Big-LaMa ONNX model card", "https://huggingface.co/Carve/LaMa-ONNX");
            });
            #[cfg(not(target_os = "android"))]
            if self.ai.runtime_path.is_none() {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    "Select a trusted local ONNX Runtime library in Settings before continuing.",
                );
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Consent, download and continue").clicked()
                    && self.ai_runtime_ready()
                {
                    self.inpaint.model_consent_open = false;
                    if let Some(brush) = self.inpaint.pending_brush.take() {
                        let existing = self.inpaint.edits.as_ref().clone();
                        let _ = self.start_remove_request(frame, existing, brush, true);
                    }
                }
                if ui.button("Cancel").clicked() {
                    self.inpaint.model_consent_open = false;
                    self.inpaint.pending_brush = None;
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
                self.egui_ctx.request_repaint_after(Duration::from_millis(50));
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
                RemoveEvent::Processing { completed, total } => {
                    self.inpaint.processing_label = Some(if total <= 1 {
                        "Applying Big-LaMa…".to_owned()
                    } else {
                        format!("Applying Big-LaMa… {completed}/{total} local crops")
                    });
                }
                RemoveEvent::Finished(result) => {
                    self.inpaint.receiver = None;
                    self.inpaint.cancellation = None;
                    let pending_brush = self.inpaint.pending_brush.take();
                    self.inpaint.processing_label = None;
                    match result {
                        Ok(stroke) => {
                            Arc::make_mut(&mut self.inpaint.edits).strokes.push(stroke);
                            self.inpaint.selected_stroke = None;
                            self.inpaint.hovered_stroke = None;
                            self.note_remove_edit_changed();
                            self.ui.notice = Some("Remove applied.".to_owned());
                        }
                        Err(error) => {
                            if error.contains("consent to its download again") {
                                self.inpaint.pending_brush = pending_brush;
                                self.inpaint.model_consent_open = true;
                                self.ui.notice = Some(
                                    "Big-LaMa needs to be installed or re-verified before Remove can continue."
                                        .to_owned(),
                                );
                            } else if !error.contains("cancelled") {
                                self.ui.notice = Some(format!("Remove failed: {error}"));
                                crate::diagnostics::record(format!("Big-LaMa Remove failed: {error}"));
                                log::error!("Big-LaMa Remove failed: {error}");
                            }
                        }
                    }
                }
            }
        }
        self.egui_ctx.request_repaint();
    }
}
