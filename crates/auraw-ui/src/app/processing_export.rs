const EXPORT_TILE_PHASE_WEIGHT: f32 = 0.90;
const EXPORT_MAX_INCOMPLETE_FRACTION: f32 = 0.99;

fn batch_export_overall_fraction(
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

fn aligned_detail_axis(
    min_uv: f32,
    max_uv: f32,
    extent: u32,
    cfa_period: u32,
    viewport_pixels: u32,
    detail_pixel_scale: f32,
) -> (u32, u32) {
    let extent = extent.max(1);
    let period = cfa_period.max(1);
    let visible_start =
        ((min_uv.clamp(0.0, 1.0) * extent as f32).floor() as u32).min(extent.saturating_sub(1));
    let visible_end =
        ((max_uv.clamp(0.0, 1.0) * extent as f32).ceil() as u32).clamp(visible_start + 1, extent);
    let visible_len = visible_end - visible_start;

    // Preserve spatial context around detail crops to prevent visible edge seams.
    let visible_detail_pixels =
        (viewport_pixels.max(1) as f32 * detail_pixel_scale.max(0.1)).max(1.0);
    let support_padding =
        (visible_len as f32 * EXPORT_TILE_HALO as f32 / visible_detail_pixels).ceil() as u32;
    let padding = ((visible_len as f32 * 0.06).ceil() as u32)
        .max(EXPORT_TILE_HALO)
        .max(support_padding);
    let padded_start = visible_start.saturating_sub(padding);
    let padded_end = visible_end.saturating_add(padding).min(extent);
    let aligned_start = (padded_start / period) * period;
    let aligned_end = padded_end
        .div_ceil(period)
        .saturating_mul(period)
        .min(extent)
        .max(aligned_start + 1);
    (aligned_start, aligned_end)
}

fn detail_texture_uv(visible: PreviewUvRect, crop: PreviewUvRect) -> PreviewUvRect {
    let crop_width = (crop.max[0] - crop.min[0]).max(f32::EPSILON);
    let crop_height = (crop.max[1] - crop.min[1]).max(f32::EPSILON);
    PreviewUvRect {
        min: [
            ((visible.min[0] - crop.min[0]) / crop_width).clamp(0.0, 1.0),
            ((visible.min[1] - crop.min[1]) / crop_height).clamp(0.0, 1.0),
        ],
        max: [
            ((visible.max[0] - crop.min[0]) / crop_width).clamp(0.0, 1.0),
            ((visible.max[1] - crop.min[1]) / crop_height).clamp(0.0, 1.0),
        ],
    }
}

fn requested_detail_edge(
    quality: PreviewQuality,
    viewport_pixels: [u32; 2],
    visible: PreviewUvRect,
    crop_width: u32,
    crop_height: u32,
    full_width: u32,
    full_height: u32,
) -> u32 {
    let visible_source_width =
        ((visible.max[0] - visible.min[0]).max(1.0 / full_width.max(1) as f32) * full_width as f32)
            .max(1.0);
    let visible_source_height = ((visible.max[1] - visible.min[1])
        .max(1.0 / full_height.max(1) as f32)
        * full_height as f32)
        .max(1.0);
    let padded_width_pixels =
        viewport_pixels[0].max(1) as f32 * crop_width as f32 / visible_source_width;
    let padded_height_pixels =
        viewport_pixels[1].max(1) as f32 * crop_height as f32 / visible_source_height;
    (padded_width_pixels.max(padded_height_pixels) * quality.detail_pixel_scale())
        .ceil()
        .clamp(
            256.0,
            quality.detail_edge_for_viewport(viewport_pixels) as f32,
        ) as u32
}

fn navigation_proxy_edge() -> u32 {
    if cfg!(target_os = "android") { 384 } else { 512 }
}

fn navigation_mask_edge() -> u32 {
    if cfg!(target_os = "android") { 256 } else { 384 }
}

fn detail_mask_edge() -> u32 {
    // This atlas covers only the zoomed source region (plus the exact shaping
    // halo), so it can match the viewport at much higher density without the
    // enormous 32-layer allocation a full-frame 4K atlas would require.
    if cfg!(target_os = "android") { 1024 } else { 2048 }
}

fn detail_mask_source_region(
    masks: &MaskStack,
    source_origin: [u32; 2],
    source_size: [u32; 2],
    full_width: u32,
    full_height: u32,
) -> [u32; 4] {
    let full_width = full_width.max(1);
    let full_height = full_height.max(1);
    let margin = masks.raster_margin_pixels(full_width, full_height);
    let x0 = source_origin[0].min(full_width - 1).saturating_sub(margin);
    let y0 = source_origin[1].min(full_height - 1).saturating_sub(margin);
    let x1 = source_origin[0]
        .saturating_add(source_size[0])
        .saturating_add(margin)
        .clamp(x0 + 1, full_width);
    let y1 = source_origin[1]
        .saturating_add(source_size[1])
        .saturating_add(margin)
        .clamp(y0 + 1, full_height);
    [x0, y0, x1 - x0, y1 - y0]
}

fn mask_source_region_uv(region: [u32; 4], full_width: u32, full_height: u32) -> [f32; 4] {
    let width = full_width.max(1) as f32;
    let height = full_height.max(1) as f32;
    [
        region[0] as f32 / width,
        region[1] as f32 / height,
        region[0].saturating_add(region[2]) as f32 / width,
        region[1].saturating_add(region[3]) as f32 / height,
    ]
}

fn mask_region_texture_extent(region: [u32; 4], max_edge: u32) -> [u32; 2] {
    let width = region[2].max(1);
    let height = region[3].max(1);
    let longest = width.max(height);
    if longest <= max_edge {
        return [width, height];
    }
    let scale = max_edge.max(1) as f64 / longest as f64;
    [
        ((width as f64 * scale).round() as u32).clamp(1, max_edge.max(1)),
        ((height as f64 * scale).round() as u32).clamp(1, max_edge.max(1)),
    ]
}

/// Start a detailed crop for every real zoom level above fit. The previous
/// 1.01 cutoff excluded an exact 101% zoom and, together with the former
/// proxy-texel shortcut, kept the tiny navigation image visible until much deeper
/// zoom levels.
const DETAIL_ZOOM_START: f32 = 1.0005;

fn zoom_detail_idle_delay() -> Duration {
    // Wait only long enough to coalesce wheel/pinch events. A full second made
    // the navigation proxy look like the final preview after zooming stopped.
    Duration::from_millis(if cfg!(target_os = "android") { 220 } else { 140 })
}

fn spawn_export_request(
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
    match format {
        ExportFormat::Png => spawn_tiled_png_export_with_program_prewarm(
            device,
            queue,
            raw,
            geometry,
            exposure,
            masks,
            inpaint,
            path,
            TileSpec::default(),
            settings,
            metadata,
            cancellation,
            gpu_export_prewarm,
        ),
        ExportFormat::Jpeg => spawn_tiled_jpeg_export_with_program_prewarm(
            device,
            queue,
            raw,
            geometry,
            exposure,
            masks,
            inpaint,
            path,
            TileSpec::default(),
            settings,
            metadata,
            cancellation,
            gpu_export_prewarm,
        ),
        ExportFormat::Tiff => spawn_tiled_tiff_export_with_program_prewarm(
            device,
            queue,
            raw,
            geometry,
            exposure,
            masks,
            inpaint,
            path,
            TileSpec::default(),
            settings,
            metadata,
            cancellation,
            gpu_export_prewarm,
        ),
    }
}

#[cfg(not(target_os = "android"))]
#[allow(clippy::too_many_arguments)]
fn spawn_desktop_library_batch_export(
    device: wgpu::Device,
    queue: wgpu::Queue,
    jobs: VecDeque<LibraryBatchExportJob>,
    format: ExportFormat,
    settings: ExportSettings,
    camera_profile_mode: CameraProfileMode,
    camera_profile_folder: Option<PathBuf>,
    last_camera_profile: Option<PathBuf>,
    default_exposure: ExposureParams,
    decode_gate: Arc<std::sync::RwLock<()>>,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
    repaint: egui::Context,
) -> mpsc::Receiver<LibraryBatchExportEvent> {
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

                let request = prepare_desktop_library_export_request(
                    &device,
                    &queue,
                    &job,
                    format,
                    &settings,
                    camera_profile_mode,
                    camera_profile_folder.as_deref(),
                    last_camera_profile.as_deref(),
                    default_exposure,
                    &decode_gate,
                    &cancellation,
                );

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
#[allow(clippy::too_many_arguments)]
fn prepare_desktop_library_export_request(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    job: &LibraryBatchExportJob,
    format: ExportFormat,
    settings: &ExportSettings,
    camera_profile_mode: CameraProfileMode,
    camera_profile_folder: Option<&std::path::Path>,
    last_camera_profile: Option<&std::path::Path>,
    default_exposure: ExposureParams,
    decode_gate: &std::sync::RwLock<()>,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<ExportTaskRequest, String> {
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
    pub(crate) fn mark_lens_correction_dirty(&mut self) {
        self.note_edit_changed();
        if self.original_raw.is_some() {
            self.lens_correction_dirty = true;
            self.lens_correction_generation = self.lens_correction_generation.wrapping_add(1);
            self.notice = None;
            self.egui_ctx.request_repaint();
        }
    }

    fn apply_pending_lens_correction(&mut self, frame: &eframe::Frame) {
        self.queue_pending_lens_correction(frame);
    }

    fn queue_pending_lens_correction(&mut self, frame: &eframe::Frame) {
        self.poll_lens_correction_worker(frame);
        if !self.lens_correction_dirty {
            return;
        }
        self.lens_correction_dirty = false;

        let Some(original_raw) = self.original_raw.as_ref().map(Arc::clone) else {
            return;
        };
        let selection = if self.lens_correction.enabled {
            let Some(selection) = self.lens_correction.selected_lens() else {
                self.lens_correction.enabled = false;
                self.lens_correction.applied = false;
                self.lens_correction.catalog.status =
                    "Select a lens profile before enabling correction.".to_owned();
                return;
            };
            Some(selection)
        } else {
            None
        };

        if let Some(restored_masks) = self.history_lens_restore_masks.take() {
            self.masks = restored_masks;
            self.rehydrate_restored_mask_state();
        }

        #[cfg(target_os = "android")]
        let cached_raws = match selection.as_ref() {
            Some(requested) => self
                .lens_corrected_preview_cache
                .as_ref()
                .filter(|(cached, quality, _, _)| {
                    cached == requested && *quality == self.preview_quality
                })
                .map(|(_, _, full_raw, preview_raw)| {
                    (Arc::clone(full_raw), Arc::clone(preview_raw))
                }),
            None => self
                .lens_original_preview_cache
                .as_ref()
                .filter(|(quality, _)| *quality == self.preview_quality)
                .map(|(_, preview_raw)| (Arc::clone(&original_raw), Arc::clone(preview_raw))),
        };
        #[cfg(not(target_os = "android"))]
        let cached_raws = None;

        let generation = self.lens_correction_generation;
        let document_id = self.sidecar_generation;
        let name = selection
            .as_ref()
            .map(|lens| format!("Applying {}", lens.label()))
            .unwrap_or_else(|| "Disabling lens correction".to_owned());
        let preview_proxy_edge = self.preview_quality.proxy_edge_for_fitted_source(
            self.preview_viewport_pixels,
            original_raw.width,
            original_raw.height,
            self.geometry,
        );
        self.enqueue_lens_background_action(
            LensCorrectionTaskRequest {
                document_id,
                generation,
                original_raw,
                selection,
                #[cfg(target_os = "android")]
                preview_quality: self.preview_quality,
                preview_proxy_edge,
                cached_raws,
            },
            name,
        );
    }

    fn start_lens_correction_task(&mut self, id: TaskId, request: LensCorrectionTaskRequest) {
        let Some(cancellation) = self.background_tasks.cancellation_token(id) else {
            self.fail_background_task(id, "Lens correction lost its cancellation state.");
            return;
        };
        self.lens_correction_task_id = Some(id);
        let status_label = request
            .selection
            .as_ref()
            .map(LensfunLens::label)
            .unwrap_or_else(|| "original RAW geometry".to_owned());
        self.lens_correction.catalog.status = if request.selection.is_some() {
            format!("Applying {status_label} in the background…")
        } else {
            "Disabling lens correction in the background…".to_owned()
        };
        self.background_tasks.update_progress(
            id,
            TaskProgress::indeterminate(if request.selection.is_some() {
                "Applying profile…"
            } else {
                "Restoring original RAW geometry…"
            }),
        );

        let repaint = self.egui_ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let document_id = request.document_id;
        let generation = request.generation;
        let spawn_result = std::thread::Builder::new()
            .name("auraw-lens-correction".to_owned())
            .spawn(move || {
                use std::sync::atomic::Ordering;
                let result = (|| -> Result<PreparedLensCorrection, String> {
                    if cancellation.load(Ordering::Acquire) {
                        return Err("background task cancelled".to_owned());
                    }
                    let applied_label = request.selection.as_ref().map(LensfunLens::label);
                    let (full_raw, preview_raw) = if let Some(cached_raws) = request.cached_raws {
                        cached_raws
                    } else {
                        let original_raw = Arc::clone(&request.original_raw);
                        let full_raw = if let Some(selection) = request.selection.as_ref() {
                            Arc::new(
                                apply_lensfun_correction(&original_raw, selection).map_err(
                                    |error| {
                                        format!(
                                            "Could not apply {}: {error:#}",
                                            selection.label()
                                        )
                                    },
                                )?,
                            )
                        } else {
                            original_raw
                        };
                        if cancellation.load(Ordering::Acquire) {
                            return Err("background task cancelled".to_owned());
                        }
                        let _ = sender.send(LensCorrectionEvent::Progress {
                            task_id: id,
                            document_id,
                            generation,
                            phase: "Building preview proxy…".to_owned(),
                        });
                        repaint.request_repaint();
                        let preview_spec = ProxySpec {
                            max_edge: request.preview_proxy_edge,
                        };
                        let preview_raw =
                            if full_raw.width.max(full_raw.height) <= preview_spec.max_edge {
                                Arc::clone(&full_raw)
                            } else {
                                Arc::new(build_proxy(&full_raw, preview_spec))
                            };
                        (full_raw, preview_raw)
                    };
                    if cancellation.load(Ordering::Acquire) {
                        return Err("background task cancelled".to_owned());
                    }
                    Ok(PreparedLensCorrection {
                        full_raw,
                        preview_raw,
                        applied_label,
                        #[cfg(target_os = "android")]
                        selection: request.selection,
                        #[cfg(target_os = "android")]
                        preview_quality: request.preview_quality,
                    })
                })();
                let _ = sender.send(LensCorrectionEvent::Finished {
                    task_id: id,
                    document_id,
                    generation,
                    result,
                });
                repaint.request_repaint();
            });
        match spawn_result {
            Ok(_) => {
                self.lens_correction_receiver = Some(receiver);
                self.egui_ctx.request_repaint_after(Duration::from_millis(50));
            }
            Err(error) => {
                self.lens_correction_task_id = None;
                self.fail_background_task(id, format!("Could not start lens correction: {error}"));
            }
        }
    }

    pub(crate) fn lens_correction_busy(&self) -> bool {
        self.lens_correction_receiver.is_some()
            || self.background_task_snapshots().iter().any(|task| {
                matches!(task.kind, TaskKind::LensCorrection { .. })
                    && task.status != TaskStatus::Failed
            })
    }

    fn poll_lens_correction_worker(&mut self, frame: &eframe::Frame) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.lens_correction_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(event) => {
                        let finished = matches!(event, LensCorrectionEvent::Finished { .. });
                        events.push(event);
                        if finished {
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let mut finished = None;
        for event in events {
            match event {
                LensCorrectionEvent::Progress {
                    task_id,
                    document_id,
                    generation,
                    phase,
                } => {
                    if document_id == self.sidecar_generation
                        && generation == self.lens_correction_generation
                        && !self.background_task_cancelled(task_id)
                    {
                        self.update_background_progress(
                            Some(task_id),
                            TaskProgress::indeterminate(phase),
                        );
                    }
                }
                LensCorrectionEvent::Finished {
                    task_id,
                    document_id,
                    generation,
                    result,
                } => finished = Some((task_id, document_id, generation, result)),
            }
        }

        if finished.is_none() && disconnected {
            self.lens_correction_receiver = None;
            if let Some(id) = self.lens_correction_task_id.take() {
                self.fail_background_task(id, "Lens-correction worker stopped unexpectedly.");
            }
            self.lens_correction.enabled = self.lens_correction.applied;
            self.lens_correction.catalog.status =
                "Lens-correction worker stopped unexpectedly.".to_owned();
            self.notice = Some(self.lens_correction.catalog.status.clone());
            return;
        }
        let Some((task_id, document_id, generation, result)) = finished else {
            return;
        };
        self.lens_correction_receiver = None;
        if self.lens_correction_task_id == Some(task_id) {
            self.lens_correction_task_id = None;
        }

        let stale = document_id != self.sidecar_generation
            || generation != self.lens_correction_generation
            || self.background_task_cancelled(task_id);
        if stale {
            self.finish_background_task(task_id);
            return;
        }

        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                if self.background_task_cancelled(task_id) {
                    self.finish_background_task(task_id);
                } else {
                    self.lens_correction.enabled = self.lens_correction.applied;
                    self.lens_correction.catalog.status = error.clone();
                    self.notice =
                        Some("Lens correction failed; restored the previous preview.".to_owned());
                    self.fail_background_task(task_id, error);
                }
                return;
            }
        };
        self.update_background_progress(
            Some(task_id),
            TaskProgress::indeterminate("Preparing GPU preview…"),
        );

        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            self.fail_background_task(task_id, self.notice.clone().unwrap_or_default());
            return;
        };

        #[cfg(target_os = "android")]
        {
            let Some(pipeline) = self.gpu_pipeline.as_ref() else {
                self.fail_background_task(task_id, "The preview pipeline is unavailable.");
                return;
            };
            if let Err(error) =
                pipeline.upload_raw_tile(&render_state.queue, &prepared.preview_raw)
            {
                self.notice = Some(format!(
                    "Could not update the lens-corrected preview pixels: {error:#}"
                ));
                self.fail_background_task(task_id, self.notice.clone().unwrap_or_default());
                return;
            }
            let params = GpuParams::new(&self.exposure, &self.masks, &prepared.preview_raw)
                .with_vignette_geometry(self.geometry);
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
            if let Some(selection) = prepared.selection.clone() {
                self.lens_corrected_preview_cache = Some((
                    selection,
                    prepared.preview_quality,
                    Arc::clone(&prepared.full_raw),
                    Arc::clone(&prepared.preview_raw),
                ));
            } else {
                self.lens_original_preview_cache = Some((
                    prepared.preview_quality,
                    Arc::clone(&prepared.preview_raw),
                ));
            }
        }

        #[cfg(not(target_os = "android"))]
        {
            let preview_masks = self.masks.clone();
            let params = GpuParams::new(&self.exposure, &preview_masks, &prepared.preview_raw)
                .with_vignette_geometry(self.geometry);
            let mut pipeline = match RawGpuPipeline::new_headless_with_quality(
                &render_state.device,
                &render_state.queue,
                &prepared.preview_raw,
                &params,
                ProcessingQuality::Preview,
            ) {
                Ok(pipeline) => pipeline,
                Err(error) => {
                    let message =
                        format!("Could not rebuild the corrected GPU preview: {error:#}");
                    self.notice = Some(message.clone());
                    self.fail_background_task(task_id, message);
                    return;
                }
            };
            if let Err(error) = self.apply_display_output_transform(&render_state.queue, &pipeline) {
                let message = format!("Could not prepare the preview color profile: {error:#}");
                self.notice = Some(message.clone());
                self.fail_background_task(task_id, message);
                return;
            }
            if let Err(error) = Self::upload_preview_masks(
                &pipeline,
                &render_state.queue,
                &preview_masks,
                &prepared.preview_raw,
            ) {
                self.notice = Some(error.clone());
                self.fail_background_task(task_id, error);
                return;
            }
            if let Err(error) = pipeline.update_inpaint_layer(
                &render_state.queue,
                self.inpaint_layer.as_ref(),
                0,
                0,
                prepared.preview_raw.width,
                prepared.preview_raw.height,
            ) {
                let message =
                    format!("Could not rebuild lens-corrected preview inpainting: {error:#}");
                self.notice = Some(message.clone());
                self.fail_background_task(task_id, message);
                return;
            }
            pipeline.recompute(&render_state.queue, &render_state.device, &params);

            if document_id != self.sidecar_generation
                || generation != self.lens_correction_generation
                || self.background_task_cancelled(task_id)
            {
                self.finish_background_task(task_id);
                return;
            }
            let mut renderer = render_state.renderer.write();
            self.take_preview_pipeline_and_release_textures(&mut renderer);
            pipeline.register_egui_texture(&render_state.device, &mut renderer);
            drop(renderer);
            self.gpu_pipeline = Some(pipeline);
        }

        if document_id != self.sidecar_generation
            || generation != self.lens_correction_generation
            || self.background_task_cancelled(task_id)
        {
            self.finish_background_task(task_id);
            return;
        }

        self.rehydrate_restored_mask_state();
        self.note_lens_correction_changed_for_masks();
        self.dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.detail_dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.navigation_dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.loaded_raw = Some(prepared.full_raw);
        self.preview_raw = Some(prepared.preview_raw);
        self.inpaint_source_cache = None;
        self.preview_zoom = 1.0;
        self.preview_center = [0.5, 0.5];
        self.preview_visible_uv = PreviewUvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        };
        self.preview_motion_at = None;
        self.preview_touch_navigation_active = false;
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.preview_detail_pending_stage = None;
        self.navigation_pending_stage = None;
        self.preview_detail_urgent = false;
        self.target_exposure = self.exposure;
        self.pending_stage = None;
        self.lens_correction.applied = prepared.applied_label.is_some();
        self.lens_correction.catalog.status = prepared.applied_label.map_or_else(
            || "Lens correction disabled; using the original RAW geometry.".to_owned(),
            |label| format!("Applied {label}"),
        );
        self.notice = None;
        self.finish_background_task(task_id);
        self.resume_persisted_ai_denoise(frame);
        self.egui_ctx.request_repaint();
    }

    fn preview_detail_is_current(&self) -> bool {
        self.preview_detail
            .as_ref()
            .is_some_and(|detail| detail.revision == self.preview_revision)
    }

    pub(crate) fn preview_processing_pending(&self) -> bool {
        self.preview_detail_pending_stage.is_some()
            || self.navigation_pending_stage.is_some()
            || (self.pending_stage.is_some()
                && (self.preview_zoom <= DETAIL_ZOOM_START
                    || !self.preview_detail_is_current()))
    }

    pub(crate) fn note_preview_motion(&mut self) {
        let edit_was_pending = self.preview_detail_pending_stage.is_some();
        let rendered_content_was_current = self.original_preview_rendered_state
            == Some((self.original_preview_requested, self.preview_revision));
        self.preview_revision = self.preview_revision.wrapping_add(1);
        // `preview_revision` also invalidates the viewport-specific detail crop,
        // but panning and pinching do not change developed pixels. Carry the
        // rendered-content marker forward so `sync_original_preview` does not
        // dispatch the full RAW compute graph for every navigation sample.
        if rendered_content_was_current {
            self.original_preview_rendered_state =
                Some((self.original_preview_requested, self.preview_revision));
        }
        self.preview_detail_urgent = edit_was_pending;
        self.preview_motion_at = Some(Instant::now());
        if edit_was_pending {
            self.egui_ctx.request_repaint();
        } else {
            self.egui_ctx
                .request_repaint_after(zoom_detail_idle_delay());
        }
    }

    pub(crate) fn queue_preview_processing(&mut self, stage: ProcessingStage) {
        self.pending_stage = Some(match self.pending_stage {
            Some(existing) => existing.min(stage),
            None => stage,
        });

        if self.preview_zoom > DETAIL_ZOOM_START {
            self.preview_detail_pending_stage = Some(match self.preview_detail_pending_stage {
                Some(existing) => existing.min(stage),
                None => stage,
            });
            self.preview_detail_urgent = true;
        }

        if self.preview_zoom > DETAIL_ZOOM_START {
            self.navigation_pending_stage = Some(match self.navigation_pending_stage {
                Some(existing) => existing.min(stage),
                None => stage,
            });
        }

        self.notice = None;
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn original_preview_visible(&self) -> bool {
        self.original_preview_requested
    }

    pub(crate) fn set_original_preview_requested(&mut self, requested: bool) {
        if self.original_preview_requested == requested {
            return;
        }
        self.original_preview_requested = requested;
        self.original_preview_rendered_state = None;
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn toggle_original_preview(&mut self) {
        self.set_original_preview_requested(!self.original_preview_requested);
    }

    pub(crate) fn sync_original_preview(&mut self, frame: &eframe::Frame) {
        let requested_state = (self.original_preview_requested, self.preview_revision);
        if self.original_preview_rendered_state == Some(requested_state) {
            return;
        }

        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let empty_masks = MaskStack::default();
        let exposure = if self.original_preview_requested {
            &self.original_preview_exposure
        } else {
            &self.target_exposure
        };
        let masks = if self.original_preview_requested {
            &empty_masks
        } else {
            &self.masks
        };
        let inpaint = if self.original_preview_requested {
            None
        } else {
            self.inpaint_layer.as_ref()
        };
        let mut textures_to_retire = Vec::new();

        // The main preview is the durable interactive surface. Optional zoom
        // pipelines are caches: a failed cache upload must not make inpainting or
        // original-preview toggling fail globally. Drop only the failed optional
        // cache and let its normal scheduler rebuild it.
        if let (Some(raw), Some(pipeline)) = (&self.preview_raw, &self.gpu_pipeline) {
            if let Err(error) = pipeline.update_inpaint_layer(
                &render_state.queue,
                inpaint,
                0,
                0,
                raw.width,
                raw.height,
            ) {
                self.original_preview_rendered_state = None;
                self.pending_stage = Some(ProcessingStage::Output);
                self.notice = Some(
                    "Could not update preview inpainting. The last complete preview is still shown."
                        .to_owned(),
                );
                crate::diagnostics::record(format!(
                    "main preview inpaint upload failed; rendered revision remains dirty: {error:#}"
                ));
                self.egui_ctx.request_repaint();
                return;
            }
        }

        let navigation_upload_error = self.preview_navigation.as_ref().and_then(|navigation| {
            navigation
                .pipeline
                .update_inpaint_layer(
                    &render_state.queue,
                    inpaint,
                    0,
                    0,
                    navigation.raw.width,
                    navigation.raw.height,
                )
                .err()
        });
        if let Some(error) = navigation_upload_error {
            crate::diagnostics::record(format!(
                "discarding navigation preview after inpaint upload failure: {error:#}"
            ));
            if let Some(old) = self.preview_navigation.take() {
                if let Some(texture_id) = old.pipeline.egui_texture_id {
                    textures_to_retire.push(texture_id);
                }
            }
            self.navigation_pending_stage = Some(ProcessingStage::Output);
        }

        let detail_upload_error = self
            .preview_detail
            .as_ref()
            .filter(|detail| detail.revision == self.preview_revision)
            .and_then(|detail| {
                detail
                    .pipeline
                    .update_inpaint_layer(
                        &render_state.queue,
                        inpaint,
                        detail.virtual_origin[0],
                        detail.virtual_origin[1],
                        detail.virtual_full_size[0],
                        detail.virtual_full_size[1],
                    )
                    .err()
            });
        if let Some(error) = detail_upload_error {
            crate::diagnostics::record(format!(
                "discarding zoom detail after inpaint upload failure: {error:#}"
            ));
            if let Some(old) = self.preview_detail.take() {
                if let Some(texture_id) = old.pipeline.egui_texture_id {
                    textures_to_retire.push(texture_id);
                }
            }
            self.preview_motion_at = Some(Instant::now());
            self.preview_detail_pending_stage = Some(ProcessingStage::Output);
            self.preview_detail_urgent = true;
        }

        if let (Some(raw), Some(pipeline)) = (&self.preview_raw, &self.gpu_pipeline) {
            let params = GpuParams::new(exposure, masks, raw).with_vignette_geometry(self.geometry);
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
        }
        if let Some(navigation) = self.preview_navigation.as_ref() {
            let params = GpuParams::new(exposure, masks, &navigation.raw)
                .with_vignette_geometry(self.geometry);
            navigation
                .pipeline
                .recompute(&render_state.queue, &render_state.device, &params);
        }
        if let Some(detail) = self
            .preview_detail
            .as_ref()
            .filter(|detail| detail.revision == self.preview_revision)
        {
            let mut params = GpuParams::new_for_tile(
                exposure,
                masks,
                &detail.raw,
                detail.virtual_origin[0],
                detail.virtual_origin[1],
                detail.virtual_full_size[0],
                detail.virtual_full_size[1],
            )
            .with_vignette_geometry(self.geometry);
            if let Some(full_raw) = self.loaded_raw.as_ref() {
                let mask_region = detail_mask_source_region(
                    masks,
                    detail.source_origin,
                    detail.source_size,
                    full_raw.width,
                    full_raw.height,
                );
                params = params.with_mask_uv_rect_and_extent(
                    mask_source_region_uv(mask_region, full_raw.width, full_raw.height),
                    mask_region_texture_extent(mask_region, detail.pipeline.mask_atlas_edge()),
                );
            }
            detail
                .pipeline
                .recompute(&render_state.queue, &render_state.device, &params);
        }
        for texture_id in textures_to_retire {
            self.retire_egui_texture(texture_id);
        }

        self.original_preview_rendered_state = Some(requested_state);
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn preview_base_pipeline(&self) -> Option<&RawGpuPipeline> {
        self.gpu_pipeline.as_ref()
    }

    pub(crate) fn preview_is_preparing(&self) -> bool {
        self.load_receiver.is_some()
            || self.ai_denoise_receiver.is_some()
            || self.preview_rebuild_receiver.is_some()
            || self.preview_quality_dirty
    }

    pub(crate) fn preview_quality_changed(&mut self) {
        self.persist_performance_settings();
        if self.loaded_raw.is_some() || self.load_receiver.is_some() {
            self.preview_quality_dirty = true;
            self.note_preview_motion();
        }
    }

    fn requested_preview_edge_for_source(&self, full_raw: &LoadedRaw) -> u32 {
        let fallback = self
            .preview_quality
            .proxy_edge_for_viewport(self.preview_viewport_pixels);
        if self.preview_zoom > DETAIL_ZOOM_START {
            return self
                .preview_raw
                .as_ref()
                .map(|raw| raw.width.max(raw.height))
                .unwrap_or(fallback)
                .min(full_raw.width.max(full_raw.height));
        }

        let span_x = (self.preview_visible_uv.max[0] - self.preview_visible_uv.min[0])
            .abs()
            .clamp(1.0 / full_raw.width.max(1) as f32, 1.0);
        let span_y = (self.preview_visible_uv.max[1] - self.preview_visible_uv.min[1])
            .abs()
            .clamp(1.0 / full_raw.height.max(1) as f32, 1.0);
        let [viewport_width, viewport_height] = self.preview_viewport_pixels.map(|value| value.max(1));
        let quarter_turn = self.geometry.quarter_turns % 2 == 1;
        let (display_for_source_x, display_for_source_y) = if quarter_turn {
            (viewport_height, viewport_width)
        } else {
            (viewport_width, viewport_height)
        };
        let source_x = full_raw.width.max(1) as f64 * f64::from(span_x);
        let source_y = full_raw.height.max(1) as f64 * f64::from(span_y);
        let source_to_display = (f64::from(display_for_source_x) / source_x)
            .max(f64::from(display_for_source_y) / source_y);
        let requested = (full_raw.width.max(full_raw.height) as f64
            * source_to_display
            * f64::from(self.preview_quality.pixel_scale()))
        .ceil() as u32;
        fallback
            .max(requested.saturating_add(6))
            .min(full_raw.width.max(full_raw.height))
    }

    pub(crate) fn preview_source_region_changed(&mut self) {
        if self.preview_zoom > DETAIL_ZOOM_START {
            return;
        }
        if let (Some(full_raw), Some(preview_raw)) =
            (self.loaded_raw.as_ref(), self.preview_raw.as_ref())
        {
            let target = self.requested_preview_edge_for_source(full_raw);
            if preview_raw.width.max(preview_raw.height).saturating_add(5) < target {
                self.preview_quality_dirty = true;
            }
        }
    }

    pub(crate) fn set_preview_viewport_pixels(&mut self, viewport_pixels: [u32; 2]) -> bool {
        if self.preview_viewport_pixels == viewport_pixels {
            return false;
        }
        self.preview_viewport_pixels = viewport_pixels;

        if self.load_receiver.is_some() {
            self.preview_quality_dirty = true;
        }

        if let (Some(full_raw), Some(preview_raw)) =
            (self.loaded_raw.as_ref(), self.preview_raw.as_ref())
        {
            let target_edge = self.requested_preview_edge_for_source(full_raw);
            let current_edge = preview_raw.width.max(preview_raw.height);
            if current_edge.saturating_add(5) < target_edge {
                self.preview_quality_dirty = true;
            }
        }
        true
    }

    fn upload_preview_masks(
        pipeline: &RawGpuPipeline,
        queue: &wgpu::Queue,
        masks: &MaskStack,
        raw: &LoadedRaw,
    ) -> Result<(), String> {
        let edge = pipeline.mask_atlas_edge();
        for layer in 0..masks.masks.len().min(MAX_LOCAL_MASKS) {
            let layer_started = std::time::Instant::now();
            let bytes = masks.rasterize_layer_f16(layer, edge, edge, raw.width, raw.height);
            let raster_elapsed = layer_started.elapsed();
            pipeline
                .update_mask_layer(queue, layer, &bytes)
                .map_err(|error| format!("Could not update preview mask: {error:#}"))?;
            crate::diagnostics::record(format!(
                "Preview mask layer {} rasterized/uploaded in {:.3}s (raster {:.3}s)",
                layer + 1,
                layer_started.elapsed().as_secs_f64(),
                raster_elapsed.as_secs_f64()
            ));
        }
        Ok(())
    }

    fn upload_detail_masks(
        pipeline: &RawGpuPipeline,
        queue: &wgpu::Queue,
        masks: &MaskStack,
        full_raw: &LoadedRaw,
        region: [u32; 4],
        dirty_layers: Option<&[bool; MAX_LOCAL_MASKS]>,
    ) -> Result<(), String> {
        let cropped = masks.cropped_for_region(
            region[0],
            region[1],
            region[2],
            region[3],
            full_raw.width,
            full_raw.height,
        );
        let edge = pipeline.mask_atlas_edge();
        let extent = mask_region_texture_extent(region, edge);
        for layer in 0..masks.masks.len().min(MAX_LOCAL_MASKS) {
            if dirty_layers.is_some_and(|dirty| !dirty[layer]) {
                continue;
            }
            let bytes = cropped.rasterize_layer_f16(
                layer,
                extent[0],
                extent[1],
                region[2],
                region[3],
            );
            pipeline
                .update_mask_layer_region(queue, layer, extent[0], extent[1], &bytes)
                .map_err(|error| format!("Could not update zoomed local mask: {error:#}"))?;
        }
        Ok(())
    }

    fn poll_preview_rebuild_worker(&mut self, frame: &eframe::Frame) {
        let received = self
            .preview_rebuild_receiver
            .as_ref()
            .map(std::sync::mpsc::Receiver::try_recv);
        let event = match received {
            Some(Ok(event)) => Some(event),
            Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => {
                self.preview_rebuild_receiver = None;
                self.preview_quality_dirty = false;
                self.notice = Some("Preview rebuild worker stopped unexpectedly.".to_owned());
                None
            }
            Some(Err(std::sync::mpsc::TryRecvError::Empty)) | None => None,
        };
        let Some(PreviewRebuildEvent::Finished(result)) = event else {
            return;
        };
        self.preview_rebuild_receiver = None;
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.preview_quality_dirty = false;
                self.notice = Some(format!("Could not prepare the preview proxy: {error}"));
                return;
            }
        };
        let source_is_current = self
            .loaded_raw
            .as_ref()
            .is_some_and(|raw| Arc::ptr_eq(raw, &prepared.source_raw));
        if !source_is_current
            || prepared.ai_enabled != self.exposure.ai_denoise_enabled
        {
            self.preview_quality_dirty |= source_is_current;
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            self.preview_quality_dirty = true;
            return;
        };
        let params = GpuParams::new(&self.exposure, &self.masks, &prepared.preview_raw)
            .with_vignette_geometry(self.geometry);
        let program_template = self
            .gpu_pipeline
            .as_ref()
            .map(RawGpuPipeline::program_template)
            .or_else(|| self.preview_program_template.clone());
        if let Some(template) = program_template.as_ref() {
            self.preview_program_template = Some(template.clone());
        }

        // GPU objects are deliberately created here, after the CPU-only worker
        // result has been accepted for the current document. An abandoned
        // worker can therefore never contend with the next RAW open. Keep the
        // current preview resident for a seamless swap whenever the budget
        // permits it; compiled programs are reused in either path.
        let build_pipeline = || {
            if let Some(template) = program_template.as_ref() {
                RawGpuPipeline::new_headless_reusing_program_template(
                    &render_state.device,
                    &render_state.queue,
                    &prepared.preview_raw,
                    &params,
                    ProcessingQuality::Preview,
                    template,
                )
            } else {
                RawGpuPipeline::new_headless_with_quality(
                    &render_state.device,
                    &render_state.queue,
                    &prepared.preview_raw,
                    &params,
                    ProcessingQuality::Preview,
                )
            }
        };
        let pipeline_started = Instant::now();
        let mut pipeline_result = build_pipeline();
        let needs_in_place_replacement = self.gpu_pipeline.is_some()
            && pipeline_result.as_ref().err().is_some_and(|error| {
                error
                    .to_string()
                    .contains("GPU pipelines already reserve")
            });
        if needs_in_place_replacement {
            crate::diagnostics::record(
                "DPI preview replacement exceeded coexistence budget; released old graph and reused its compiled programs",
            );
            let previous = {
                let mut renderer = render_state.renderer.write();
                self.take_preview_pipeline_and_release_textures(&mut renderer)
            };
            drop(previous);
            pipeline_result = build_pipeline();
        }
        let mut pipeline = match pipeline_result {
            Ok(pipeline) => pipeline,
            Err(error) => {
                self.preview_quality_dirty = false;
                self.notice = Some(format!("Could not rebuild the GPU preview: {error:#}"));
                return;
            }
        };
        crate::diagnostics::record(format!(
            "DPI preview GPU graph prepared on the UI thread in {:.3}s",
            pipeline_started.elapsed().as_secs_f64()
        ));
        #[cfg(not(target_os = "android"))]
        if let Err(error) = self.apply_display_output_transform(&render_state.queue, &pipeline)
        {
            self.notice = Some(format!("Could not prepare the preview color profile: {error:#}"));
            self.preview_quality_dirty = false;
            return;
        }
        if let Err(error) = Self::upload_preview_masks(
            &pipeline,
            &render_state.queue,
            &self.masks,
            &prepared.preview_raw,
        ) {
            self.notice = Some(error);
            self.preview_quality_dirty = false;
            return;
        }
        if let Err(error) = pipeline.update_inpaint_layer(
            &render_state.queue,
            self.inpaint_layer.as_ref(),
            0,
            0,
            prepared.preview_raw.width,
            prepared.preview_raw.height,
        ) {
            self.notice = Some(format!("Could not rebuild preview inpainting: {error:#}"));
            self.preview_quality_dirty = false;
            return;
        }
        pipeline.recompute(&render_state.queue, &render_state.device, &params);
        let previous = {
            let mut renderer = render_state.renderer.write();
            let previous = self.take_preview_pipeline_and_release_textures(&mut renderer);
            pipeline.register_egui_texture(&render_state.device, &mut renderer);
            previous
        };
        drop(previous);

        self.preview_program_template = Some(pipeline.program_template());
        self.preview_raw = Some(prepared.preview_raw);
        self.gpu_pipeline = Some(pipeline);
        #[cfg(target_os = "android")]
        {
            // Keep the mobile lens toggle cache coherent with the newly
            // selected DPI proxy. Without this, the next lens toggle can
            // restore the lower-resolution proxy that preceded this rebuild.
            if self.lens_correction.applied {
                if let (Some(selection), Some(full_raw), Some(preview_raw)) = (
                    self.lens_correction.selected_lens(),
                    self.loaded_raw.as_ref(),
                    self.preview_raw.as_ref(),
                ) {
                    self.lens_corrected_preview_cache = Some((
                        selection,
                        prepared.quality,
                        Arc::clone(full_raw),
                        Arc::clone(preview_raw),
                    ));
                }
            } else if let Some(preview_raw) = self.preview_raw.as_ref() {
                self.lens_original_preview_cache =
                    Some((prepared.quality, Arc::clone(preview_raw)));
            }
        }
        self.inpaint_source_cache = None;
        self.target_exposure = self.exposure;
        self.pending_stage = None;
        self.preview_detail_pending_stage = None;
        self.navigation_pending_stage = None;
        self.preview_detail_urgent = false;
        self.dirty_mask_layers.fill(false);
        self.detail_dirty_mask_layers.fill(false);
        self.navigation_dirty_mask_layers.fill(false);
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.preview_motion_at = (self.preview_zoom > DETAIL_ZOOM_START).then(Instant::now);
        if let (Some(full), Some(preview)) = (&self.loaded_raw, &self.preview_raw) {
            self.image_status = format!(
                "{} {} — full {}×{}, preview {}×{} ({})",
                full.camera_make,
                full.camera_model,
                full.width,
                full.height,
                preview.width,
                preview.height,
                prepared.quality.label(),
            );
            let latest_edge = self.requested_preview_edge_for_source(full);
            if prepared.quality != self.preview_quality
                || preview.width.max(preview.height).saturating_add(5) < latest_edge
            {
                self.preview_quality_dirty = true;
            }
        }
        crate::diagnostics::record(format!(
            "DPI preview rebuild installed: edge {} -> {}x{} ({})",
            prepared.requested_edge,
            self.preview_raw.as_ref().map_or(0, |raw| raw.width),
            self.preview_raw.as_ref().map_or(0, |raw| raw.height),
            prepared.quality.label(),
        ));
    }

    fn apply_pending_preview_quality(&mut self, _frame: &eframe::Frame) {
        if self.preview_rebuild_receiver.is_some() {
            return;
        }
        if !self.preview_quality_dirty
            || self.load_receiver.is_some()
            || self.lens_correction_busy()
        {
            return;
        }
        #[cfg(target_os = "android")]
        if self.export_receiver.is_some()
            || self.export_publish_pending
            || self.library_batch_export.is_some()
            || self.ai_denoise_receiver.is_some()
        {
            return;
        }
        let Some(source_raw) = self.loaded_raw.as_ref().map(Arc::clone) else {
            self.preview_quality_dirty = false;
            return;
        };
        let requested_edge = self.requested_preview_edge_for_source(&source_raw);
        let quality = self.preview_quality;
        let ai_enabled = self.exposure.ai_denoise_enabled;

        // Viewport measurements often settle during the same frame in which a
        // RAW is installed. Do not rebuild an already-sharp proxy merely because
        // that measurement set the dirty bit. This also makes fit -> zoom -> fit
        // a no-op for the main surface.
        let current_is_sufficient = self
            .preview_raw
            .as_ref()
            .zip(self.gpu_pipeline.as_ref())
            .is_some_and(|(raw, pipeline)| {
                raw.width.max(raw.height).saturating_add(5) >= requested_edge
                    && pipeline.immutable_ai_source_matches(source_raw.cfa_kind, ai_enabled)
            });
        if current_is_sufficient {
            self.preview_quality_dirty = false;
            return;
        }

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
        let (sender, receiver) = std::sync::mpsc::channel();
        let context = self.egui_ctx.clone();
        let worker_source = Arc::clone(&source_raw);
        let spawn = std::thread::Builder::new()
            .name("auraw-preview-rebuild".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let preview_raw = if worker_source.width.max(worker_source.height)
                        <= requested_edge
                    {
                        Arc::clone(&worker_source)
                    } else {
                        Arc::new(build_proxy(
                            &worker_source,
                            ProxySpec {
                                max_edge: requested_edge,
                            },
                        ))
                    };
                    Ok::<_, anyhow::Error>(PreparedPreviewRebuild {
                        source_raw: worker_source,
                        preview_raw,
                        quality,
                        requested_edge,
                        ai_enabled,
                    })
                }))
                .unwrap_or_else(|panic| {
                    let message = panic
                        .downcast_ref::<&str>()
                        .copied()
                        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                        .unwrap_or("unknown panic");
                    Err(anyhow::anyhow!("preview rebuild panicked: {message}"))
                })
                .map_err(|error| format!("{error:#}"));
                let _ = sender.send(PreviewRebuildEvent::Finished(result));
                context.request_repaint();
            });
        match spawn {
            Ok(_) => {
                self.preview_quality_dirty = false;
                self.preview_rebuild_receiver = Some(receiver);
                self.egui_ctx.request_repaint_after(Duration::from_millis(50));
            }
            Err(error) => {
                self.notice = Some(format!("Could not start preview rebuild: {error}"));
            }
        }
    }

    fn advance_preview_detail(&mut self, frame: &eframe::Frame) {
        let idle_delay = zoom_detail_idle_delay();
        if self.preview_zoom <= DETAIL_ZOOM_START {
            if frame.wgpu_render_state().is_some() {
                if let Some(old) = self.preview_detail.take() {
                    if let Some(texture_id) = old.pipeline.egui_texture_id {
                        self.retire_egui_texture(texture_id);
                    }
                }
            }
            self.preview_motion_at = None;
            self.preview_detail_pending_stage = None;
            self.preview_detail_urgent = false;
            return;
        }
        // Building and installing a detail crop is intentionally deferred until
        // both fingers are lifted. A stationary pause during a pinch must not run
        // CPU proxy preparation and GPU pipeline setup on the UI thread.
        if self.preview_touch_navigation_active {
            return;
        }
        if self.active_tab != AppTab::Develop
            || self.preview_quality_dirty
            || self.lens_correction_dirty
            || self.lens_correction_busy()
            || self.load_receiver.is_some()
            || self.ai_denoise_receiver.is_some()
        {
            return;
        }

        let detail_is_current = self.preview_detail_is_current();
        if detail_is_current {
            return;
        }

        let urgent = self.preview_detail_urgent;
        if !urgent {
            let Some(motion_at) = self.preview_motion_at else {
                return;
            };
            let elapsed = motion_at.elapsed();
            if elapsed < idle_delay {
                self.egui_ctx.request_repaint_after(idle_delay - elapsed);
                return;
            }
        }

        let Some(full_raw) = self.loaded_raw.as_ref().map(Arc::clone) else {
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.egui_ctx.request_repaint();
            return;
        };

        self.preview_motion_at = None;
        self.preview_detail_urgent = false;
        self.preview_detail_pending_stage = None;
        let visible = self.preview_visible_uv;

        let cfa_period = match full_raw.cfa_kind {
            crate::pipeline::CfaKind::Bayer => 2,
            crate::pipeline::CfaKind::XTrans => 6,
        };
        let (x0, x1) = aligned_detail_axis(
            visible.min[0],
            visible.max[0],
            full_raw.width,
            cfa_period,
            self.preview_viewport_pixels[0],
            self.preview_quality.detail_pixel_scale(),
        );
        let (y0, y1) = aligned_detail_axis(
            visible.min[1],
            visible.max[1],
            full_raw.height,
            cfa_period,
            self.preview_viewport_pixels[1],
            self.preview_quality.detail_pixel_scale(),
        );
        let crop_width = x1 - x0;
        let crop_height = y1 - y0;
        let crop_uv = PreviewUvRect {
            min: [
                x0 as f32 / full_raw.width as f32,
                y0 as f32 / full_raw.height as f32,
            ],
            max: [
                x1 as f32 / full_raw.width as f32,
                y1 as f32 / full_raw.height as f32,
            ],
        };
        let texture_uv_rect = detail_texture_uv(visible, crop_uv);
        let detail_spec = ProxySpec {
            max_edge: requested_detail_edge(
                self.preview_quality,
                self.preview_viewport_pixels,
                visible,
                crop_width,
                crop_height,
                full_raw.width,
                full_raw.height,
            ),
        };
        let detail_raw = Arc::new(build_region_proxy(
            &full_raw,
            x0,
            y0,
            crop_width,
            crop_height,
            detail_spec,
        ));
        // Keep mask atlases in full-image coordinates to avoid double-applying crop offsets.
        let virtual_full_width =
            ((detail_raw.width as f64 * full_raw.width as f64 / crop_width as f64).round() as u32)
                .max(detail_raw.width);
        let virtual_full_height = ((detail_raw.height as f64 * full_raw.height as f64
            / crop_height as f64)
            .round() as u32)
            .max(detail_raw.height);
        let virtual_origin_x =
            (x0 as f64 / full_raw.width as f64 * virtual_full_width as f64).round() as i32;
        let virtual_origin_y =
            (y0 as f64 / full_raw.height as f64 * virtual_full_height as f64).round() as i32;
        let mask_region = detail_mask_source_region(
            &self.masks,
            [x0, y0],
            [crop_width, crop_height],
            full_raw.width,
            full_raw.height,
        );
        let params = GpuParams::new_for_tile(
            &self.target_exposure,
            &self.masks,
            &detail_raw,
            virtual_origin_x,
            virtual_origin_y,
            virtual_full_width,
            virtual_full_height,
        )
        .with_vignette_geometry(self.geometry)
        .with_mask_uv_rect_and_extent(
            mask_source_region_uv(mask_region, full_raw.width, full_raw.height),
            mask_region_texture_extent(mask_region, detail_mask_edge()),
        );
        // Prefer the normal proxy whenever its tone statistics are still current.
        let normal_tone_is_current = !matches!(
            self.pending_stage,
            Some(ProcessingStage::Raw | ProcessingStage::Tone)
        );
        let full_frame_tone_pipeline = if normal_tone_is_current {
            self.gpu_pipeline.as_ref().or_else(|| {
                self.preview_navigation
                    .as_ref()
                    .map(|preview| &preview.pipeline)
            })
        } else {
            self.preview_navigation
                .as_ref()
                .map(|preview| &preview.pipeline)
                .or(self.gpu_pipeline.as_ref())
        };
        let required_mask_layers = self.masks.masks.len().max(1);
        if let Some(detail) = self.preview_detail.as_mut().filter(|detail| {
            detail.pipeline.width == detail_raw.width
                && detail.pipeline.height == detail_raw.height
                && detail.pipeline.mask_layer_capacity() >= required_mask_layers
        }) {
            if let Err(error) = detail
                .pipeline
                .upload_raw_tile(&render_state.queue, &detail_raw)
            {
                self.notice = Some(format!(
                    "Could not update the zoomed preview crop: {error:#}"
                ));
                return;
            }
            // The atlas is viewport-local. A moved crop therefore needs fresh
            // mask pixels even when the underlying geometry did not change.
            if let Err(error) = Self::upload_detail_masks(
                &detail.pipeline,
                &render_state.queue,
                &self.masks,
                &full_raw,
                mask_region,
                None,
            ) {
                self.notice = Some(error);
                return;
            }
            detail.pipeline.dispatch_stage(
                &render_state.queue,
                &render_state.device,
                &params,
                ProcessingStage::Raw,
            );
            detail.pipeline.dispatch_stage(
                &render_state.queue,
                &render_state.device,
                &params,
                ProcessingStage::Tone,
            );
            if let Some(full_frame) = full_frame_tone_pipeline {
                detail.pipeline.inherit_tone_statistics(
                    &render_state.queue,
                    &render_state.device,
                    full_frame,
                );
            }
            if let Err(error) = detail.pipeline.update_inpaint_layer(
                &render_state.queue,
                self.inpaint_layer.as_ref(),
                virtual_origin_x,
                virtual_origin_y,
                virtual_full_width,
                virtual_full_height,
            ) {
                self.notice = Some(format!("Could not update zoomed inpainting: {error:#}"));
                return;
            }
            detail.pipeline.dispatch_stage(
                &render_state.queue,
                &render_state.device,
                &params,
                ProcessingStage::Output,
            );
            detail.uv_rect = visible;
            detail.texture_uv_rect = texture_uv_rect;
            detail.revision = self.preview_revision;
            detail.raw = Arc::clone(&detail_raw);
            detail.source_origin = [x0, y0];
            detail.source_size = [crop_width, crop_height];
            detail.mask_source_region = mask_region;
            detail.virtual_origin = [virtual_origin_x, virtual_origin_y];
            detail.virtual_full_size = [virtual_full_width, virtual_full_height];
            self.detail_dirty_mask_layers.fill(false);
            self.egui_ctx.request_repaint();
            return;
        }

        let Some(program_template) = self.gpu_pipeline.as_ref() else {
            return;
        };
        let mut pipeline = match RawGpuPipeline::new_headless_reusing_programs_with_mask_edge(
            &render_state.device,
            &render_state.queue,
            &detail_raw,
            &params,
            ProcessingQuality::Preview,
            program_template,
            detail_mask_edge(),
        ) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                self.notice = Some(format!("Could not render the zoomed preview: {error:#}"));
                return;
            }
        };
        #[cfg(not(target_os = "android"))]
        if let Err(error) = self.apply_display_output_transform(&render_state.queue, &pipeline) {
            self.notice = Some(
                "Could not prepare the preview color profile. The previous complete preview remains available."
                    .to_owned(),
            );
            crate::diagnostics::record(format!(
                "preview pipeline display-profile install failed: {error:#}"
            ));
            return;
        }
        if let Err(error) = Self::upload_detail_masks(
            &pipeline,
            &render_state.queue,
            &self.masks,
            &full_raw,
            mask_region,
            None,
        ) {
            self.notice = Some(error);
            return;
        }
        pipeline.dispatch_stage(
            &render_state.queue,
            &render_state.device,
            &params,
            ProcessingStage::Raw,
        );
        pipeline.dispatch_stage(
            &render_state.queue,
            &render_state.device,
            &params,
            ProcessingStage::Tone,
        );
        if let Some(full_frame) = full_frame_tone_pipeline {
            pipeline.inherit_tone_statistics(&render_state.queue, &render_state.device, full_frame);
        }
        if let Err(error) = pipeline.update_inpaint_layer(
            &render_state.queue,
            self.inpaint_layer.as_ref(),
            virtual_origin_x,
            virtual_origin_y,
            virtual_full_width,
            virtual_full_height,
        ) {
            self.notice = Some(format!("Could not update zoomed inpainting: {error:#}"));
            return;
        }
        pipeline.dispatch_stage(
            &render_state.queue,
            &render_state.device,
            &params,
            ProcessingStage::Output,
        );

        let mut renderer = render_state.renderer.write();
        if let Some(old) = self.preview_detail.take() {
            if let Some(texture_id) = old.pipeline.egui_texture_id {
                self.retire_egui_texture(texture_id);
            }
        }
        pipeline.register_egui_texture(&render_state.device, &mut renderer);
        drop(renderer);

        self.preview_detail = Some(PreviewDetail {
            pipeline,
            uv_rect: visible,
            texture_uv_rect,
            revision: self.preview_revision,
            raw: detail_raw,
            source_origin: [x0, y0],
            source_size: [crop_width, crop_height],
            mask_source_region: mask_region,
            virtual_origin: [virtual_origin_x, virtual_origin_y],
            virtual_full_size: [virtual_full_width, virtual_full_height],
        });
        self.detail_dirty_mask_layers.fill(false);
        self.egui_ctx.request_repaint();
    }

    fn advance_navigation_preview(&mut self, frame: &eframe::Frame) {
        if self.ai_denoise_receiver.is_some() {
            return;
        }
        let zoomed = self.preview_zoom > DETAIL_ZOOM_START;
        let should_update = self.navigation_pending_stage.is_some();
        // The normal preview is already a complete full-frame fallback while the
        // user only zooms or pans. Create the tiny navigation pipeline lazily when
        // an actual edit needs its fast full-frame update, then retain it until fit.
        // Eager creation here caused a visible hitch on the first pinch frame.
        let should_exist = zoomed && (self.preview_navigation.is_some() || should_update);
        if !should_exist && !should_update {
            // Release the navigation proxy when fit view is stable.
            if frame.wgpu_render_state().is_some() {
                if let Some(old) = self.preview_navigation.take() {
                    if let Some(texture_id) = old.pipeline.egui_texture_id {
                        self.retire_egui_texture(texture_id);
                    }
                }
            } else {
                self.preview_navigation = None;
            }
            return;
        }
        let Some(full_raw) = self.loaded_raw.as_ref().map(Arc::clone) else {
            self.navigation_pending_stage = None;
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        let navigation_capacity_stale = self.preview_navigation.as_ref().is_some_and(|preview| {
            preview.pipeline.mask_layer_capacity() < self.masks.masks.len().max(1)
        });
        if navigation_capacity_stale {
            if let Some(old) = self.preview_navigation.take() {
                if let Some(texture_id) = old.pipeline.egui_texture_id {
                    self.retire_egui_texture(texture_id);
                }
            }
        }

        if self.preview_navigation.is_none() {
            if !should_exist {
                self.navigation_pending_stage = None;
                return;
            }
            let raw = if full_raw.width.max(full_raw.height) <= navigation_proxy_edge() {
                Arc::clone(&full_raw)
            } else {
                Arc::new(build_proxy(
                    &full_raw,
                    ProxySpec {
                        max_edge: navigation_proxy_edge(),
                    },
                ))
            };
            let params = GpuParams::new(&self.target_exposure, &self.masks, &raw)
                .with_vignette_geometry(self.geometry);
            let Some(template) = self.gpu_pipeline.as_ref() else {
                return;
            };
            let mut pipeline = match RawGpuPipeline::new_headless_reusing_programs_with_mask_edge(
                &render_state.device,
                &render_state.queue,
                &raw,
                &params,
                ProcessingQuality::Preview,
                template,
                navigation_mask_edge(),
            ) {
                Ok(pipeline) => pipeline,
                Err(error) => {
                    self.notice = Some(format!(
                        "Could not prepare the adjusted navigation preview: {error:#}"
                    ));
                    return;
                }
            };
            #[cfg(not(target_os = "android"))]
            if let Err(error) = self.apply_display_output_transform(&render_state.queue, &pipeline) {
                self.notice = Some(
                    "Could not prepare the preview color profile. The previous complete preview remains available."
                        .to_owned(),
                );
                crate::diagnostics::record(format!(
                    "preview pipeline display-profile install failed: {error:#}"
                ));
                return;
            }
            if let Err(error) =
                Self::upload_preview_masks(&pipeline, &render_state.queue, &self.masks, &raw)
            {
                self.notice = Some(error);
                return;
            }
            if let Err(error) = pipeline.update_inpaint_layer(
                &render_state.queue,
                self.inpaint_layer.as_ref(),
                0,
                0,
                raw.width,
                raw.height,
            ) {
                self.notice = Some(format!("Could not update navigation inpainting: {error:#}"));
                return;
            }
            pipeline.recompute(&render_state.queue, &render_state.device, &params);
            let mut renderer = render_state.renderer.write();
            pipeline.register_egui_texture(&render_state.device, &mut renderer);
            drop(renderer);
            self.preview_navigation = Some(PreviewNavigation { pipeline, raw });
            self.navigation_pending_stage = None;
            self.navigation_dirty_mask_layers.fill(false);
            self.egui_ctx.request_repaint();
            return;
        }

        let Some(stage) = self.navigation_pending_stage else {
            return;
        };
        let Some(preview) = self.preview_navigation.as_mut() else {
            return;
        };
        if self.navigation_dirty_mask_layers.iter().any(|dirty| *dirty) {
            let edge = preview.pipeline.mask_atlas_edge();
            for layer in 0..MAX_LOCAL_MASKS {
                if !self.navigation_dirty_mask_layers[layer] {
                    continue;
                }
                let bytes = self.masks.rasterize_layer_f16(
                    layer,
                    edge,
                    edge,
                    preview.raw.width,
                    preview.raw.height,
                );
                if let Err(error) =
                    preview
                        .pipeline
                        .update_mask_layer(&render_state.queue, layer, &bytes)
                {
                    self.notice = Some(format!(
                        "Could not update the navigation local mask: {error:#}"
                    ));
                    return;
                }
                self.navigation_dirty_mask_layers[layer] = false;
            }
        }

        if let Err(error) = preview.pipeline.update_inpaint_layer(
            &render_state.queue,
            self.inpaint_layer.as_ref(),
            0,
            0,
            preview.raw.width,
            preview.raw.height,
        ) {
            self.notice = Some(format!("Could not update navigation inpainting: {error:#}"));
            return;
        }
        let params = GpuParams::new(&self.target_exposure, &self.masks, &preview.raw)
            .with_vignette_geometry(self.geometry);
        let stages = match stage {
            ProcessingStage::Raw => &[
                ProcessingStage::Raw,
                ProcessingStage::Tone,
                ProcessingStage::Output,
            ][..],
            ProcessingStage::Tone => &[ProcessingStage::Tone, ProcessingStage::Output][..],
            ProcessingStage::Output => &[ProcessingStage::Output][..],
        };
        for stage in stages {
            preview.pipeline.dispatch_stage(
                &render_state.queue,
                &render_state.device,
                &params,
                *stage,
            );
        }
        self.navigation_pending_stage = None;
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn mark_pipeline_dirty(&mut self) {
        self.note_edit_changed();
        if self.gpu_pipeline.is_none() {
            self.target_exposure = self.exposure;
            return;
        }

        if let Some(stage) = affected_stage(&self.target_exposure, &self.exposure) {
            self.target_exposure = self.exposure;
            self.queue_preview_processing(stage);
        }
    }

    pub(crate) fn apply_white_balance_area(&mut self, area: [[f32; 2]; 2]) -> bool {
        let result = self.loaded_raw.as_ref().and_then(|raw| {
            raw.white_balance_offsets_from_area(
                area[0],
                area[1],
                self.exposure.black_point,
            )
        });
        self.white_balance_picker_active = false;
        self.white_balance_picker_drag = None;
        let Some((temperature, tint)) = result else {
            self.notice = Some(
                "Could not estimate white balance there. Choose a brighter, unclipped neutral area."
                    .to_owned(),
            );
            self.egui_ctx.request_repaint();
            return false;
        };
        self.exposure.temperature = temperature;
        self.exposure.tint = tint;
        self.notice = Some("White balance sampled from the selected image area.".to_owned());
        self.mark_pipeline_dirty();
        true
    }

    fn advance_zoomed_processing(&mut self, frame: &eframe::Frame) {
        let Some(stage) = self.preview_detail_pending_stage else {
            return;
        };
        let Some(detail) = self
            .preview_detail
            .as_ref()
            .filter(|detail| detail.revision == self.preview_revision)
        else {
            return;
        };
        if detail.pipeline.mask_layer_capacity() < self.masks.masks.len().max(1) {
            // Explicit-edge detail atlases allocate only active layers. Adding
            // another mask invalidates that small texture and rebuilds just the
            // detail pipeline on the next frame.
            if let Some(detail) = self.preview_detail.as_mut() {
                detail.revision = self.preview_revision.wrapping_sub(1);
            }
            self.preview_detail_pending_stage = None;
            self.preview_detail_urgent = true;
            self.preview_motion_at = Some(Instant::now());
            self.egui_ctx.request_repaint();
            return;
        }
        let Some(full_raw) = self.loaded_raw.as_ref() else {
            self.preview_detail_pending_stage = None;
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        let detail_raw = Arc::clone(&detail.raw);
        let virtual_origin = detail.virtual_origin;
        let virtual_full_size = detail.virtual_full_size;
        let mask_region = detail_mask_source_region(
            &self.masks,
            detail.source_origin,
            detail.source_size,
            full_raw.width,
            full_raw.height,
        );
        let params = GpuParams::new_for_tile(
            &self.target_exposure,
            &self.masks,
            &detail_raw,
            virtual_origin[0],
            virtual_origin[1],
            virtual_full_size[0],
            virtual_full_size[1],
        )
        .with_vignette_geometry(self.geometry)
        .with_mask_uv_rect_and_extent(
            mask_source_region_uv(mask_region, full_raw.width, full_raw.height),
            mask_region_texture_extent(mask_region, detail.pipeline.mask_atlas_edge()),
        );

        let normal_tone_is_current = !matches!(
            self.pending_stage,
            Some(ProcessingStage::Raw | ProcessingStage::Tone)
        );
        let full_frame_tone_pipeline = if normal_tone_is_current {
            self.gpu_pipeline.as_ref().or_else(|| {
                self.preview_navigation
                    .as_ref()
                    .map(|preview| &preview.pipeline)
            })
        } else {
            self.preview_navigation
                .as_ref()
                .map(|preview| &preview.pipeline)
                .or(self.gpu_pipeline.as_ref())
        };
        let Some(detail) = self.preview_detail.as_mut() else {
            return;
        };
        if stage == ProcessingStage::Output
            && self.detail_dirty_mask_layers.iter().any(|dirty| *dirty)
        {
            let region_changed = detail.mask_source_region != mask_region;
            if let Err(error) = Self::upload_detail_masks(
                &detail.pipeline,
                &render_state.queue,
                &self.masks,
                full_raw,
                mask_region,
                (!region_changed).then_some(&self.detail_dirty_mask_layers),
            ) {
                self.notice = Some(error);
                self.preview_detail_pending_stage = None;
                return;
            }
            detail.mask_source_region = mask_region;
            self.detail_dirty_mask_layers.fill(false);
        }

        if stage == ProcessingStage::Output {
            if let Err(error) = detail.pipeline.update_inpaint_layer(
                &render_state.queue,
                self.inpaint_layer.as_ref(),
                virtual_origin[0],
                virtual_origin[1],
                virtual_full_size[0],
                virtual_full_size[1],
            ) {
                self.notice = Some(format!("Could not update zoomed inpainting: {error:#}"));
                self.preview_detail_pending_stage = None;
                return;
            }
            if let Some(full_frame) = full_frame_tone_pipeline {
                detail.pipeline.inherit_tone_statistics(
                    &render_state.queue,
                    &render_state.device,
                    full_frame,
                );
            }
        }
        detail
            .pipeline
            .dispatch_stage(&render_state.queue, &render_state.device, &params, stage);
        self.preview_detail_pending_stage = match stage {
            ProcessingStage::Raw => Some(ProcessingStage::Tone),
            ProcessingStage::Tone => Some(ProcessingStage::Output),
            ProcessingStage::Output => None,
        };
        if self.preview_detail_pending_stage.is_none() {
            detail.revision = self.preview_revision;
            self.preview_detail_urgent = false;
        }
        self.egui_ctx.request_repaint();
    }

    fn advance_processing(&mut self, frame: &eframe::Frame) {
        if self.preview_zoom > DETAIL_ZOOM_START {
            self.advance_zoomed_processing(frame);
            if self.preview_detail_is_current() {
                return;
            }
        }

        let Some(stage) = self.pending_stage else {
            return;
        };
        let (Some(raw), Some(pipeline)) = (&self.preview_raw, &self.gpu_pipeline) else {
            self.pending_stage = None;
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        if stage == ProcessingStage::Output && self.dirty_mask_layers.iter().any(|dirty| *dirty) {
            let edge = pipeline.mask_atlas_edge();
            let mut upload_error = None;
            for layer in 0..MAX_LOCAL_MASKS {
                if !self.dirty_mask_layers[layer] {
                    continue;
                }
                let bytes = self
                    .masks
                    .rasterize_layer_f16(layer, edge, edge, raw.width, raw.height);
                if let Err(error) = pipeline.update_mask_layer(&render_state.queue, layer, &bytes) {
                    upload_error = Some(format!("Could not update local mask: {error:#}"));
                    break;
                }
                self.dirty_mask_layers[layer] = false;
            }
            if let Some(error) = upload_error {
                self.notice = Some(error);
                return;
            }
        }

        if stage == ProcessingStage::Output {
            if let Err(error) = pipeline.update_inpaint_layer(
                &render_state.queue,
                self.inpaint_layer.as_ref(),
                0,
                0,
                raw.width,
                raw.height,
            ) {
                self.notice = Some(format!("Could not update preview inpainting: {error:#}"));
                return;
            }
        }
        let params = GpuParams::new(&self.target_exposure, &self.masks, raw)
            .with_vignette_geometry(self.geometry);
        pipeline.dispatch_stage(&render_state.queue, &render_state.device, &params, stage);
        self.pending_stage = match stage {
            ProcessingStage::Raw => Some(ProcessingStage::Tone),
            ProcessingStage::Tone => Some(ProcessingStage::Output),
            ProcessingStage::Output => None,
        };
    }

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

        let default_name = self
            .current_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}-auraw.png"))
            .unwrap_or_else(|| "auraw-export.png".to_owned());
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

        let default_name = self
            .current_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}-auraw.jpg"))
            .unwrap_or_else(|| "auraw-export.jpg".to_owned());
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

        let default_name = self
            .current_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}-auraw.tif"))
            .unwrap_or_else(|| "auraw-export.tif".to_owned());
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
    fn export_android(&mut self, frame: &eframe::Frame, format: ExportFormat) {
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
    fn suspend_android_preview_for_export(
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

    fn capture_export_task_request(
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

    fn start_export(
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

    fn request_library_batch_export_cancellation(&mut self) -> bool {
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
    fn start_next_library_export(&mut self, frame: &eframe::Frame) {
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
            match &job.target {
                AndroidLibraryExportTarget::Local { uri, .. } => {
                    match crate::android::open_library_document(
                        &self.android_app,
                        uri,
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
                AndroidLibraryExportTarget::Cloud { path, .. } => {
                    self.android_batch_load_pending = true;
                    self.picker_pending = false;
                    self.notice = None;
                    self.status = format!("Opening {display_name}…");
                    self.active_tab = AppTab::Library;
                    self.open_path_labeled(
                        path.clone(),
                        display_name.clone(),
                        false,
                        crate::sidecar::SidecarTarget::Desktop {
                            raw_path: path.clone(),
                        },
                        frame,
                        None,
                    );
                    if self.load_receiver.is_none() {
                        self.android_batch_load_pending = false;
                        let error = self.notice.clone().unwrap_or_else(|| {
                            "The cloud RAW decode worker could not be started.".to_owned()
                        });
                        self.complete_android_library_batch_export_item(Err(error));
                    }
                    return;
                }
            }
        }
    }

    #[cfg(target_os = "android")]
    fn on_library_batch_load_finished(&mut self, success: bool, frame: &eframe::Frame) {
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
    fn complete_android_library_batch_export_item(&mut self, result: Result<(), String>) {
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
    fn resume_android_library_batch_export_if_possible(&mut self, frame: &eframe::Frame) {
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

    fn finish_library_batch_export(&mut self) {
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
    fn on_library_batch_load_finished(&mut self, _success: bool, _frame: &eframe::Frame) {
        // Desktop batch export owns a separate decode/export worker and never
        // consumes the document opened in Develop.
    }

    #[cfg(target_os = "android")]
    fn poll_android_export_publish(&mut self) {
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
    fn poll_library_batch_export_worker(&mut self) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.library_batch_export_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

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

    fn poll_export_worker(&mut self, _frame: &eframe::Frame) {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.export_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(event) => events.push(event),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

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

    fn refresh_status(&mut self) {
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
        // not one of the Lightroom-style Develop adjustments.
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

#[cfg(test)]
mod batch_export_progress_tests {
    use super::batch_export_overall_fraction;

    #[test]
    fn completed_images_do_not_reach_full_progress_early() {
        let progress = batch_export_overall_fraction(2, 3, false, None);
        assert!((progress - (2.0 / 3.0)).abs() < f32::EPSILON);
        assert!(progress < 1.0);
    }

    #[test]
    fn fully_rendered_current_image_reserves_finalization_progress() {
        let progress = batch_export_overall_fraction(2, 3, true, Some((10, 10)));
        assert!((progress - (2.9 / 3.0)).abs() < 0.000_01);
        assert!(progress < 1.0);
    }

    #[test]
    fn batch_reaches_one_only_after_every_image_is_finished() {
        assert_eq!(batch_export_overall_fraction(3, 3, false, None), 1.0);
    }

    #[test]
    fn stale_tile_progress_is_ignored_without_a_current_image() {
        let progress = batch_export_overall_fraction(1, 3, false, Some((10, 10)));
        assert!((progress - (1.0 / 3.0)).abs() < f32::EPSILON);
    }
}
