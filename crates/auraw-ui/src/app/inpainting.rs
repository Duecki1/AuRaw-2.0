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
        self.receiver = None;
        self.processing_label = None;
        self.hovered_stroke = None;
        self.selected_stroke = None;
    }

    pub(crate) fn processing(&self) -> bool {
        self.receiver.is_some()
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
    ) -> bool {
        let Some(raw) = self.develop.loaded_raw.as_ref().cloned() else {
            self.ui.notice = Some("Open a RAW image before using Remove.".to_owned());
            return false;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.ui.notice = Some("GPU rendering is unavailable for Remove.".to_owned());
            return false;
        };
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
            runtime_path: self.ai.runtime_path.clone(),
            runtime_sha256: self.ai.runtime_sha256.clone(),
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
        let existing = self.inpaint.edits.as_ref().clone();
        let _ = self.start_remove_request(frame, existing, brush);
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
                RemoveEvent::DownloadProgress { downloaded, total } => {
                    let fraction = if total == 0 {
                        0.0
                    } else {
                        downloaded as f64 / total as f64
                    };
                    self.inpaint.processing_label = Some(format!(
                        "Downloading Big-LaMa… {:.0}%",
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
                    self.inpaint.pending_brush = None;
                    self.inpaint.processing_label = None;
                    match result {
                        Ok(stroke) => {
                            Arc::make_mut(&mut self.inpaint.edits).strokes.push(stroke);
                            self.inpaint.selected_stroke = None;
                            self.inpaint.hovered_stroke = None;
                            // Rebuild the RAW/scene stage once so the cached patch enters
                            // the normal image graph. Subsequent Develop adjustments reuse
                            // it and never invoke Big-LaMa again.
                            self.note_remove_edit_changed();
                            self.ui.notice = Some("Remove applied.".to_owned());
                        }
                        Err(error) => {
                            if !error.contains("cancelled") {
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
