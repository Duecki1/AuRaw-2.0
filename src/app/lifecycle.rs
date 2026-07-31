fn remove_temporary_raw(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_file(path) {
        log::warn!(
            "could not remove imported Android RAW cache file {}: {error}",
            path.display()
        );
    }
}

fn load_sidecar_for_target(
    target: &crate::sidecar::SidecarTarget,
    #[cfg(target_os = "android")] android_app: &android_activity::AndroidApp,
) -> Result<Option<crate::sidecar::LoadedSidecar>, crate::sidecar::SidecarError> {
    match target {
        crate::sidecar::SidecarTarget::Desktop { raw_path } => {
            crate::sidecar::load_desktop(raw_path)
        }
        #[cfg(target_os = "android")]
        crate::sidecar::SidecarTarget::Android {
            raw_uri,
            display_name,
        } => crate::sidecar::load_android(android_app, raw_uri, display_name),
    }
}

fn raw_cache_key_for_target(target: &crate::sidecar::SidecarTarget) -> String {
    match target {
        crate::sidecar::SidecarTarget::Desktop { raw_path } => {
            let metadata = std::fs::metadata(raw_path).ok();
            let bytes = metadata
                .as_ref()
                .map(std::fs::Metadata::len)
                .unwrap_or_default();
            let modified = metadata
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            format!("desktop:{}:{bytes}:{modified}", raw_path.display())
        }
        #[cfg(target_os = "android")]
        crate::sidecar::SidecarTarget::Android { raw_uri, .. } => {
            format!("android:{raw_uri}")
        }
    }
}

fn prewarm_dcp_profile_folder(folder: Option<std::path::PathBuf>) {
    let Some(folder) = folder.filter(|folder| folder.is_dir()) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("auraw-dcp-prewarm".to_owned())
        .spawn(move || {
            let started = Instant::now();
            crate::pipeline::prewarm_dcp_profile_index(&folder);
            crate::diagnostics::record(format!(
                "DCP profile index prewarmed in {:.3}s",
                started.elapsed().as_secs_f64()
            ));
        });
}

#[cfg(not(target_os = "android"))]
fn selected_picker_directory(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if path.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
    }
}

#[cfg(any(target_os = "android", test))]
fn gpu_preview_prewarm_cfa_kind() -> crate::pipeline::CfaKind {
    crate::pipeline::CfaKind::Bayer
}

#[cfg(target_os = "android")]
fn spawn_gpu_preview_prewarm(
    cc: &eframe::CreationContext<'_>,
    cache_root: Option<std::path::PathBuf>,
    export_prewarm: Arc<crate::pipeline::GpuProgramPrewarm>,
) -> Option<mpsc::Receiver<Result<RawGpuPipeline, String>>> {
    let Some(render_state) = cc.wgpu_render_state.as_ref() else {
        export_prewarm.publish(Err(
            "eframe is not running with the wgpu backend".to_owned(),
        ));
        return None;
    };
    let device = render_state.device.clone();
    let queue = render_state.queue.clone();
    let adapter_info = render_state.adapter.get_info();
    let repaint = cc.egui_ctx.clone();
    let (sender, receiver) = mpsc::channel();
    let export_prewarm_for_thread = Arc::clone(&export_prewarm);
    let spawn_result = std::thread::Builder::new()
        .name("auraw-gpu-preview-prewarm".to_owned())
        .spawn(move || {
            let started = Instant::now();
            crate::diagnostics::record("GPU preview prewarm started at app initialization");

            let persistent_cache = match cache_root.as_deref() {
                Some(cache_root) => {
                    match crate::pipeline::PersistentGpuPipelineCache::load_or_create(
                        &device,
                        &adapter_info,
                        cache_root,
                    ) {
                        Ok(Some((cache, loaded_bytes))) => {
                            if loaded_bytes == 0 {
                                crate::diagnostics::record(format!(
                                    "GPU pipeline cache cold start: {}",
                                    cache.path().display()
                                ));
                            } else {
                                crate::diagnostics::record(format!(
                                    "GPU pipeline cache loaded: {} bytes from {}",
                                    loaded_bytes,
                                    cache.path().display()
                                ));
                            }
                            Some(cache)
                        }
                        Ok(None) => {
                            crate::diagnostics::record(
                                "GPU pipeline cache unavailable on this wgpu device/backend",
                            );
                            None
                        }
                        Err(error) => {
                            crate::diagnostics::record(format!(
                                "GPU pipeline cache could not be initialized: {error:#}"
                            ));
                            None
                        }
                    }
                }
                None => {
                    crate::diagnostics::record(
                        "GPU pipeline cache path unavailable; using in-process prewarm only",
                    );
                    None
                }
            };

            let cache_to_persist = persistent_cache.clone();
            let result = RawGpuPipeline::prewarm_preview_template_with_cache(
                &device,
                &queue,
                gpu_preview_prewarm_cfa_kind(),
                persistent_cache.clone(),
            )
            .map_err(|error| format!("GPU preview prewarm failed: {error:#}"));
            match &result {
                Ok(_) => crate::diagnostics::record(format!(
                    "GPU preview prewarm finished in {:.3}s",
                    started.elapsed().as_secs_f64()
                )),
                Err(error) => crate::diagnostics::record(error),
            }

            // Deliver the compiled template immediately. Cache serialization is
            // intentionally done afterwards so a RAW open waiting on prewarm
            // never pays filesystem write latency.
            let _ = sender.send(result);
            repaint.request_repaint();

            let export_started = Instant::now();
            let export_result = RawGpuPipeline::prewarm_export_program_template_with_cache(
                &device,
                &queue,
                gpu_preview_prewarm_cfa_kind(),
                persistent_cache,
            )
            .map_err(|error| format!("GPU export program prewarm failed: {error:#}"));
            match &export_result {
                Ok(_) => crate::diagnostics::record(format!(
                    "GPU export program prewarm finished in {:.3}s",
                    export_started.elapsed().as_secs_f64()
                )),
                Err(error) => crate::diagnostics::record(error),
            }
            export_prewarm_for_thread.publish(export_result);
            repaint.request_repaint();

            if let Some(cache) = cache_to_persist {
                let cache_save_started = Instant::now();
                match cache.persist() {
                    Ok(bytes) if bytes > 0 => crate::diagnostics::record(format!(
                        "GPU pipeline cache saved: {} bytes in {:.3}s to {}",
                        bytes,
                        cache_save_started.elapsed().as_secs_f64(),
                        cache.path().display()
                    )),
                    Ok(_) => crate::diagnostics::record(format!(
                        "GPU pipeline cache returned no persistent data for {}",
                        cache.path().display()
                    )),
                    Err(error) => crate::diagnostics::record(format!(
                        "GPU pipeline cache could not be saved: {error:#}"
                    )),
                }
            }
        });
    match spawn_result {
        Ok(_) => Some(receiver),
        Err(error) => {
            export_prewarm.publish(Err(format!(
                "GPU prewarm thread could not start: {error}"
            )));
            crate::diagnostics::record(format!(
                "GPU preview prewarm thread could not start: {error}"
            ));
            None
        }
    }
}

fn append_notice(notice: &mut Option<String>, message: &str) {
    match notice {
        Some(existing) => {
            existing.push(' ');
            existing.push_str(message);
        }
        None => *notice = Some(message.to_owned()),
    }
}

fn needs_canonical_mask_source(masks: &MaskStack) -> bool {
    masks.masks.iter().any(|mask| {
        mask.components.iter().any(|component| {
            matches!(
                &component.geometry,
                MaskGeometry::LuminanceRange { source: None, .. }
                    | MaskGeometry::ColorRange { source: None, .. }
                    | MaskGeometry::Object { .. }
                _ => {}
            }
        }
    }
}

impl AurawApp {
    fn install_lightroom_visuals(ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        ctx.set_fonts(fonts);

        // Start from egui's robust dark palette, then make the editor panels a
        // little calmer and denser for a Lightroom-like darkroom layout.
        let mut visuals = egui::Visuals::dark();
        let accent = egui::Color32::from_rgb(56, 139, 253);

        visuals.panel_fill = egui::Color32::from_rgb(24, 26, 29);
        visuals.window_fill = egui::Color32::from_rgb(27, 29, 33);
        visuals.faint_bg_color = egui::Color32::from_rgb(35, 38, 43);
        visuals.extreme_bg_color = egui::Color32::from_rgb(16, 18, 20);
        visuals.selection.bg_fill = accent;
        visuals.hyperlink_color = accent;
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(42, 45, 50);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(54, 58, 65);
        visuals.widgets.active.bg_fill = accent;
        ctx.set_visuals(visuals);

        let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(12.5));
        style
            .text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(11.5));
        style.spacing.slider_width = 220.0;
        style.spacing.item_spacing = egui::vec2(7.0, 4.0);
        style.spacing.button_padding = egui::vec2(9.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.indent = 12.0;
        ctx.set_style_of(egui::Theme::Dark, style);
    }

    #[cfg(not(target_os = "android"))]
    fn empty(ctx: &egui::Context) -> Self {
        let performance_settings_path = crate::performance_settings::desktop_path();
        let performance = crate::performance_settings::load(performance_settings_path.as_deref());
        let last_library_folder = performance.last_library_folder.clone();
        let mut camera_profile_folder = performance.camera_profile_folder.clone();
        let mut camera_profile_folder_label = performance.camera_profile_folder_label.clone();
        if performance.camera_profile_auto_detect
            && camera_profile_folder
                .as_ref()
                .is_none_or(|folder| !folder.is_dir())
        {
            camera_profile_folder =
                crate::performance_settings::detected_adobe_camera_profile_folder();
            if let Some(folder) = &camera_profile_folder {
                crate::diagnostics::record(format!(
                    "Camera profiles: auto-detected Adobe Camera Raw folder {}",
                    folder.display()
                ));
                camera_profile_folder_label = Some("Adobe Camera Raw (auto-detected)".to_owned());
            } else {
                camera_profile_folder_label = None;
            }
        }
        prewarm_dcp_profile_folder(camera_profile_folder.clone());
        let exposure = ExposureParams::scene_referred_default();
        let masks = MaskStack::default();
        let lens_correction = LensCorrectionState::default();
        let edit_history = EditHistory::new(&exposure, &masks, &lens_correction);
        let runtime_selection = Self::load_onnx_runtime_selection();
        let onnx_runtime_path = runtime_selection.as_ref().map(|(path, _)| path.clone());
        let onnx_runtime_sha256 = runtime_selection.map(|(_, sha256)| sha256);
        let mut app = Self {
            current_path: None,
            original_raw: None,
            loaded_raw: None,
            preview_raw: None,
            gpu_pipeline: None,
            retired_egui_textures: Vec::new(),
            preview_quality: performance.preview_quality,
            preview_zoom: 1.0,
            preview_center: [0.5, 0.5],
            preview_visible_uv: PreviewUvRect {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
            preview_viewport_pixels: [1, 1],
            preview_motion_at: None,
            preview_touch_navigation_active: false,
            preview_revision: 0,
            preview_detail: None,
            preview_navigation: None,
            preview_detail_pending_stage: None,
            navigation_pending_stage: None,
            preview_detail_urgent: false,
            preview_quality_dirty: false,
            original_preview_exposure: exposure,
            original_preview_requested: false,
            original_preview_rendered_state: None,
            android_original_hold: None,
            exposure,
            library: LibraryState::new_with_workers(ctx, performance.thumbnail_workers),
            adjustment_copy_settings: performance.adjustment_copy_settings,
            adjustment_clipboard: None,
            raw_cache: VecDeque::new(),
            raw_cache_limit: performance.raw_cache_files,
            performance_settings_path,
            #[cfg(not(target_os = "android"))]
            display_color_management: performance.display_color_management,
            #[cfg(not(target_os = "android"))]
            display_profile_override: performance.display_profile_override.clone(),
            #[cfg(not(target_os = "android"))]
            display_profile_label: "sRGB fallback".to_owned(),
            #[cfg(not(target_os = "android"))]
            display_profile_source: None,
            #[cfg(not(target_os = "android"))]
            display_profile_fingerprint: None,
            #[cfg(not(target_os = "android"))]
            display_profile_last_probe: None,
            #[cfg(not(target_os = "android"))]
            display_profile_last_screen_point: None,
            #[cfg(not(target_os = "android"))]
            display_output_transform: crate::pipeline::IccOutputTransform::srgb(),
            camera_profile_mode: performance.camera_profile_mode,
            camera_profile_folder,
            camera_profile_folder_label,
            camera_profile_auto_detect: performance.camera_profile_auto_detect,
            last_camera_profile: performance.last_camera_profile.clone(),
            selected_camera_profile: None,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
            #[cfg(not(target_os = "android"))]
            desktop_sidebar_width: None,
            geometry: GeometryTransform::default(),
            crop_constraint_reference: None,
            crop_drag: None,
            straighten_tool_active: false,
            straighten_drag: None,
            geometry_revision: 0,
            adjustment_section: AdjustmentSection::default(),
            mask_section: MaskSection::default(),
            tone_curve_tab: ToneCurveTab::default(),
            color_grade_tab: ColorGradeTab::default(),
            export_settings: ExportSettings::default(),
            masks,
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            mask_drag: None,
            last_brush_point: None,
            mask_touch_gesture_backup: None,
            mask_interaction_dirty_layer: None,
            mask_interaction_last_upload: None,
            mask_interaction_has_uncommitted_change: false,
            mask_overlay_revision: 0,
            mask_overlay_texture: None,
            mask_overlay_texture_key: None,
            mask_overlay_blink: None,
            mask_thumbnail_revision: 0,
            mask_thumbnail_group_textures: Vec::new(),
            mask_thumbnail_component_mask: None,
            mask_thumbnail_component_textures: Vec::new(),
            mask_source_cache: None,
            subject_mask_cache: None,
            ai_masks_need_update: false,
            ai_mask_update_active: false,
            ai_mask_update_subject_pending: false,
            ai_mask_update_object_queue: VecDeque::new(),
            ai_mask_update_failed: false,
            onnx_runtime_path,
            onnx_runtime_sha256,
            #[cfg(not(target_os = "android"))]
            desktop_picker_receiver: None,
            status: "Open a RAW file to get started.".to_owned(),
            expert_mode: false,
            lens_correction,
            edit_history,
            history_lens_restore_masks: None,
            sidecar_target: None,
            sidecar_generation: 0,
            sidecar_saved_revision: None,
            sidecar_failed_revision: None,
            sidecar_pending: VecDeque::new(),
            sidecar_in_flight: None,
            sidecar_receiver: None,
            sidecar_save_feedback_until: None,
            sidecar_autosave_deadline: None,
            developed_thumbnail_pending: None,
            developed_thumbnail_in_flight: None,
            developed_thumbnail_receiver: None,
            egui_ctx: ctx.clone(),
            background_tasks: BackgroundTaskManager::default(),
            background_actions: HashMap::new(),
            export_task_id: None,
            library_batch_export_task_id: None,
            library_ai_mask_refresh_task_id: None,
            subject_task_id: None,
            object_task_id: None,
            inpaint_task_id: None,
            target_exposure: exposure,
            pending_stage: None,
            lens_correction_dirty: false,
            lens_correction_generation: 0,
            lens_correction_receiver: None,
            lens_correction_task_id: None,
            #[cfg(target_os = "android")]
            lens_original_preview_cache: None,
            #[cfg(target_os = "android")]
            lens_corrected_preview_cache: None,
            load_receiver: None,
            loading_label: None,
            export_receiver: None,
            export_progress: None,
            #[cfg(not(target_os = "android"))]
            library_batch_export_receiver: None,
            #[cfg(not(target_os = "android"))]
            library_batch_export_tile_progress: None,
            library_batch_export: None,
            library_ai_mask_refresh: None,
            export_publish_pending: false,
            image_status: "Open a RAW file to get started.".to_owned(),
            current_label: None,
            notice: None,
            dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            detail_dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            navigation_dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            subject_consent_open: false,
            subject_receiver: None,
            subject_generation: 0,
            subject_job_document_id: 0,
            subject_job_generation: 0,
            subject_download_progress: None,
            subject_inferencing: false,
            object_consent_open: false,
            object_pending_target: None,
            object_receiver: None,
            object_download_progress: None,
            object_inferencing: false,
            object_decoder_only: false,
            object_error_dialog: None,
            object_generation: 0,
            object_job_generation: 0,
            object_job_document_id: 0,
            object_job_target: None,
            object_cache: None,
            inpaint_brush_size: 0.055,
            inpaint_stroke: Vec::new(),
            inpaint_strokes: Vec::new(),
            last_inpaint_brush_point: None,
            inpaint_layer: None,
            inpaint_texture: None,
            inpaint_texture_revision: 0,
            inpaint_texture_key: None,
            inpaint_stroke_texture: None,
            inpaint_stroke_texture_key: None,
            inpaint_hovered_stroke: None,
            inpaint_selected_stroke: None,
            inpaint_focus_texture: None,
            inpaint_focus_texture_key: None,
            inpaint_source_cache: None,
            inpaint_pending_source: None,
            inpaint_active_dabs: None,
            inpaint_revision: 0,
            inpaint_job_document_id: 0,
            inpaint_job_generation: 0,
            inpaint_consent_open: false,
            inpaint_receiver: None,
            inpaint_download_progress: None,
            inpaint_inferencing: false,
            ai_denoise_consent_open: false,
            ai_denoise_receiver: None,
            ai_denoise_download_progress: None,
            ai_denoise_apply_progress: None,
            ai_denoise_cancellation: None,
            ai_denoise_job_document_id: 0,
            ai_denoise_resume_pending: false,
        };
        if let Some(folder) = last_library_folder.filter(|folder| folder.is_dir()) {
            app.library.open_folder(folder, ctx);
        }
        app
    }

    #[cfg(not(target_os = "android"))]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::install_lightroom_visuals(&cc.egui_ctx);
        crate::diagnostics::record("AuRaw desktop UI initialized");
        Self::empty(&cc.egui_ctx)
    }

    #[cfg(target_os = "android")]
    pub fn new_android(
        cc: &eframe::CreationContext<'_>,
        android_app: android_activity::AndroidApp,
    ) -> Self {
        crate::android::install_context(&cc.egui_ctx);
        Self::install_lightroom_visuals(&cc.egui_ctx);
        match crate::android::device_diagnostics(&android_app) {
            Ok(info) => crate::diagnostics::set_device_info(info),
            Err(error) => crate::diagnostics::record(error),
        }
        crate::diagnostics::record("AuRaw Android UI initialized");
        let performance_settings_path = crate::android::performance_settings_path(&android_app)
            .map_err(|error| log::warn!("{error}"))
            .ok();
        let gpu_pipeline_cache_root = crate::android::gpu_pipeline_cache_dir(&android_app)
            .map_err(|error| log::warn!("{error}"))
            .ok();
        if std::env::var_os("AURAW_LENSFUN_DB").is_none() {
            match crate::android::lensfun_database_dir(&android_app) {
                Ok(path) => {
                    std::env::set_var("AURAW_LENSFUN_DB", &path);
                    crate::diagnostics::record(format!(
                        "bundled Lensfun database materialized at {}",
                        path.display()
                    ));
                }
                Err(error) => log::warn!("{error}"),
            }
        }
        let performance = crate::performance_settings::load(performance_settings_path.as_deref());
        prewarm_dcp_profile_folder(performance.camera_profile_folder.clone());
        let gpu_export_prewarm = Arc::new(crate::pipeline::GpuProgramPrewarm::new());
        let gpu_preview_prewarm_receiver = spawn_gpu_preview_prewarm(
            cc,
            gpu_pipeline_cache_root,
            Arc::clone(&gpu_export_prewarm),
        );
        let exposure = ExposureParams::scene_referred_default();
        let masks = MaskStack::default();
        let lens_correction = LensCorrectionState::default();
        let edit_history = EditHistory::new(&exposure, &masks, &lens_correction);
        Self {
            current_path: None,
            original_raw: None,
            loaded_raw: None,
            preview_raw: None,
            gpu_pipeline: None,
            retired_egui_textures: Vec::new(),
            gpu_preview_prewarm_receiver,
            gpu_export_prewarm: Some(gpu_export_prewarm),
            preview_quality: performance.preview_quality,
            preview_zoom: 1.0,
            preview_center: [0.5, 0.5],
            preview_visible_uv: PreviewUvRect {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            },
            preview_viewport_pixels: [1, 1],
            preview_motion_at: None,
            preview_touch_navigation_active: false,
            preview_revision: 0,
            preview_detail: None,
            preview_navigation: None,
            preview_detail_pending_stage: None,
            navigation_pending_stage: None,
            preview_detail_urgent: false,
            preview_quality_dirty: false,
            original_preview_exposure: exposure,
            original_preview_requested: false,
            original_preview_rendered_state: None,
            android_original_hold: None,
            exposure,
            library: LibraryState::new_android_with_workers(
                android_app.clone(),
                &cc.egui_ctx,
                performance.thumbnail_workers,
            ),
            adjustment_copy_settings: performance.adjustment_copy_settings,
            adjustment_clipboard: None,
            raw_cache: VecDeque::new(),
            raw_cache_limit: performance.raw_cache_files,
            performance_settings_path,
            #[cfg(not(target_os = "android"))]
            display_color_management: performance.display_color_management,
            #[cfg(not(target_os = "android"))]
            display_profile_override: performance.display_profile_override.clone(),
            #[cfg(not(target_os = "android"))]
            display_profile_label: "sRGB fallback".to_owned(),
            #[cfg(not(target_os = "android"))]
            display_profile_source: None,
            #[cfg(not(target_os = "android"))]
            display_profile_fingerprint: None,
            #[cfg(not(target_os = "android"))]
            display_profile_last_probe: None,
            #[cfg(not(target_os = "android"))]
            display_profile_last_screen_point: None,
            #[cfg(not(target_os = "android"))]
            display_output_transform: crate::pipeline::IccOutputTransform::srgb(),
            camera_profile_mode: performance.camera_profile_mode,
            camera_profile_folder: performance.camera_profile_folder.clone(),
            camera_profile_folder_label: performance.camera_profile_folder_label.clone(),
            camera_profile_auto_detect: performance.camera_profile_auto_detect,
            last_camera_profile: performance.last_camera_profile.clone(),
            selected_camera_profile: None,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
            #[cfg(not(target_os = "android"))]
            desktop_sidebar_width: None,
            geometry: GeometryTransform::default(),
            crop_constraint_reference: None,
            crop_drag: None,
            straighten_tool_active: false,
            straighten_drag: None,
            geometry_revision: 0,
            adjustment_section: AdjustmentSection::default(),
            mask_section: MaskSection::default(),
            tone_curve_tab: ToneCurveTab::default(),
            color_grade_tab: ColorGradeTab::default(),
            export_settings: ExportSettings::default(),
            masks,
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            mask_drag: None,
            last_brush_point: None,
            mask_touch_gesture_backup: None,
            mask_interaction_dirty_layer: None,
            mask_interaction_last_upload: None,
            mask_interaction_has_uncommitted_change: false,
            mask_overlay_revision: 0,
            mask_overlay_texture: None,
            mask_overlay_texture_key: None,
            mask_overlay_blink: None,
            mask_thumbnail_revision: 0,
            mask_thumbnail_group_textures: Vec::new(),
            mask_thumbnail_component_mask: None,
            mask_thumbnail_component_textures: Vec::new(),
            mask_source_cache: None,
            subject_mask_cache: None,
            ai_masks_need_update: false,
            ai_mask_update_active: false,
            ai_mask_update_subject_pending: false,
            ai_mask_update_object_queue: VecDeque::new(),
            ai_mask_update_failed: false,
            status: "Open a RAW file to get started.".to_owned(),
            expert_mode: false,
            lens_correction,
            edit_history,
            history_lens_restore_masks: None,
            sidecar_target: None,
            sidecar_generation: 0,
            sidecar_saved_revision: None,
            sidecar_failed_revision: None,
            sidecar_pending: VecDeque::new(),
            sidecar_in_flight: None,
            sidecar_receiver: None,
            sidecar_save_feedback_until: None,
            sidecar_autosave_deadline: None,
            developed_thumbnail_pending: None,
            developed_thumbnail_in_flight: None,
            developed_thumbnail_receiver: None,
            egui_ctx: cc.egui_ctx.clone(),
            background_tasks: BackgroundTaskManager::default(),
            background_actions: HashMap::new(),
            export_task_id: None,
            library_batch_export_task_id: None,
            library_ai_mask_refresh_task_id: None,
            subject_task_id: None,
            object_task_id: None,
            inpaint_task_id: None,
            target_exposure: exposure,
            pending_stage: None,
            lens_correction_dirty: false,
            lens_correction_generation: 0,
            lens_correction_receiver: None,
            lens_correction_task_id: None,
            lens_original_preview_cache: None,
            lens_corrected_preview_cache: None,
            load_receiver: None,
            loading_label: None,
            export_receiver: None,
            export_progress: None,
            #[cfg(not(target_os = "android"))]
            library_batch_export_receiver: None,
            #[cfg(not(target_os = "android"))]
            library_batch_export_tile_progress: None,
            library_batch_export: None,
            library_ai_mask_refresh: None,
            export_publish_pending: false,
            image_status: "Open a RAW file to get started.".to_owned(),
            current_label: None,
            notice: None,
            dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            detail_dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            navigation_dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            subject_consent_open: false,
            subject_receiver: None,
            subject_generation: 0,
            subject_job_document_id: 0,
            subject_job_generation: 0,
            subject_download_progress: None,
            subject_inferencing: false,
            object_consent_open: false,
            object_pending_target: None,
            object_receiver: None,
            object_download_progress: None,
            object_inferencing: false,
            object_decoder_only: false,
            object_error_dialog: None,
            object_generation: 0,
            object_job_generation: 0,
            object_job_document_id: 0,
            object_job_target: None,
            object_cache: None,
            inpaint_brush_size: 0.055,
            inpaint_stroke: Vec::new(),
            inpaint_strokes: Vec::new(),
            last_inpaint_brush_point: None,
            inpaint_layer: None,
            inpaint_texture: None,
            inpaint_texture_revision: 0,
            inpaint_texture_key: None,
            inpaint_stroke_texture: None,
            inpaint_stroke_texture_key: None,
            inpaint_hovered_stroke: None,
            inpaint_selected_stroke: None,
            inpaint_focus_texture: None,
            inpaint_focus_texture_key: None,
            inpaint_source_cache: None,
            inpaint_pending_source: None,
            inpaint_active_dabs: None,
            inpaint_revision: 0,
            inpaint_job_document_id: 0,
            inpaint_job_generation: 0,
            inpaint_consent_open: false,
            inpaint_receiver: None,
            inpaint_download_progress: None,
            inpaint_inferencing: false,
            ai_denoise_consent_open: false,
            ai_denoise_receiver: None,
            ai_denoise_download_progress: None,
            ai_denoise_apply_progress: None,
            ai_denoise_cancellation: None,
            ai_denoise_job_document_id: 0,
            ai_denoise_resume_pending: false,
            android_app,
            picker_pending: false,
            android_batch_load_pending: false,
            pending_android_library_reset_reload: false,
            camera_profile_folder_importing_label: None,
            pending_android_profile_reload: None,
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn open_file_dialog(&mut self, _frame: &eframe::Frame) {
        if self.desktop_picker_receiver.is_some() {
            return;
        }
        let extensions = crate::pipeline::SUPPORTED_RAW_EXTENSIONS
            .iter()
            .flat_map(|extension| [extension.to_string(), extension.to_ascii_uppercase()])
            .collect::<Vec<_>>();
        let initial_directory = self
            .current_path
            .as_deref()
            .and_then(selected_picker_directory)
            .or_else(|| self.library.folder().map(std::path::Path::to_path_buf));
        let mut dialog = rfd::AsyncFileDialog::new().add_filter("RAW images", &extensions);
        if let Some(directory) = initial_directory {
            dialog = dialog.set_directory(directory);
        }
        let (sender, receiver) = mpsc::channel();
        let context = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let path = pollster::block_on(dialog.pick_file())
                .map(|handle| handle.path().to_path_buf());
            let _ = sender.send(crate::app::DesktopPickerEvent::RawFile(path));
            context.request_repaint();
        });
        self.desktop_picker_receiver = Some(receiver);
    }

    #[cfg(not(target_os = "android"))]
    pub fn open_library_folder_dialog(&mut self) {
        if self.desktop_picker_receiver.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new();
        if let Some(folder) = self.library.folder() {
            dialog = dialog.set_directory(folder);
        }
        let (sender, receiver) = mpsc::channel();
        let context = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let folder = pollster::block_on(dialog.pick_folder())
                .map(|handle| handle.path().to_path_buf());
            let _ = sender.send(crate::app::DesktopPickerEvent::LibraryFolder(folder));
            context.request_repaint();
        });
        self.desktop_picker_receiver = Some(receiver);
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn choose_camera_profile_folder(&mut self) {
        if self.desktop_picker_receiver.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new();
        if let Some(folder) = &self.camera_profile_folder {
            dialog = dialog.set_directory(folder);
        }
        let (sender, receiver) = mpsc::channel();
        let context = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let folder =
                pollster::block_on(dialog.pick_folder()).map(|handle| handle.path().to_path_buf());
            let _ = sender.send(crate::app::DesktopPickerEvent::CameraProfileFolder(folder));
            context.request_repaint();
        });
        self.desktop_picker_receiver = Some(receiver);
    }

    #[cfg(not(target_os = "android"))]
    fn apply_camera_profile_folder(&mut self, folder: PathBuf) {
        crate::pipeline::invalidate_dcp_profile_index();
        self.camera_profile_folder_label = folder
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);
        self.camera_profile_folder = Some(folder);
        self.camera_profile_auto_detect = false;
        self.last_camera_profile = None;
        self.raw_cache.clear();
        self.persist_performance_settings();
        self.notice = Some(
            "Camera profile folder updated. Reopen the RAW to apply the new profile selection."
                .to_owned(),
        );
    }

    #[cfg(target_os = "android")]
    pub(crate) fn choose_camera_profile_folder(&mut self) {
        if self.picker_pending {
            self.notice = Some("Finish the current Android file picker first.".to_owned());
            return;
        }
        match crate::android::open_camera_profile_folder(&self.android_app) {
            Ok(()) => {
                self.picker_pending = true;
                self.camera_profile_folder_importing_label = None;
                self.notice = None;
                self.status = "Choose a CameraProfiles folder…".to_owned();
            }
            Err(error) => self.notice = Some(error),
        }
    }

    pub(crate) fn clear_camera_profile_folder(&mut self) {
        let previous_folder = self.camera_profile_folder.take();
        if previous_folder.is_some() || self.camera_profile_auto_detect {
            crate::pipeline::invalidate_dcp_profile_index();
            self.camera_profile_folder_label = None;
            self.camera_profile_auto_detect = false;
            self.last_camera_profile = None;
            self.raw_cache.clear();
            #[cfg(target_os = "android")]
            if let Err(error) = crate::android::clear_camera_profile_folder_picker_location(
                &self.android_app,
            ) {
                log::warn!("{error}");
            }
            if self.persist_performance_settings() {
                #[cfg(target_os = "android")]
                if let Some(previous_folder) = previous_folder {
                    if let Err(error) = crate::android::remove_camera_profile_mirror(
                        &self.android_app,
                        &previous_folder,
                    ) {
                        log::warn!("{error}");
                    }
                }
            }
            self.notice = Some(
                "Camera profile folder cleared. Reopen the RAW to apply the new profile selection."
                    .to_owned(),
            );
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn auto_detect_camera_profile_folder(&mut self) {
        crate::pipeline::invalidate_dcp_profile_index();
        self.camera_profile_auto_detect = true;
        match crate::performance_settings::detected_adobe_camera_profile_folder() {
            Some(folder) => {
                self.camera_profile_folder = Some(folder.clone());
                self.camera_profile_folder_label =
                    Some("Adobe Camera Raw (auto-detected)".to_owned());
                self.last_camera_profile = None;
                self.raw_cache.clear();
                self.persist_performance_settings();
                self.notice = Some(format!(
                    "Using Adobe Camera Raw camera profiles from {}. Reopen the RAW to apply them.",
                    folder.display()
                ));
            }
            None => {
                self.camera_profile_folder = None;
                self.camera_profile_folder_label = None;
                self.last_camera_profile = None;
                self.raw_cache.clear();
                self.persist_performance_settings();
                self.notice = Some(
                    "No Adobe Camera Raw CameraProfiles folder was found in the standard location."
                        .to_owned(),
                );
            }
        }
    }

    pub(crate) fn set_camera_profile_mode(&mut self, mode: CameraProfileMode) {
        if self.camera_profile_mode == mode {
            return;
        }
        self.camera_profile_mode = mode;
        self.raw_cache.clear();
        self.persist_performance_settings();
        self.notice = Some(
            "RAW color profile mode changed. Reopen the RAW to apply the new profile selection."
                .to_owned(),
        );
    }

    pub(crate) fn select_camera_profile_for_current(
        &mut self,
        selection: Option<PathBuf>,
        frame: &eframe::Frame,
    ) {
        #[cfg(target_os = "android")]
        let _ = frame;
        #[cfg(target_os = "android")]
        if self.android_foreground_task_active() {
            self.notice = Some(
                "Wait for the current foreground operation to finish before changing camera profile."
                    .to_owned(),
            );
            self.egui_ctx.request_repaint();
            return;
        }
        if self.load_receiver.is_some() {
            self.notice = Some(
                "Wait for the current RAW load to finish before changing camera profile."
                    .to_owned(),
            );
            return;
        }
        if self.selected_camera_profile == selection {
            return;
        }
        let Some(sidecar_target) = self.sidecar_target.clone() else {
            self.notice = Some("Open a RAW before choosing a camera profile.".to_owned());
            return;
        };
        if let Some(selected) = selection.as_ref() {
            let is_available = self.loaded_raw.as_ref().is_some_and(|raw| {
                raw.available_camera_profiles
                    .iter()
                    .any(|candidate| candidate.path == *selected)
            });
            if !is_available {
                self.notice = Some(
                    "That DCP is no longer available for the current camera. Refresh the profile folder and reopen the RAW."
                        .to_owned(),
                );
                return;
            }
        }

        self.selected_camera_profile = selection.clone();
        // Only an explicit dropdown change updates the sticky default. Merely
        // opening an edited photo never mutates this preference.
        self.last_camera_profile = selection
            .as_ref()
            .zip(self.camera_profile_folder.as_ref())
            .and_then(|(selected, root)| selected.strip_prefix(root).ok())
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf);
        self.persist_performance_settings();
        self.queue_explicit_sidecar_save();
        let edit_override = self.capture_sidecar_edit_state();

        #[cfg(not(target_os = "android"))]
        {
            let crate::sidecar::SidecarTarget::Desktop { raw_path } = sidecar_target;
            let label = self
                .current_label
                .clone()
                .unwrap_or_else(|| raw_path.display().to_string());
            let sidecar_target = crate::sidecar::SidecarTarget::Desktop {
                raw_path: raw_path.clone(),
            };
            self.open_path_labeled_with_options(
                raw_path,
                label,
                false,
                sidecar_target,
                frame,
                Some(selection),
                Some(edit_override),
                None,
            );
        }

        #[cfg(target_os = "android")]
        {
            let (raw_uri, display_name) = match sidecar_target {
                crate::sidecar::SidecarTarget::Android {
                    raw_uri,
                    display_name,
                } => (raw_uri, display_name),
                crate::sidecar::SidecarTarget::Desktop { .. } => {
                    self.notice = Some(
                        "The current Android RAW does not have a reloadable library target."
                            .to_owned(),
                    );
                    return;
                }
            };
            match crate::android::open_library_document(&self.android_app, &raw_uri, &display_name)
            {
                Ok(()) => {
                    self.pending_android_profile_reload = Some((selection, edit_override));
                    self.picker_pending = true;
                    self.notice = None;
                    self.status = format!("Applying camera profile to {display_name}…");
                }
                Err(error) => {
                    self.notice = Some(format!("Could not reload RAW for camera profile: {error}"));
                }
            }
        }
    }

    #[cfg(target_os = "android")]
    pub fn open_file_dialog(&mut self, _frame: &eframe::Frame) {
        if self.android_foreground_task_active() {
            self.notice = Some(
                "Wait for the current foreground operation to finish before opening another RAW."
                    .to_owned(),
            );
            self.egui_ctx.request_repaint();
            return;
        }
        if self.picker_pending {
            return;
        }
        match crate::android::open_raw_document(&self.android_app) {
            Ok(()) => {
                self.picker_pending = true;
                self.notice = None;
                self.status = "Choose one or more RAW files…".to_owned();
            }
            Err(error) => self.notice = Some(error),
        }
    }

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

        // This is a user-owned picker result. Keep it distinct from the
        // Android batch exporter's internal document-load result routing.
        self.android_batch_load_pending = false;
        match crate::android::open_library_document(&self.android_app, uri, display_name) {
            Ok(()) => {
                self.picker_pending = true;
                self.notice = None;
                self.status = format!("Opening {display_name}…");
            }
            Err(error) => self.notice = Some(error),
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

    pub(crate) fn raw_cache_limit(&self) -> usize {
        self.raw_cache_limit
    }

    pub(crate) fn set_raw_cache_limit(&mut self, limit: usize) {
        let limit = limit.min(maximum_raw_cache_limit());
        if self.raw_cache_limit == limit {
            return;
        }
        self.raw_cache_limit = limit;
        self.trim_raw_cache();
        self.persist_performance_settings();
    }

    pub(crate) fn thumbnail_worker_count(&self) -> usize {
        self.library.thumbnail_worker_count()
    }

    pub(crate) fn set_thumbnail_worker_count(&mut self, workers: usize) {
        let context = self.egui_ctx.clone();
        let previous = self.library.thumbnail_worker_count();
        self.library.set_thumbnail_worker_count(workers, &context);
        if self.library.thumbnail_worker_count() != previous {
            self.persist_performance_settings();
        }
    }

    pub(crate) fn set_adjustment_copy_settings(&mut self, settings: AdjustmentCopySettings) {
        if self.adjustment_copy_settings == settings {
            return;
        }
        self.adjustment_copy_settings = settings;
        self.persist_performance_settings();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_display_color_management(&mut self, enabled: bool) {
        if self.display_color_management == enabled {
            return;
        }
        self.display_color_management = enabled;
        self.display_profile_last_probe = None;
        self.display_profile_fingerprint = None;
        self.persist_performance_settings();
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn choose_display_profile_override(&mut self) {
        if self.desktop_picker_receiver.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new().add_filter("ICC profiles", &["icc", "icm"]);
        if let Some(path) = self.display_profile_override.as_deref() {
            if let Some(parent) = path.parent() {
                dialog = dialog.set_directory(parent);
            }
        }
        let (sender, receiver) = mpsc::channel();
        let context = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let path =
                pollster::block_on(dialog.pick_file()).map(|handle| handle.path().to_path_buf());
            let _ = sender.send(crate::app::DesktopPickerEvent::DisplayProfile(path));
            context.request_repaint();
        });
        self.desktop_picker_receiver = Some(receiver);
    }

    #[cfg(not(target_os = "android"))]
    fn apply_display_profile_override(&mut self, path: PathBuf) {
        self.display_profile_override = Some(path);
        self.display_profile_last_probe = None;
        self.display_profile_fingerprint = None;
        self.persist_performance_settings();
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn poll_desktop_picker(&mut self, frame: &eframe::Frame) {
        let result = self
            .desktop_picker_receiver
            .as_ref()
            .and_then(|receiver| receiver.try_recv().ok());
        let Some(result) = result else {
            return;
        };
        self.desktop_picker_receiver = None;
        match result {
            crate::app::DesktopPickerEvent::RawFile(Some(path)) => self.open_path(path, frame),
            crate::app::DesktopPickerEvent::LibraryFolder(Some(folder)) => {
                self.library.open_folder(folder, &self.egui_ctx);
                self.persist_performance_settings();
                self.active_tab = AppTab::Library;
            }
            crate::app::DesktopPickerEvent::CameraProfileFolder(Some(folder)) => {
                self.apply_camera_profile_folder(folder);
            }
            crate::app::DesktopPickerEvent::OnnxRuntime(Ok(Some((path, sha256)))) => {
                self.onnx_runtime_path = Some(path);
                self.onnx_runtime_sha256 = Some(sha256);
                self.notice = Some(
                    "ONNX Runtime selection and SHA-256 pin saved. Restart AuRaw before generating another subject mask."
                        .to_owned(),
                );
            }
            crate::app::DesktopPickerEvent::OnnxRuntime(Err(error)) => {
                self.notice = Some(error);
            }
            crate::app::DesktopPickerEvent::DisplayProfile(Some(path)) => {
                self.apply_display_profile_override(path);
            }
            _ => {}
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn clear_display_profile_override(&mut self) {
        if self.display_profile_override.take().is_none() {
            return;
        }
        self.display_profile_last_probe = None;
        self.display_profile_fingerprint = None;
        self.persist_performance_settings();
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn display_profile_source(&self) -> Option<&str> {
        self.display_profile_source.as_deref()
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn sync_display_color_management(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        use std::hash::{Hash, Hasher};

        let screen_point = ctx.input(|input| {
            let viewport = input.viewport();
            let native_pixels_per_point = viewport
                .native_pixels_per_point
                .unwrap_or_else(|| ctx.pixels_per_point());
            let coordinate_scale = if cfg!(target_os = "macos") {
                1.0
            } else {
                native_pixels_per_point
            };
            viewport.outer_rect.map(|rect| {
                let center = rect.center();
                [
                    (center.x * coordinate_scale).round() as i32,
                    (center.y * coordinate_scale).round() as i32,
                ]
            })
        });
        let screen_changed = match (screen_point, self.display_profile_last_screen_point) {
            (Some(current), Some(previous)) => current != previous,
            (Some(_), None) | (None, Some(_)) => true,
            (None, None) => false,
        };
        let elapsed = self
            .display_profile_last_probe
            .map(|instant| instant.elapsed())
            .unwrap_or(Duration::MAX);
        if elapsed < Duration::from_secs(1)
            || (!screen_changed && elapsed < Duration::from_secs(10))
        {
            return;
        }
        self.display_profile_last_probe = Some(Instant::now());
        self.display_profile_last_screen_point = screen_point;

        let resolved = if !self.display_color_management {
            Ok(None)
        } else if let Some(path) = self.display_profile_override.as_deref() {
            crate::pipeline::read_display_icc_profile(path).map(Some)
        } else {
            crate::pipeline::discover_display_icc_profile(screen_point)
        };

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let (transform, label, source, fingerprint) = match resolved {
            Ok(Some(profile)) => {
                profile.bytes.hash(&mut hasher);
                let fingerprint = hasher.finish();
                let source = Some(profile.source);
                if self.display_profile_fingerprint == Some(fingerprint)
                    && self.display_profile_label == profile.label
                    && self.display_profile_source == source
                {
                    return;
                }
                match crate::pipeline::IccOutputTransform::from_icc(
                    &profile.bytes,
                    crate::pipeline::RenderingIntent::RelativeColorimetric,
                ) {
                    Ok(transform) => (transform, profile.label, source, fingerprint),
                    Err(error) => {
                        log::warn!("display ICC profile could not be built; using sRGB: {error:#}");
                        (
                            crate::pipeline::IccOutputTransform::srgb(),
                            "sRGB fallback".to_owned(),
                            Some(format!("ICC error: {error:#}")),
                            0,
                        )
                    }
                }
            }
            Ok(None) => {
                let label = if self.display_color_management {
                    "sRGB fallback".to_owned()
                } else {
                    "sRGB (color management disabled)".to_owned()
                };
                if self.display_profile_fingerprint == Some(0)
                    && self.display_profile_label == label
                    && self.display_profile_source.is_none()
                {
                    return;
                }
                (crate::pipeline::IccOutputTransform::srgb(), label, None, 0)
            }
            Err(error) => {
                let source = Some(format!("Profile discovery error: {error:#}"));
                if self.display_profile_fingerprint == Some(0)
                    && self.display_profile_label == "sRGB fallback"
                    && self.display_profile_source == source
                {
                    return;
                }
                log::warn!("display ICC discovery failed; using sRGB: {error:#}");
                (
                    crate::pipeline::IccOutputTransform::srgb(),
                    "sRGB fallback".to_owned(),
                    source,
                    0,
                )
            }
        };

        if self.display_profile_fingerprint == Some(fingerprint)
            && self.display_profile_label == label
            && self.display_profile_source == source
        {
            return;
        }

        let Some(render_state) = frame.wgpu_render_state() else {
            // No GPU state exists to update yet; committing the logical profile is
            // safe because each later pipeline install applies it before visibility.
            self.display_output_transform = transform;
            self.display_profile_label = label;
            self.display_profile_source = source;
            self.display_profile_fingerprint = Some(fingerprint);
            return;
        };

        let previous_transform = self.display_output_transform.clone();
        let mut updates = Vec::new();
        if let Some(pipeline) = self.gpu_pipeline.as_ref() {
            updates.push((
                "main preview",
                pipeline.write_output_transform(&render_state.queue, &transform),
            ));
        }
        if let Some(detail) = self.preview_detail.as_ref() {
            updates.push((
                "detail preview",
                detail
                    .pipeline
                    .write_output_transform(&render_state.queue, &transform),
            ));
        }
        if let Some(navigation) = self.preview_navigation.as_ref() {
            updates.push((
                "navigation preview",
                navigation
                    .pipeline
                    .write_output_transform(&render_state.queue, &transform),
            ));
        }
        if let Err(error) = collect_pipeline_update_results("install display ICC LUT", updates) {
            // Some buffers may already have received the new LUT, but no output
            // dispatch occurs before this point. Restore the previous transform on
            // every present pipeline and leave the logical profile/revision dirty.
            let mut rollbacks = Vec::new();
            if let Some(pipeline) = self.gpu_pipeline.as_ref() {
                rollbacks.push((
                    "main preview",
                    pipeline.write_output_transform(&render_state.queue, &previous_transform),
                ));
            }
            if let Some(detail) = self.preview_detail.as_ref() {
                rollbacks.push((
                    "detail preview",
                    detail
                        .pipeline
                        .write_output_transform(&render_state.queue, &previous_transform),
                ));
            }
            if let Some(navigation) = self.preview_navigation.as_ref() {
                rollbacks.push((
                    "navigation preview",
                    navigation
                        .pipeline
                        .write_output_transform(&render_state.queue, &previous_transform),
                ));
            }
            let rollback = collect_pipeline_update_results("restore display ICC LUT", rollbacks);
            self.pending_stage = Some(ProcessingStage::Output);
            self.notice = Some(
                "Could not update every preview color profile. The previous display transform remains active."
                    .to_owned(),
            );
            crate::diagnostics::record(format!(
                "transactional display-profile update failed: {error:#}; rollback={rollback:#?}"
            ));
            return;
        }

        // Commit logical metadata only after every present pipeline accepted the LUT.
        self.display_output_transform = transform;
        self.display_profile_label = label;
        self.display_profile_source = source;
        self.display_profile_fingerprint = Some(fingerprint);
        if self.gpu_pipeline.is_some() {
            self.queue_preview_processing(ProcessingStage::Output);
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn apply_display_output_transform(
        &self,
        queue: &wgpu::Queue,
        pipeline: &RawGpuPipeline,
    ) -> anyhow::Result<()> {
        pipeline
            .write_output_transform(queue, &self.display_output_transform)
            .map_err(|error| {
                anyhow::anyhow!("preview pipeline: install display ICC LUT: {error:#}")
            })
    }

    fn persist_performance_settings(&self) -> bool {
        let settings = crate::performance_settings::PerformanceSettings {
            raw_cache_files: self.raw_cache_limit,
            thumbnail_workers: self.library.thumbnail_worker_count(),
            preview_quality: self.preview_quality,
            camera_profile_mode: self.camera_profile_mode,
            camera_profile_folder: self.camera_profile_folder.clone(),
            camera_profile_folder_label: self.camera_profile_folder_label.clone(),
            camera_profile_auto_detect: self.camera_profile_auto_detect,
            last_camera_profile: self.last_camera_profile.clone(),
            #[cfg(not(target_os = "android"))]
            display_color_management: self.display_color_management,
            #[cfg(not(target_os = "android"))]
            display_profile_override: self.display_profile_override.clone(),
            adjustment_copy_settings: self.adjustment_copy_settings,
            #[cfg(not(target_os = "android"))]
            last_library_folder: self.library.folder().map(|folder| folder.to_path_buf()),
            ..Default::default()
        };
        let Some(settings_path) = self.performance_settings_path.as_deref() else {
            return false;
        };
        match crate::performance_settings::save(Some(settings_path), settings) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("{error}");
                false
            }
        }
    }

    fn cached_raw_decode(&mut self, key: &str) -> Option<Arc<LoadedRaw>> {
        let index = self.raw_cache.iter().position(|entry| entry.key == key)?;
        let entry = self.raw_cache.remove(index)?;
        let raw = Arc::clone(&entry.raw);
        self.raw_cache.push_back(entry);
        Some(raw)
    }

    fn cache_raw_decode(&mut self, key: String, raw: Arc<LoadedRaw>) {
        if self.raw_cache_limit == 0 {
            self.raw_cache.clear();
            return;
        }
        if let Some(index) = self.raw_cache.iter().position(|entry| entry.key == key) {
            self.raw_cache.remove(index);
        }
        self.raw_cache.push_back(CachedRawDecode { key, raw });
        self.trim_raw_cache();
    }

    fn trim_raw_cache(&mut self) {
        while self.raw_cache.len() > self.raw_cache_limit {
            self.raw_cache.pop_front();
        }
    }

    fn new_image_exposure(&self) -> ExposureParams {
        let previous = self.exposure;
        let mut exposure = ExposureParams::scene_referred_default();

        // These controls are application-level reconstruction preferences.
        // Creative and per-image calibration controls must not leak from the
        // previously opened photograph into a new RAW.
        exposure.highlight_method = previous.highlight_method;
        exposure.highlight_clip = previous.highlight_clip;
        exposure.highlight_reconstruction = previous.highlight_reconstruction;
        exposure.highlight_iterations = previous.highlight_iterations;
        exposure.highlight_color_adaptation = previous.highlight_color_adaptation;
        exposure.demosaic_mode = previous.demosaic_mode;
        exposure.dual_threshold = previous.dual_threshold;
        exposure.frequency_chroma = previous.frequency_chroma;
        exposure
    }

    fn open_path_labeled(
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
    fn open_path_labeled_with_options(
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
        let sidecar_generation = self.begin_sidecar_open();
        // Reuse compiled GPU programs across RAW opens; retire the old texture IDs for next-frame cleanup.
        let reusable_preview_pipeline = {
            let mut renderer = render_state.renderer.write();
            self.take_preview_pipeline_and_release_textures(&mut renderer)
        };
        #[cfg(target_os = "android")]
        let export_active_while_opening = self.export_receiver.is_some();
        #[cfg(target_os = "android")]
        let startup_gpu_prewarm_receiver = self.gpu_preview_prewarm_receiver.take();
        self.original_raw = None;
        self.loaded_raw = None;
        self.preview_raw = None;
        self.current_path = None;
        self.current_label = None;
        self.selected_camera_profile = None;
        self.image_status = format!("Loading {label}…");
        let initial_exposure = self.new_image_exposure();
        let preview_quality_setting = self.preview_quality;
        let preview_proxy_edge_setting =
            preview_quality_setting.proxy_edge_for_viewport(self.preview_viewport_pixels);
        let camera_profile_mode = self.camera_profile_mode;
        let camera_profile_folder = self.camera_profile_folder.clone();
        let last_camera_profile = self.last_camera_profile.clone();
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
                                        camera_profile_folder
                                            .as_ref()
                                            .map(|root| root.join(relative))
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
                let decode_started = Instant::now();
                let decoded: anyhow::Result<Arc<LoadedRaw>> = match cached_original_raw {
                    Some(raw) => Ok(raw),
                    None => match decode_gate.write() {
                        Ok(_decode_guard) => load_raw_file_with_profile_selection(
                            &path,
                            camera_profile_mode,
                            camera_profile_folder.as_deref(),
                            requested_camera_profile.as_deref(),
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
                                    "Loaded edits were migrated to the current processing version."
                                        .to_owned()
                                });
                                let use_adaptive_detail_defaults = loaded.migrated
                                    && loaded
                                        .edits
                                        .exposure
                                        .has_legacy_default_detail_settings();
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
                        "Edit state: process_version={} exposure={:.3} temperature={:.3} tint={:.3} saturation={:.3} vibrance={:.3} luminance_nr={:.1} color_nr={:.1} demosaic={:?} highlight={:?} masks={}",
                        rendered_exposure.process_version,
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
                    let selected_camera_profile = requested_camera_profile
                        .clone()
                        .filter(|requested| {
                            original_raw
                                .camera_profile_source
                                .as_ref()
                                .is_some_and(|applied| applied == requested)
                        });
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
                    let preview_spec = ProxySpec {
                        max_edge: preview_proxy_edge_setting,
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
                    // Interactive previews use bounded half-float working
                    // surfaces on every platform. Full-float remains mandatory
                    // for regression rendering and tiled export readback.
                    let preview_quality = ProcessingQuality::Preview;
                    #[cfg(target_os = "android")]
                    let mut startup_gpu_prewarm_template = None;
                    #[cfg(target_os = "android")]
                    if reusable_preview_pipeline.is_none() {
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
                    #[cfg(target_os = "android")]
                    let reusable_program_template = reusable_preview_pipeline
                        .as_ref()
                        .or(startup_gpu_prewarm_template.as_ref());
                    #[cfg(not(target_os = "android"))]
                    let reusable_program_template = reusable_preview_pipeline.as_ref();
                    let pipeline_started = Instant::now();
                    let pipeline = if let Some(template) = reusable_program_template {
                        match RawGpuPipeline::new_headless_reusing_programs(
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
                    #[cfg(target_os = "android")]
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
            self.notice = Some(format!("could not start RAW decode worker: {error}"));
            self.refresh_status();
        }
    }

    #[cfg(target_os = "android")]
    fn poll_android_picker(&mut self, frame: &eframe::Frame) {
        while let Some(result) = crate::android::take_camera_profile_folder_result() {
            match result {
                crate::android::CameraProfileFolderResult::ImportStarted { label } => {
                    self.camera_profile_folder_importing_label = Some(label.clone());
                    self.status = format!("Importing DCP profiles from {label}…");
                    self.notice = None;
                    // The SAF picker has returned, but its tree is copied on a Java
                    // worker thread. Keep picker_pending set so a second picker cannot
                    // race the import; eframe_impl keeps polling while it is pending.
                }
                crate::android::CameraProfileFolderResult::Picked {
                    path,
                    label,
                    profiles,
                } => {
                    self.picker_pending = false;
                    self.camera_profile_folder_importing_label = None;
                    crate::pipeline::invalidate_dcp_profile_index();
                    let previous_folder = self.camera_profile_folder.replace(path);
                    self.camera_profile_folder_label = Some(label.clone());
                    self.camera_profile_auto_detect = false;
                    self.last_camera_profile = None;
                    self.raw_cache.clear();
                    if self.persist_performance_settings() {
                        if let Some(previous_folder) = previous_folder {
                            if self.camera_profile_folder.as_deref()
                                != Some(previous_folder.as_path())
                            {
                                if let Err(error) = crate::android::remove_camera_profile_mirror(
                                    &self.android_app,
                                    &previous_folder,
                                ) {
                                    log::warn!("{error}");
                                }
                            }
                        }
                    }
                    self.notice = Some(format!(
                        "Camera profile folder '{label}' imported with {profiles} DCP {}. Reopen the RAW to apply the new profile selection.",
                        if profiles == 1 { "profile" } else { "profiles" }
                    ));
                }
                crate::android::CameraProfileFolderResult::Cancelled => {
                    self.picker_pending = false;
                    self.camera_profile_folder_importing_label = None;
                    self.notice = Some("No camera profile folder selected.".to_owned());
                }
                crate::android::CameraProfileFolderResult::Failed(error) => {
                    self.picker_pending = false;
                    self.camera_profile_folder_importing_label = None;
                    self.notice = Some(format!("Could not import camera profiles: {error}"));
                }
            }
        }

        while let Some(result) = crate::android::take_picker_result() {
            self.picker_pending = false;
            match result {
                crate::android::PickerResult::Picked(document) => {
                    self.library.refresh(&self.egui_ctx);
                    let batch_owned_open = self.android_batch_load_pending;
                    let profile_reload_owned_open = self.pending_android_profile_reload.is_some();
                    let reset_reload_owned_open =
                        std::mem::take(&mut self.pending_android_library_reset_reload);
                    let library_refresh_owned_open = !batch_owned_open
                        && !profile_reload_owned_open
                        && !reset_reload_owned_open
                        && self.library_ai_mask_refresh.is_some();
                    let keep_library_for_profile_reload =
                        profile_reload_owned_open && self.active_tab == AppTab::Library;
                    let keep_library_for_reset =
                        reset_reload_owned_open && self.active_tab == AppTab::Library;
                    self.active_tab = if batch_owned_open
                        || library_refresh_owned_open
                        || keep_library_for_profile_reload
                        || keep_library_for_reset
                    {
                        AppTab::Library
                    } else {
                        AppTab::Develop
                    };
                    let sidecar_target = crate::sidecar::SidecarTarget::Android {
                        raw_uri: document.library_uri,
                        display_name: document.display_name.clone(),
                    };
                    if let Some((selection, edit_override)) =
                        self.pending_android_profile_reload.take()
                    {
                        self.open_path_labeled_with_options(
                            document.path,
                            document.display_name,
                            document.delete_after_decode,
                            sidecar_target,
                            frame,
                            Some(selection),
                            Some(edit_override),
                            document.raw_fd_guard,
                        );
                    } else {
                        self.open_path_labeled(
                            document.path,
                            document.display_name,
                            document.delete_after_decode,
                            sidecar_target,
                            frame,
                            document.raw_fd_guard,
                        );
                    }

                    // `open_path_labeled*` reports setup failures synchronously by
                    // leaving `load_receiver` empty. Route that failure back to the
                    // internal owner immediately; otherwise Android batch export or
                    // library AI refresh would wait forever for a load event that can
                    // never arrive.
                    if self.load_receiver.is_none() {
                        let error = self.notice.clone().unwrap_or_else(|| {
                            "The RAW decode worker could not be started.".to_owned()
                        });
                        if batch_owned_open {
                            self.android_batch_load_pending = false;
                            self.complete_android_library_batch_export_item(Err(error));
                        } else if library_refresh_owned_open {
                            self.complete_android_library_ai_mask_open_failure(error, frame);
                        }
                    }
                }
                crate::android::PickerResult::BatchImported {
                    imported,
                    failed,
                    errors,
                } => {
                    self.pending_android_library_reset_reload = false;
                    self.active_tab = AppTab::Library;
                    self.library.refresh(&self.egui_ctx);
                    self.status = match (imported, failed) {
                        (0, 0) => "No RAW files were imported.".to_owned(),
                        (_, 0) => format!(
                            "Imported {imported} RAW {}.",
                            if imported == 1 { "file" } else { "files" }
                        ),
                        _ => format!(
                            "Imported {imported} RAW {}; {failed} failed.",
                            if imported == 1 { "file" } else { "files" }
                        ),
                    };
                    self.notice = if failed > 0 {
                        Some(if errors.is_empty() {
                            format!("{failed} selected RAW imports failed.")
                        } else {
                            format!("Some RAW files could not be imported:\n{errors}")
                        })
                    } else {
                        None
                    };
                }
                crate::android::PickerResult::Cancelled => {
                    self.pending_android_profile_reload = None;
                    let was_reset_reload =
                        std::mem::take(&mut self.pending_android_library_reset_reload);
                    if self.android_batch_load_pending {
                        self.android_batch_load_pending = false;
                        self.complete_android_library_batch_export_item(Err(
                            "RAW open was canceled".to_owned(),
                        ));
                    } else if self.library_ai_mask_refresh.is_some() {
                        self.complete_android_library_ai_mask_open_failure(
                            "RAW open was canceled".to_owned(),
                            frame,
                        );
                    } else if was_reset_reload {
                        self.notice = Some(
                            "The RAW could not be reloaded after resetting adjustments. Reopen it from the Library before continuing in Develop."
                                .to_owned(),
                        );
                    } else {
                        self.notice = Some("No RAW files selected.".to_owned());
                    }
                }
                crate::android::PickerResult::Failed(error) => {
                    let was_profile_reload = self.pending_android_profile_reload.take().is_some();
                    let was_reset_reload =
                        std::mem::take(&mut self.pending_android_library_reset_reload);
                    if self.android_batch_load_pending && !was_profile_reload && !was_reset_reload {
                        self.android_batch_load_pending = false;
                        self.complete_android_library_batch_export_item(Err(error));
                    } else if self.library_ai_mask_refresh.is_some()
                        && !was_profile_reload
                        && !was_reset_reload
                    {
                        self.complete_android_library_ai_mask_open_failure(error, frame);
                    } else {
                        self.notice = Some(if was_profile_reload {
                            format!("Could not reload RAW for camera profile: {error}")
                        } else if was_reset_reload {
                            format!("Could not reload RAW after resetting adjustments: {error}")
                        } else {
                            format!("Could not import the selected file: {error}")
                        });
                    }
                }
            }
        }
    }

    fn poll_load_worker(&mut self, frame: &eframe::Frame) {
        let received = self
            .load_receiver
            .as_ref()
            .map(|receiver| receiver.try_recv());
        let event = match received {
            Some(Ok(event)) => Some(event),
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.load_receiver = None;
                self.loading_label = None;
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
                    let previous =
                        self.take_preview_pipeline_and_release_textures(&mut renderer);
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
                self.lens_correction_generation =
                    self.lens_correction_generation.wrapping_add(1);
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

#[cfg(test)]
mod lifecycle_invariant_tests {
    use super::gpu_preview_prewarm_cfa_kind;
    use crate::pipeline::CfaKind;

    #[test]
    fn preview_prewarm_uses_the_bayer_template_explicitly() {
        assert_eq!(gpu_preview_prewarm_cfa_kind(), CfaKind::Bayer);
    }
}
