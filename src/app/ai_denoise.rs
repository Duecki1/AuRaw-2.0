use super::AurawApp;
use crate::ai_denoise::{AiDenoiseEvent, RAWNIND_PACKAGE_BYTES};
use crate::pipeline::ProcessingStage;
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

impl AurawApp {
    fn rawnind_model_dir(&self) -> PathBuf {
        #[cfg(target_os = "android")]
        {
            self.android_app
                .internal_data_path()
                .unwrap_or_else(std::env::temp_dir)
                .join("models/rawdenoise-nind-1.0")
        }
        #[cfg(not(target_os = "android"))]
        {
            crate::ai_denoise::model_cache_dir()
        }
    }

    pub(crate) fn set_ai_denoise_enabled(&mut self, enabled: bool, frame: &eframe::Frame) {
        if !enabled {
            self.ai_denoise_consent_open = false;
            if let Some(cancellation) = &self.ai_denoise_cancellation {
                cancellation.store(true, Ordering::Release);
            }
            self.exposure.ai_denoise_enabled = false;
            self.mark_pipeline_dirty();
            return;
        }
        if self.ai_denoise_receiver.is_some() || self.loaded_raw.is_none() {
            return;
        }
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
            self.exposure.ai_denoise_enabled = false;
            self.target_exposure.ai_denoise_enabled = false;
            return;
        }
        if self
            .loaded_raw
            .as_ref()
            .is_some_and(|raw| raw.ai_denoised_image().is_some())
        {
            self.exposure.ai_denoise_enabled = true;
            self.note_edit_changed();
            self.preview_quality_dirty = true;
            self.preview_detail = None;
            self.preview_navigation = None;
            return;
        }
        let model_dir = self.rawnind_model_dir();
        if crate::ai_denoise::models_are_verified(&model_dir) {
            self.start_ai_denoise(frame);
        } else {
            self.ai_denoise_consent_open = true;
            self.egui_ctx.request_repaint();
        }
    }

    fn start_ai_denoise(&mut self, frame: &eframe::Frame) {
        if self.ai_denoise_receiver.is_some() {
            return;
        }
        let Some(raw) = self.loaded_raw.as_ref().map(Arc::clone) else {
            self.notice = Some("Open a RAW image before enabling AI denoise.".to_owned());
            return;
        };
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
            self.exposure.ai_denoise_enabled = false;
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("AI denoise requires AuRaw's wgpu renderer.".to_owned());
            self.exposure.ai_denoise_enabled = false;
            return;
        };
        raw.clear_ai_denoised_image();
        let cancellation = Arc::new(AtomicBool::new(false));
        let receiver = crate::ai_denoise::spawn_rawnind_denoise(
            self.rawnind_model_dir(),
            {
                #[cfg(not(target_os = "android"))]
                {
                    self.onnx_runtime_path.clone()
                }
                #[cfg(target_os = "android")]
                {
                    None
                }
            },
            {
                #[cfg(not(target_os = "android"))]
                {
                    self.onnx_runtime_sha256.clone()
                }
                #[cfg(target_os = "android")]
                {
                    None
                }
            },
            raw,
            Some(render_state.device.clone()),
            Some(render_state.queue.clone()),
            Arc::clone(&cancellation),
        );
        self.ai_denoise_consent_open = false;
        self.ai_denoise_receiver = Some(receiver);
        self.ai_denoise_download_progress = None;
        self.ai_denoise_apply_progress = Some(("Preparing RawNIND models", 0, 0));
        self.ai_denoise_cancellation = Some(cancellation);
        self.ai_denoise_job_document_id = self.sidecar_generation;
        self.exposure.ai_denoise_enabled = true;
        self.mark_pipeline_dirty();
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn poll_ai_denoise_worker(&mut self) {
        let mut events = Vec::new();
        let disconnected = if let Some(receiver) = &self.ai_denoise_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(mpsc::TryRecvError::Empty) => break false,
                    Err(mpsc::TryRecvError::Disconnected) => break true,
                }
            }
        } else {
            false
        };
        let mut finished = None;
        for event in events {
            match event {
                AiDenoiseEvent::DownloadProgress { downloaded, total } => {
                    self.ai_denoise_download_progress = Some((downloaded, total));
                    self.ai_denoise_apply_progress = None;
                }
                AiDenoiseEvent::Progress {
                    phase,
                    completed,
                    total,
                } => {
                    self.ai_denoise_download_progress = None;
                    self.ai_denoise_apply_progress = Some((phase, completed, total));
                }
                AiDenoiseEvent::Finished(result) => finished = Some(result),
            }
        }
        if disconnected && finished.is_none() {
            finished = Some(Err("RawNIND worker stopped unexpectedly.".to_owned()));
        }
        let Some(result) = finished else {
            return;
        };
        self.ai_denoise_receiver = None;
        self.ai_denoise_download_progress = None;
        self.ai_denoise_apply_progress = None;
        self.ai_denoise_cancellation = None;
        let stale = self.ai_denoise_job_document_id != self.sidecar_generation;
        if stale {
            return;
        }
        match result {
            Ok(image) if self.exposure.ai_denoise_enabled => {
                let install = self
                    .loaded_raw
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("the RAW was closed"))
                    .and_then(|raw| raw.set_ai_denoised_image(image));
                match install {
                    Ok(()) => {
                        // Rebuild every proxy from the full native model result.
                        // Ordinary stage invalidation cannot retrofit a new scene
                        // source into already-allocated GPU pipelines.
                        self.preview_quality_dirty = true;
                        self.preview_detail = None;
                        self.preview_navigation = None;
                        self.pending_stage = None;
                        self.preview_detail_pending_stage = None;
                        self.navigation_pending_stage = None;
                        self.notice = Some(
                            "AI denoise applied locally. Standard denoise values were preserved."
                                .to_owned(),
                        );
                    }
                    Err(error) => {
                        self.exposure.ai_denoise_enabled = false;
                        self.notice = Some(format!("Could not install AI denoise: {error:#}"));
                        self.queue_preview_processing(ProcessingStage::Raw);
                    }
                }
            }
            Ok(_) => {
                // The user cancelled after the final tile was already running.
                self.queue_preview_processing(ProcessingStage::Raw);
            }
            Err(error) => {
                self.exposure.ai_denoise_enabled = false;
                self.target_exposure.ai_denoise_enabled = true;
                self.mark_pipeline_dirty();
                if !error.contains("cancelled") {
                    self.notice = Some(format!("AI denoise failed: {error}"));
                }
            }
        }
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn abandon_ai_denoise_worker(&mut self) {
        if let Some(cancellation) = self.ai_denoise_cancellation.take() {
            cancellation.store(true, Ordering::Release);
        }
        self.ai_denoise_receiver = None;
        self.ai_denoise_download_progress = None;
        self.ai_denoise_apply_progress = None;
        self.ai_denoise_consent_open = false;
    }

    pub(crate) fn resume_persisted_ai_denoise(&mut self, frame: &eframe::Frame) {
        self.ai_denoise_resume_pending = false;
        if self.exposure.ai_denoise_enabled
            && self.ai_denoise_receiver.is_none()
            && self.loaded_raw.is_some()
        {
            // A sidecar persists intent, never derived pixels. Reopening a RAW
            // therefore reuses an installed verified model or asks before the
            // first network transfer on this device.
            self.set_ai_denoise_enabled(true, frame);
        }
    }

    pub(crate) fn resume_pending_ai_denoise(&mut self, frame: &eframe::Frame) {
        if self.ai_denoise_resume_pending {
            self.resume_persisted_ai_denoise(frame);
        }
    }

    pub(crate) fn show_ai_denoise_dialogs(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.ai_denoise_consent_open {
            crate::ui::responsive_popup(
                egui::Window::new("Download RawNIND AI denoise models?"),
                ctx,
                540.0,
            )
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("AI Denoise uses darktable-ai's RawNIND UtNet2 package: joint Bayer denoise/demosaic and a linear Rec.2020 model for X-Trans.");
                ui.label(format!(
                    "The first use downloads {:.1} MB from GitHub and stores about 62 MB of verified ONNX models in AuRaw's cache.",
                    RAWNIND_PACKAGE_BYTES as f64 / 1_000_000.0
                ));
                ui.label("Model and integration license: GPL-3.0. Inference is local; no photograph is uploaded.");
                ui.label("GitHub receives ordinary connection data such as your IP address and request time. AuRaw sends no account identifier or telemetry.");
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to(
                        "RawNIND model card",
                        "https://github.com/darktable-org/darktable-ai/tree/master/models/rawdenoise-nind",
                    );
                    ui.separator();
                    ui.hyperlink_to(
                        "GitHub privacy statement",
                        "https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement",
                    );
                    ui.separator();
                    ui.hyperlink_to(
                        "GPL-3.0 license",
                        "https://github.com/darktable-org/darktable-ai/blob/master/LICENSE",
                    );
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Consent, download and apply").clicked() {
                        self.start_ai_denoise(frame);
                    }
                    if ui.button("Cancel").clicked() {
                        self.ai_denoise_consent_open = false;
                        self.exposure.ai_denoise_enabled = false;
                    }
                });
            });
        }

        if self.ai_denoise_receiver.is_some() {
            let mut cancel = false;
            crate::ui::responsive_popup(egui::Window::new("Applying AI denoise"), ctx, 440.0)
                .collapsible(false)
                .resizable(false)
                .movable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("RawNIND is being applied before sharpening. This dialog stays open until the operation finishes or is cancelled.");
                    if let Some((downloaded, total)) = self.ai_denoise_download_progress {
                        let fraction = downloaded as f32 / total.max(1) as f32;
                        ui.label("Downloading verified darktable-ai model package…");
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .show_percentage()
                                .text(format!(
                                    "{:.1} / {:.1} MB",
                                    downloaded as f64 / 1_000_000.0,
                                    total as f64 / 1_000_000.0
                                )),
                        );
                    } else if let Some((phase, completed, total)) =
                        self.ai_denoise_apply_progress
                    {
                        ui.label(format!("{phase}…"));
                        if total > 0 {
                            ui.add(
                                egui::ProgressBar::new(completed as f32 / total as f32)
                                    .show_percentage()
                                    .text(format!("{completed} / {total} tiles")),
                            );
                        } else {
                            ui.add(egui::ProgressBar::new(0.0).animate(true));
                        }
                    } else {
                        ui.add(egui::ProgressBar::new(0.0).animate(true));
                    }
                    ui.add_space(8.0);
                    cancel = ui.button("Cancel").clicked();
                });
            if cancel {
                if let Some(cancellation) = &self.ai_denoise_cancellation {
                    cancellation.store(true, Ordering::Release);
                }
                self.exposure.ai_denoise_enabled = false;
                self.ai_denoise_apply_progress = Some(("Cancelling", 0, 0));
            }
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}
