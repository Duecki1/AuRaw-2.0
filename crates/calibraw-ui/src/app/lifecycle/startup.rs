use super::*;

struct StartupResources {
    performance_settings_path: Option<std::path::PathBuf>,
    camera_profile_folder: Option<std::path::PathBuf>,
    camera_profile_folder_label: Option<String>,
    library: LibraryState,
    gpu_export_prewarm: Arc<crate::pipeline::GpuProgramPrewarm>,
    gpu_preview_prewarm_receiver: Option<mpsc::Receiver<Result<RawGpuPipeline, String>>>,
    #[cfg(not(target_os = "android"))]
    discord_presence: DiscordPresence,
    #[cfg(target_os = "android")]
    android_app: calibraw_ffi::AndroidApp,
}

impl CalibRawApp {
    fn build(
        ctx: &egui::Context,
        performance: crate::performance_settings::PerformanceSettings,
        resources: StartupResources,
    ) -> Self {
        let StartupResources {
            performance_settings_path,
            camera_profile_folder,
            camera_profile_folder_label,
            library,
            gpu_export_prewarm,
            gpu_preview_prewarm_receiver,
            #[cfg(not(target_os = "android"))]
            discord_presence,
            #[cfg(target_os = "android")]
            android_app,
        } = resources;
        let exposure = ExposureParams::scene_referred_default();
        let masks = MaskStack::default();
        let lens_correction = LensCorrectionState::default();
        let edit_history = EditHistory::new(&exposure, &masks, &lens_correction);
        #[cfg(not(target_os = "android"))]
        let runtime_selection = Self::load_onnx_runtime_selection();
        #[cfg(not(target_os = "android"))]
        let onnx_runtime_path = runtime_selection.as_ref().map(|(path, _)| path.clone());
        #[cfg(not(target_os = "android"))]
        let onnx_runtime_sha256 = runtime_selection.map(|(_, sha256)| sha256);
        Self {
            develop: DevelopState {
                current_path: None,
                original_raw: None,
                loaded_raw: None,
                preview_raw: None,
                exposure,
                raw_cache: VecDeque::new(),
                raw_cache_limit: performance.raw_cache_files,
                selected_camera_profile: None,
                geometry: GeometryTransform::default(),
                geometry_revision: 0,
                lens_correction,
                target_exposure: exposure,
                lens_correction_dirty: false,
                load_receiver: None,
                loading_label: None,
                image_status: "Open a RAW or TIFF file to get started.".to_owned(),
                current_label: None,
            },
            preview: PreviewState {
                gpu_pipeline: None,
                program_template: None,
                retired_egui_textures: Vec::new(),
                gpu_prewarm_receiver: gpu_preview_prewarm_receiver,
                quality: performance.preview_quality,
                zoom: 1.0,
                center: [0.5, 0.5],
                visible_uv: PreviewUvRect {
                    min: [0.0, 0.0],
                    max: [1.0, 1.0],
                },
                viewport_pixels: [1, 1],
                motion_at: None,
                touch_navigation_active: false,
                revision: 0,
                detail: None,
                navigation: None,
                detail_pending_stage: None,
                navigation_pending_stage: None,
                detail_urgent: false,
                quality_dirty: false,
                rebuild_receiver: None,
                detail_rebuild_receiver: None,
                original_exposure: exposure,
                original_requested: false,
                original_rendered_state: None,
                #[cfg(target_os = "android")]
                original_hold: None,
                pending_stage: None,
                #[cfg(target_os = "android")]
                lens_original_cache: None,
                #[cfg(target_os = "android")]
                lens_corrected_cache: None,
            },
            develop_ui: DevelopUiState {
                #[cfg(not(target_os = "android"))]
                reference: DevelopReferenceState::default(),
                loading_thumbnail: DevelopLoadingThumbnailState::default(),
                #[cfg(not(target_os = "android"))]
                filmstrip_open: performance.develop_filmstrip_open,
                #[cfg(not(target_os = "android"))]
                filmstrip_centered_path: None,
                #[cfg(not(target_os = "android"))]
                sidebar_open: true,
                crop_constraint_reference: None,
                crop_drag: None,
                straighten_tool_active: false,
                straighten_drag: None,
                white_balance_picker_active: false,
                white_balance_picker_drag: None,
                adjustment_section: AdjustmentSection::default(),
                mask_section: MaskSection::default(),
                tone_curve_tab: ToneCurveTab::default(),
                color_grade_tab: ColorGradeTab::default(),
                hsl_mixer_color: HslMixerColor::default(),
            },
            masks: MaskState {
                stack: masks,
                active_tool: None,
                brush_mode: BrushMode::Paint,
                subject_refinement_active: false,
                drag: None,
                last_brush_point: None,
                touch_gesture_backup: None,
                interaction_dirty_layer: None,
                interaction_last_upload: None,
                interaction_has_uncommitted_change: false,
                overlay_revision: 0,
                overlay_texture: None,
                overlay_texture_key: None,
                overlay_blink: None,
                thumbnail_revision: 0,
                thumbnail_group_textures: Vec::new(),
                thumbnail_component_mask: None,
                thumbnail_component_textures: Vec::new(),
                source_cache: None,
                subject_cache: None,
                dirty_layers: [false; MAX_LOCAL_MASKS],
                detail_dirty_layers: [false; MAX_LOCAL_MASKS],
                navigation_dirty_layers: [false; MAX_LOCAL_MASKS],
            },
            ai: AiState {
                birefnet_quality: performance.birefnet_quality,
                #[cfg(not(target_os = "android"))]
                subject_crop_refinement: performance.subject_crop_refinement,
                #[cfg(not(target_os = "android"))]
                gpu_acceleration: performance.ai_gpu_acceleration,
                masks_need_update: false,
                mask_update_active: false,
                mask_update_subject_pending: false,
                mask_update_object_queue: VecDeque::new(),
                mask_update_failed: false,
                #[cfg(not(target_os = "android"))]
                runtime_mode: performance.onnx_runtime_mode,
                #[cfg(not(target_os = "android"))]
                runtime_path: onnx_runtime_path,
                #[cfg(not(target_os = "android"))]
                runtime_sha256: onnx_runtime_sha256,
                runtime_download_consent_pending: false,
                library_mask_refresh: None,
                subject_consent_open: false,
                object_consent_open: false,
                object_pending_target: None,
                object_error_dialog: None,
                object_cache: None,
                denoise_consent_open: false,
                denoise_resume_pending: false,
            },
            inpaint: InpaintState {
                tool: InpaintTool::Remove,
                brush_size: 0.055,
                brush_hardness: 0.5,
                brush_opacity: 1.0,
                alignment: RetouchAlignment::None,
                source_point: None,
                source_pick_active: false,
                aligned_offset: None,
                edits: Arc::new(RemoveEditState::default()),
                active_points: Vec::new(),
                last_brush_uv: None,
                pending_brush: None,
                pending_retouch: None,
                model_consent_open: false,
                receiver: None,
                cancellation: None,
                processing_label: None,
                hovered_stroke: None,
                selected_stroke: None,
                stroke_opacity_edit_pending: false,
            },
            export: ExportState {
                gpu_prewarm: Some(gpu_export_prewarm),
                settings: ExportSettings::default(),
                task: None,
                batch: None,
                publish_pending: false,
                #[cfg(target_os = "android")]
                android_batch_load_pending: false,
            },
            persistence: PersistenceState {
                history: edit_history,
                lens_restore_masks: None,
                sidecar_target: None,
                sidecar_generation: 0,
                sidecar_saved_revision: None,
                sidecar_failed_revision: None,
                sidecar_pending: VecDeque::new(),
                sidecar_in_flight: None,
                sidecar_receiver: None,
                sidecar_save_feedback_until: None,
                sidecar_save_error_dialog: None,
                sidecar_autosave_deadline: None,
                developed_thumbnail_pending: None,
                developed_thumbnail_in_flight: None,
                developed_thumbnail_receiver: None,
            },
            preferences: PreferencesState {
                image_relative_brush_size: performance.image_relative_brush_size,
                show_develop_navigation_labels: performance.show_develop_navigation_labels,
                export_name_template: performance.export_name_template.clone(),
                #[cfg(not(target_os = "android"))]
                discord_rich_presence: performance.discord_rich_presence,
                ui_design: performance.ui_design,
                preview_backdrop: performance.preview_backdrop,
                onboarding_completed: performance.onboarding_completed,
                auto_check_updates: performance.auto_check_updates,
                ignored_update_version: performance.ignored_update_version.clone(),
                adjustment_copy_settings: performance.adjustment_copy_settings,
                performance_settings_path,
                camera_profile_mode: performance.camera_profile_mode,
                camera_profile_folder,
                camera_profile_folder_label,
                camera_profile_auto_detect: performance.camera_profile_auto_detect,
                last_camera_profile: performance.last_camera_profile.clone(),
            },
            ui: UiState {
                thumbnail_cache_size: None,
                thumbnail_cache_size_receiver: None,
                active_tab: AppTab::default(),
                sidebar_tab: SidebarTab::default(),
                #[cfg(not(target_os = "android"))]
                desktop_picker_receiver: None,
                status: "Open a RAW or TIFF file to get started.".to_owned(),
                adaptive_preview_backdrop: crate::ui::theme::CANVAS_BACKDROP,
                notice: None,
                onboarding_step: (!performance.onboarding_completed)
                    .then_some(OnboardingStep::Appearance),
                version_check: Default::default(),
            },
            library,
            #[cfg(not(target_os = "android"))]
            discord_presence,
            #[cfg(not(target_os = "android"))]
            app_icon_texture: crate::ui::top_bar::load_app_icon_texture(ctx),
            egui_ctx: ctx.clone(),
            foreground_operation: None,
            #[cfg(target_os = "android")]
            android: AndroidState {
                android_app,
                picker_pending: false,
                pending_android_library_reset_reload: false,
                camera_profile_folder_importing_label: None,
                pending_android_profile_reload: None,
            },
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            calibraw_gpu::install_uncaptured_gpu_error_handler(&render_state.device);
        }
        crate::ui::theme::install(&cc.egui_ctx);
        crate::diagnostics::record("CalibRaw desktop UI initialized");
        let performance_settings_path = crate::performance_settings::desktop_path();
        let performance = crate::performance_settings::load(performance_settings_path.as_deref());
        calibraw_ai::set_ai_acceleration_enabled(performance.ai_gpu_acceleration);
        let discord_presence = DiscordPresence::new(performance.discord_rich_presence);
        let last_library_folder = performance.last_library_folder.clone();
        let last_library_selected_folder = performance.last_library_selected_folder.clone();
        let mut camera_profile_folder = performance.camera_profile_folder.clone();
        let mut camera_profile_folder_label = performance.camera_profile_folder_label.clone();
        if performance.camera_profile_auto_detect
            && camera_profile_folder
                .as_ref()
                .is_none_or(|folder| !folder.is_dir())
        {
            camera_profile_folder = crate::performance_settings::detected_camera_profile_folder();
            if let Some(folder) = &camera_profile_folder {
                crate::diagnostics::record(format!(
                    "DCP profiles: auto-detected folder {}",
                    folder.display()
                ));
                camera_profile_folder_label = Some("DCP profiles (auto-detected)".to_owned());
            } else {
                camera_profile_folder_label = None;
            }
        }
        prewarm_dcp_profile_folder(camera_profile_folder.clone());
        let gpu_export_prewarm = Arc::new(crate::pipeline::GpuProgramPrewarm::new());
        let gpu_preview_prewarm_receiver = spawn_gpu_preview_prewarm(
            cc,
            Some(crate::thumbnail_cache::desktop_app_cache_root()),
            Arc::clone(&gpu_export_prewarm),
        );
        let library = LibraryState::new_desktop_with_preferences(
            performance.thumbnail_workers,
            performance.library_thumbnail_size,
            performance.library_sort_order,
            performance.library_folder_sidebar_open,
            performance.render_edited_thumbnails_during_indexing,
        );
        let mut app = Self::build(
            &cc.egui_ctx,
            performance,
            StartupResources {
                performance_settings_path,
                camera_profile_folder,
                camera_profile_folder_label,
                library,
                gpu_export_prewarm,
                gpu_preview_prewarm_receiver,
                discord_presence,
            },
        );
        crate::ui::theme::apply(&cc.egui_ctx, app.preferences.ui_design);
        if let Some(folder) = last_library_folder.filter(|folder| folder.is_dir()) {
            app.library
                .restore_folder(folder, last_library_selected_folder, &cc.egui_ctx);
        }
        if app.preferences.auto_check_updates {
            app.check_for_updates(false);
        }
        app
    }

    #[cfg(target_os = "android")]
    pub fn new_android(
        cc: &eframe::CreationContext<'_>,
        android_app: calibraw_ffi::AndroidApp,
    ) -> Self {
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            calibraw_gpu::install_uncaptured_gpu_error_handler(&render_state.device);
        }
        crate::android::install_context(&cc.egui_ctx);
        crate::ui::theme::install(&cc.egui_ctx);
        match crate::android::device_diagnostics(&android_app) {
            Ok(info) => crate::diagnostics::set_device_info(info),
            Err(error) => crate::diagnostics::record(error),
        }
        crate::diagnostics::record("CalibRaw Android UI initialized");
        let performance_settings_path = crate::android::performance_settings_path(&android_app)
            .map_err(|error| log::warn!("{error}"))
            .ok();
        let gpu_pipeline_cache_root = crate::android::gpu_pipeline_cache_dir(&android_app)
            .map_err(|error| log::warn!("{error}"))
            .ok();
        if std::env::var_os("CALIBRAW_LENSFUN_DB").is_none() {
            match crate::android::lensfun_database_dir(&android_app) {
                Ok(path) => {
                    std::env::set_var("CALIBRAW_LENSFUN_DB", &path);
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
        let gpu_preview_prewarm_receiver =
            spawn_gpu_preview_prewarm(cc, gpu_pipeline_cache_root, Arc::clone(&gpu_export_prewarm));
        let camera_profile_folder = performance.camera_profile_folder.clone();
        let camera_profile_folder_label = performance.camera_profile_folder_label.clone();
        prewarm_dcp_profile_folder(camera_profile_folder.clone());
        let library = LibraryState::new_android_with_workers(
            android_app.clone(),
            &cc.egui_ctx,
            performance.thumbnail_workers,
            performance.library_thumbnail_size,
            performance.library_sort_order,
            performance.last_android_library_folder.clone(),
            performance.render_edited_thumbnails_during_indexing,
        );
        let mut app = Self::build(
            &cc.egui_ctx,
            performance,
            StartupResources {
                performance_settings_path,
                camera_profile_folder,
                camera_profile_folder_label,
                library,
                gpu_export_prewarm,
                gpu_preview_prewarm_receiver,
                android_app,
            },
        );
        crate::ui::theme::apply(&cc.egui_ctx, app.preferences.ui_design);
        if let Err(error) = crate::android::set_light_system_bars(
            &app.android.android_app,
            !app.preferences.ui_design.is_dark(),
        ) {
            log::warn!("{error}");
        }
        if app.preferences.auto_check_updates {
            app.check_for_updates(false);
        }
        app
    }
}
