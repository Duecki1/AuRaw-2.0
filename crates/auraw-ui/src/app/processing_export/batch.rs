use super::*;

#[cfg(not(target_os = "android"))]
use super::export::spawn_export_request;

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
pub(in crate::app) struct DesktopLibraryBatchExportRequest {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub jobs: VecDeque<LibraryBatchExportJob>,
    pub format: ExportFormat,
    pub settings: ExportSettings,
    pub camera_profile_mode: CameraProfileMode,
    pub camera_profile_folder: Option<PathBuf>,
    pub last_camera_profile: Option<PathBuf>,
    pub default_exposure: ExposureParams,
    pub decode_gate: Arc<std::sync::RwLock<()>>,
    pub cancellation: Arc<std::sync::atomic::AtomicBool>,
    pub repaint: egui::Context,
}

#[cfg(not(target_os = "android"))]
struct DesktopLibraryExportContext<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    format: ExportFormat,
    settings: &'a ExportSettings,
    camera_profile_mode: CameraProfileMode,
    camera_profile_folder: Option<&'a std::path::Path>,
    last_camera_profile: Option<&'a std::path::Path>,
    default_exposure: ExposureParams,
    decode_gate: &'a std::sync::RwLock<()>,
    cancellation: &'a std::sync::atomic::AtomicBool,
}

#[cfg(not(target_os = "android"))]
pub(in crate::app) fn spawn_desktop_library_batch_export(
    request: DesktopLibraryBatchExportRequest,
) -> mpsc::Receiver<LibraryBatchExportEvent> {
    let DesktopLibraryBatchExportRequest {
        device,
        queue,
        jobs,
        format,
        settings,
        camera_profile_mode,
        camera_profile_folder,
        last_camera_profile,
        default_exposure,
        decode_gate,
        cancellation,
        repaint,
    } = request;
    use std::sync::atomic::Ordering;

    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let spawn_result = std::thread::Builder::new()
        .name("auraw-library-batch-export".to_owned())
        .spawn(move || {
            let total = jobs.len();
            let mut completed = 0usize;
            for job in jobs {
                if cancellation.load(Ordering::Acquire) {
                    break;
                }
                let _ = worker_sender.send(LibraryBatchExportEvent::Started {
                    job: job.clone(),
                    completed,
                    total,
                });
                repaint.request_repaint();

                let context = DesktopLibraryExportContext {
                    device: &device,
                    queue: &queue,
                    format,
                    settings: &settings,
                    camera_profile_mode,
                    camera_profile_folder: camera_profile_folder.as_deref(),
                    last_camera_profile: last_camera_profile.as_deref(),
                    default_exposure,
                    decode_gate: &decode_gate,
                    cancellation: &cancellation,
                };
                let request = prepare_desktop_library_export_request(&job, &context);

                if cancellation.load(Ordering::Acquire) {
                    break;
                }

                let request = match request {
                    Ok(request) => request,
                    Err(error) => {
                        completed += 1;
                        let _ = worker_sender.send(LibraryBatchExportEvent::ItemFinished {
                            completed,
                            error: Some(error),
                        });
                        repaint.request_repaint();
                        continue;
                    }
                };

                let export_receiver =
                    spawn_export_request(request, Arc::clone(&cancellation));

                let mut item_result = Err("export worker stopped unexpectedly".to_owned());
                while let Ok(event) = export_receiver.recv() {
                    match event {
                        ExportEvent::Progress {
                            completed_tiles,
                            total_tiles,
                        } => {
                            let _ = worker_sender.send(LibraryBatchExportEvent::Progress {
                                completed,
                                total,
                                completed_tiles,
                                total_tiles,
                            });
                            repaint.request_repaint();
                        }
                        ExportEvent::Finished(result) => {
                            item_result = result.map(|_| ());
                            break;
                        }
                    }
                }

                let cancelled = cancellation.load(Ordering::Acquire);
                if !cancelled || item_result.is_ok() {
                    // A cancellation request can arrive just after the current
                    // image was published. Count that image, but do not report a
                    // cooperative cancellation result as an export failure.
                    completed += 1;
                    let error = (!cancelled)
                        .then(|| item_result.err())
                        .flatten()
                        .map(|error| format!("{}: {error}", job.source.display()));
                    let _ = worker_sender.send(LibraryBatchExportEvent::ItemFinished {
                        completed,
                        error,
                    });
                    repaint.request_repaint();
                }
                if cancelled {
                    break;
                }
            }

            let _ = worker_sender.send(LibraryBatchExportEvent::Finished {
                cancelled: cancellation.load(Ordering::Acquire),
            });
            repaint.request_repaint();
        });

    if let Err(error) = spawn_result {
        let _ = sender.send(LibraryBatchExportEvent::ItemFinished {
            completed: 0,
            error: Some(format!("could not start batch export worker: {error}")),
        });
        let _ = sender.send(LibraryBatchExportEvent::Finished { cancelled: false });
    }
    receiver
}

#[cfg(not(target_os = "android"))]
fn prepare_desktop_library_export_request(
    job: &LibraryBatchExportJob,
    context: &DesktopLibraryExportContext<'_>,
) -> Result<ExportTaskRequest, String> {
    let DesktopLibraryExportContext {
        device,
        queue,
        format,
        settings,
        camera_profile_mode,
        camera_profile_folder,
        last_camera_profile,
        default_exposure,
        decode_gate,
        cancellation,
    } = *context;
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
    let display_name = job
        .source
        .file_stem()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("image")
        .to_owned();
    Ok(ExportTaskRequest {
        device: device.clone(),
        queue: queue.clone(),
        metadata: ExportMetadata::from_raw(&raw, source_file_name),
        raw,
        geometry: edits.geometry.sanitized(),
        exposure: edits.exposure,
        masks,
        inpaint,
        path: job.destination.clone(),
        format,
        settings: settings.clone(),
        display_name,
        gpu_export_prewarm: None,
    })
}

impl AurawApp {
    pub(crate) fn export_progress_state(&self) -> Option<(usize, usize)> {
        self.export_progress
    }

    pub(crate) fn library_batch_export_progress(&self) -> Option<(usize, usize)> {
        self.library_batch_export
            .as_ref()
            .map(|batch| (batch.completed, batch.total))
    }

    pub(crate) fn library_batch_export_tile_progress(&self) -> Option<(usize, usize)> {
        #[cfg(not(target_os = "android"))]
        {
            self.library_batch_export_tile_progress
        }
        #[cfg(target_os = "android")]
        {
            self.export_progress
        }
    }

    pub(crate) fn library_batch_export_overall_fraction(&self) -> Option<f32> {
        self.library_batch_export.as_ref().map(|batch| {
            batch_export_overall_fraction(
                batch.completed,
                batch.total,
                batch.current.is_some(),
                self.library_batch_export_tile_progress(),
            )
        })
    }

    pub(crate) fn library_batch_export_status(
        &self,
    ) -> Option<(usize, usize, usize, Option<String>, bool)> {
        self.library_batch_export.as_ref().map(|batch| {
            let current = batch.current.as_ref().map(|job| {
                #[cfg(not(target_os = "android"))]
                {
                    job.source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("image")
                        .to_owned()
                }
                #[cfg(target_os = "android")]
                {
                    job.target.display_name().to_owned()
                }
            });
            (
                batch.completed,
                batch.total,
                batch.failures.len(),
                current,
                batch.cancel_requested,
            )
        })
    }

    pub(in crate::app) fn request_library_batch_export_cancellation(&mut self) -> bool {
        if let Some(task_id) = self.library_batch_export_task_id {
            if !self.background_task_cancelled(task_id) {
                let _ = self.background_tasks.request_cancel(task_id);
            }
        }
        if let Some(batch) = self.library_batch_export.as_mut() {
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
        self.sync_library_batch_background_progress();
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
        if targets.is_empty() {
            return;
        }
        let pending = targets
            .into_iter()
            .map(|target| LibraryBatchExportJob { target })
            .collect::<VecDeque<_>>();
        let total = pending.len();
        self.enqueue_background_action(
            TaskKind::LibraryBatchExport,
            format!(
                "Exporting {total} {}",
                if total == 1 { "image" } else { "images" }
            ),
            TaskProgress::units(
                0,
                total as u64,
                Some("images".to_owned()),
                "Queued for batch export…",
            ),
            false,
            BackgroundAction::LibraryBatchExport {
                jobs: pending,
                settings,
                format,
            },
        );
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn start_next_library_export(&mut self, _frame: &eframe::Frame) {
        // Android's batch path must use the SAF document bridge. Once the user
        // enters Develop, do not replace that interactive document with the next
        // batch item. The current export may finish; remaining items resume when
        // the user returns to Library.
        if self.active_tab == AppTab::Develop {
            if let Some(task_id) = self.library_batch_export_task_id {
                self.update_background_progress(
                    Some(task_id),
                    TaskProgress::indeterminate("Paused while Develop is in use"),
                );
            }
            return;
        }

        loop {
            let next = {
                let Some(batch) = self.library_batch_export.as_mut() else {
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

            let display_name = job.target.display_name().to_owned();
            match crate::android::open_library_document(
                &self.android_app,
                &job.target.uri,
                &display_name,
            ) {
                Ok(()) => {
                    self.android_batch_load_pending = true;
                    self.picker_pending = true;
                    self.notice = None;
                    self.status = format!("Opening {display_name}…");
                    self.active_tab = AppTab::Library;
                    return;
                }
                Err(error) => {
                    self.android_batch_load_pending = false;
                    if let Some(batch) = self.library_batch_export.as_mut() {
                        batch.failures.push(format!("{display_name}: {error}"));
                        batch.completed += 1;
                        batch.current = None;
                    }
                }
            }
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn on_library_batch_load_finished(&mut self, success: bool, frame: &eframe::Frame) {
        let Some(batch) = self.library_batch_export.as_ref() else {
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
            if let Some(batch) = self.library_batch_export.as_mut() {
                if !batch.cancel_requested {
                    batch.failures.push(format!("{name}: RAW load failed"));
                    batch.completed += 1;
                }
                batch.current = None;
            }
            return;
        }

        let format = batch.format;
        let settings = batch.settings.clone();
        let display_name = current.target.display_name().to_owned();
        self.export_settings = settings.clone();
        let Some(data_dir) = self.android_app.internal_data_path() else {
            self.complete_android_library_batch_export_item(Err(format!(
                "{display_name}: Android did not provide an app data directory"
            )));
            return;
        };
        let export_dir = data_dir.join("cache").join("exports");
        if let Err(error) = std::fs::create_dir_all(&export_dir) {
            self.complete_android_library_batch_export_item(Err(format!(
                "{display_name}: could not prepare export cache: {error}"
            )));
            return;
        }
        let stem = std::path::Path::new(&display_name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.is_empty())
            .unwrap_or("AuRaw-export");
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let cached_destination = export_dir.join(format!(
            "{stem}-auraw-{timestamp}.{}",
            format.extension()
        ));
        let gallery_name = format!("{stem}-auraw.{}", format.extension());
        let destination = match crate::android::prepare_direct_export(
            &self.android_app,
            &export_dir,
            &gallery_name,
            format.mime_type(),
        ) {
            Ok(Some(path)) => path,
            Ok(None) => cached_destination,
            Err(error) => {
                log::warn!("direct Android batch export unavailable, falling back to cache: {error}");
                cached_destination
            }
        };
        let direct_path = crate::android::is_direct_export_path(&destination)
            .then(|| destination.clone());
        let mut start_error = None;
        let started = if let Some(task_id) = self.library_batch_export_task_id {
            if let Some(request) =
                self.capture_export_task_request(destination, frame, format)
            {
                // The batch task already owns the global FIFO slot. Starting a
                // nested SingleExport task here would queue it behind its own
                // parent and show a second "waiting" dialog indefinitely. The
                // shared task starter also releases Android preview GPU resources
                // before allocating the full-quality tiled export pipeline.
                match self.start_export_task(task_id, request, frame) {
                    Ok(()) => {
                        let started = self.export_task_id == Some(task_id)
                            && self.export_receiver.is_some();
                        if started {
                            self.sync_library_batch_background_progress();
                        }
                        started
                    }
                    Err(error) => {
                        self.notice = Some(format!("Export failed: {error}"));
                        start_error = Some(error);
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        };
        self.active_tab = AppTab::Library;
        if !started {
            if let Some(path) = direct_path {
                crate::android::cancel_direct_export(&self.android_app, &path);
            }
            let error = start_error.unwrap_or_else(|| "could not start export".to_owned());
            self.complete_android_library_batch_export_item(Err(format!(
                "{display_name}: {error}"
            )));
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn complete_android_library_batch_export_item(&mut self, result: Result<(), String>) {
        if let Some(batch) = self.library_batch_export.as_mut() {
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
        let finished_or_cancelled = self.library_batch_export.as_ref().is_some_and(|batch| {
            batch.cancel_requested || batch.pending.is_empty()
        });
        if finished_or_cancelled {
            self.finish_library_batch_export();
        }
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn resume_android_library_batch_export_if_possible(&mut self, frame: &eframe::Frame) {
        if self.active_tab == AppTab::Develop
            || self.picker_pending
            || self.load_receiver.is_some()
            || self.export_receiver.is_some()
        {
            return;
        }
        let should_resume = self.library_batch_export.as_ref().is_some_and(|batch| {
            !batch.cancel_requested && batch.current.is_none() && !batch.pending.is_empty()
        });
        if should_resume {
            self.start_next_library_export(frame);
        }
    }

    pub(in crate::app) fn finish_library_batch_export(&mut self) {
        let Some(batch) = self.library_batch_export.take() else {
            return;
        };
        #[cfg(not(target_os = "android"))]
        {
            self.library_batch_export_tile_progress = None;
        }

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
        self.notice = Some(message);
        self.export_task_id = None;
        if let Some(id) = self.library_batch_export_task_id.take() {
            if batch.cancel_requested || batch.failures.is_empty() {
                self.finish_background_task(id);
            } else {
                self.fail_background_task(id, batch.failures.join(" · "));
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn start_library_exports(
        &mut self,
        jobs: Vec<(PathBuf, PathBuf)>,
        settings: ExportSettings,
        format: ExportFormat,
        _frame: &eframe::Frame,
    ) {
        if jobs.is_empty() {
            return;
        }
        let pending = jobs
            .into_iter()
            .map(|(source, destination)| LibraryBatchExportJob { source, destination })
            .collect::<VecDeque<_>>();
        let total = pending.len();
        self.enqueue_background_action(
            TaskKind::LibraryBatchExport,
            format!(
                "Exporting {total} {}",
                if total == 1 { "image" } else { "images" }
            ),
            TaskProgress::units(
                0,
                total as u64,
                Some("images".to_owned()),
                "Waiting for earlier background work…",
            ),
            true,
            BackgroundAction::LibraryBatchExport {
                jobs: pending,
                settings,
                format,
            },
        );
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn on_library_batch_load_finished(&mut self, _success: bool, _frame: &eframe::Frame) {
        // Desktop batch export owns a separate decode/export worker and never
        // consumes the document opened in Develop.
    }

    #[cfg(target_os = "android")]
    pub(in crate::app) fn poll_android_export_publish(&mut self) {
        while let Some(result) = crate::android::take_export_publish_result() {
            self.export_publish_pending = false;
            if self.library_batch_export.is_some() {
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
                    self.notice = Some(format!("Exported to {location}"));
                    if let Some(id) = self.export_task_id.take() {
                        self.finish_background_task(id);
                    }
                }
                crate::android::ExportPublishResult::Failed(error) => {
                    self.notice = Some(format!("Export failed: {error}"));
                    if let Some(id) = self.export_task_id.take() {
                        self.fail_background_task(id, error.clone());
                    }
                    log::error!("Android export publish failed: {error}");
                }
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn poll_library_batch_export_worker(&mut self) {
        let (events, disconnected) =
            drain_worker_events(self.library_batch_export_receiver.as_ref(), |_| false);

        let mut finished = false;
        for event in events {
            match event {
                LibraryBatchExportEvent::Started {
                    job,
                    completed,
                    total,
                } => {
                    if let Some(batch) = self.library_batch_export.as_mut() {
                        batch.current = Some(job);
                        batch.completed = completed;
                        batch.total = total;
                    }
                    self.library_batch_export_tile_progress = Some((0, 0));
                    self.sync_library_batch_background_progress();
                }
                LibraryBatchExportEvent::Progress {
                    completed,
                    total,
                    completed_tiles,
                    total_tiles,
                } => {
                    if let Some(batch) = self.library_batch_export.as_mut() {
                        batch.completed = completed;
                        batch.total = total;
                    }
                    self.library_batch_export_tile_progress = Some((completed_tiles, total_tiles));
                    self.sync_library_batch_background_progress();
                }
                LibraryBatchExportEvent::ItemFinished { completed, error } => {
                    if let Some(batch) = self.library_batch_export.as_mut() {
                        batch.completed = completed;
                        batch.current = None;
                        if let Some(error) = error {
                            batch.failures.push(error);
                        }
                    }
                    self.library_batch_export_tile_progress = None;
                    self.sync_library_batch_background_progress();
                }
                LibraryBatchExportEvent::Finished { cancelled } => {
                    if let Some(batch) = self.library_batch_export.as_mut() {
                        batch.cancel_requested |= cancelled;
                    }
                    finished = true;
                }
            }
        }

        if disconnected && !finished {
            if let Some(batch) = self.library_batch_export.as_mut() {
                batch.failures.push("Batch export worker stopped unexpectedly.".to_owned());
            }
            finished = true;
        }

        if finished {
            self.library_batch_export_receiver = None;
            self.library_batch_export_tile_progress = None;
            self.finish_library_batch_export();
        }
    }
}
