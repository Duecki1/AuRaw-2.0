use super::*;

use super::batch::batch_export_overall_fraction;
use super::preview::DETAIL_ZOOM_START;

impl ExportTask {
    pub(super) fn new(
        kind: ExportTaskKind,
        cancellation: Arc<std::sync::atomic::AtomicBool>,
        receiver: Option<ExportTaskReceiver>,
        destination: Option<ExportDestination>,
        total: usize,
    ) -> Self {
        Self {
            kind,
            cancellation,
            receiver,
            destination,
            progress: 0.0,
            phase: if kind == ExportTaskKind::LibraryBatch {
                "Preparing batch export…".to_owned()
            } else {
                "Preparing tiled export…".to_owned()
            },
            completed: 0,
            total,
            completed_tiles: 0,
            total_tiles: 0,
            minimized: false,
            cancelling: false,
        }
    }

    fn start_item(
        &mut self,
        receiver: mpsc::Receiver<ExportEvent>,
        destination: ExportDestination,
    ) {
        self.receiver = Some(ExportTaskReceiver::Tiled(receiver));
        self.destination = Some(destination);
        self.completed_tiles = 0;
        self.total_tiles = 0;
        self.phase = "Preparing tiled export…".to_owned();
    }

    pub(super) fn minimize(&mut self) {
        self.minimized = true;
    }

    pub(super) fn restore(&mut self) {
        self.minimized = false;
    }

    pub(super) fn request_cancel(&mut self) {
        use std::sync::atomic::Ordering;
        self.cancellation.store(true, Ordering::Release);
        self.cancelling = true;
        self.phase = if self.kind == ExportTaskKind::LibraryBatch {
            "Cancelling batch export…".to_owned()
        } else {
            "Cancelling export…".to_owned()
        };
    }
}

pub(super) fn clear_export_task(slot: &mut Option<ExportTask>) {
    *slot = None;
}

pub(in crate::app) fn export_source_stem(
    current_path: Option<&std::path::Path>,
    current_label: Option<&str>,
) -> String {
    current_label
        .and_then(|label| std::path::Path::new(label).file_stem())
        .and_then(std::ffi::OsStr::to_str)
        .or_else(|| {
            current_path
                .and_then(std::path::Path::file_stem)
                .and_then(std::ffi::OsStr::to_str)
        })
        .filter(|stem| !stem.is_empty())
        .unwrap_or("calibraw-export")
        .to_owned()
}

pub(in crate::app) fn spawn_export_item(
    request: ExportItemRequest,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
) -> mpsc::Receiver<ExportEvent> {
    let ExportItemRequest {
        device,
        queue,
        source,
        destination,
        format,
        settings,
    } = request;
    let PreparedExportSource {
        raw,
        geometry,
        exposure,
        masks,
        remove,
        source_file_name,
        gpu_export_prewarm,
    } = source;
    let metadata = ExportMetadata::from_raw(&raw, source_file_name);
    let path = destination.path().to_path_buf();
    spawn_tiled_export(
        format,
        TiledExportJob {
            device,
            queue,
            raw,
            geometry,
            exposure,
            masks,
            remove,
            path,
            tile_spec: TileSpec::default(),
            settings,
            metadata,
            cancellation,
            program_prewarm: gpu_export_prewarm,
        },
    )
}

#[cfg(not(target_os = "android"))]
pub(in crate::app) fn run_export_item(
    request: ExportItemRequest,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
    mut progress: impl FnMut(usize, usize),
) -> Result<(), String> {
    let receiver = spawn_export_item(request, cancellation);
    while let Ok(event) = receiver.recv() {
        match event {
            ExportEvent::Progress {
                completed_tiles,
                total_tiles,
            } => progress(completed_tiles, total_tiles),
            ExportEvent::Finished(result) => return result.map(|_| ()),
        }
    }
    Err("export worker stopped unexpectedly".to_owned())
}

impl CalibRawApp {
    pub(crate) fn export_task_active(&self) -> bool {
        self.export.task.is_some()
    }

    pub(crate) fn can_export(&self) -> bool {
        self.develop.loaded_raw.is_some()
            && self.develop.preview_raw.is_some()
            && self.export.task.is_none()
            && !self.export.publish_pending
            && self.develop.load_receiver.is_none()
    }

    fn templated_export_stem(&self) -> Option<String> {
        let raw = self.develop.loaded_raw.as_deref()?;
        let original_name = export_source_stem(
            self.develop.current_path.as_deref(),
            self.develop.current_label.as_deref(),
        );
        #[cfg(not(target_os = "android"))]
        let edited_at = self
            .develop
            .current_path
            .as_deref()
            .and_then(crate::export_naming::edited_time_for_path);
        #[cfg(target_os = "android")]
        let edited_at = None;
        let context =
            crate::export_naming::ExportNameContext::from_raw(original_name, raw, edited_at);
        Some(crate::export_naming::render_export_stem_or_default(
            &self.preferences.export_name_template,
            &context,
        ))
    }

    #[cfg(not(target_os = "android"))]
    fn export_desktop(&mut self, frame: &eframe::Frame, format: ExportFormat) {
        if !self.can_export() {
            return;
        }

        let Some(stem) = self.templated_export_stem() else {
            return;
        };
        let default_name = format!("{stem}.{}", format.extension());
        let initial_directory = self
            .develop
            .current_path
            .as_deref()
            .and_then(|path| path.parent());
        let Some(path) =
            crate::ui::choose_export_file_path(format, &default_name, initial_directory)
        else {
            return;
        };
        self.start_export(path, frame, format);
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn export_png(&mut self, frame: &eframe::Frame) {
        self.export_desktop(frame, ExportFormat::Png);
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn export_jpeg(&mut self, frame: &eframe::Frame) {
        self.export_desktop(frame, ExportFormat::Jpeg);
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn export_tiff(&mut self, frame: &eframe::Frame) {
        self.export_desktop(frame, ExportFormat::Tiff);
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

        let Some(stem) = self.templated_export_stem() else {
            return;
        };
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let display_name = format!("{stem}.{}", format.extension());
        let cache_file_name = format!("{stem}-{timestamp}.{}", format.extension());
        let destination = match self.prepare_android_export_destination(
            display_name.clone(),
            cache_file_name,
            format,
        ) {
            Ok(destination) => destination,
            Err(error) => {
                self.ui.notice = Some(error);
                return;
            }
        };
        let cleanup = destination.clone();
        if self
            .start_export_destination(destination, frame, format)
            .is_none()
        {
            self.cancel_android_export_destination(&cleanup);
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn prepare_android_export_destination(
        &self,
        display_name: String,
        cache_file_name: String,
        format: ExportFormat,
    ) -> Result<ExportDestination, String> {
        let Some(data_dir) = self.android.android_app.internal_data_path() else {
            return Err("Android did not provide an app data directory.".to_owned());
        };
        let export_dir = data_dir.join("cache").join("exports");
        std::fs::create_dir_all(&export_dir)
            .map_err(|error| format!("Could not prepare Android export cache: {error}"))?;
        match crate::android::prepare_direct_export(
            &self.android.android_app,
            &export_dir,
            &display_name,
            format.mime_type(),
        ) {
            Ok(Some(path)) => Ok(ExportDestination::AndroidDirect { path }),
            Ok(None) => Ok(ExportDestination::AndroidGallery {
                path: export_dir.join(cache_file_name),
                display_name,
                format,
            }),
            Err(error) => {
                log::warn!("direct Android export unavailable, falling back to cache: {error}");
                Ok(ExportDestination::AndroidGallery {
                    path: export_dir.join(cache_file_name),
                    display_name,
                    format,
                })
            }
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn cancel_android_export_destination(
        &self,
        destination: &ExportDestination,
    ) {
        if let ExportDestination::AndroidDirect { path } = destination {
            crate::android::cancel_direct_export(&self.android.android_app, path);
        }
    }

    pub(in crate::app) fn capture_export_task_request(
        &mut self,
        destination: ExportDestination,
        frame: &eframe::Frame,
        format: ExportFormat,
    ) -> Option<ExportItemRequest> {
        if self.develop.loaded_raw.is_none()
            || self.develop.preview_raw.is_none()
            || self.export.publish_pending
            || self.develop.load_receiver.is_some()
            || self.inpaint.processing()
        {
            return None;
        }

        let raw = self.develop.loaded_raw.as_ref().map(Arc::clone)?;
        let Some(render_state) = frame.wgpu_render_state() else {
            self.ui.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            return None;
        };
        let source_file_name = self
            .develop
            .current_path
            .as_ref()
            .and_then(|source| source.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .or_else(|| self.develop.current_label.clone());
        Some(ExportItemRequest {
            device: render_state.device.clone(),
            queue: render_state.queue.clone(),
            source: PreparedExportSource {
                raw,
                geometry: self.develop.geometry,
                exposure: self.develop.exposure,
                masks: self.masks.stack.clone(),
                remove: self.inpaint.edits.as_ref().clone(),
                source_file_name,
                gpu_export_prewarm: self.export.gpu_prewarm.as_ref().map(Arc::clone),
            },
            destination,
            format,
            settings: self.export.settings.clone(),
        })
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn start_export(
        &mut self,
        path: PathBuf,
        frame: &eframe::Frame,
        format: ExportFormat,
    ) -> Option<()> {
        self.start_export_destination(ExportDestination::File(path), frame, format)
    }

    fn start_export_destination(
        &mut self,
        destination: ExportDestination,
        frame: &eframe::Frame,
        format: ExportFormat,
    ) -> Option<()> {
        if !self.can_export() {
            return None;
        }
        let request = self.capture_export_task_request(destination, frame, format)?;
        if let Err(error) = self.start_export_task(request, ExportTaskKind::Single) {
            self.ui.notice = Some(format!("Export failed: {error}"));
            return None;
        }
        Some(())
    }

    pub(in crate::app) fn start_export_task(
        &mut self,
        request: ExportItemRequest,
        kind: ExportTaskKind,
    ) -> Result<(), String> {
        let cancellation = match (kind, self.export.task.as_ref()) {
            (ExportTaskKind::Single, None) => Arc::new(std::sync::atomic::AtomicBool::new(false)),
            (ExportTaskKind::Single, Some(_)) => {
                return Err("another export is already active".to_owned());
            }
            (ExportTaskKind::LibraryBatch, Some(task))
                if task.kind == ExportTaskKind::LibraryBatch =>
            {
                Arc::clone(&task.cancellation)
            }
            (ExportTaskKind::LibraryBatch, _) => {
                return Err("the library batch export is no longer active".to_owned());
            }
        };
        let destination = request.destination.clone();
        let receiver = spawn_export_item(request, Arc::clone(&cancellation));
        if kind == ExportTaskKind::Single {
            self.export.task = Some(ExportTask::new(
                kind,
                cancellation,
                Some(ExportTaskReceiver::Tiled(receiver)),
                Some(destination),
                1,
            ));
        } else if let Some(task) = self.export.task.as_mut() {
            task.start_item(receiver, destination);
        }
        self.ui.notice = None;
        self.egui_ctx.request_repaint();
        Ok(())
    }

    pub(crate) fn cancel_export_task(&mut self) -> bool {
        let Some(task) = self.export.task.as_mut() else {
            return false;
        };
        task.request_cancel();
        if let Some(batch) = self.export.batch.as_mut() {
            batch.cancel_requested = true;
            batch.pending.clear();
        }
        self.egui_ctx.request_repaint();
        true
    }

    pub(crate) fn minimize_export_task(&mut self) {
        if let Some(task) = self.export.task.as_mut() {
            task.minimize();
            self.egui_ctx.request_repaint();
        }
    }

    pub(crate) fn restore_export_task(&mut self) {
        if let Some(task) = self.export.task.as_mut() {
            task.restore();
            self.egui_ctx.request_repaint();
        }
    }

    pub(crate) fn show_export_task_indicator(&mut self, ui: &mut egui::Ui) {
        let Some(task) = self.export.task.as_ref() else {
            return;
        };
        if !task.minimized {
            return;
        }
        let label = if task.total > 1 {
            format!(
                "Exporting {} / {}",
                task.completed.min(task.total),
                task.total
            )
        } else {
            format!("Exporting {:.0}%", task.progress.clamp(0.0, 1.0) * 100.0)
        };
        if ui
            .small_button(label)
            .on_hover_text("Show export progress")
            .clicked()
        {
            self.restore_export_task();
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn sync_android_export_notification(&self) {
        let Some(task) = self.export.task.as_ref() else {
            if let Err(error) =
                crate::android::clear_background_task_notification(&self.android.android_app)
            {
                log::warn!("{error}");
            }
            return;
        };
        let title = if task.kind == ExportTaskKind::LibraryBatch {
            "CalibRaw batch export"
        } else {
            "CalibRaw export"
        };
        let detail = (task.total > 1).then(|| {
            format!(
                "{} / {} images complete",
                task.completed.min(task.total),
                task.total
            )
        });
        let percent = (task.progress.clamp(0.0, 1.0) * 100.0).round() as i32;
        if let Err(error) = crate::android::update_background_task_notification(
            &self.android.android_app,
            title,
            &task.phase,
            detail.as_deref(),
            percent,
            task.total_tiles == 0 && task.progress <= 0.0,
            0,
        ) {
            log::warn!("{error}");
        }
    }

    pub(crate) fn show_export_task_dialog(&mut self, ctx: &egui::Context) {
        let Some(task) = self.export.task.as_ref() else {
            return;
        };
        if task.minimized {
            return;
        }
        let progress = task.progress.clamp(0.0, 1.0);
        let phase = task.phase.clone();
        let completed = task.completed;
        let total = task.total;
        let cancelling = task.cancelling;
        let mut minimize = false;
        let mut cancel = false;
        crate::ui::responsive_popup(egui::Window::new("Exporting"), ctx, 430.0)
            .id(egui::Id::new("active-export-progress"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                if total > 1 {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} / {} images complete",
                            completed.min(total),
                            total
                        ))
                        .strong(),
                    );
                } else {
                    ui.label(egui::RichText::new("Exporting image").strong());
                }
                ui.label(&phase);
                ui.add_space(6.0);
                ui.add(
                    egui::ProgressBar::new(progress)
                        .show_percentage()
                        .animate(!cancelling),
                );
                if cancelling {
                    ui.label(
                        egui::RichText::new("Stopping at the next safe point…")
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Minimize").clicked() {
                        minimize = true;
                    }
                    if ui
                        .add_enabled(!cancelling, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            });
        if minimize {
            self.minimize_export_task();
        }
        if cancel {
            self.cancel_export_task();
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }

    pub(in crate::app) fn poll_export_worker(&mut self, _frame: &eframe::Frame) {
        let (events, disconnected) = match self
            .export
            .task
            .as_ref()
            .and_then(|task| task.receiver.as_ref())
        {
            Some(ExportTaskReceiver::Tiled(receiver)) => {
                drain_worker_events(Some(receiver), |event| {
                    matches!(event, ExportEvent::Finished(_))
                })
            }
            _ => return,
        };

        let mut finished = false;
        #[cfg(target_os = "android")]
        let mut android_batch_result: Option<Result<(), String>> = None;
        for event in events {
            match event {
                ExportEvent::Progress {
                    completed_tiles,
                    total_tiles,
                } => {
                    let batch_current = self
                        .export
                        .batch
                        .as_ref()
                        .is_some_and(|batch| batch.current.is_some());
                    if let Some(task) = self.export.task.as_mut() {
                        task.completed_tiles = completed_tiles;
                        task.total_tiles = total_tiles;
                        let tile_fraction = if total_tiles == 0 {
                            0.0
                        } else {
                            (completed_tiles as f32 / total_tiles as f32).clamp(0.0, 1.0)
                        };
                        if task.kind == ExportTaskKind::LibraryBatch {
                            task.progress = batch_export_overall_fraction(
                                task.completed,
                                task.total,
                                batch_current,
                                Some((completed_tiles, total_tiles)),
                            );
                        } else {
                            task.progress = (tile_fraction * EXPORT_TILE_PHASE_WEIGHT)
                                .min(EXPORT_MAX_INCOMPLETE_FRACTION);
                        }
                        task.phase = if total_tiles == 0 {
                            "Preparing tiled export…".to_owned()
                        } else if completed_tiles >= total_tiles {
                            "Finalizing export…".to_owned()
                        } else {
                            format!("Rendering tile {completed_tiles}/{total_tiles}")
                        };
                    }
                }
                ExportEvent::Finished(result) => {
                    finished = true;
                    if let Some(task) = self.export.task.as_mut() {
                        task.receiver = None;
                        task.completed_tiles = 0;
                        task.total_tiles = 0;
                        task.progress = task.progress.min(EXPORT_MAX_INCOMPLETE_FRACTION);
                        task.phase = "Finalizing export…".to_owned();
                    }
                    let is_batch = self.export.batch.is_some();
                    let was_cancelled = self
                        .export
                        .task
                        .as_ref()
                        .is_some_and(|task| task.cancelling);

                    match result {
                        Ok(path) => {
                            #[cfg(not(target_os = "android"))]
                            {
                                if !is_batch {
                                    self.ui.notice = Some(format!("Exported {}", path.display()));
                                    clear_export_task(&mut self.export.task);
                                }
                            }

                            #[cfg(target_os = "android")]
                            {
                                let destination = self
                                    .export
                                    .task
                                    .as_ref()
                                    .and_then(|task| task.destination.clone());
                                match destination {
                                    Some(ExportDestination::AndroidDirect {
                                        path: direct_path,
                                    }) => {
                                        debug_assert_eq!(path, direct_path);
                                        match crate::android::finalize_direct_export(
                                            &self.android.android_app,
                                            &direct_path,
                                        ) {
                                            Ok(location) => {
                                                if is_batch {
                                                    android_batch_result = Some(Ok(()));
                                                } else {
                                                    self.ui.notice =
                                                        Some(format!("Exported to {location}"));
                                                    clear_export_task(&mut self.export.task);
                                                }
                                            }
                                            Err(error) => {
                                                if is_batch {
                                                    android_batch_result = Some(Err(error.clone()));
                                                } else {
                                                    clear_export_task(&mut self.export.task);
                                                }
                                                self.ui.notice =
                                                    Some(format!("Export failed: {error}"));
                                                log::error!("Android direct export finalize failed: {error}");
                                            }
                                        }
                                    }
                                    Some(ExportDestination::AndroidGallery {
                                        path: cache_path,
                                        display_name,
                                        format,
                                    }) => {
                                        debug_assert_eq!(path, cache_path);
                                        match crate::android::publish_image(
                                            &self.android.android_app,
                                            &cache_path,
                                            &display_name,
                                            format.mime_type(),
                                        ) {
                                            Ok(()) => {
                                                self.export.publish_pending = true;
                                                self.ui.notice =
                                                    Some("Saving to Pictures/CalibRaw…".to_owned());
                                                if let Some(task) = self.export.task.as_mut() {
                                                    task.phase = "Publishing to Pictures/CalibRaw…"
                                                        .to_owned();
                                                    task.progress = task
                                                        .progress
                                                        .max(EXPORT_MAX_INCOMPLETE_FRACTION);
                                                }
                                            }
                                            Err(error) => {
                                                let _ = std::fs::remove_file(&cache_path);
                                                if is_batch {
                                                    android_batch_result = Some(Err(error.clone()));
                                                } else {
                                                    clear_export_task(&mut self.export.task);
                                                }
                                                self.ui.notice =
                                                    Some(format!("Export failed: {error}"));
                                            }
                                        }
                                    }
                                    None => {
                                        let error = "export destination state was lost".to_owned();
                                        if is_batch {
                                            android_batch_result = Some(Err(error.clone()));
                                        } else {
                                            clear_export_task(&mut self.export.task);
                                        }
                                        self.ui.notice = Some(format!("Export failed: {error}"));
                                        log::error!("Android export finalization failed: {error}");
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            #[cfg(target_os = "android")]
                            {
                                crate::android::cancel_all_direct_exports(
                                    &self.android.android_app,
                                );
                                if is_batch {
                                    android_batch_result = Some(Err(error.clone()));
                                } else {
                                    clear_export_task(&mut self.export.task);
                                }
                            }
                            #[cfg(not(target_os = "android"))]
                            if !is_batch {
                                clear_export_task(&mut self.export.task);
                            }
                            if was_cancelled {
                                self.ui.notice = Some("Export cancelled.".to_owned());
                                log::info!("export cancelled");
                            } else {
                                self.ui.notice = Some(format!("Export failed: {error}"));
                                log::error!("export failed: {error}");
                            }
                        }
                    }
                }
            }
        }

        if disconnected && !finished {
            if let Some(task) = self.export.task.as_mut() {
                task.receiver = None;
            }
            self.ui.notice = Some("Export worker stopped unexpectedly.".to_owned());
            #[cfg(target_os = "android")]
            {
                crate::android::cancel_all_direct_exports(&self.android.android_app);
                if self.export.batch.is_some() {
                    android_batch_result =
                        Some(Err("export worker stopped unexpectedly".to_owned()));
                } else {
                    clear_export_task(&mut self.export.task);
                }
            }
            #[cfg(not(target_os = "android"))]
            if self.export.batch.is_none() {
                clear_export_task(&mut self.export.task);
            }
        }

        #[cfg(target_os = "android")]
        if let Some(result) = android_batch_result {
            self.complete_android_library_batch_export_item(result);
        }
    }

    pub(in crate::app) fn refresh_status(&mut self) {
        self.ui.status = if let Some(label) = &self.develop.loading_label {
            format!("Decoding and preparing proxy for {label}…")
        } else if self.lens_correction_busy() {
            self.develop.lens_correction.catalog.status.clone()
        } else if let Some(task) = self.export.task.as_ref() {
            task.phase.clone()
        } else if self.export.publish_pending {
            "Saving to Pictures/CalibRaw…".to_owned()
        } else if self.preview.zoom > DETAIL_ZOOM_START {
            if let Some(stage) = self.preview.detail_pending_stage {
                format!("Updating visible zoom crop — {}…", stage.label())
            } else if let Some(notice) = &self.ui.notice {
                notice.clone()
            } else {
                self.develop.image_status.clone()
            }
        } else if let Some(stage) = self.preview.pending_stage {
            format!("Updating preview — {}…", stage.label())
        } else if let Some(notice) = &self.ui.notice {
            notice.clone()
        } else {
            self.develop.image_status.clone()
        };
    }

    pub(crate) fn reset_develop_adjustments(&mut self) {
        self.develop.exposure = ExposureParams::scene_referred_default();
        self.develop_ui.white_balance_picker_active = false;
        self.develop_ui.white_balance_picker_drag = None;
        self.mark_pipeline_dirty();
    }
}
