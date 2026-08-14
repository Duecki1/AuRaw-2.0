use super::*;

use super::preview::DETAIL_ZOOM_START;

pub(in crate::app) fn export_source_stem(current_path: Option<&std::path::Path>, current_label: Option<&str>) -> String {
    current_label
        .and_then(|label| std::path::Path::new(label).file_stem())
        .and_then(std::ffi::OsStr::to_str)
        .or_else(|| {
            current_path
                .and_then(std::path::Path::file_stem)
                .and_then(std::ffi::OsStr::to_str)
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or("auraw-export")
        .to_owned()
}

pub(in crate::app) fn spawn_export_request(
    request: ExportTaskRequest,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
) -> mpsc::Receiver<ExportEvent> {
    let ExportTaskRequest {
        device,
        queue,
        raw,
        geometry,
        exposure,
        masks,
        inpaint,
        path,
        format,
        settings,
        metadata,
        display_name: _,
        gpu_export_prewarm,
    } = request;
    spawn_tiled_export(
        format,
        TiledExportJob {
            device,
            queue,
            raw,
            geometry,
            exposure,
            masks,
            inpaint,
            path,
            tile_spec: TileSpec::default(),
            settings,
            metadata,
            cancellation,
            program_prewarm: gpu_export_prewarm,
        },
    )
}

impl AurawApp {
    pub(crate) fn can_export(&self) -> bool {
        self.loaded_raw.is_some()
            && self.preview_raw.is_some()
            && !self.export_publish_pending
            && self.load_receiver.is_none()
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn export_png(&mut self, frame: &eframe::Frame) {
        if !self.can_export() {
            return;
        }

        let default_name = format!(
            "{}-auraw.png",
            export_source_stem(self.current_path.as_deref(), self.current_label.as_deref())
        );
        let mut dialog = rfd::FileDialog::new()
            .add_filter("PNG image", &["png"])
            .set_file_name(default_name);
        if let Some(parent) = self
            .current_path
            .as_deref()
            .and_then(|path| path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            dialog = dialog.set_directory(parent);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        let has_png_extension = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some(extension) if extension.eq_ignore_ascii_case("png")
        );
        if !has_png_extension {
            path.set_extension("png");
        }

        self.start_export(path, frame, ExportFormat::Png);
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn export_jpeg(&mut self, frame: &eframe::Frame) {
        if !self.can_export() {
            return;
        }

        let default_name = format!(
            "{}-auraw.jpg",
            export_source_stem(self.current_path.as_deref(), self.current_label.as_deref())
        );
        let mut dialog = rfd::FileDialog::new()
            .add_filter("JPEG image", &["jpg", "jpeg"])
            .set_file_name(default_name);
        if let Some(parent) = self
            .current_path
            .as_deref()
            .and_then(|path| path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            dialog = dialog.set_directory(parent);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        let has_jpeg_extension = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some(extension)
                if extension.eq_ignore_ascii_case("jpg")
                    || extension.eq_ignore_ascii_case("jpeg")
        );
        if !has_jpeg_extension {
            path.set_extension("jpg");
        }

        self.start_export(path, frame, ExportFormat::Jpeg);
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn export_tiff(&mut self, frame: &eframe::Frame) {
        if !self.can_export() {
            return;
        }

        let default_name = format!(
            "{}-auraw.tif",
            export_source_stem(self.current_path.as_deref(), self.current_label.as_deref())
        );
        let mut dialog = rfd::FileDialog::new()
            .add_filter("TIFF image", &["tif", "tiff"])
            .set_file_name(default_name);
        if let Some(parent) = self
            .current_path
            .as_deref()
            .and_then(|path| path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            dialog = dialog.set_directory(parent);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        let has_tiff_extension = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some(extension)
                if extension.eq_ignore_ascii_case("tif")
                    || extension.eq_ignore_ascii_case("tiff")
        );
        if !has_tiff_extension {
            path.set_extension("tif");
        }

        self.start_export(path, frame, ExportFormat::Tiff);
    }

    #[cfg(target_os = "android")]
    pub(crate) fn export_png(&mut self, frame: &eframe::Frame) {
        self.export_android(frame, ExportFormat::Png);
    }

    #[cfg(target_os = "android")]
    pub(crate) fn export_jpeg(&mut self, frame: &eframe::Frame) {
        self.export_android(frame, ExportFormat::Jpeg);
    }

    #[cfg(target_os = "android")]
    pub(crate) fn export_tiff(&mut self, frame: &eframe::Frame) {
        self.export_android(frame, ExportFormat::Tiff);
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn export_android(&mut self, frame: &eframe::Frame, format: ExportFormat) {
        if !self.can_export() {
            return;
        }

        let Some(data_dir) = self.android_app.internal_data_path() else {
            self.notice = Some("Android did not provide an app data directory.".to_owned());
            return;
        };
        let export_dir = data_dir.join("cache").join("exports");
        if let Err(error) = std::fs::create_dir_all(&export_dir) {
            self.notice = Some(format!("Could not prepare Android export cache: {error}"));
            return;
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let display_name = format!("AuRaw-{timestamp}.{}", format.extension());
        match crate::android::prepare_direct_export(
            &self.android_app,
            &export_dir,
            &display_name,
            format.mime_type(),
        ) {
            Ok(Some(path)) => {
                let direct_path = path.clone();
                if self.start_export(path, frame, format).is_none() {
                    crate::android::cancel_direct_export(&self.android_app, &direct_path);
                }
            }
            Ok(None) => {
                // Android 8/9 still need the legacy cache + permission flow.
                self.start_export(export_dir.join(display_name), frame, format);
            }
            Err(error) => {
                log::warn!("direct Android export unavailable, falling back to cache: {error}");
                self.start_export(export_dir.join(display_name), frame, format);
            }
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn suspend_android_preview_for_export(
        &mut self,
        frame: &eframe::Frame,
        restore_after_export: bool,
    ) -> Result<(), String> {
        let Some(render_state) = frame.wgpu_render_state() else {
            return Err("eframe is not running with the wgpu backend.".to_owned());
        };

        // Mobile export and preview pipelines cannot coexist within AuRaw's
        // conservative GPU residency budget on high-resolution RAWs. Retire every
        // preview texture for cleanup at the start of the next frame, then drop the
        // GPU pipeline only after releasing the renderer lock. Keeping the destructor
        // out of the renderer critical section also avoids lock-order inversions.
        let previous_pipeline = {
            let mut renderer = render_state.renderer.write();
            self.take_preview_pipeline_and_release_textures(&mut renderer)
        };
        drop(previous_pipeline);

        self.pending_stage = None;
        self.preview_detail_pending_stage = None;
        self.navigation_pending_stage = None;
        self.preview_detail_urgent = false;
        if restore_after_export {
            self.preview_quality_dirty = true;
        }
        Ok(())
    }

    pub(in crate::app) fn capture_export_task_request(
        &mut self,
        path: PathBuf,
        frame: &eframe::Frame,
        format: ExportFormat,
    ) -> Option<ExportTaskRequest> {
        if !self.can_export() {
            return None;
        }

        let raw = self.loaded_raw.as_ref().map(Arc::clone)?;
        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            return None;
        };
        let source_file_name = self
            .current_path
            .as_ref()
            .and_then(|source| source.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .or_else(|| self.current_label.clone());
        let display_name = source_file_name
            .as_deref()
            .and_then(|name| std::path::Path::new(name).file_stem())
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or("image")
            .to_owned();

        Some(ExportTaskRequest {
            device: render_state.device.clone(),
            queue: render_state.queue.clone(),
            metadata: ExportMetadata::from_raw(&raw, source_file_name),
            raw,
            geometry: self.geometry,
            exposure: self.exposure,
            masks: self.masks.clone(),
            inpaint: self.inpaint_layer.clone(),
            path,
            format,
            settings: self.export_settings.clone(),
            display_name,
            gpu_export_prewarm: self.gpu_export_prewarm.as_ref().map(Arc::clone),
        })
    }

    pub(in crate::app) fn start_export(
        &mut self,
        path: PathBuf,
        frame: &eframe::Frame,
        format: ExportFormat,
    ) -> Option<TaskId> {
        let request = self.capture_export_task_request(path, frame, format)?;
        let display_name = request.display_name.clone();
        let task_id = self.enqueue_background_action(
            TaskKind::SingleExport,
            format!("Exporting {display_name}"),
            TaskProgress::indeterminate("Waiting for earlier background work…"),
            true,
            BackgroundAction::SingleExport(Box::new(request)),
        );
        Some(task_id)
    }

    pub(in crate::app) fn poll_export_worker(&mut self, _frame: &eframe::Frame) {
        let (events, disconnected) =
            drain_worker_events(self.export_receiver.as_ref(), |_| false);

        let mut finished = false;
        #[cfg(target_os = "android")]
        let mut android_batch_result: Option<Result<(), String>> = None;
        for event in events {
            match event {
                ExportEvent::Progress {
                    completed_tiles,
                    total_tiles,
                } => {
                    self.export_progress = Some((completed_tiles, total_tiles));
                    if self.library_batch_export.is_some() {
                        self.sync_library_batch_background_progress();
                    } else if let Some(id) = self.export_task_id {
                        let progress = if total_tiles == 0 {
                            TaskProgress::indeterminate("Preparing tiled export…")
                        } else {
                            // Tile completion covers rendering/readback only. Keep
                            // the final encoding, metadata, publication, and rename
                            // phase below 100% until the worker reports Finished.
                            let tile_fraction =
                                (completed_tiles as f32 / total_tiles as f32).clamp(0.0, 1.0);
                            let fraction = (tile_fraction * EXPORT_TILE_PHASE_WEIGHT)
                                .min(EXPORT_MAX_INCOMPLETE_FRACTION);
                            let phase = if completed_tiles >= total_tiles {
                                "Finalizing export…".to_owned()
                            } else {
                                format!("Rendering tile {completed_tiles}/{total_tiles}")
                            };
                            TaskProgress::fraction(fraction, phase)
                                .with_detail(format!("{completed_tiles}/{total_tiles} tiles"))
                        };
                        self.update_background_progress(Some(id), progress);
                    }
                },
                ExportEvent::Finished(result) => {
                    finished = true;
                    self.export_progress = None;

                    match result {
                        Ok(path) => {
                            #[cfg(not(target_os = "android"))]
                            {
                                self.notice = Some(format!("Exported {}", path.display()));
                                if self.library_batch_export.is_none() {
                                    if let Some(id) = self.export_task_id.take() {
                                        self.finish_background_task(id);
                                    }
                                }
                            }

                            #[cfg(target_os = "android")]
                            {
                                if crate::android::is_direct_export_path(&path) {
                                    match crate::android::finalize_direct_export(
                                        &self.android_app,
                                        &path,
                                    ) {
                                        Ok(location) => {
                                            if self.library_batch_export.is_some() {
                                                android_batch_result = Some(Ok(()));
                                            } else {
                                                self.notice = Some(format!("Exported to {location}"));
                                                if let Some(id) = self.export_task_id.take() {
                                                    self.finish_background_task(id);
                                                }
                                            }
                                        }
                                        Err(error) => {
                                            if self.library_batch_export.is_some() {
                                                android_batch_result = Some(Err(error.clone()));
                                            } else if let Some(id) = self.export_task_id.take() {
                                                self.fail_background_task(id, error.clone());
                                            }
                                            self.notice = Some(format!("Export failed: {error}"));
                                            log::error!("Android direct export finalize failed: {error}");
                                        }
                                    }
                                } else {
                                    let format = match path
                                        .extension()
                                        .and_then(|extension| extension.to_str())
                                    {
                                        Some(extension)
                                            if extension.eq_ignore_ascii_case("jpg")
                                                || extension.eq_ignore_ascii_case("jpeg") =>
                                        {
                                            ExportFormat::Jpeg
                                        }
                                        Some(extension)
                                            if extension.eq_ignore_ascii_case("tif")
                                                || extension.eq_ignore_ascii_case("tiff") =>
                                        {
                                            ExportFormat::Tiff
                                        }
                                        _ => ExportFormat::Png,
                                    };
                                    let fallback_name =
                                        format!("AuRaw-export.{}", format.extension());
                                    let display_name = self
                                        .library_batch_export
                                        .as_ref()
                                        .and_then(|batch| batch.current.as_ref())
                                        .map(|job| {
                                            let stem = std::path::Path::new(
                                                job.target.display_name(),
                                            )
                                                .file_stem()
                                                .and_then(|stem| stem.to_str())
                                                .filter(|stem| !stem.is_empty())
                                                .unwrap_or("AuRaw-export");
                                            format!("{stem}-auraw.{}", format.extension())
                                        })
                                        .or_else(|| {
                                            path.file_name()
                                                .and_then(|name| name.to_str())
                                                .map(str::to_owned)
                                        })
                                        .unwrap_or(fallback_name);
                                    match crate::android::publish_image(
                                        &self.android_app,
                                        &path,
                                        &display_name,
                                        format.mime_type(),
                                    ) {
                                        Ok(()) => {
                                            self.export_publish_pending = true;
                                            self.notice =
                                                Some("Saving to Pictures/AuRaw…".to_owned());
                                            self.update_background_progress(
                                                self.export_task_id,
                                                TaskProgress::indeterminate(
                                                    "Publishing to Pictures/AuRaw…",
                                                ),
                                            );
                                        }
                                        Err(error) => {
                                            let _ = std::fs::remove_file(&path);
                                            if self.library_batch_export.is_some() {
                                                android_batch_result = Some(Err(error.clone()));
                                            } else if let Some(id) = self.export_task_id.take() {
                                                self.fail_background_task(id, error.clone());
                                            }
                                            self.notice = Some(format!("Export failed: {error}"));
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            let was_cancelled = self.library_batch_export.is_none()
                                && self
                                    .export_task_id
                                    .is_some_and(|id| self.background_task_cancelled(id));
                            #[cfg(target_os = "android")]
                            {
                                crate::android::cancel_all_direct_exports(&self.android_app);
                                if self.library_batch_export.is_some() {
                                    android_batch_result = Some(Err(error.clone()));
                                } else if let Some(id) = self.export_task_id.take() {
                                    if was_cancelled {
                                        self.finish_background_task(id);
                                    } else {
                                        self.fail_background_task(id, error.clone());
                                    }
                                }
                            }
                            #[cfg(not(target_os = "android"))]
                            if self.library_batch_export.is_none() {
                                if let Some(id) = self.export_task_id.take() {
                                    if was_cancelled {
                                        self.finish_background_task(id);
                                    } else {
                                        self.fail_background_task(id, error.clone());
                                    }
                                }
                            }
                            if was_cancelled {
                                self.notice = Some("Export cancelled.".to_owned());
                                log::info!("export cancelled");
                            } else {
                                self.notice = Some(format!("Export failed: {error}"));
                                log::error!("export failed: {error}");
                            }
                        }
                    }
                }
            }
        }

        if finished || disconnected {
            self.export_receiver = None;
            if disconnected && self.notice.is_none() {
                self.export_progress = None;
                self.notice = Some("Export worker stopped unexpectedly.".to_owned());
            }
            if disconnected && self.library_batch_export.is_none() {
                if let Some(id) = self.export_task_id.take() {
                    self.fail_background_task(id, "Export worker stopped unexpectedly.");
                }
            }
        }

        #[cfg(target_os = "android")]
        if let Some(result) = android_batch_result {
            self.complete_android_library_batch_export_item(result);
        } else if disconnected && self.library_batch_export.is_some() {
            crate::android::cancel_all_direct_exports(&self.android_app);
            self.complete_android_library_batch_export_item(Err(
                "export worker stopped unexpectedly".to_owned(),
            ));
        } else if disconnected {
            crate::android::cancel_all_direct_exports(&self.android_app);
        }
    }

    pub(in crate::app) fn refresh_status(&mut self) {
        self.status = if let Some(label) = &self.loading_label {
            format!("Decoding and preparing proxy for {label}…")
        } else if self.lens_correction_busy() {
            self.lens_correction.catalog.status.clone()
        } else if let Some((completed, total)) = self.export_progress {
            if total == 0 {
                "Preparing tiled export…".to_owned()
            } else {
                format!("Exporting image — tile {completed}/{total}")
            }
        } else if self.export_publish_pending {
            "Saving to Pictures/AuRaw…".to_owned()
        } else if self.preview_zoom > DETAIL_ZOOM_START {
            if let Some(stage) = self.preview_detail_pending_stage {
                format!("Updating visible zoom crop — {}…", stage.label())
            } else if let Some(notice) = &self.notice {
                notice.clone()
            } else {
                self.image_status.clone()
            }
        } else if let Some(stage) = self.pending_stage {
            format!("Updating preview — {}…", stage.label())
        } else if let Some(notice) = &self.notice {
            notice.clone()
        } else {
            self.image_status.clone()
        };
    }

    pub(crate) fn reset_develop_adjustments(&mut self) {
        let previous = self.exposure;
        self.exposure = ExposureParams::scene_referred_default();
        self.white_balance_picker_active = false;
        self.white_balance_picker_drag = None;

        // Highlight reconstruction is an application-level processing preference,
        // not a Develop adjustment.
        self.exposure.highlight_method = previous.highlight_method;
        self.exposure.highlight_clip = previous.highlight_clip;
        self.exposure.highlight_reconstruction = previous.highlight_reconstruction;

        // Demosaic selection is likewise a raw-processing preference rather
        // than a Develop adjustment. Resetting exposure/tone controls must not
        // silently change the reconstruction algorithm.
        self.exposure.demosaic_mode = previous.demosaic_mode;
        self.exposure.dual_threshold = previous.dual_threshold;
        self.exposure.frequency_chroma = previous.frequency_chroma;

        self.mark_pipeline_dirty();
    }

    pub(crate) fn reset_highlight_reconstruction_settings(&mut self) {
        let defaults = ExposureParams::default();
        self.exposure.highlight_method = defaults.highlight_method;
        self.exposure.highlight_clip = defaults.highlight_clip;
        self.exposure.highlight_reconstruction = defaults.highlight_reconstruction;
        self.mark_pipeline_dirty();
    }
}
