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
            )
        })
    })
}

fn install_missing_range_sources(masks: &mut MaskStack, source: &MaskRgbImage) {
    for mask in &mut masks.masks {
        for component in &mut mask.components {
            match &mut component.geometry {
                MaskGeometry::LuminanceRange { source: target, .. }
                | MaskGeometry::ColorRange { source: target, .. }
                    if target.is_none() =>
                {
                    *target = Some(source.clone());
                }
                _ => {}
            }
        }
    }
}

impl AurawApp {
    fn install_lightroom_visuals(ctx: &egui::Context) {
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
            preview_quality: PreviewQuality::default(),
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
            raw_cache: VecDeque::new(),
            raw_cache_limit: performance.raw_cache_files,
            performance_settings_path,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
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
            onnx_runtime_path,
            onnx_runtime_sha256,
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
            sidecar_autosave_deadline: None,
            developed_thumbnail_pending: None,
            developed_thumbnail_in_flight: None,
            developed_thumbnail_receiver: None,
            egui_ctx: ctx.clone(),
            target_exposure: exposure,
            pending_stage: None,
            lens_correction_dirty: false,
            load_receiver: None,
            loading_label: None,
            export_receiver: None,
            export_progress: None,
            export_publish_pending: false,
            image_status: "Open a RAW file to get started.".to_owned(),
            current_label: None,
            notice: None,
            dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            detail_dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            navigation_dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            subject_consent_open: false,
            subject_receiver: None,
            subject_download_progress: None,
            subject_inferencing: false,
            object_consent_open: false,
            object_pending_target: None,
            object_receiver: None,
            object_download_progress: None,
            object_inferencing: false,
            object_decoder_only: false,
            object_generation: 0,
            object_job_generation: 0,
            object_job_target: None,
            object_cache: None,
            android_tab_swipe: AndroidTabSwipe::default(),
            tab_swipe_surface_id: None,
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
        let performance = crate::performance_settings::load(performance_settings_path.as_deref());
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
            preview_quality: PreviewQuality::default(),
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
            raw_cache: VecDeque::new(),
            raw_cache_limit: performance.raw_cache_files,
            performance_settings_path,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
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
            sidecar_autosave_deadline: None,
            developed_thumbnail_pending: None,
            developed_thumbnail_in_flight: None,
            developed_thumbnail_receiver: None,
            egui_ctx: cc.egui_ctx.clone(),
            target_exposure: exposure,
            pending_stage: None,
            lens_correction_dirty: false,
            load_receiver: None,
            loading_label: None,
            export_receiver: None,
            export_progress: None,
            export_publish_pending: false,
            image_status: "Open a RAW file to get started.".to_owned(),
            current_label: None,
            notice: None,
            dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            detail_dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            navigation_dirty_mask_layers: [false; MAX_LOCAL_MASKS],
            subject_consent_open: false,
            subject_receiver: None,
            subject_download_progress: None,
            subject_inferencing: false,
            object_consent_open: false,
            object_pending_target: None,
            object_receiver: None,
            object_download_progress: None,
            object_inferencing: false,
            object_decoder_only: false,
            object_generation: 0,
            object_job_generation: 0,
            object_job_target: None,
            object_cache: None,
            android_tab_swipe: AndroidTabSwipe::default(),
            tab_swipe_surface_id: None,
            android_app,
            picker_pending: false,
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn open_file_dialog(&mut self, frame: &eframe::Frame) {
        let extensions = crate::pipeline::SUPPORTED_RAW_EXTENSIONS
            .iter()
            .flat_map(|extension| [extension.to_string(), extension.to_ascii_uppercase()])
            .collect::<Vec<_>>();
        let Some(path) = rfd::FileDialog::new()
            .add_filter("RAW images", &extensions)
            .pick_file()
        else {
            return;
        };

        self.open_path(path, frame);
    }

    #[cfg(not(target_os = "android"))]
    pub fn open_library_folder_dialog(&mut self) {
        let Some(folder) = rfd::FileDialog::new().pick_folder() else {
            return;
        };
        self.library.open_folder(folder, &self.egui_ctx);
        self.persist_performance_settings();
        self.active_tab = AppTab::Library;
    }

    #[cfg(target_os = "android")]
    pub fn open_file_dialog(&mut self, _frame: &eframe::Frame) {
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
        if self.picker_pending {
            return;
        }
        match crate::android::open_library_document(&self.android_app, uri, display_name) {
            Ok(()) => {
                self.picker_pending = true;
                self.notice = None;
                self.status = format!("Opening {display_name}…");
            }
            Err(error) => self.notice = Some(error),
        }
    }

    pub fn open_path(&mut self, path: PathBuf, frame: &eframe::Frame) {
        let label = path.display().to_string();
        self.active_tab = AppTab::Develop;
        let sidecar_target = crate::sidecar::SidecarTarget::Desktop {
            raw_path: path.clone(),
        };
        self.open_path_labeled(path, label, false, sidecar_target, frame);
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

    fn persist_performance_settings(&self) {
        let settings = crate::performance_settings::PerformanceSettings {
            raw_cache_files: self.raw_cache_limit,
            thumbnail_workers: self.library.thumbnail_worker_count(),
            #[cfg(not(target_os = "android"))]
            last_library_folder: self.library.folder().map(|folder| folder.to_path_buf()),
            ..Default::default()
        };
        if let Err(error) =
            crate::performance_settings::save(self.performance_settings_path.as_deref(), settings)
        {
            log::warn!("{error}");
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
    ) {
        if self.load_receiver.is_some()
            || self.export_receiver.is_some()
            || self.export_publish_pending
        {
            if delete_after_decode {
                remove_temporary_raw(&path);
            }
            self.notice = Some(if self.load_receiver.is_some() {
                "Wait for the current RAW to finish opening.".to_owned()
            } else {
                "Wait for the current export to finish before opening another RAW.".to_owned()
            });
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            if delete_after_decode {
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
        let raw_cache_key = raw_cache_key_for_target(&sidecar_target);
        let cached_original_raw = self.cached_raw_decode(&raw_cache_key);
        let decode_was_cached = cached_original_raw.is_some();
        crate::diagnostics::record(format!(
            "RAW open requested: label=\"{label}\" cached={decode_was_cached} preview_quality={}",
            self.preview_quality.label()
        ));
        let sidecar_generation = self.begin_sidecar_open();
        {
            let mut renderer = render_state.renderer.write();
            if let Some(old) = self.gpu_pipeline.take() {
                if let Some(texture_id) = old.egui_texture_id {
                    renderer.free_texture(&texture_id);
                }
            }
            if let Some(old) = self.preview_detail.take() {
                if let Some(texture_id) = old.pipeline.egui_texture_id {
                    renderer.free_texture(&texture_id);
                }
            }
            if let Some(old) = self.preview_navigation.take() {
                if let Some(texture_id) = old.pipeline.egui_texture_id {
                    renderer.free_texture(&texture_id);
                }
            }
        }
        self.original_raw = None;
        self.loaded_raw = None;
        self.preview_raw = None;
        self.current_path = None;
        self.current_label = None;
        self.image_status = format!("Loading {label}…");
        let initial_exposure = self.new_image_exposure();
        let preview_quality_setting = self.preview_quality;
        self.original_preview_exposure = initial_exposure;
        self.original_preview_requested = false;
        self.original_preview_rendered_state = None;
        self.android_original_hold = None;
        self.exposure = initial_exposure;
        self.target_exposure = initial_exposure;
        self.masks.clear();
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
        self.subject_consent_open = false;
        self.subject_receiver = None;
        self.subject_download_progress = None;
        self.subject_inferencing = false;
        self.object_consent_open = false;
        self.object_pending_target = None;
        self.object_receiver = None;
        self.object_download_progress = None;
        self.object_inferencing = false;
        self.object_decoder_only = false;
        self.object_generation = self.object_generation.wrapping_add(1);
        self.object_job_generation = 0;
        self.object_job_target = None;
        self.object_cache = None;
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
        self.preview_viewport_pixels = [1, 1];
        self.preview_motion_at = None;
        self.preview_touch_navigation_active = false;
        self.preview_revision = self.preview_revision.wrapping_add(1);
        self.lens_correction = LensCorrectionState::default();
        self.lens_correction_dirty = false;
        self.reset_edit_history();
        let source_path = (!delete_after_decode).then_some(path.clone());
        let repaint = self.egui_ctx.clone();
        let (sender, receiver) = mpsc::channel();
        let cleanup_path_on_spawn_failure = delete_after_decode.then(|| path.clone());
        #[cfg(target_os = "android")]
        let sidecar_android_app = self.android_app.clone();

        self.load_receiver = Some(receiver);
        self.loading_label = Some(label.clone());
        self.notice = None;
        self.refresh_status();

        let spawn_result = std::thread::Builder::new()
            .name("auraw-decode-preview".to_owned())
            .spawn(move || {
                let open_started = Instant::now();
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
                let decode_started = Instant::now();
                let decoded: anyhow::Result<Arc<LoadedRaw>> = match cached_original_raw {
                    Some(raw) => Ok(raw),
                    None => match decode_gate.write() {
                        Ok(_decode_guard) => load_raw_file(&path).map(Arc::new),
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
                if delete_after_decode {
                    remove_temporary_raw(&path);
                }

                let result = (|| {
                    let (
                        rendered_exposure,
                        mut rendered_masks,
                        saved_lens,
                        mut sidecar_warning,
                        sidecar_needs_rewrite,
                    ) = match loaded_sidecar {
                        Ok(Some(loaded)) => {
                            let warning = loaded.migrated.then(|| {
                                "Loaded edits were migrated to the current processing version."
                                    .to_owned()
                            });
                            (
                                loaded.edits.exposure,
                                Arc::unwrap_or_clone(loaded.edits.masks),
                                Some(loaded.edits.lens),
                                warning,
                                loaded.migrated,
                            )
                        }
                        Ok(None) => (
                            initial_exposure,
                            MaskStack::default(),
                            None,
                            None,
                            false,
                        ),
                        Err(error) => (
                            initial_exposure,
                            MaskStack::default(),
                            None,
                            Some(format!(
                                "Could not load this RAW's sidecar; using default edits: {error}"
                            )),
                            false,
                        ),
                    };
                    crate::diagnostics::record(format!(
                        "Edit state: process_version={} exposure={:.3} temperature={:.3} tint={:.3} saturation={:.3} vibrance={:.3} demosaic={:?} highlight={:?} masks={}",
                        rendered_exposure.process_version,
                        rendered_exposure.exposure,
                        rendered_exposure.temperature,
                        rendered_exposure.tint,
                        rendered_exposure.saturation,
                        rendered_exposure.vibrance,
                        rendered_exposure.demosaic_mode,
                        rendered_exposure.highlight_method,
                        rendered_masks.masks.len(),
                    ));
                    let original_raw = decoded.map_err(|error| format!("{error:#}"))?;
                    let mut lens_correction =
                        LensCorrectionState::from_catalog(lensfun_catalog(&original_raw));
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
                            match apply_lensfun_correction(&original_raw, &selection) {
                                Ok(corrected) => {
                                    lens_correction.applied = true;
                                    lens_correction.catalog.status = format!(
                                        "Automatically applied {} from RAW metadata",
                                        selection.label()
                                    );
                                    Arc::new(corrected)
                                }
                                Err(error) => {
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
                    let preview_spec = ProxySpec {
                        max_edge: preview_quality_setting.proxy_edge(),
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
                        GpuParams::new(&rendered_exposure, &rendered_masks, &preview_raw);
                    // Interactive previews use bounded half-float working
                    // surfaces on every platform. Full-float remains mandatory
                    // for regression rendering and tiled export readback.
                    let preview_quality = ProcessingQuality::Preview;
                    let pipeline_started = Instant::now();
                    let pipeline = RawGpuPipeline::new_headless_with_quality(
                        &device,
                        &queue,
                        &preview_raw,
                        &initial_params,
                        preview_quality,
                    )
                    .map_err(|error| format!("GPU preview setup failed: {error:#}"))?;
                    crate::diagnostics::record(format!(
                        "GPU preview pipeline created in {:.3}s",
                        pipeline_started.elapsed().as_secs_f64()
                    ));

                    // Range and promptable-object source images are canonical RAW renditions,
                    // not user edit data. Sidecars omit these large shared
                    // caches and reconstruct one source on this decode worker.
                    let mut mask_source = None;
                    if needs_canonical_mask_source(&rendered_masks) {
                        let source_edge = if cfg!(target_os = "android") {
                            1600
                        } else {
                            2048
                        };
                        let source_raw = if full_raw.width.max(full_raw.height) <= source_edge {
                            Arc::clone(&full_raw)
                        } else {
                            Arc::new(build_proxy(
                                &full_raw,
                                ProxySpec {
                                    max_edge: source_edge,
                                },
                            ))
                        };
                        let reference_exposure = ExposureParams::scene_referred_default();
                        let reference_masks = MaskStack::default();
                        let reference_params =
                            GpuParams::new(&reference_exposure, &reference_masks, &source_raw);
                        let reference_pipeline = RawGpuPipeline::new_headless_reusing_programs(
                            &device,
                            &queue,
                            &source_raw,
                            &reference_params,
                            ProcessingQuality::Preview,
                            &pipeline,
                        )
                        .map_err(|error| {
                            format!("range-mask source setup failed: {error:#}")
                        })?;
                        reference_pipeline.recompute(&queue, &device, &reference_params);
                        let rgba = reference_pipeline
                            .read_output_region_blocking(
                                &device,
                                &queue,
                                0,
                                0,
                                reference_pipeline.width,
                                reference_pipeline.height,
                            )
                            .map_err(|error| {
                                format!("range-mask source readback failed: {error:#}")
                            })?;
                        let source = MaskRgbImage::new(
                            reference_pipeline.width,
                            reference_pipeline.height,
                            rgba,
                        )
                        .ok_or_else(|| "range-mask source dimensions are invalid".to_owned())?;
                        install_missing_range_sources(&mut rendered_masks, &source);
                        mask_source = Some(source);
                    }

                    let params =
                        GpuParams::new(&rendered_exposure, &rendered_masks, &preview_raw);
                    Self::upload_preview_masks(
                        &pipeline,
                        &queue,
                        &rendered_masks,
                        &preview_raw,
                    )?;
                    let first_render_started = Instant::now();
                    pipeline.recompute(&queue, &device, &params);
                    crate::diagnostics::record(format!(
                        "Initial GPU preview dispatch submitted in {:.3}s",
                        first_render_started.elapsed().as_secs_f64()
                    ));
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
                        mask_source,
                        lens_correction,
                        sidecar_target,
                        sidecar_generation,
                        sidecar_warning,
                        sidecar_needs_rewrite,
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
        while let Some(result) = crate::android::take_picker_result() {
            self.picker_pending = false;
            match result {
                crate::android::PickerResult::Picked(document) => {
                    self.library.refresh(&self.egui_ctx);
                    self.active_tab = AppTab::Develop;
                    self.open_path_labeled(
                        document.path,
                        document.display_name.clone(),
                        document.delete_after_decode,
                        crate::sidecar::SidecarTarget::Android {
                            raw_uri: document.library_uri,
                            display_name: document.display_name,
                        },
                        frame,
                    )
                }
                crate::android::PickerResult::BatchImported {
                    imported,
                    failed,
                    errors,
                } => {
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
                    self.notice = Some("No RAW files selected.".to_owned());
                }
                crate::android::PickerResult::Failed(error) => {
                    self.notice = Some(format!("Could not import the selected file: {error}"));
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
                None
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => None,
        };
        let Some(LoadEvent::Finished(result)) = event else {
            return;
        };

        self.load_receiver = None;
        self.loading_label = None;

        match result {
            Ok(mut loaded) => {
                let Some(render_state) = frame.wgpu_render_state() else {
                    self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
                    return;
                };
                let mut renderer = render_state.renderer.write();
                if let Some(old) = self.gpu_pipeline.take() {
                    if let Some(texture_id) = old.egui_texture_id {
                        renderer.free_texture(&texture_id);
                    }
                }
                if let Some(old) = self.preview_detail.take() {
                    if let Some(texture_id) = old.pipeline.egui_texture_id {
                        renderer.free_texture(&texture_id);
                    }
                }
                if let Some(old) = self.preview_navigation.take() {
                    if let Some(texture_id) = old.pipeline.egui_texture_id {
                        renderer.free_texture(&texture_id);
                    }
                }
                loaded
                    .pipeline
                    .register_egui_texture(&render_state.device, &mut renderer);

                let full_width = loaded.full_raw.width;
                let full_height = loaded.full_raw.height;
                let preview_width = loaded.preview_raw.width;
                let preview_height = loaded.preview_raw.height;
                self.image_status = format!(
                    "{} {} — full {}×{}, preview {}×{} ({})",
                    loaded.full_raw.camera_make,
                    loaded.full_raw.camera_model,
                    full_width,
                    full_height,
                    preview_width,
                    preview_height,
                    self.preview_quality.label(),
                );
                self.current_path = loaded.source_path;
                self.current_label = Some(loaded.label.clone());
                self.cache_raw_decode(loaded.raw_cache_key, Arc::clone(&loaded.original_raw));
                self.original_raw = Some(loaded.original_raw);
                self.loaded_raw = Some(loaded.full_raw);
                self.preview_raw = Some(loaded.preview_raw);
                self.gpu_pipeline = Some(loaded.pipeline);
                self.exposure = loaded.rendered_exposure;
                self.masks = loaded.rendered_masks;
                self.rehydrate_restored_mask_state();
                if loaded.mask_source.is_some() {
                    self.mask_source_cache = loaded.mask_source;
                }
                self.preview_zoom = 1.0;
                self.preview_center = [0.5, 0.5];
                self.preview_visible_uv = PreviewUvRect {
                    min: [0.0, 0.0],
                    max: [1.0, 1.0],
                };
                self.preview_viewport_pixels = [1, 1];
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
                log::info!("loaded RAW preview for {}", loaded.label);
            }
            Err(error) => {
                self.notice = Some(format!("Failed to decode or render RAW: {error}"));
                log::error!("RAW load failed: {error}");
            }
        }
    }
}
