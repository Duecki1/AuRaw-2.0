use super::*;

impl AurawApp {
    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn empty(ctx: &egui::Context) -> Self {
        let performance_settings_path = crate::performance_settings::desktop_path();
        let performance = crate::performance_settings::load(performance_settings_path.as_deref());
        auraw_ai::set_ai_acceleration_enabled(performance.ai_gpu_acceleration);
        let cloud_config = crate::cloud::CloudConfig {
            enabled: performance.cloud_enabled,
            server_url: performance.cloud_server_url.clone(),
            access_token: performance.cloud_access_token.clone(),
        };
        let cloud_cache_root = crate::cloud::cache_root(performance_settings_path.as_deref());
        let last_library_folder = performance.last_library_folder.clone();
        let last_library_selected_folder = performance.last_library_selected_folder.clone();
        let last_library_view = performance.last_library_view;
        let last_cloud_library_folder = performance.last_cloud_library_folder.clone();
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
            preview_program_template: None,
            retired_egui_textures: Vec::new(),
            gpu_preview_prewarm_receiver: None,
            gpu_export_prewarm: None,
            preview_quality: performance.preview_quality,
            image_relative_brush_size: performance.image_relative_brush_size,
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
            preview_rebuild_receiver: None,
            original_preview_exposure: exposure,
            original_preview_requested: false,
            original_preview_rendered_state: None,
            android_original_hold: None,
            exposure,
            library: LibraryState::new_desktop_with_preferences(
                ctx,
                performance.thumbnail_workers,
                performance.library_thumbnail_size,
                performance.library_sort_order,
                performance.library_folder_sidebar_open,
            ),
            develop_reference: DevelopReferenceState::default(),
            develop_loading_thumbnail: DevelopLoadingThumbnailState::default(),
            develop_filmstrip_open: performance.develop_filmstrip_open,
            develop_filmstrip_centered_path: None,
            develop_sidebar_open: true,
            adjustment_copy_settings: performance.adjustment_copy_settings,
            adjustment_clipboard: None,
            raw_cache: VecDeque::new(),
            raw_cache_limit: performance.raw_cache_files,
            performance_settings_path,
            thumbnail_cache_size: None,
            thumbnail_cache_size_receiver: None,
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
            geometry: GeometryTransform::default(),
            crop_constraint_reference: None,
            crop_drag: None,
            straighten_tool_active: false,
            straighten_drag: None,
            white_balance_picker_active: false,
            white_balance_picker_drag: None,
            geometry_revision: 0,
            adjustment_section: AdjustmentSection::default(),
            mask_section: MaskSection::default(),
            tone_curve_tab: ToneCurveTab::default(),
            color_grade_tab: ColorGradeTab::default(),
            hsl_mixer_color: HslMixerColor::default(),
            export_settings: ExportSettings::default(),
            masks,
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            subject_refinement_active: false,
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
            birefnet_quality: performance.birefnet_quality,
            ai_gpu_acceleration: performance.ai_gpu_acceleration,
            ai_masks_need_update: false,
            ai_mask_update_active: false,
            ai_mask_update_subject_pending: false,
            ai_mask_update_object_queue: VecDeque::new(),
            ai_mask_update_failed: false,
            onnx_runtime_path,
            onnx_runtime_sha256,
            #[cfg(not(target_os = "android"))]
            desktop_picker_receiver: None,
            status: "Open a RAW or TIFF file to get started.".to_owned(),
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
            sidecar_save_error_dialog: None,
            sidecar_conflict_receiver: None,
            sidecar_conflict_resolution_error: None,
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
            image_status: "Open a RAW or TIFF file to get started.".to_owned(),
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
            inpaint_tool: InpaintStrokeKind::Remove,
            inpaint_source_anchor: None,
            inpaint_source_offset: None,
            inpaint_source_pick_active: false,
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
            inpaint_replace_index: None,
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
            app.library
                .restore_folder(folder, last_library_selected_folder, ctx);
        }
        app.library
            .configure_cloud(cloud_config, cloud_cache_root, ctx);
        app.library.restore_navigation(
            last_library_view,
            last_cloud_library_folder,
            ctx,
        );
        app
    }

    #[cfg(not(target_os = "android"))]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            auraw_gpu::install_uncaptured_gpu_error_handler(&render_state.device);
        }
        crate::ui::theme::install(&cc.egui_ctx);
        crate::diagnostics::record("AuRaw desktop UI initialized");
        let gpu_export_prewarm = Arc::new(crate::pipeline::GpuProgramPrewarm::new());
        let gpu_preview_prewarm_receiver = spawn_gpu_preview_prewarm(
            cc,
            Some(crate::thumbnail_cache::desktop_app_cache_root()),
            Arc::clone(&gpu_export_prewarm),
        );
        let mut app = Self::empty(&cc.egui_ctx);
        app.gpu_preview_prewarm_receiver = gpu_preview_prewarm_receiver;
        app.gpu_export_prewarm = Some(gpu_export_prewarm);
        app
    }

    #[cfg(target_os = "android")]
    pub fn new_android(
        cc: &eframe::CreationContext<'_>,
        android_app: auraw_ffi::AndroidApp,
    ) -> Self {
        if let Some(render_state) = cc.wgpu_render_state.as_ref() {
            auraw_gpu::install_uncaptured_gpu_error_handler(&render_state.device);
        }
        crate::android::install_context(&cc.egui_ctx);
        // Share AuRaw's palette, typography, icon font, and widget styling with
        crate::ui::theme::install(&cc.egui_ctx);
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
        let cloud_config = crate::cloud::CloudConfig {
            enabled: performance.cloud_enabled,
            server_url: performance.cloud_server_url.clone(),
            access_token: performance.cloud_access_token.clone(),
        };
        let cloud_cache_root = crate::cloud::cache_root(performance_settings_path.as_deref());
        let last_library_view = performance.last_library_view;
        let last_cloud_library_folder = performance.last_cloud_library_folder.clone();
        prewarm_dcp_profile_folder(performance.camera_profile_folder.clone());
        let gpu_export_prewarm = Arc::new(crate::pipeline::GpuProgramPrewarm::new());
        let gpu_preview_prewarm_receiver =
            spawn_gpu_preview_prewarm(cc, gpu_pipeline_cache_root, Arc::clone(&gpu_export_prewarm));
        let exposure = ExposureParams::scene_referred_default();
        let masks = MaskStack::default();
        let lens_correction = LensCorrectionState::default();
        let edit_history = EditHistory::new(&exposure, &masks, &lens_correction);
        let mut app = Self {
            current_path: None,
            original_raw: None,
            loaded_raw: None,
            preview_raw: None,
            gpu_pipeline: None,
            preview_program_template: None,
            retired_egui_textures: Vec::new(),
            gpu_preview_prewarm_receiver,
            gpu_export_prewarm: Some(gpu_export_prewarm),
            preview_quality: performance.preview_quality,
            image_relative_brush_size: performance.image_relative_brush_size,
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
            preview_rebuild_receiver: None,
            original_preview_exposure: exposure,
            original_preview_requested: false,
            original_preview_rendered_state: None,
            android_original_hold: None,
            exposure,
            library: LibraryState::new_android_with_workers(
                android_app.clone(),
                &cc.egui_ctx,
                performance.thumbnail_workers,
                performance.library_thumbnail_size,
                performance.library_sort_order,
                performance.last_android_library_folder.clone(),
            ),
            develop_loading_thumbnail: DevelopLoadingThumbnailState::default(),
            adjustment_copy_settings: performance.adjustment_copy_settings,
            adjustment_clipboard: None,
            raw_cache: VecDeque::new(),
            raw_cache_limit: performance.raw_cache_files,
            performance_settings_path,
            thumbnail_cache_size: None,
            thumbnail_cache_size_receiver: None,
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
            geometry: GeometryTransform::default(),
            crop_constraint_reference: None,
            crop_drag: None,
            straighten_tool_active: false,
            straighten_drag: None,
            white_balance_picker_active: false,
            white_balance_picker_drag: None,
            geometry_revision: 0,
            adjustment_section: AdjustmentSection::default(),
            mask_section: MaskSection::default(),
            tone_curve_tab: ToneCurveTab::default(),
            color_grade_tab: ColorGradeTab::default(),
            hsl_mixer_color: HslMixerColor::default(),
            export_settings: ExportSettings::default(),
            masks,
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            subject_refinement_active: false,
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
            birefnet_quality: performance.birefnet_quality,
            ai_masks_need_update: false,
            ai_mask_update_active: false,
            ai_mask_update_subject_pending: false,
            ai_mask_update_object_queue: VecDeque::new(),
            ai_mask_update_failed: false,
            status: "Open a RAW or TIFF file to get started.".to_owned(),
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
            sidecar_save_error_dialog: None,
            sidecar_conflict_receiver: None,
            sidecar_conflict_resolution_error: None,
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
            image_status: "Open a RAW or TIFF file to get started.".to_owned(),
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
            inpaint_tool: InpaintStrokeKind::Remove,
            inpaint_source_anchor: None,
            inpaint_source_offset: None,
            inpaint_source_pick_active: false,
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
            inpaint_replace_index: None,
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
        };
        app.library
            .configure_cloud(cloud_config, cloud_cache_root, &cc.egui_ctx);
        app.library.restore_navigation(
            last_library_view,
            last_cloud_library_folder,
            &cc.egui_ctx,
        );
        app
    }
}
