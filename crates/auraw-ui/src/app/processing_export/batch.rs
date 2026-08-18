use super::*;

#[cfg(not(target_os = "android"))]
use super::export::run_export_item;

pub(in crate::app) fn batch_export_overall_fraction(
    completed: usize,
    total: usize,
    has_current: bool,
    tile_progress: Option<(usize, usize)>,
) -> f32 {
    if total == 0 {
        return 0.0;
    }

    let completed = completed.min(total);
    if completed == total {
        return 1.0;
    }

    // Tile completion only covers rendering/readback. Encoding, metadata writing,
    // publication, and final rename still have to finish before the image counts.
    let current_fraction = if has_current {
        tile_progress
            .and_then(|(tiles_done, tiles_total)| {
                (tiles_total > 0).then(|| {
                    (tiles_done as f32 / tiles_total as f32).clamp(0.0, 1.0)
                })
            })
            .unwrap_or(0.0)
            * EXPORT_TILE_PHASE_WEIGHT
    } else {
        0.0
    };

    ((completed as f32 + current_fraction) / total as f32)
        .clamp(0.0, EXPORT_MAX_INCOMPLETE_FRACTION)
}

#[cfg(not(target_os = "android"))]
struct DesktopLibraryBatchExportRequest {
    jobs: VecDeque<LibraryBatchExportJob>,
    context: DesktopLibraryExportContext,
    repaint: egui::Context,
}

#[cfg(not(target_os = "android"))]
struct DesktopLibraryExportContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: ExportFormat,
    settings: ExportSettings,
    camera_profile_mode: CameraProfileMode,
    camera_profile_folder: Option<PathBuf>,
    last_camera_profile: Option<PathBuf>,
    default_exposure: ExposureParams,
    decode_gate: Arc<std::sync::RwLock<()>>,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(not(target_os = "android"))]
pub(in crate::app) fn spawn_desktop_library_batch_export(
    request: DesktopLibraryBatchExportRequest,
) -> mpsc::Receiver<LibraryBatchExportEvent> {
    let DesktopLibraryBatchExportRequest {
        jobs,
        context,
        repaint,
    } = request;
    use std::sync::atomic::Ordering;

    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let spawn_result = std::thread::Builder::new()
        .name("auraw-library-batch-export".to_owned())
        .spawn(move || {
            for job in jobs {
                if context.cancellation.load(Ordering::Acquire) {
                    break;
                }
                let _ = worker_sender.send(LibraryBatchExportEvent::Started { job: job.clone() });
                repaint.request_repaint();

                let request = prepare_desktop_library_export_item(&job, &context);

                if context.cancellation.load(Ordering::Acquire) {
                    break;
                }

                let request = match request {
                    Ok(request) => request,
                    Err(error) => {
                        let _ = worker_sender.send(LibraryBatchExportEvent::ItemFinished {
                            error: Some(error),
                        });
                        repaint.request_repaint();
                        continue;
                    }
                };

                let item_result = run_export_item(
                    request,
                    Arc::clone(&context.cancellation),
                    |completed_tiles, total_tiles| {
                        let _ = worker_sender.send(LibraryBatchExportEvent::Progress {
                            completed_tiles,
                            total_tiles,
                        });
                        repaint.request_repaint();
                    },
                );

                let cancelled = context.cancellation.load(Ordering::Acquire);
                if !cancelled || item_result.is_ok() {
                    // A cancellation request can arrive just after the current
                    // image was published. Count that image, but do not report a
                    // cooperative cancellation result as an export failure.
                    let error = (!cancelled)
                        .then(|| item_result.err())
                        .flatten()
                        .map(|error| format!("{}: {error}", job.source.display()));
                    let _ = worker_sender.send(LibraryBatchExportEvent::ItemFinished { error });
                    repaint.request_repaint();
                }
                if cancelled {
                    break;
                }
            }

            let _ = worker_sender.send(LibraryBatchExportEvent::Finished {
                cancelled: context.cancellation.load(Ordering::Acquire),
                error: None,
            });
            repaint.request_repaint();
        });

    if let Err(error) = spawn_result {
        let _ = sender.send(LibraryBatchExportEvent::Finished {
            cancelled: false,
            error: Some(format!("could not start batch export worker: {error}")),
        });
    }
    receiver
}

#[cfg(not(target_os = "android"))]
fn prepare_desktop_library_export_item(
    job: &LibraryBatchExportJob,
    context: &DesktopLibraryExportContext,
) -> Result<ExportItemRequest, String> {
    let device = &context.device;
    let queue = &context.queue;
    let format = context.format;
    let settings = &context.settings;
    let camera_profile_mode = context.camera_profile_mode;
    let camera_profile_folder = context.camera_profile_folder.as_deref();
    let last_camera_profile = context.last_camera_profile.as_deref();
    let default_exposure = context.default_exposure;
    let decode_gate = &context.decode_gate;
    let cancellation = &context.cancellation;
    use std::sync::atomic::Ordering;

    if cancellation.load(Ordering::Acquire) {
        return Err("batch export cancelled".to_owned());
    }

    let (mut edits, requested_camera_profile, use_adaptive_detail_defaults) =
        match crate::sidecar::load_desktop(&job.source) {
        Ok(Some(loaded)) => {
            let requested = loaded
                .edits
                .camera_profile
                .as_ref()
                .and_then(|relative| camera_profile_folder.map(|root| root.join(relative)));
            let use_adaptive = false;
            (loaded.edits, requested, use_adaptive)
        }
        Ok(None) => {
            let mut edits = crate::sidecar::default_edit_state();
            edits.exposure = default_exposure;
            let requested = last_camera_profile
                .and_then(|relative| camera_profile_folder.map(|root| root.join(relative)));
            (edits, requested, true)
        }
        Err(error) => {
            log::warn!(
                "ignoring invalid sidecar during batch export for {}: {error}",
                job.source.display()
            );
            let mut edits = crate::sidecar::default_edit_state();
            edits.exposure = default_exposure;
            let requested = last_camera_profile
                .and_then(|relative| camera_profile_folder.map(|root| root.join(relative)));
            (edits, requested, true)
        }
    };

    let original_raw = {
        let _decode_guard = decode_gate
            .write()
            .map_err(|_| "RAW decode gate was poisoned".to_owned())?;
        load_raw_file_with_profile_selection(
            &job.source,
            camera_profile_mode,
            camera_profile_folder,
            requested_camera_profile.as_deref(),
        )
        .map(Arc::new)
        .map_err(|error| {
            format!("{}: RAW decode failed: {error:#}", job.source.display())
        })?
    };
    if use_adaptive_detail_defaults {
        original_raw.apply_adaptive_detail_defaults(&mut edits.exposure);
    }

    if cancellation.load(Ordering::Acquire) {
        return Err("batch export cancelled".to_owned());
    }

    let raw = if edits.lens.enabled {
        let catalog = lensfun_catalog(&original_raw);
        let selected = catalog
            .lenses
            .iter()
            .find(|lens| lens.maker == edits.lens.maker && lens.model == edits.lens.model)
            .cloned()
            .or_else(|| {
                (!edits.lens.maker.is_empty() || !edits.lens.model.is_empty()).then(|| {
                    LensfunLens {
                        maker: edits.lens.maker.clone(),
                        model: edits.lens.model.clone(),
                    }
                })
            })
            .or(catalog.auto_match);
        if let Some(selected) = selected {
            Arc::new(apply_lensfun_correction(&original_raw, &selected).map_err(|error| {
                format!(
                    "{}: lens correction failed: {error:#}",
                    job.source.display()
                )
            })?)
        } else {
            Arc::clone(&original_raw)
        }
    } else {
        Arc::clone(&original_raw)
    };

    let mut masks = Arc::unwrap_or_clone(edits.masks);
    let inpaint_strokes = Arc::unwrap_or_clone(edits.inpainting);
    let inpaint = compose_inpaint_strokes(&inpaint_strokes);
    if needs_canonical_mask_source(&masks) {
        let source_raw = if raw.width.max(raw.height) <= 2048 {
            Arc::clone(&raw)
        } else {
            Arc::new(build_proxy(&raw, ProxySpec { max_edge: 2048 }))
        };
        let neutral_exposure = ExposureParams::scene_referred_default();
        let neutral_masks = MaskStack::default();
        let neutral_params = GpuParams::new(&neutral_exposure, &neutral_masks, &source_raw);
        let pipeline = RawGpuPipeline::new_headless_with_quality(
            device,
            queue,
            &source_raw,
            &neutral_params,
            ProcessingQuality::Preview,
        )
        .map_err(|error| {
            format!(
                "{}: range-mask source setup failed: {error:#}",
                job.source.display()
            )
        })?;
        pipeline
            .update_inpaint_layer(
                queue,
                inpaint.as_ref(),
                0,
                0,
                source_raw.width,
                source_raw.height,
            )
            .map_err(|error| {
                format!(
                    "{}: range-mask inpainting setup failed: {error:#}",
                    job.source.display()
                )
            })?;
        pipeline.recompute(queue, device, &neutral_params);
        let rgba = pipeline
            .read_output_region_blocking(
                device,
                queue,
                0,
                0,
                source_raw.width,
                source_raw.height,
            )
            .map_err(|error| {
                format!(
                    "{}: range-mask source readback failed: {error:#}",
                    job.source.display()
                )
            })?;
        let source = MaskRgbImage::new(source_raw.width, source_raw.height, rgba)
            .ok_or_else(|| "range-mask source dimensions are invalid".to_owned())?;
        install_missing_range_sources(&mut masks, &source);
    }

    if cancellation.load(Ordering::Acquire) {
        return Err("batch export cancelled".to_owned());
    }

    let source_file_name = job
        .source
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    Ok(ExportItemRequest {
        device: device.clone(),
        queue: queue.clone(),
        source: PreparedExportSource {
            raw,
            geometry: edits.geometry.sanitized(),
            exposure: edits.exposure,
            masks,
            inpaint,
            source_file_name,
            gpu_export_prewarm: None,
        },
        destination: ExportDestination::File(job.destination.clone()),
        format,
        settings: settings.clone(),
    })
}

impl AurawApp {
    pub(crate) fn export_progress_state(&self) -> Option<(usize, usize)> {
        self.export.task.as_ref().and_then(|task| {
            (task.total_tiles > 0).then_some((task.completed_tiles, task.total_tiles))
        })
    }

    pub(crate) fn library_batch_export_progress(&self) -> Option<(usize, usize)> {
        self.export.task.as_ref().and_then(|task| {
            (task.kind == ExportTaskKind::LibraryBatch).then_some((task.completed, task.total))
        })
    }

    fn update_library_batch_export_progress(&mut self) {
        let Some(batch) = self.export.batch.as_ref() else {
            return;
        };
        let Some(task) = self.export.task.as_mut() else {
            return;
        };
        if task.kind != ExportTaskKind::LibraryBatch {
            return;
        }
        task.completed = batch.completed.min(batch.total);
        task.total = batch.total;
        task.progress = batch_export_overall_fraction(
            task.completed,
            task.total,
            batch.current.is_some(),
            (task.total_tiles > 0).then_some((task.completed_tiles, task.total_tiles)),
        );
    }

    pub(in crate::app) fn request_library_batch_export_cancellation(&mut self) -> bool {
        self.cancel_export_task();
        if let Some(batch) = self.export.batch.as_mut() {
            batch.cancel_requested = true;
            batch.pending.clear();
            batch.current.is_none()
        } else {
            false
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn cancel_library_batch_export(&mut self) {
        self.request_library_batch_export_cancellation();
    }

    #[cfg(target_os = "android")]
    pub(crate) fn cancel_library_batch_export(&mut self) {
        if self.request_library_batch_export_cancellation() {
            self.finish_library_batch_export();
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn start_android_library_exports(
        &mut self,
        targets: Vec<AndroidLibraryExportTarget>,
        settings: ExportSettings,
        format: ExportFormat,
    ) {
        if targets.is_empty() || self.export.task.is_some() {
            if self.export.task.is_some() {
                self.ui.notice = Some("An export is already running.".to_owned());
            }
            return;
        }
        let pending = targets
            .into_iter()
            .map(|target| LibraryBatchExportJob { target })
            .collect::<VecDeque<_>>();
        let total = pending.len();
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.export.settings = settings.clone();
        self.export.batch = Some(LibraryBatchExportState {
            pending,
            current: None,
            total,
            completed: 0,
            failures: Vec::new(),
            cancel_requested: false,
            format,
            settings,
        });
        self.export.task = Some(ExportTask::new(
            ExportTaskKind::LibraryBatch,
            cancellation,
            None,
            None,
            total,
        ));
        self.ui.notice = None;
        self.egui_ctx.request_repaint();
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn start_next_library_export(&mut self, _frame: &eframe::Frame) {
        // Android's batch path must use the SAF document bridge. Once the user
        // enters Develop, do not replace that interactive document with the next
        // batch item. The current export may finish; remaining items resume when
        // the user returns to Library.
        if self.ui.active_tab == AppTab::Develop {
            if let Some(task) = self.export.task.as_mut() {
                if task.kind == ExportTaskKind::LibraryBatch {
                    task.phase = "Paused while Develop is in use".to_owned();
                }
            }
            return;
        }

        loop {
            let next = {
                let Some(batch) = self.export.batch.as_mut() else {
                    return;
                };
                if batch.current.is_some() {
                    return;
                }
                if batch.cancel_requested {
                    None
                } else {
                    batch.pending.pop_front().inspect(|job| {
                        batch.current = Some(job.clone());
                    })
                }
            };

            let Some(job) = next else {
                self.finish_library_batch_export();
                return;
            };

            if let Some(task) = self.export.task.as_mut() {
                task.phase = format!("Opening {}…", job.target.display_name());
                task.completed_tiles = 0;
                task.total_tiles = 0;
            }
            let display_name = job.target.display_name().to_owned();
            match crate::android::open_library_document(
                &self.android.android_app,
                &job.target.uri,
                &display_name,
            ) {
                Ok(()) => {
                    self.export.android_batch_load_pending = true;
                    self.android.picker_pending = true;
                    self.ui.notice = None;
                    self.ui.status = format!("Opening {display_name}…");
                    self.ui.active_tab = AppTab::Library;
                    return;
                }
                Err(error) => {
                    self.export.android_batch_load_pending = false;
                    if let Some(batch) = self.export.batch.as_mut() {
                        batch.failures.push(format!("{display_name}: {error}"));
                        batch.completed += 1;
                        batch.current = None;
                    }
                    self.update_library_batch_export_progress();
                }
            }
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn on_library_batch_load_finished(
        &mut self,
        success: bool,
        frame: &eframe::Frame,
    ) {
        let Some(batch) = self.export.batch.as_ref() else {
            return;
        };
        let Some(current) = batch.current.as_ref() else {
            return;
        };

        if batch.cancel_requested {
            self.complete_android_library_batch_export_item(Err(
                "batch export cancelled".to_owned(),
            ));
            return;
        }

        if !success {
            let name = current.target.display_name().to_owned();
            if let Some(batch) = self.export.batch.as_mut() {
                if !batch.cancel_requested {
                    batch.failures.push(format!("{name}: RAW load failed"));
                    batch.completed += 1;
                }
                batch.current = None;
            }
            self.update_library_batch_export_progress();
            return;
        }

        let format = batch.format;
        let settings = batch.settings.clone();
        let display_name = current.target.display_name().to_owned();
        self.export.settings = settings;
        let stem = std::path::Path::new(&display_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("AuRaw-export");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let cache_file_name = format!(
            "{stem}-auraw-{timestamp}.{}",
            format.extension()
        );
        let gallery_name = format!("{stem}-auraw.{}", format.extension());
        let destination = match self.prepare_android_export_destination(
            gallery_name,
            cache_file_name,
            format,
        ) {
            Ok(destination) => destination,
            Err(error) => {
                self.complete_android_library_batch_export_item(Err(format!(
                    "{display_name}: {error}"
                )));
                return;
            }
        };
        let cleanup = destination.clone();
        let mut start_error = None;
        let started = self
            .capture_export_task_request(destination, frame, format)
            .and_then(|request| match self.start_export_task(request, ExportTaskKind::LibraryBatch) {
                Ok(()) => Some(()),
                Err(error) => {
                    start_error = Some(error);
                    None
                }
            })
            .is_some();
        self.ui.active_tab = AppTab::Library;
        if !started {
            self.cancel_android_export_destination(&cleanup);
            let error = start_error.unwrap_or_else(|| "could not start export".to_owned());
            self.complete_android_library_batch_export_item(Err(format!(
                "{display_name}: {error}"
            )));
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn complete_android_library_batch_export_item(
        &mut self,
        result: Result<(), String>,
    ) {
        if let Some(batch) = self.export.batch.as_mut() {
            let name = batch
                .current
                .as_ref()
                .map(|job| job.target.display_name().to_owned())
                .unwrap_or_else(|| "image".to_owned());
            match result {
                Ok(()) => batch.completed += 1,
                Err(error) if !batch.cancel_requested => {
                    batch.failures.push(if error.starts_with(&name) {
                        error
                    } else {
                        format!("{name}: {error}")
                    });
                    batch.completed += 1;
                }
                Err(_) => {}
            }
            batch.current = None;
        }
        if let Some(task) = self.export.task.as_mut() {
            if task.kind == ExportTaskKind::LibraryBatch {
                task.receiver = None;
                task.destination = None;
                task.completed_tiles = 0;
                task.total_tiles = 0;
                task.phase = "Preparing next image…".to_owned();
            }
        }
        self.update_library_batch_export_progress();
        let finished_or_cancelled = self.export.batch.as_ref().is_some_and(|batch| {
            batch.cancel_requested || batch.pending.is_empty()
        });
        if finished_or_cancelled {
            self.finish_library_batch_export();
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn resume_android_library_batch_export_if_possible(
        &mut self,
        frame: &eframe::Frame,
    ) {
        let export_worker_active = self.export.task.as_ref().is_some_and(|task| {
            task.kind == ExportTaskKind::LibraryBatch
                && matches!(task.receiver.as_ref(), Some(ExportTaskReceiver::Tiled(_)))
        });
        if self.ui.active_tab == AppTab::Develop
            || self.android.picker_pending
            || self.develop.load_receiver.is_some()
            || export_worker_active
            || self.export.publish_pending
        {
            return;
        }
        let should_resume = self.export.batch.as_ref().is_some_and(|batch| {
            !batch.cancel_requested && batch.current.is_none() && !batch.pending.is_empty()
        });
        if should_resume {
            self.start_next_library_export(frame);
        }
    }

    pub(in crate::app) fn finish_library_batch_export(&mut self) {
        let Some(batch) = self.export.batch.take() else {
            return;
        };
        let succeeded = batch.completed.saturating_sub(batch.failures.len());
        let mut message = if batch.cancel_requested {
            format!(
                "Batch export cancelled after {succeeded} of {} images exported.",
                batch.total
            )
        } else if batch.failures.is_empty() {
            if cfg!(target_os = "android") {
                format!(
                    "Exported {succeeded} {} to Pictures/AuRaw.",
                    if succeeded == 1 { "image" } else { "images" }
                )
            } else {
                format!(
                    "Exported {succeeded} {}.",
                    if succeeded == 1 { "image" } else { "images" }
                )
            }
        } else {
            format!(
                "Exported {succeeded} of {} images. {}",
                batch.total,
                batch.failures.join(" · ")
            )
        };
        if batch.cancel_requested && !batch.failures.is_empty() {
            message.push_str(&format!(
                " {} failed. {}",
                batch.failures.len(),
                batch.failures.join(" · ")
            ));
        }
        self.ui.notice = Some(message);
        super::export::clear_export_task(&mut self.export.task);
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn start_library_exports(
        &mut self,
        jobs: Vec<(PathBuf, PathBuf)>,
        settings: ExportSettings,
        format: ExportFormat,
        frame: &eframe::Frame,
    ) {
        if jobs.is_empty() || self.export.task.is_some() {
            if self.export.task.is_some() {
                self.ui.notice = Some("An export is already running.".to_owned());
            }
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            self.ui.notice = Some("Export requires the wgpu renderer.".to_owned());
            return;
        };
        let pending = jobs
            .into_iter()
            .map(|(source, destination)| LibraryBatchExportJob { source, destination })
            .collect::<VecDeque<_>>();
        let total = pending.len();
        let cancellation = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let receiver = spawn_desktop_library_batch_export(DesktopLibraryBatchExportRequest {
            jobs: pending,
            context: DesktopLibraryExportContext {
                device: render_state.device.clone(),
                queue: render_state.queue.clone(),
                format,
                settings,
                camera_profile_mode: self.preferences.camera_profile_mode,
                camera_profile_folder: self.preferences.camera_profile_folder.clone(),
                last_camera_profile: self.preferences.last_camera_profile.clone(),
                default_exposure: self.new_image_exposure(),
                decode_gate: self.library.decode_gate(),
                cancellation: Arc::clone(&cancellation),
            },
            repaint: self.egui_ctx.clone(),
        });
        self.export.batch = Some(LibraryBatchExportState {
            pending: VecDeque::new(),
            current: None,
            total,
            completed: 0,
            failures: Vec::new(),
            cancel_requested: false,
        });
        self.export.task = Some(ExportTask::new(
            ExportTaskKind::LibraryBatch,
            cancellation,
            Some(ExportTaskReceiver::LibraryBatch(receiver)),
            None,
            total,
        ));
        self.ui.notice = None;
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn on_library_batch_load_finished(
        &mut self,
        _success: bool,
        _frame: &eframe::Frame,
    ) {
        // Desktop batch export owns a separate decode/export worker and never
        // consumes the document opened in Develop.
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn poll_android_export_publish(&mut self) {
        while let Some(result) = crate::android::take_export_publish_result() {
            self.export.publish_pending = false;
            if self.export.batch.is_some() {
                match result {
                    crate::android::ExportPublishResult::Published(_) => {
                        self.complete_android_library_batch_export_item(Ok(()));
                    }
                    crate::android::ExportPublishResult::Failed(error) => {
                        log::error!("Android batch export publish failed: {error}");
                        self.complete_android_library_batch_export_item(Err(error));
                    }
                }
                continue;
            }
            match result {
                crate::android::ExportPublishResult::Published(location) => {
                    self.ui.notice = Some(format!("Exported to {location}"));
                }
                crate::android::ExportPublishResult::Failed(error) => {
                    self.ui.notice = Some(format!("Export failed: {error}"));
                    log::error!("Android export publish failed: {error}");
                }
            }
            super::export::clear_export_task(&mut self.export.task);
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn poll_library_batch_export_worker(&mut self) {
        let (events, disconnected) = {
            let receiver = self.export.task.as_ref().and_then(|task| {
                if task.kind != ExportTaskKind::LibraryBatch {
                    return None;
                }
                match task.receiver.as_ref() {
                    Some(ExportTaskReceiver::LibraryBatch(receiver)) => Some(receiver),
                    _ => None,
                }
            });
            drain_worker_events(receiver, |_| false)
        };

        let mut finished = false;
        for event in events {
            match event {
                LibraryBatchExportEvent::Started { job } => {
                    if let Some(batch) = self.export.batch.as_mut() {
                        batch.current = Some(job);
                    }
                    if let Some(task) = self.export.task.as_mut() {
                        task.completed_tiles = 0;
                        task.total_tiles = 0;
                        task.phase = "Rendering image…".to_owned();
                    }
                    self.update_library_batch_export_progress();
                }
                LibraryBatchExportEvent::Progress {
                    completed_tiles,
                    total_tiles,
                } => {
                    if let Some(task) = self.export.task.as_mut() {
                        task.completed_tiles = completed_tiles;
                        task.total_tiles = total_tiles;
                        task.phase = "Rendering image…".to_owned();
                    }
                    self.update_library_batch_export_progress();
                }
                LibraryBatchExportEvent::ItemFinished { error } => {
                    if let Some(batch) = self.export.batch.as_mut() {
                        batch.completed += 1;
                        batch.current = None;
                        if let Some(error) = error {
                            batch.failures.push(error);
                        }
                    }
                    if let Some(task) = self.export.task.as_mut() {
                        task.completed_tiles = 0;
                        task.total_tiles = 0;
                        task.phase = "Preparing next image…".to_owned();
                    }
                    self.update_library_batch_export_progress();
                }
                LibraryBatchExportEvent::Finished { cancelled, error } => {
                    if let Some(batch) = self.export.batch.as_mut() {
                        batch.cancel_requested |= cancelled;
                        if let Some(error) = error {
                            batch.failures.push(error);
                        }
                    }
                    finished = true;
                }
            }
        }

        if disconnected && !finished {
            if let Some(batch) = self.export.batch.as_mut() {
                batch
                    .failures
                    .push("Batch export worker stopped unexpectedly.".to_owned());
            }
            finished = true;
        }

        if finished {
            if let Some(task) = self.export.task.as_mut() {
                task.receiver = None;
            }
            self.finish_library_batch_export();
        }
    }
}
