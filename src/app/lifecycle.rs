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
        let exposure = ExposureParams::scene_referred_default();
        let runtime_selection = Self::load_onnx_runtime_selection();
        let onnx_runtime_path = runtime_selection.as_ref().map(|(path, _)| path.clone());
        let onnx_runtime_sha256 = runtime_selection.map(|(_, sha256)| sha256);
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
            exposure,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
            adjustment_section: AdjustmentSection::default(),
            mask_section: MaskSection::default(),
            tone_curve_tab: ToneCurveTab::default(),
            color_grade_tab: ColorGradeTab::default(),
            export_settings: ExportSettings::default(),
            masks: MaskStack::default(),
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            mask_drag: None,
            last_brush_point: None,
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
            lens_correction: LensCorrectionState::default(),
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
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::install_lightroom_visuals(&cc.egui_ctx);
        Self::empty(&cc.egui_ctx)
    }

    #[cfg(target_os = "android")]
    pub fn new_android(
        cc: &eframe::CreationContext<'_>,
        android_app: android_activity::AndroidApp,
    ) -> Self {
        crate::android::install_context(&cc.egui_ctx);
        Self::install_lightroom_visuals(&cc.egui_ctx);
        let exposure = ExposureParams::scene_referred_default();
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
            exposure,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
            adjustment_section: AdjustmentSection::default(),
            mask_section: MaskSection::default(),
            tone_curve_tab: ToneCurveTab::default(),
            color_grade_tab: ColorGradeTab::default(),
            export_settings: ExportSettings::default(),
            masks: MaskStack::default(),
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            mask_drag: None,
            last_brush_point: None,
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
            lens_correction: LensCorrectionState::default(),
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
            android_app,
            picker_pending: false,
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn open_file_dialog(&mut self, frame: &eframe::Frame) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "RAW images",
                &[
                    "cr2", "CR2", "cr3", "CR3", "nef", "NEF", "arw", "ARW", "raf", "RAF", "rw2",
                    "RW2", "orf", "ORF", "dng", "DNG", "pef", "PEF", "srw", "SRW",
                ],
            )
            .pick_file()
        else {
            return;
        };

        self.open_path(path, frame);
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
                self.status = "Choose a RAW file…".to_owned();
            }
            Err(error) => self.notice = Some(error),
        }
    }

    pub fn open_path(&mut self, path: PathBuf, frame: &eframe::Frame) {
        let label = path.display().to_string();
        self.open_path_labeled(path, label, false, frame);
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
        frame: &eframe::Frame,
    ) {
        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            self.refresh_status();
            return;
        };

        let device = render_state.device.clone();
        let queue = render_state.queue.clone();
        let initial_exposure = self.new_image_exposure();
        let preview_quality_setting = self.preview_quality;
        self.exposure = initial_exposure;
        self.target_exposure = initial_exposure;
        self.masks.clear();
        self.active_mask_tool = None;
        self.brush_mode = BrushMode::Paint;
        self.mask_drag = None;
        self.last_brush_point = None;
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
        self.original_raw = None;
        self.lens_correction = LensCorrectionState::default();
        self.lens_correction_dirty = false;
        let source_path = (!delete_after_decode).then_some(path.clone());
        let repaint = self.egui_ctx.clone();
        let (sender, receiver) = mpsc::channel();

        self.load_receiver = Some(receiver);
        self.loading_label = Some(label.clone());
        self.notice = None;
        self.refresh_status();

        let spawn_result = std::thread::Builder::new()
            .name("auraw-decode-preview".to_owned())
            .spawn(move || {
                let decoded = load_raw_file(&path);
                if delete_after_decode {
                    if let Err(error) = std::fs::remove_file(&path) {
                        log::warn!("could not remove imported Android RAW cache file: {error}");
                    }
                }

                let result = (|| {
                    let original_raw = Arc::new(decoded.map_err(|error| format!("{error:#}"))?);
                    let mut lens_correction =
                        LensCorrectionState::from_catalog(lensfun_catalog(&original_raw));
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
                    let preview_raw =
                        if full_raw.width.max(full_raw.height) <= preview_spec.max_edge {
                            Arc::clone(&full_raw)
                        } else {
                            Arc::new(build_proxy(&full_raw, preview_spec))
                        };
                    let params =
                        GpuParams::new(&initial_exposure, &MaskStack::default(), &preview_raw);
                    // Interactive previews use bounded half-float working
                    // surfaces on every platform. Full-float remains mandatory
                    // for regression rendering and tiled export readback.
                    let preview_quality = ProcessingQuality::Preview;
                    let pipeline = RawGpuPipeline::new_headless_with_quality(
                        &device,
                        &queue,
                        &preview_raw,
                        &params,
                        preview_quality,
                    )
                    .map_err(|error| format!("GPU preview setup failed: {error:#}"))?;
                    pipeline.recompute(&queue, &device, &params);

                    Ok(LoadedPreview {
                        source_path,
                        label,
                        original_raw,
                        full_raw,
                        preview_raw,
                        pipeline,
                        rendered_exposure: initial_exposure,
                        lens_correction,
                    })
                })();

                let _ = sender.send(LoadEvent::Finished(result));
                repaint.request_repaint();
            });

        if let Err(error) = spawn_result {
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
                    self.open_path_labeled(document.path, document.display_name, true, frame)
                }
                crate::android::PickerResult::Cancelled => {
                    self.notice = Some("No RAW file selected.".to_owned());
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
                self.original_raw = Some(loaded.original_raw);
                self.loaded_raw = Some(loaded.full_raw);
                self.preview_raw = Some(loaded.preview_raw);
                self.gpu_pipeline = Some(loaded.pipeline);
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
                self.preview_detail = None;
                self.preview_navigation = None;
                self.preview_detail_pending_stage = None;
                self.navigation_pending_stage = None;
                self.preview_detail_urgent = false;
                self.detail_dirty_mask_layers.fill(false);
                self.navigation_dirty_mask_layers.fill(false);
                self.lens_correction = loaded.lens_correction;
                self.lens_correction_dirty = false;
                self.target_exposure = loaded.rendered_exposure;
                self.pending_stage = affected_stage(&self.target_exposure, &self.exposure);
                self.target_exposure = self.exposure;
                self.notice = None;
                log::info!("loaded RAW preview for {}", loaded.label);
            }
            Err(error) => {
                self.notice = Some(format!("Failed to decode or render RAW: {error}"));
                log::error!("RAW load failed: {error}");
            }
        }
    }
}
