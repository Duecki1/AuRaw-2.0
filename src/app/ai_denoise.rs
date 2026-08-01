use super::AurawApp;
use crate::ai_denoise::{AiDenoiseEvent, RAWNIND_PACKAGE_BYTES};
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
    fn discard_ai_preview_caches(&mut self) {
        for texture_id in [
            self.preview_detail
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
            self.preview_navigation
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
        ]
        .into_iter()
        .flatten()
        {
            self.retire_egui_texture(texture_id);
        }
    }

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

    fn rawnind_result_cache_dir(&self) -> PathBuf {
        #[cfg(target_os = "android")]
        {
            self.android_app
                .internal_data_path()
                .unwrap_or_else(std::env::temp_dir)
                .join("ai-denoise-results-v2")
        }
        #[cfg(not(target_os = "android"))]
        {
            let model_dir = self.rawnind_model_dir();
            model_dir
                .parent()
                .and_then(std::path::Path::parent)
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(std::env::temp_dir)
                .join("ai-denoise-results-v2")
        }
    }

    pub(crate) fn rawnind_result_cache_path_for_target(
        &self,
        target: &crate::sidecar::SidecarTarget,
    ) -> PathBuf {
        let identity = match target {
            crate::sidecar::SidecarTarget::Desktop { raw_path } => {
                format!("desktop:{}", raw_path.display())
            }
            #[cfg(target_os = "android")]
            crate::sidecar::SidecarTarget::Android { raw_uri, .. } => {
                format!("android:{raw_uri}")
            }
        };
        crate::ai_denoise::result_cache_path(&self.rawnind_result_cache_dir(), &identity)
    }

    fn rawnind_result_cache_path(&self) -> Option<PathBuf> {
        self.sidecar_target
            .as_ref()
            .map(|target| self.rawnind_result_cache_path_for_target(target))
    }

    pub(crate) fn set_ai_denoise_enabled(&mut self, enabled: bool, frame: &eframe::Frame) {
        if !enabled {
            self.ai_denoise_consent_open = false;
            if let Some(cancellation) = &self.ai_denoise_cancellation {
                cancellation.store(true, Ordering::Release);
            }
            let changed = self.exposure.ai_denoise_enabled;
            self.exposure.ai_denoise_enabled = false;
            self.target_exposure.ai_denoise_enabled = false;
            // Bayer AI denoise now supplies the pipeline's CFA texture. A
            // normal stage update cannot swap that immutable source texture,
            // so disabling it must rebuild the preview from the original RAW.
            self.preview_quality_dirty = true;
            self.discard_ai_preview_caches();
            self.pending_stage = None;
            self.preview_detail_pending_stage = None;
            self.navigation_pending_stage = None;
            if changed {
                self.note_edit_changed();
            }
            self.egui_ctx.request_repaint();
            return;
        }
        if self.ai_denoise_receiver.is_some() {
            return;
        }
        if self.loaded_raw.is_none() {
            self.exposure.ai_denoise_enabled = false;
            self.target_exposure.ai_denoise_enabled = false;
            self.notice = Some("Open a RAW image before enabling AI denoise.".to_owned());
            self.egui_ctx.request_repaint();
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
            self.discard_ai_preview_caches();
            return;
        }
        let saved_result_exists = self
            .rawnind_result_cache_path()
            .is_some_and(|path| path.is_file());
        #[cfg(not(target_os = "android"))]
        if !saved_result_exists && !self.validate_onnx_runtime_for_ai() {
            self.exposure.ai_denoise_enabled = false;
            self.target_exposure.ai_denoise_enabled = false;
            return;
        }
        let model_dir = self.rawnind_model_dir();
        if saved_result_exists || crate::ai_denoise::models_are_verified(&model_dir) {
            self.start_ai_denoise(frame, false);
        } else {
            self.ai_denoise_consent_open = true;
            self.egui_ctx.request_repaint();
        }
    }

    fn start_ai_denoise(&mut self, frame: &eframe::Frame, allow_model_download: bool) {
        if self.ai_denoise_receiver.is_some() {
            return;
        }
        let Some(raw) = self.loaded_raw.as_ref().map(Arc::clone) else {
            self.notice = Some("Open a RAW image before enabling AI denoise.".to_owned());
            return;
        };
        let result_cache_path = self.rawnind_result_cache_path();
        let saved_result_exists = result_cache_path
            .as_ref()
            .is_some_and(|path| path.is_file());
        #[cfg(not(target_os = "android"))]
        if !saved_result_exists && !self.validate_onnx_runtime_for_ai() {
            self.exposure.ai_denoise_enabled = false;
            return;
        }
        #[cfg(target_os = "android")]
        if !saved_result_exists {
            if let Err(error) = crate::ai_masks::initialize_runtime(None, None) {
                self.exposure.ai_denoise_enabled = false;
                self.target_exposure.ai_denoise_enabled = false;
                self.notice = Some(format!(
                    "Could not initialize Android AI denoise: {error:#}"
                ));
                crate::diagnostics::record(format!(
                    "Android RawNIND runtime initialization failed before worker start: {error:#}"
                ));
                self.egui_ctx.request_repaint();
                return;
            }
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("AI denoise requires AuRaw's wgpu renderer.".to_owned());
            self.exposure.ai_denoise_enabled = false;
            self.target_exposure.ai_denoise_enabled = false;
            return;
        };
        raw.clear_ai_denoised_image();
        // RawNIND's tensors and the full-resolution blended CFA have a large
        // temporary memory peak. Release disposable preview graphs while the
        // worker runs so Android does not retain display-sized GPU surfaces on
        // top of that peak. Bayer inference itself no longer needs a GPU
        // demosaic pipeline.
        #[cfg(target_os = "android")]
        {
            // Keeping a DPI-sized preview resident alongside ONNX Runtime's
            // tensors can exceed the mobile process budget. Retire every
            // preview texture and rebuild it after every success, failure, or
            // cancellation path below.
            let previous_pipeline = {
                let mut renderer = render_state.renderer.write();
                self.take_preview_pipeline_and_release_textures(&mut renderer)
            };
            drop(previous_pipeline);
        }
        #[cfg(not(target_os = "android"))]
        for texture_id in [
            self.preview_detail
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
            self.preview_navigation
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
        ]
        .into_iter()
        .flatten()
        {
            self.retire_egui_texture(texture_id);
        }
        self.pending_stage = None;
        self.preview_detail_pending_stage = None;
        self.navigation_pending_stage = None;
        self.preview_detail_urgent = false;
        self.preview_quality_dirty = false;
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
            result_cache_path,
            allow_model_download,
            Arc::clone(&cancellation),
        );
        self.ai_denoise_consent_open = false;
        self.ai_denoise_receiver = Some(receiver);
        self.ai_denoise_download_progress = None;
        self.ai_denoise_apply_progress = Some((
            if saved_result_exists {
                "Restoring saved AI denoise"
            } else {
                "Preparing RawNIND models"
            },
            0,
            0,
        ));
        self.ai_denoise_cancellation = Some(cancellation);
        self.ai_denoise_job_document_id = self.sidecar_generation;
        let changed = !self.exposure.ai_denoise_enabled;
        self.exposure.ai_denoise_enabled = true;
        self.target_exposure.ai_denoise_enabled = true;
        if changed {
            // This is the semantic toggle transaction. Persist it once here;
            // reopening an already-enabled sidecar reaches this path with
            // `changed == false` and therefore does not create another edit.
            self.note_edit_changed();
        }
        crate::diagnostics::record(format!(
            "RawNIND worker started for document {} on {}",
            self.ai_denoise_job_document_id,
            if cfg!(target_os = "android") {
                "Android"
            } else {
                "desktop"
            }
        ));
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
        // A model result changes the scene source. Cancellation and failure
        // also need a rebuild on Android because start_ai_denoise deliberately
        // released the preview. Keep this one terminal transaction for every
        // path so the UI can never remain blank after the worker exits.
        self.preview_quality_dirty = true;
        self.discard_ai_preview_caches();
        self.pending_stage = None;
        self.preview_detail_pending_stage = None;
        self.navigation_pending_stage = None;
        self.preview_detail_urgent = false;
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
                        self.notice = Some(
                            "AI denoise applied locally. Standard denoise values were preserved."
                                .to_owned(),
                        );
                    }
                    Err(error) => {
                        let changed = self.exposure.ai_denoise_enabled;
                        self.exposure.ai_denoise_enabled = false;
                        self.target_exposure.ai_denoise_enabled = false;
                        if changed {
                            self.note_edit_changed();
                        }
                        self.notice = Some(format!("Could not install AI denoise: {error:#}"));
                    }
                }
            }
            Ok(_) => {
                // The user cancelled after the final tile was already running.
                self.exposure.ai_denoise_enabled = false;
                self.target_exposure.ai_denoise_enabled = false;
            }
            Err(error) => {
                let changed = self.exposure.ai_denoise_enabled;
                self.exposure.ai_denoise_enabled = false;
                self.target_exposure.ai_denoise_enabled = false;
                if changed {
                    self.note_edit_changed();
                }
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
            if self
                .loaded_raw
                .as_ref()
                .is_some_and(|raw| raw.ai_denoised_image().is_some())
            {
                self.target_exposure.ai_denoise_enabled = true;
                crate::diagnostics::record(
                    "Restored the persisted AI-denoise scene without rerunning RawNIND",
                );
                return;
            }
            // The sidecar retains intent. A missing, stale, or corrupt derived
            // cache is safely rebuilt from the original sensor mosaic.
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
                        self.start_ai_denoise(frame, true);
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
