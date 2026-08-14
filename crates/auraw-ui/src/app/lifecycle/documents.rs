use super::*;

impl AurawApp {
    #[cfg(target_os = "android")]
    pub fn open_android_library_document(&mut self, uri: &str, display_name: &str) {
        if self.android_foreground_task_active() {
            self.notice = Some(format!(
                "{display_name} cannot be opened while an export or another foreground operation is running. Wait for it to finish or cancel it first."
            ));
            self.egui_ctx.request_repaint();
            return;
        }
        if self.picker_pending {
            return;
        }

        // Android batch export has already decoded its current item into the
        // regular Develop document before the export worker is launched. If the
        // user taps that exact item, reopening it would allocate a second preview
        // pipeline beside the export pipeline and exceed the mobile GPU budget.
        // The existing document is already the requested RAW, so just expose it.
        let already_loaded = self.loaded_raw.is_some()
            && self.preview_raw.is_some()
            && self.gpu_pipeline.is_some()
            && matches!(
                self.sidecar_target.as_ref(),
                Some(crate::sidecar::SidecarTarget::Android {
                    raw_uri,
                    display_name: current_name,
                }) if raw_uri == uri && current_name == display_name
            );
        if already_loaded {
            self.activate_tab(AppTab::Develop);
            self.notice = None;
            self.refresh_status();
            self.egui_ctx.request_repaint();
            return;
        }

        self.prepare_android_develop_loading_thumbnail(uri);

        // This is a user-owned picker result. Keep it distinct from the
        // Android batch exporter's internal document-load result routing.
        self.android_batch_load_pending = false;
        match crate::android::open_library_document(&self.android_app, uri, display_name) {
            Ok(()) => {
                self.picker_pending = true;
                self.notice = None;
                self.status = format!("Opening {display_name}…");
            }
            Err(error) => {
                self.develop_loading_thumbnail.clear();
                self.notice = Some(error);
            }
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn reload_android_library_document_after_reset(
        &mut self,
        uri: &str,
        display_name: &str,
    ) {
        if self.picker_pending {
            self.notice = Some(format!(
                "Could not reload {display_name} after resetting adjustments because another Android document operation is still pending."
            ));
            return;
        }
        // Establish ownership before invoking Java so even an unusually fast
        // result cannot be mistaken for an interactive open.
        self.android_batch_load_pending = false;
        self.pending_android_library_reset_reload = true;
        match crate::android::open_library_document(&self.android_app, uri, display_name) {
            Ok(()) => {
                self.picker_pending = true;
                self.notice = None;
                self.status = format!("Reloading {display_name} after reset…");
            }
            Err(error) => {
                self.pending_android_library_reset_reload = false;
                self.notice = Some(format!(
                    "Could not reload {display_name} after resetting adjustments: {error}"
                ));
            }
        }
    }

    pub fn open_path(&mut self, path: PathBuf, frame: &eframe::Frame) {
        let label = path.display().to_string();
        self.active_tab = AppTab::Develop;
        let sidecar_target = crate::sidecar::SidecarTarget::Desktop {
            raw_path: path.clone(),
        };
        self.open_path_labeled(path, label, false, sidecar_target, frame, None);
    }

    pub(crate) fn open_cloud_cached_asset(
        &mut self,
        cached: crate::cloud::CachedCloudAsset,
        frame: &eframe::Frame,
    ) {
        let offline_reason = cached.offline_reason.clone();
        self.active_tab = AppTab::Develop;
        let sidecar_target = crate::sidecar::SidecarTarget::Desktop {
            raw_path: cached.raw_path.clone(),
        };
        self.open_path_labeled(
            cached.raw_path,
            cached.label,
            false,
            sidecar_target,
            frame,
            None,
        );
        if let Some(reason) = offline_reason {
            self.notice = Some(format!(
                "Opened the cached cloud RAW offline. Edits will sync when the server is reachable. {reason}"
            ));
            self.refresh_status();
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn reload_desktop_library_document_after_reset(
        &mut self,
        path: PathBuf,
        frame: &eframe::Frame,
    ) {
        let label = path.display().to_string();
        let sidecar_target = crate::sidecar::SidecarTarget::Desktop {
            raw_path: path.clone(),
        };
        // Reset All is a Library action. Reload the current document so its
        // in-memory edit state matches the deleted sidecar, but do not navigate
        // away from the Library merely because the reset target was current.
        self.open_path_labeled(path, label, false, sidecar_target, frame, None);
    }

    pub(crate) fn open_path_labeled(
        &mut self,
        path: PathBuf,
        label: String,
        delete_after_decode: bool,
        sidecar_target: crate::sidecar::SidecarTarget,
        frame: &eframe::Frame,
        raw_fd_guard: Option<std::fs::File>,
    ) {
        self.open_path_labeled_with_options(
            path,
            label,
            delete_after_decode,
            sidecar_target,
            frame,
            None,
            None,
            raw_fd_guard,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::app) fn open_path_labeled_with_options(
        &mut self,
        path: PathBuf,
        label: String,
        delete_after_decode: bool,
        sidecar_target: crate::sidecar::SidecarTarget,
        frame: &eframe::Frame,
        // None = use sidecar selection; Some(None) = automatic; Some(Some(path)) = explicit DCP.
        profile_selection_override: Option<Option<PathBuf>>,
        edit_override: Option<SidecarEditState>,
        raw_fd_guard: Option<std::fs::File>,
    ) {
        if self.load_receiver.is_some() {
            if delete_after_decode && raw_fd_guard.is_none() {
                remove_temporary_raw(&path);
            }
            self.notice = Some("Wait for the current RAW to finish opening.".to_owned());
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            if delete_after_decode && raw_fd_guard.is_none() {
                remove_temporary_raw(&path);
            }
            self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            self.refresh_status();
            return;
        };

        #[cfg(not(target_os = "android"))]
        {
            if self.active_tab == AppTab::Develop {
                let crate::sidecar::SidecarTarget::Desktop { raw_path } = &sidecar_target;
                self.prepare_develop_loading_thumbnail(raw_path);
            } else {
                self.develop_loading_thumbnail.clear();
            }
        }
        #[cfg(target_os = "android")]
        {
            let loading_thumbnail_matches = self.active_tab == AppTab::Develop
                && matches!(
                    &sidecar_target,
                    crate::sidecar::SidecarTarget::Android { raw_uri, .. }
                        if self.develop_loading_thumbnail.source_uri.as_deref()
                            == Some(raw_uri.as_str())
                );
            if !loading_thumbnail_matches {
                self.develop_loading_thumbnail.clear();
            }
        }

        let device = render_state.device.clone();
        let queue = render_state.queue.clone();
        self.library.prepare_for_develop();
        let decode_gate = self.library.decode_gate();
        let profile_selection_key = match profile_selection_override.as_ref() {
            Some(Some(path)) => path.to_string_lossy().into_owned(),
            Some(None) => "automatic".to_owned(),
            None => "sidecar".to_owned(),
        };
        let raw_cache_key = format!(
            "{}|profile:{}|folder:{}|selection:{}",
            raw_cache_key_for_target(&sidecar_target),
            self.camera_profile_mode.cache_key(),
            self.camera_profile_folder
                .as_deref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
            profile_selection_key,
        );
        // A normal open may obtain an explicit per-image profile from the
        // sidecar, which is intentionally read on the worker. Do not reuse an
        // ambiguous cache entry before that selection is known.
        let cache_selection_is_known = profile_selection_override.is_some()
            || self.camera_profile_mode == CameraProfileMode::MatrixOnly
            || self.camera_profile_folder.is_none();
        let cached_original_raw = cache_selection_is_known
            .then(|| self.cached_raw_decode(&raw_cache_key))
            .flatten();
        let decode_was_cached = cached_original_raw.is_some();
        crate::diagnostics::record(format!(
            "RAW open requested: label=\"{label}\" cached={decode_was_cached} preview_quality={}",
            self.preview_quality.label()
        ));
        // Image-bound workers may still be inside a native phase. Request
        // cancellation before advancing the document identity, and keep their
        // receivers alive so their terminal events can be drained safely.
        self.cancel_document_bound_background_tasks();
        self.abandon_ai_denoise_worker();
        // A DPI rebuild belongs to the outgoing document. Dropping the
        // receiver lets its worker dispose the result instead of installing it
        // over the newly opened RAW.
        self.preview_rebuild_receiver = None;
        let sidecar_generation = self.begin_sidecar_open();
        // Reuse compiled GPU programs across RAW opens; retire the old texture IDs for next-frame cleanup.
        let reusable_preview_pipeline = {
            let mut renderer = render_state.renderer.write();
            self.take_preview_pipeline_and_release_textures(&mut renderer)
        };
        let retained_preview_program_template = self.preview_program_template.clone();
        #[cfg(target_os = "android")]
        let export_active_while_opening = self.export_receiver.is_some();
        let startup_gpu_prewarm_receiver = self.gpu_preview_prewarm_receiver.take();
        self.original_raw = None;
        self.loaded_raw = None;
        self.preview_raw = None;
        self.white_balance_picker_active = false;
        self.white_balance_picker_drag = None;
        self.current_path = None;
        self.current_label = None;
        self.selected_camera_profile = None;
        self.image_status = format!("Loading {label}…");
        let initial_exposure = self.new_image_exposure();
        let preview_quality_setting = self.preview_quality;
        let preview_viewport_pixels_setting = self.preview_viewport_pixels;
        let camera_profile_mode = self.camera_profile_mode;
        let camera_profile_folder = self.camera_profile_folder.clone();
        let last_camera_profile = self.last_camera_profile.clone();
        let ai_denoise_cache_path = self.rawnind_result_cache_path_for_target(&sidecar_target);
        self.original_preview_exposure = initial_exposure;
        self.original_preview_requested = false;
        self.original_preview_rendered_state = None;
        self.android_original_hold = None;
        self.exposure = initial_exposure;
        self.target_exposure = initial_exposure;
        self.masks.clear();
        self.reset_inpainting_state();
        self.active_mask_tool = None;
        self.brush_mode = BrushMode::Paint;
        self.subject_refinement_active = false;
        self.mask_drag = None;
        self.last_brush_point = None;
        self.mask_touch_gesture_backup = None;
        self.mask_interaction_dirty_layer = None;
        self.mask_interaction_last_upload = None;
        self.mask_interaction_has_uncommitted_change = false;
        self.mask_overlay_revision = self.mask_overlay_revision.wrapping_add(1);
        self.mask_overlay_texture = None;
        self.mask_overlay_texture_key = None;
        self.mask_overlay_blink = None;
        self.mask_thumbnail_group_textures.clear();
        self.mask_thumbnail_component_mask = None;
        self.mask_thumbnail_component_textures.clear();
        self.mask_thumbnail_revision = self.mask_overlay_revision;
        self.mask_source_cache = None;
        self.subject_mask_cache = None;
        self.ai_masks_need_update = false;
        self.ai_mask_update_active = false;
        self.ai_mask_update_subject_pending = false;
        self.ai_mask_update_object_queue.clear();
        self.ai_mask_update_failed = false;
        self.subject_consent_open = false;
        self.subject_generation = self.subject_generation.wrapping_add(1);
        if self.subject_receiver.is_none() {
            self.subject_task_id = None;
            self.subject_download_progress = None;
            self.subject_inferencing = false;
        }
        self.object_consent_open = false;
        self.object_pending_target = None;
        self.object_generation = self.object_generation.wrapping_add(1);
        if self.object_receiver.is_none() {
            self.object_task_id = None;
            self.object_download_progress = None;
            self.object_inferencing = false;
            self.object_decoder_only = false;
            self.object_job_generation = 0;
            self.object_job_target = None;
        }
        self.object_cache = None;
        }
        self.dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.detail_dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.navigation_dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.pending_stage = None;
        self.preview_detail_pending_stage = None;
        self.navigation_pending_stage = None;
        self.preview_detail_urgent = false;
        self.preview_zoom = 1.0;
        self.preview_center = [0.5, 0.5];
        self.preview_visible_uv = PreviewUvRect {
            min: [0.0, 0.0],
            max: [1.0, 1.0],
        };
        self.preview_motion_at = None;
        self.preview_touch_navigation_active = false;
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.lens_correction = LensCorrectionState::default();
        self.lens_correction_dirty = false;
        self.lens_correction_generation = self.lens_correction_generation.wrapping_add(1);
        if self.lens_correction_receiver.is_none() {
            self.lens_correction_task_id = None;
        }
        #[cfg(target_os = "android")]
        {
            self.lens_original_preview_cache = None;
            self.lens_corrected_preview_cache = None;
        }
        self.reset_edit_history();
        let fd_backed_source = raw_fd_guard.is_some();
        let source_path = (!delete_after_decode && !fd_backed_source).then_some(path.clone());
        let repaint = self.egui_ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let cleanup_path_on_spawn_failure =
            (delete_after_decode && !fd_backed_source).then(|| path.clone());
        #[cfg(target_os = "android")]
        let sidecar_android_app = self.android_app.clone();
        #[cfg(not(target_os = "android"))]
        let display_output_transform = self.display_output_transform.clone();

        self.load_receiver = Some(receiver);
        self.loading_label = Some(label.clone());
        self.notice = None;
        self.refresh_status();

        let spawn_result = std::thread::Builder::new()
            .name("auraw-decode-preview".to_owned())
            .spawn(move || {
                let open_started = Instant::now();
                #[cfg(target_os = "android")]
                let reusable_preview_pipeline = if export_active_while_opening {
                    // A live tiled export already consumes most of Android's GPU
                    // working-set allowance. Keeping the old preview solely as a
                    // program template would retain its full resource reservation
                    // and make the replacement preview fail admission. Drop it
                    // before allocating the new preview; the persistent Vulkan
                    // pipeline cache still avoids most driver compilation work.
                    crate::diagnostics::record(
                        "Released the previous Android preview before concurrent RAW open",
                    );
                    drop(reusable_preview_pipeline);
                    None
                } else {
                    reusable_preview_pipeline
                };
                let sidecar_started = Instant::now();
                let loaded_sidecar = load_sidecar_for_target(
                    &sidecar_target,
                    #[cfg(target_os = "android")]
                    &sidecar_android_app,
                );
                crate::diagnostics::record(format!(
                    "RAW sidecar lookup finished in {:.3}s",
                    sidecar_started.elapsed().as_secs_f64()
                ));
                // Existing sidecar edits win; only new RAWs inherit the last valid DCP.
                let (requested_camera_profile, requested_profile_from_sidecar) =
                    match profile_selection_override {
                        Some(selection) => (selection, false),
                        None => match loaded_sidecar.as_ref() {
                            Ok(Some(loaded)) => (
                                loaded
                                    .edits
                                    .camera_profile
                                    .as_ref()
                                    .and_then(|relative| {
                                        camera_profile_folder.as_ref().map(|root| {
                                            if relative == std::path::Path::new(".") {
                                                root.clone()
                                            } else {
                                                root.join(relative)
                                            }
                                        })
                                    }),
                                loaded.edits.camera_profile.is_some(),
                            ),
                            Ok(None) => (
                                last_camera_profile.as_ref().and_then(|relative| {
                                    camera_profile_folder
                                        .as_ref()
                                        .map(|root| root.join(relative))
                                }),
                                false,
                            ),
                            Err(_) => (None, false),
                        },
                    };
                let embedded_matrix_selected = requested_camera_profile
                    .as_ref()
                    .zip(camera_profile_folder.as_ref())
                    .is_some_and(|(selected, root)| selected == root);
                let effective_camera_profile_mode = if embedded_matrix_selected {
                    CameraProfileMode::MatrixOnly
                } else {
                    camera_profile_mode
                };
                let decode_started = Instant::now();
                let decoded: anyhow::Result<Arc<LoadedRaw>> = match cached_original_raw {
                    Some(raw) => Ok(raw),
                    None => match decode_gate.write() {
                        Ok(_decode_guard) => load_raw_file_with_profile_selection(
                            &path,
                            effective_camera_profile_mode,
                            camera_profile_folder.as_deref(),
                            (!embedded_matrix_selected)
                                .then_some(requested_camera_profile.as_deref())
                                .flatten(),
                        )
                        .map(Arc::new),
                        Err(_) => Err(anyhow::anyhow!("RAW decode gate was poisoned")),
                    },
                };
                match &decoded {
                    Ok(raw) => {
                        crate::diagnostics::record(format!(
                            "RAW decode finished in {:.3}s (cached={decode_was_cached})",
                            decode_started.elapsed().as_secs_f64()
                        ));
                        crate::diagnostics::record_raw("Decoded RAW", raw);
                    }
                    Err(error) => crate::diagnostics::record(format!(
                        "RAW decode failed after {:.3}s: {error:#}",
                        decode_started.elapsed().as_secs_f64()
                    )),
                }
                // The descriptor must remain open until LibRaw has finished reading the
                // `/proc/self/fd/<n>` path. It can be closed immediately after decode.
                drop(raw_fd_guard);
                if delete_after_decode && !fd_backed_source {
                    remove_temporary_raw(&path);
                }

                let result = (|| {
                    let (
                        mut rendered_exposure,
                        mut rendered_masks,
                        inpaint_strokes,
                        saved_lens,
                        pasted_ai_masks_need_update,
                        mut sidecar_warning,
                        mut sidecar_needs_rewrite,
                        geometry,
                        use_adaptive_detail_defaults,
                    ) = if let Some(edits) = edit_override {
                        (
                            edits.exposure,
                            Arc::unwrap_or_clone(edits.masks),
                            Arc::unwrap_or_clone(edits.inpainting),
                            Some(edits.lens),
                            edits.ai_masks_need_update,
                            None,
                            true,
                            edits.geometry.sanitized(),
                            false,
                        )
                    } else {
                        match loaded_sidecar {
                            Ok(Some(loaded)) => {
                                let warning = loaded.migrated.then(|| {
                                    "Loaded edits were migrated to the current sidecar format."
                                        .to_owned()
                                });
                                let use_adaptive_detail_defaults = false;
                                (
                                    loaded.edits.exposure,
                                    Arc::unwrap_or_clone(loaded.edits.masks),
                                    Arc::unwrap_or_clone(loaded.edits.inpainting),
                                    Some(loaded.edits.lens),
                                    loaded.edits.ai_masks_need_update,
                                    warning,
                                    loaded.migrated,
                                    loaded.edits.geometry.sanitized(),
                                    use_adaptive_detail_defaults,
                                )
                            }
                            Ok(None) => (
                                initial_exposure,
                                MaskStack::default(),
                                Vec::new(),
                                None,
                                false,
                                None,
                                false,
                                GeometryTransform::default(),
                                true,
                            ),
                            Err(error) => (
                                initial_exposure,
                                MaskStack::default(),
                                Vec::new(),
                                None,
                                false,
                                Some(format!(
                                    "Could not load this RAW's sidecar; using default edits: {error}"
                                )),
                                false,
                                GeometryTransform::default(),
                                true,
                            ),
                        }
                    };
                    let original_raw = decoded.map_err(|error| format!("{error:#}"))?;
                    if use_adaptive_detail_defaults {
                        original_raw.apply_adaptive_detail_defaults(&mut rendered_exposure);
                    }
                    crate::diagnostics::record(format!(
                        "Edit state: exposure={:.3} temperature={:.3} tint={:.3} saturation={:.3} vibrance={:.3} luminance_nr={:.1} color_nr={:.1} demosaic={:?} highlight={:?} masks={}",
                        rendered_exposure.exposure,
                        rendered_exposure.temperature,
                        rendered_exposure.tint,
                        rendered_exposure.saturation,
                        rendered_exposure.vibrance,
                        rendered_exposure.luminance_denoise,
                        rendered_exposure.chroma_denoise * 100.0,
                        rendered_exposure.demosaic_mode,
                        rendered_exposure.highlight_method,
                        rendered_masks.masks.len(),
                    ));
                    let selected_camera_profile = embedded_matrix_selected
                        .then(|| camera_profile_folder.clone())
                        .flatten()
                        .or_else(|| requested_camera_profile
                        .clone()
                        .filter(|requested| {
                            original_raw
                                .camera_profile_source
                                .as_ref()
                                .is_some_and(|applied| applied == requested)
                        }));
                    if requested_profile_from_sidecar
                        && requested_camera_profile.is_some()
                        && selected_camera_profile.is_none()
                    {
                        sidecar_needs_rewrite = true;
                        append_notice(
                            &mut sidecar_warning,
                            "The saved camera profile was not found or did not match this camera; automatic profile selection was used instead.",
                        );
                    }
                    let lens_started = Instant::now();
                    let lens_catalog_started = Instant::now();
                    let mut lens_correction =
                        LensCorrectionState::from_catalog(lensfun_catalog(&original_raw));
                    crate::diagnostics::record(format!(
                        "Lensfun catalog lookup finished in {:.3}s",
                        lens_catalog_started.elapsed().as_secs_f64()
                    ));
                    if let Some(saved) = saved_lens {
                        lens_correction.selected_maker = saved.maker;
                        lens_correction.selected_model = saved.model;
                        lens_correction.enabled = saved.enabled && lens_correction.catalog.available;
                        if saved.enabled && !lens_correction.catalog.available {
                            append_notice(
                                &mut sidecar_warning,
                                "The saved lens correction is unavailable in this build.",
                            );
                        }
                    }
                    let full_raw = if lens_correction.enabled {
                        if let Some(selection) = lens_correction.selected_lens() {
                            let lens_apply_started = Instant::now();
                            match apply_lensfun_correction(&original_raw, &selection) {
                                Ok(corrected) => {
                                    crate::diagnostics::record(format!(
                                        "Lensfun full-resolution correction applied in {:.3}s",
                                        lens_apply_started.elapsed().as_secs_f64()
                                    ));
                                    lens_correction.applied = true;
                                    lens_correction.catalog.status = format!(
                                        "Automatically applied {} from RAW metadata",
                                        selection.label()
                                    );
                                    Arc::new(corrected)
                                }
                                Err(error) => {
                                    crate::diagnostics::record(format!(
                                        "Lensfun full-resolution correction failed after {:.3}s",
                                        lens_apply_started.elapsed().as_secs_f64()
                                    ));
                                    lens_correction.enabled = false;
                                    lens_correction.applied = false;
                                    lens_correction.catalog.status = format!(
                                        "Matched {}, but correction failed: {error:#}",
                                        selection.label()
                                    );
                                    append_notice(
                                        &mut sidecar_warning,
                                        "The saved lens correction failed; the original RAW geometry is shown.",
                                    );
                                    Arc::clone(&original_raw)
                                }
                            }
                        } else {
                            lens_correction.enabled = false;
                            Arc::clone(&original_raw)
                        }
                    } else {
                        Arc::clone(&original_raw)
                    };
                    crate::diagnostics::record(format!(
                        "Lensfun catalog/correction prepared in {:.3}s",
                        lens_started.elapsed().as_secs_f64()
                    ));
                    if rendered_exposure.ai_denoise_enabled {
                        if full_raw.ai_denoised_image().is_none() {
                            let cache_started = Instant::now();
                            match crate::ai_denoise::load_result_cache(
                                &ai_denoise_cache_path,
                                &full_raw,
                            ) {
                                Ok(Some(image)) => {
                                    full_raw.set_ai_denoised_image(image).map_err(|error| {
                                        format!(
                                            "could not install saved AI-denoise result: {error:#}"
                                        )
                                    })?;
                                    crate::diagnostics::record(format!(
                                        "AI-denoise result cache restored in {:.3}s from {}",
                                        cache_started.elapsed().as_secs_f64(),
                                        ai_denoise_cache_path.display()
                                    ));
                                }
                                Ok(None) => crate::diagnostics::record(
                                    "AI-denoise result cache miss; RawNIND will run after open",
                                ),
                                Err(error) => {
                                    log::warn!(
                                        "discarding invalid AI-denoise result cache {}: {error:#}",
                                        ai_denoise_cache_path.display()
                                    );
                                    crate::diagnostics::record(format!(
                                        "AI-denoise result cache rejected: {error:#}"
                                    ));
                                    if let Err(remove_error) =
                                        std::fs::remove_file(&ai_denoise_cache_path)
                                    {
                                        if remove_error.kind() != std::io::ErrorKind::NotFound {
                                            log::warn!(
                                                "could not remove invalid AI-denoise cache {}: {remove_error}",
                                                ai_denoise_cache_path.display()
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        // Decoded RAWs are reused in-process. Do not retain a
                        // previous document's large derived scene when the
                        // current sidecar has AI denoise disabled.
                        full_raw.clear_ai_denoised_image();
                    }
                    let preview_spec = ProxySpec {
                        max_edge: preview_quality_setting.proxy_edge_for_fitted_source(
                            preview_viewport_pixels_setting,
                            full_raw.width,
                            full_raw.height,
                            geometry,
                        ),
                    };
                    let proxy_started = Instant::now();
                    let preview_raw =
                        if full_raw.width.max(full_raw.height) <= preview_spec.max_edge {
                            Arc::clone(&full_raw)
                        } else {
                            Arc::new(build_proxy(&full_raw, preview_spec))
                        };
                    crate::diagnostics::record(format!(
                        "Preview proxy prepared in {:.3}s: {}x{} -> {}x{}",
                        proxy_started.elapsed().as_secs_f64(),
                        full_raw.width,
                        full_raw.height,
                        preview_raw.width,
                        preview_raw.height
                    ));
                    crate::diagnostics::record_raw("Preview proxy", &preview_raw);
                    let initial_params =
                        GpuParams::new(&rendered_exposure, &rendered_masks, &preview_raw)
                            .with_vignette_geometry(geometry);
                    // Preview
                    let preview_quality = ProcessingQuality::Preview;
                    let mut startup_gpu_prewarm_template = None;
                    if reusable_preview_pipeline.is_none()
                        && retained_preview_program_template.is_none()
                    {
                        if let Some(receiver) = startup_gpu_prewarm_receiver {
                            let wait_started = Instant::now();
                            match receiver.recv() {
                                Ok(Ok(template)) => {
                                    crate::diagnostics::record(format!(
                                        "GPU preview startup prewarm available after {:.3}s wait",
                                        wait_started.elapsed().as_secs_f64()
                                    ));
                                    startup_gpu_prewarm_template = Some(template);
                                }
                                Ok(Err(error)) => crate::diagnostics::record(error),
                                Err(error) => crate::diagnostics::record(format!(
                                    "GPU preview startup prewarm unavailable: {error}"
                                )),
                            }
                        }
                    }
                    let reusable_program_template = reusable_preview_pipeline
                        .as_ref()
                        .map(RawGpuPipeline::program_template)
                        .or(retained_preview_program_template)
                        .or_else(|| {
                            startup_gpu_prewarm_template
                                .as_ref()
                                .map(RawGpuPipeline::program_template)
                        });
                    let pipeline_started = Instant::now();
                    let pipeline = if let Some(template) = reusable_program_template.as_ref() {
                        match RawGpuPipeline::new_headless_reusing_program_template(
                            &device,
                            &queue,
                            &preview_raw,
                            &initial_params,
                            preview_quality,
                            template,
                        ) {
                            Ok(pipeline) => {
                                crate::diagnostics::record(
                                    "GPU preview reused precompiled programs",
                                );
                                pipeline
                            }
                            Err(reuse_error) => {
                                crate::diagnostics::record(format!(
                                    "GPU preview program reuse unavailable ({reuse_error:#}); compiling programs"
                                ));
                                RawGpuPipeline::new_headless_with_quality(
                                    &device,
                                    &queue,
                                    &preview_raw,
                                    &initial_params,
                                    preview_quality,
                                )
                                .map_err(|error| {
                                    format!("GPU preview setup failed: {error:#}")
                                })?
                            }
                        }
                    } else {
                        RawGpuPipeline::new_headless_with_quality(
                            &device,
                            &queue,
                            &preview_raw,
                            &initial_params,
                            preview_quality,
                        )
                        .map_err(|error| format!("GPU preview setup failed: {error:#}"))?
                    };
                    crate::diagnostics::record(format!(
                        "GPU preview pipeline created in {:.3}s",
                        pipeline_started.elapsed().as_secs_f64()
                    ));
                    // Program handles have been cloned into `pipeline`; release the old
                    // preview textures before doing any readback from the new preview.
                    drop(reusable_preview_pipeline);
                    drop(startup_gpu_prewarm_template);

                    let composed_inpaint = compose_inpaint_strokes(&inpaint_strokes);
                    let inpaint_upload_started = Instant::now();
                    pipeline
                        .update_inpaint_layer(
                            &queue,
                            composed_inpaint.as_ref(),
                            0,
                            0,
                            preview_raw.width,
                            preview_raw.height,
                        )
                        .map_err(|error| format!("preview inpainting setup failed: {error:#}"))?;
                    crate::diagnostics::record(format!(
                        "Preview inpaint layer uploaded in {:.3}s",
                        inpaint_upload_started.elapsed().as_secs_f64()
                    ));

                    // Range and promptable-object source images are canonical RAW renditions,
                    // not user edit data. Render that neutral source through the preview
                    // pipeline itself instead of allocating a second full pipeline. Keeping
                    // only one preview allocation is important when a tiled export is using
                    // GPU resources at the same time.
                    let mut mask_source = None;
                    if needs_canonical_mask_source(&rendered_masks) {
                        let mask_source_started = Instant::now();
                        let reference_exposure = ExposureParams::scene_referred_default();
                        let reference_masks = MaskStack::default();
                        let reference_params =
                            GpuParams::new(&reference_exposure, &reference_masks, &preview_raw);
                        pipeline.recompute(&queue, &device, &reference_params);
                        let rgba = pipeline
                            .read_output_region_blocking(
                                &device,
                                &queue,
                                0,
                                0,
                                pipeline.width,
                                pipeline.height,
                            )
                            .map_err(|error| {
                                format!("range-mask source readback failed: {error:#}")
                            })?;
                        let source = MaskRgbImage::new(pipeline.width, pipeline.height, rgba)
                            .ok_or_else(|| {
                                "range-mask source dimensions are invalid".to_owned()
                            })?;
                        install_missing_range_sources(&mut rendered_masks, &source);
                        mask_source = Some(source);
                        crate::diagnostics::record(format!(
                            "Canonical mask source reconstructed with the preview pipeline in {:.3}s",
                            mask_source_started.elapsed().as_secs_f64()
                        ));
                    }

                    let params =
                        GpuParams::new(&rendered_exposure, &rendered_masks, &preview_raw)
                            .with_vignette_geometry(geometry);
                    let mask_upload_started = Instant::now();
                    Self::upload_preview_masks(
                        &pipeline,
                        &queue,
                        &rendered_masks,
                        &preview_raw,
                    )?;
                    crate::diagnostics::record(format!(
                        "Preview masks rasterized/uploaded in {:.3}s",
                        mask_upload_started.elapsed().as_secs_f64()
                    ));
                    #[cfg(not(target_os = "android"))]
                    pipeline
                        .write_output_transform(&queue, &display_output_transform)
                        .map_err(|error| format!("display ICC LUT upload failed: {error:#}"))?;
                    let first_render_started = Instant::now();
                    pipeline.recompute(&queue, &device, &params);
                    crate::diagnostics::record(format!(
                        "Initial GPU preview dispatch submitted in {:.3}s",
                        first_render_started.elapsed().as_secs_f64()
                    ));

                    // Inpainting now captures only the required full-resolution
                    // RAW crop when a stroke is released. Avoid precomputing and
                    // retaining an unused preview-resolution proxy source here.
                    let inpaint_source = None;
                    crate::diagnostics::record(format!(
                        "RAW open worker finished in {:.3}s",
                        open_started.elapsed().as_secs_f64()
                    ));

                    Ok(LoadedPreview {
                        source_path,
                        raw_cache_key,
                        label,
                        original_raw,
                        full_raw,
                        preview_raw,
                        pipeline,
                        rendered_exposure,
                        rendered_masks,
                        inpaint_strokes,
                        ai_masks_need_update: pasted_ai_masks_need_update,
                        mask_source,
                        inpaint_source,
                        lens_correction,
                        sidecar_target,
                        sidecar_generation,
                        sidecar_warning,
                        sidecar_needs_rewrite,
                        selected_camera_profile,
                        geometry,
                    })
                })();

                if let Err(error) = &result {
                    crate::diagnostics::record(format!(
                        "RAW open worker failed after {:.3}s: {error}",
                        open_started.elapsed().as_secs_f64()
                    ));
                }
                let _ = sender.send(LoadEvent::Finished(result));
                repaint.request_repaint();
            });

        if let Err(error) = spawn_result {
            if let Some(path) = cleanup_path_on_spawn_failure {
                remove_temporary_raw(&path);
            }
            self.load_receiver = None;
            self.loading_label = None;
            self.develop_loading_thumbnail.clear();
            self.notice = Some(format!("could not start RAW decode worker: {error}"));
            self.refresh_status();
        }
    }

    pub(in crate::app) fn poll_load_worker(&mut self, frame: &eframe::Frame) {
        let received = self
            .load_receiver
            .as_ref()
            .map(|receiver| receiver.try_recv());
        let event = match received {
            Some(Ok(event)) => Some(event),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.load_receiver = None;
                self.loading_label = None;
                self.develop_loading_thumbnail.clear();
                self.notice = Some("RAW decode worker stopped unexpectedly.".to_owned());
                self.on_library_ai_mask_refresh_load_finished(false, frame);
                #[cfg(target_os = "android")]
                if std::mem::take(&mut self.android_batch_load_pending) {
                    self.on_library_batch_load_finished(false, frame);
                }
                #[cfg(not(target_os = "android"))]
                self.on_library_batch_load_finished(false, frame);
                None
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => None,
        };
        let Some(LoadEvent::Finished(result)) = event else {
            return;
        };

        self.load_receiver = None;
        self.loading_label = None;
        self.develop_loading_thumbnail.clear();
        #[cfg(target_os = "android")]
        let batch_owned_load = std::mem::take(&mut self.android_batch_load_pending);

        match result {
            Ok(mut loaded) => {
                let Some(render_state) = frame.wgpu_render_state() else {
                    self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
                    self.on_library_ai_mask_refresh_load_finished(false, frame);
                    #[cfg(target_os = "android")]
                    if batch_owned_load {
                        self.on_library_batch_load_finished(false, frame);
                    }
                    #[cfg(not(target_os = "android"))]
                    self.on_library_batch_load_finished(false, frame);
                    return;
                };
                // Do not keep the renderer write lock alive while installing the
                // decoded document or notifying internal batch owners. Android
                // batch export immediately releases this temporary preview before
                // starting its tiled worker; retaining this guard until the end of
                // the match arm caused a recursive write-lock acquisition and the
                // epaint 10-second deadlock panic.
                let previous_pipeline = {
                    let mut renderer = render_state.renderer.write();
                    let previous = self.take_preview_pipeline_and_release_textures(&mut renderer);
                    loaded
                        .pipeline
                        .register_egui_texture(&render_state.device, &mut renderer);
                    previous
                };
                drop(previous_pipeline);

                let full_width = loaded.full_raw.width;
                let full_height = loaded.full_raw.height;
                let preview_width = loaded.preview_raw.width;
                let preview_height = loaded.preview_raw.height;
                let profile_label = loaded
                    .full_raw
                    .camera_profile
                    .name
                    .as_deref()
                    .map(|name| format!(", profile {name}"))
                    .unwrap_or_default();
                self.image_status = format!(
                    "{} {} — full {}×{}, preview {}×{} ({}{})",
                    loaded.full_raw.camera_make,
                    loaded.full_raw.camera_model,
                    full_width,
                    full_height,
                    preview_width,
                    preview_height,
                    self.preview_quality.label(),
                    profile_label,
                );
                self.current_path = loaded.source_path;
                self.current_label = Some(loaded.label.clone());
                self.selected_camera_profile = loaded.selected_camera_profile.clone();
                // Loading an existing sidecar must not change the sticky global
                // profile preference. Only an explicit user dropdown change in
                // `select_camera_profile_for_current` may update it.
                self.cache_raw_decode(loaded.raw_cache_key, Arc::clone(&loaded.original_raw));
                self.original_raw = Some(loaded.original_raw);
                self.loaded_raw = Some(loaded.full_raw);
                self.preview_raw = Some(loaded.preview_raw);
                self.preview_program_template = Some(loaded.pipeline.program_template());
                self.gpu_pipeline = Some(loaded.pipeline);
                self.exposure = loaded.rendered_exposure;
                self.geometry = loaded.geometry.sanitized();
                self.crop_constraint_reference = None;
                self.crop_drag = None;
                self.straighten_tool_active = false;
                self.straighten_drag = None;
                self.geometry_revision = 0;
                self.masks = loaded.rendered_masks;
                self.inpaint_strokes = loaded.inpaint_strokes;
                self.inpaint_layer = compose_inpaint_strokes(&self.inpaint_strokes);
                self.inpaint_texture = None;
                self.inpaint_texture_key = None;
                self.inpaint_texture_revision = self.inpaint_texture_revision.wrapping_add(1);
                self.inpaint_revision = 0;
                self.inpaint_source_cache = loaded.inpaint_source;
                self.ai_masks_need_update = loaded.ai_masks_need_update;
                self.rehydrate_restored_mask_state();
                self.ai_masks_need_update |= loaded.ai_masks_need_update;
                if loaded.mask_source.is_some() {
                    self.mask_source_cache = loaded.mask_source;
                }
                self.preview_zoom = 1.0;
                self.preview_center = [0.5, 0.5];
                self.preview_visible_uv = PreviewUvRect {
                    min: [0.0, 0.0],
                    max: [1.0, 1.0],
                };
                self.preview_motion_at = None;
                self.preview_touch_navigation_active = false;
                self.preview_revision = self.preview_revision.wrapping_add(1);
                self.original_preview_rendered_state = None;
                self.preview_detail = None;
                self.preview_navigation = None;
                self.preview_detail_pending_stage = None;
                self.navigation_pending_stage = None;
                self.preview_detail_urgent = false;
                self.detail_dirty_mask_layers.fill(false);
                self.navigation_dirty_mask_layers.fill(false);
                self.dirty_mask_layers.fill(false);
                self.lens_correction = loaded.lens_correction;
                self.lens_correction_dirty = false;
                self.lens_correction_generation = self.lens_correction_generation.wrapping_add(1);
                #[cfg(target_os = "android")]
                {
                    if self.lens_correction.applied {
                        self.lens_corrected_preview_cache = match (
                            self.lens_correction.selected_lens(),
                            self.loaded_raw.as_ref(),
                            self.preview_raw.as_ref(),
                        ) {
                            (Some(selection), Some(full_raw), Some(preview_raw)) => Some((
                                selection,
                                self.preview_quality,
                                Arc::clone(full_raw),
                                Arc::clone(preview_raw),
                            )),
                            _ => None,
                        };
                        self.lens_original_preview_cache = None;
                    } else {
                        self.lens_original_preview_cache = self
                            .preview_raw
                            .as_ref()
                            .map(|raw| (self.preview_quality, Arc::clone(raw)));
                        self.lens_corrected_preview_cache = None;
                    }
                }
                self.target_exposure = loaded.rendered_exposure;
                self.pending_stage = None;
                self.notice = loaded.sidecar_warning;
                // Automatic lens matching and initial render setup are the
                // baseline for this RAW, not user edits inherited from the
                // previous image or from the decode worker.
                self.reset_edit_history();
                self.install_sidecar_target(
                    loaded.sidecar_target,
                    loaded.sidecar_generation,
                    loaded.sidecar_needs_rewrite,
                );
                self.cancel_stale_document_background_tasks();
                self.resume_persisted_ai_denoise(frame);
                log::info!("loaded RAW preview for {}", loaded.label);
                self.on_library_ai_mask_refresh_load_finished(true, frame);
                #[cfg(target_os = "android")]
                if batch_owned_load {
                    self.on_library_batch_load_finished(true, frame);
                }
                #[cfg(not(target_os = "android"))]
                self.on_library_batch_load_finished(true, frame);
            }
            Err(error) => {
                self.notice = Some(format!("Failed to decode or render RAW: {error}"));
                log::error!("RAW load failed: {error}");
                self.on_library_ai_mask_refresh_load_finished(false, frame);
                #[cfg(target_os = "android")]
                if batch_owned_load {
                    self.on_library_batch_load_finished(false, frame);
                }
                #[cfg(not(target_os = "android"))]
                self.on_library_batch_load_finished(false, frame);
            }
        }
    }
}
