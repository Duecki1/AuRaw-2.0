use super::*;
use crate::ai_denoise::{AiDenoiseEvent, RAWNIND_PACKAGE_BYTES};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        atomic::AtomicBool,
        Arc,
    },
};

impl AurawApp {
    fn discard_ai_preview_caches(&mut self) {
        for texture_id in [
            self.preview.detail
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
            self.preview.navigation
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
            self.android.android_app
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
            self.android.android_app
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
        self.persistence.sidecar_target
            .as_ref()
            .map(|target| self.rawnind_result_cache_path_for_target(target))
    }

    pub(crate) fn set_ai_denoise_enabled(&mut self, enabled: bool, frame: &eframe::Frame) {
        if !enabled {
            self.ai.denoise_consent_open = false;
            self.cancel_foreground_operation_if(ForegroundOperationKind::AiDenoise);
            let changed = self.develop.exposure.ai_denoise_enabled;
            self.develop.exposure.ai_denoise_enabled = false;
            self.develop.target_exposure.ai_denoise_enabled = false;
            self.preview.quality_dirty = true;
            self.discard_ai_preview_caches();
            self.preview.pending_stage = None;
            self.preview.detail_pending_stage = None;
            self.preview.navigation_pending_stage = None;
            if changed {
                self.note_edit_changed();
            }
            self.egui_ctx.request_repaint();
            return;
        }
        if self.foreground_operation_active() {
            self.ui.notice = Some("Finish or cancel the current editing operation first.".to_owned());
            return;
        }
        if self.develop.loaded_raw.is_none() {
            self.develop.exposure.ai_denoise_enabled = false;
            self.develop.target_exposure.ai_denoise_enabled = false;
            self.ui.notice = Some("Open a RAW image before enabling AI denoise.".to_owned());
            self.egui_ctx.request_repaint();
            return;
        }
        if self.develop.loaded_raw
            .as_ref()
            .is_some_and(|raw| raw.is_pre_demosaiced_raster())
        {
            self.develop.exposure.ai_denoise_enabled = false;
            self.develop.target_exposure.ai_denoise_enabled = false;
            self.ui.notice = Some(
                "AI denoise is a sensor-RAW operation; rendered TIFFs use the standard Detail controls."
                    .to_owned(),
            );
            self.egui_ctx.request_repaint();
            return;
        }
        if self.develop.loaded_raw
            .as_ref()
            .is_some_and(|raw| raw.ai_denoised_image().is_some())
        {
            self.develop.exposure.ai_denoise_enabled = true;
            self.note_edit_changed();
            self.preview.quality_dirty = true;
            self.discard_ai_preview_caches();
            return;
        }
        let saved_result_exists = self
            .rawnind_result_cache_path()
            .is_some_and(|path| path.is_file());
        #[cfg(not(target_os = "android"))]
        if !saved_result_exists && !self.validate_onnx_runtime_for_ai() {
            self.develop.exposure.ai_denoise_enabled = false;
            self.develop.target_exposure.ai_denoise_enabled = false;
            return;
        }
        let model_dir = self.rawnind_model_dir();
        if saved_result_exists || crate::ai_denoise::models_are_verified(&model_dir) {
            self.start_ai_denoise(frame, false);
        } else {
            self.ai.denoise_consent_open = true;
            self.egui_ctx.request_repaint();
        }
    }

    fn start_ai_denoise(&mut self, frame: &eframe::Frame, allow_model_download: bool) {
        if self.foreground_operation_active() {
            return;
        }
        let Some(raw) = self.develop.loaded_raw.as_ref().map(Arc::clone) else {
            self.ui.notice = Some("Open a RAW image before enabling AI denoise.".to_owned());
            return;
        };
        let result_cache_path = self.rawnind_result_cache_path();
        let saved_result_exists = result_cache_path
            .as_ref()
            .is_some_and(|path| path.is_file());
        #[cfg(not(target_os = "android"))]
        if !saved_result_exists && !self.validate_onnx_runtime_for_ai() {
            self.develop.exposure.ai_denoise_enabled = false;
            return;
        }
        #[cfg(target_os = "android")]
        if !saved_result_exists {
            if let Err(error) = crate::ai_masks::initialize_runtime(None, None) {
                self.develop.exposure.ai_denoise_enabled = false;
                self.develop.target_exposure.ai_denoise_enabled = false;
                self.ui.notice = Some(format!(
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
            self.ui.notice = Some("AI denoise requires AuRaw's wgpu renderer.".to_owned());
            self.develop.exposure.ai_denoise_enabled = false;
            self.develop.target_exposure.ai_denoise_enabled = false;
            return;
        };
        raw.clear_ai_denoised_image();
        #[cfg(target_os = "android")]
        {
            let previous_pipeline = {
                let mut renderer = render_state.renderer.write();
                self.take_preview_pipeline_and_release_textures(&mut renderer)
            };
            drop(previous_pipeline);
        }
        #[cfg(not(target_os = "android"))]
        for texture_id in [
            self.preview.detail
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
            self.preview.navigation
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
        ]
        .into_iter()
        .flatten()
        {
            self.retire_egui_texture(texture_id);
        }
        self.preview.pending_stage = None;
        self.preview.detail_pending_stage = None;
        self.preview.navigation_pending_stage = None;
        self.preview.detail_urgent = false;
        self.preview.quality_dirty = false;
        let cancellation = Arc::new(AtomicBool::new(false));
        let receiver = crate::ai_denoise::spawn_rawnind_denoise(
            self.rawnind_model_dir(),
            {
                #[cfg(not(target_os = "android"))]
                {
                    self.ai.runtime_path.clone()
                }
                #[cfg(target_os = "android")]
                {
                    None
                }
            },
            {
                #[cfg(not(target_os = "android"))]
                {
                    self.ai.runtime_sha256.clone()
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
        self.ai.denoise_consent_open = false;
        let progress = ForegroundProgress::indeterminate(if saved_result_exists {
            "Restoring saved AI denoise…"
        } else {
            "Preparing RawNIND models…"
        });
        self.begin_foreground_operation(ForegroundOperation {
            kind: ForegroundOperationKind::AiDenoise,
            document_id: self.persistence.sidecar_generation,
            cancellation,
            progress,
            cancelling: false,
            receiver: ForegroundOperationReceiver::AiDenoise(receiver),
            context: ForegroundOperationContext::AiDenoise,
        });
        let changed = !self.develop.exposure.ai_denoise_enabled;
        self.develop.exposure.ai_denoise_enabled = true;
        self.develop.target_exposure.ai_denoise_enabled = true;
        if changed {
            self.note_edit_changed();
        }
        crate::diagnostics::record(format!(
            "RawNIND worker started for document {} on {}",
            self.persistence.sidecar_generation,
            if cfg!(target_os = "android") {
                "Android"
            } else {
                "desktop"
            }
        ));
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn poll_ai_denoise_worker(&mut self) {
        if !self.foreground_operation_is(ForegroundOperationKind::AiDenoise) {
            return;
        }
        let Some(mut operation) = self.foreground_operation.take() else {
            return;
        };
        let ForegroundOperationReceiver::AiDenoise(receiver) = &operation.receiver else {
            self.foreground_operation = Some(operation);
            return;
        };
        let (events, disconnected) = drain_worker_events(Some(receiver), |event| {
            matches!(event, AiDenoiseEvent::Finished(_))
        });
        let mut finished = None;
        for event in events {
            match event {
                AiDenoiseEvent::DownloadProgress { downloaded, total } => {
                    operation.progress = ForegroundProgress::units(
                        downloaded,
                        total,
                        Some("bytes".to_owned()),
                        "Downloading verified RawNIND model package",
                    )
                    .with_detail(format!(
                        "{:.1} / {:.1} MB",
                        downloaded as f64 / 1_000_000.0,
                        total as f64 / 1_000_000.0
                    ));
                }
                AiDenoiseEvent::Progress {
                    phase,
                    completed,
                    total,
                } => {
                    operation.progress = if total > 0 {
                        ForegroundProgress::units(
                            completed as u64,
                            total as u64,
                            Some("tiles".to_owned()),
                            phase,
                        )
                    } else {
                        ForegroundProgress::indeterminate(format!("{phase}…"))
                    };
                }
                AiDenoiseEvent::Finished(result) => finished = Some(result),
            }
        }
        if disconnected && finished.is_none() {
            finished = Some(Err("RawNIND worker stopped unexpectedly.".to_owned()));
        }
        let Some(result) = finished else {
            self.foreground_operation = Some(operation);
            return;
        };
        let stale = operation.document_id != self.persistence.sidecar_generation;
        if stale {
            return;
        }
        self.preview.quality_dirty = true;
        self.discard_ai_preview_caches();
        self.preview.pending_stage = None;
        self.preview.detail_pending_stage = None;
        self.preview.navigation_pending_stage = None;
        self.preview.detail_urgent = false;
        match result {
            Ok(image) if self.develop.exposure.ai_denoise_enabled && !operation.is_cancelled() => {
                let install = self.develop.loaded_raw
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("the RAW was closed"))
                    .and_then(|raw| raw.set_ai_denoised_image(image));
                match install {
                    Ok(()) => {
                        self.ui.notice = Some(
                            "AI denoise applied locally. Standard denoise values were preserved."
                                .to_owned(),
                        );
                    }
                    Err(error) => {
                        let changed = self.develop.exposure.ai_denoise_enabled;
                        self.develop.exposure.ai_denoise_enabled = false;
                        self.develop.target_exposure.ai_denoise_enabled = false;
                        if changed {
                            self.note_edit_changed();
                        }
                        self.ui.notice = Some(format!("Could not install AI denoise: {error:#}"));
                    }
                }
            }
            Ok(_) => {
                self.develop.exposure.ai_denoise_enabled = false;
                self.develop.target_exposure.ai_denoise_enabled = false;
            }
            Err(error) => {
                let changed = self.develop.exposure.ai_denoise_enabled;
                self.develop.exposure.ai_denoise_enabled = false;
                self.develop.target_exposure.ai_denoise_enabled = false;
                if changed {
                    self.note_edit_changed();
                }
                if !error.contains("cancelled") {
                    self.ui.notice = Some(format!("AI denoise failed: {error}"));
                }
            }
        }
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn abandon_ai_denoise_worker(&mut self) {
        self.cancel_foreground_operation_if(ForegroundOperationKind::AiDenoise);
        self.ai.denoise_consent_open = false;
    }

    pub(crate) fn resume_persisted_ai_denoise(&mut self, frame: &eframe::Frame) {
        self.ai.denoise_resume_pending = false;
        if self.develop.exposure.ai_denoise_enabled && self.develop.loaded_raw.is_some() {
            if self.foreground_operation_active() {
                self.ai.denoise_resume_pending = true;
                return;
            }
            if self.develop.loaded_raw
                .as_ref()
                .is_some_and(|raw| raw.ai_denoised_image().is_some())
            {
                self.develop.target_exposure.ai_denoise_enabled = true;
                crate::diagnostics::record(
                    "Restored the persisted AI-denoise scene without rerunning RawNIND",
                );
                return;
            }
            self.set_ai_denoise_enabled(true, frame);
        }
    }

    pub(crate) fn resume_pending_ai_denoise(&mut self, frame: &eframe::Frame) {
        if self.ai.denoise_resume_pending {
            self.resume_persisted_ai_denoise(frame);
        }
    }

    pub(crate) fn show_ai_denoise_dialogs(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.ai.denoise_consent_open {
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
                        self.ai.denoise_consent_open = false;
                        self.develop.exposure.ai_denoise_enabled = false;
                    }
                });
            });
        }

    }
}
