use crate::pipeline::{
    affected_stage, build_proxy, load_raw_file, spawn_tiled_png_export, ExportEvent,
    BrushMode, ExportMetadata, ExportSettings, ExposureParams, GpuParams, LoadedRaw, MaskKind,
    MaskStack, ProcessingQuality, ProcessingStage, ProxySpec, RawGpuPipeline, TileSpec,
    MAX_LOCAL_MASKS,
};
use crate::ui::layout::ScreenLayout;
use crate::ui::library::Library;
use crate::ui::preview::Preview;
use crate::ui::settings::Settings;
use crate::ui::sidebar::Sidebar;
use crate::ui::top_bar::TopBar;
use eframe::egui;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppTab {
    Library,
    #[default]
    Develop,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarTab {
    #[default]
    Adjustments,
    Masks,
    Inpainting,
    Export,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToneCurveTab {
    #[default]
    Rgb,
    Red,
    Green,
    Blue,
}

struct LoadedPreview {
    source_path: Option<PathBuf>,
    label: String,
    full_raw: Arc<LoadedRaw>,
    preview_raw: Arc<LoadedRaw>,
    pipeline: RawGpuPipeline,
    rendered_exposure: ExposureParams,
}

enum LoadEvent {
    Finished(Result<LoadedPreview, String>),
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum MaskDragState {
    Create([f32; 2]),
    MoveRadial {
        pointer: [f32; 2],
        center: [f32; 2],
    },
    ResizeRadial {
        axis: usize,
    },
    MoveLinear {
        pointer: [f32; 2],
        start: [f32; 2],
        end: [f32; 2],
    },
    LinearStart,
    LinearEnd,
}

pub struct AurawApp {
    pub current_path: Option<PathBuf>,
    pub loaded_raw: Option<Arc<LoadedRaw>>,
    pub preview_raw: Option<Arc<LoadedRaw>>,
    pub gpu_pipeline: Option<RawGpuPipeline>,
    pub exposure: ExposureParams,
    pub active_tab: AppTab,
    pub sidebar_tab: SidebarTab,
    pub tone_curve_tab: ToneCurveTab,
    pub export_settings: ExportSettings,
    pub masks: MaskStack,
    pub(crate) active_mask_tool: Option<MaskKind>,
    pub(crate) brush_mode: BrushMode,
    pub(crate) mask_drag: Option<MaskDragState>,
    pub(crate) last_brush_point: Option<[f32; 2]>,
    pub(crate) mask_properties_active: bool,
    pub(crate) mask_overlay_revision: u64,
    pub(crate) mask_overlay_texture: Option<egui::TextureHandle>,
    pub(crate) mask_overlay_texture_key: Option<(usize, u64, u32, u32)>,
    pub status: String,
    /// Reveals low-level darktable/raw controls. The default Lightroom-like
    /// interface intentionally keeps these implementation details hidden.
    pub expert_mode: bool,

    egui_ctx: egui::Context,
    target_exposure: ExposureParams,
    pending_stage: Option<ProcessingStage>,
    load_receiver: Option<mpsc::Receiver<LoadEvent>>,
    loading_label: Option<String>,
    export_receiver: Option<mpsc::Receiver<ExportEvent>>,
    export_progress: Option<(usize, usize)>,
    export_publish_pending: bool,
    image_status: String,
    current_label: Option<String>,
    notice: Option<String>,
    dirty_mask_layers: [bool; MAX_LOCAL_MASKS],

    #[cfg(target_os = "android")]
    android_app: android_activity::AndroidApp,
    #[cfg(target_os = "android")]
    picker_pending: bool,
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
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::proportional(13.0),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::proportional(12.5),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::proportional(11.5),
        );
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
        Self {
            current_path: None,
            loaded_raw: None,
            preview_raw: None,
            gpu_pipeline: None,
            exposure,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
            tone_curve_tab: ToneCurveTab::default(),
            export_settings: ExportSettings::default(),
            masks: MaskStack::default(),
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            mask_drag: None,
            last_brush_point: None,
            mask_properties_active: false,
            mask_overlay_revision: 0,
            mask_overlay_texture: None,
            mask_overlay_texture_key: None,
            status: "Open a RAW file to get started.".to_owned(),
            expert_mode: false,
            egui_ctx: ctx.clone(),
            target_exposure: exposure,
            pending_stage: None,
            load_receiver: None,
            loading_label: None,
            export_receiver: None,
            export_progress: None,
            export_publish_pending: false,
            image_status: "Open a RAW file to get started.".to_owned(),
            current_label: None,
            notice: None,
            dirty_mask_layers: [false; MAX_LOCAL_MASKS],
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
            loaded_raw: None,
            preview_raw: None,
            gpu_pipeline: None,
            exposure,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
            tone_curve_tab: ToneCurveTab::default(),
            export_settings: ExportSettings::default(),
            masks: MaskStack::default(),
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            mask_drag: None,
            last_brush_point: None,
            mask_properties_active: false,
            mask_overlay_revision: 0,
            mask_overlay_texture: None,
            mask_overlay_texture_key: None,
            status: "Open a RAW file to get started.".to_owned(),
            expert_mode: false,
            egui_ctx: cc.egui_ctx.clone(),
            target_exposure: exposure,
            pending_stage: None,
            load_receiver: None,
            loading_label: None,
            export_receiver: None,
            export_progress: None,
            export_publish_pending: false,
            image_status: "Open a RAW file to get started.".to_owned(),
            current_label: None,
            notice: None,
            dirty_mask_layers: [false; MAX_LOCAL_MASKS],
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
                    "cr2", "CR2", "cr3", "CR3", "nef", "NEF", "arw", "ARW", "raf", "RAF",
                    "rw2", "RW2", "orf", "ORF", "dng", "DNG", "pef", "PEF", "srw", "SRW",
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
        self.exposure = initial_exposure;
        self.target_exposure = initial_exposure;
        self.masks.clear();
        self.active_mask_tool = None;
        self.brush_mode = BrushMode::Paint;
        self.mask_drag = None;
        self.last_brush_point = None;
        self.mask_properties_active = false;
        self.mask_overlay_revision = self.mask_overlay_revision.wrapping_add(1);
        self.mask_overlay_texture = None;
        self.mask_overlay_texture_key = None;
        self.dirty_mask_layers = [false; MAX_LOCAL_MASKS];
        self.pending_stage = None;
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
                    let full_raw = Arc::new(decoded.map_err(|error| format!("{error:#}"))?);
                    let preview_spec = ProxySpec::default();
                    let preview_raw = if full_raw.width.max(full_raw.height) <= preview_spec.max_edge {
                        Arc::clone(&full_raw)
                    } else {
                        Arc::new(build_proxy(&full_raw, preview_spec))
                    };
                    let params = GpuParams::new(&initial_exposure, &MaskStack::default(), &preview_raw);
                    // Desktop has enough bandwidth for the 32-bit working
                    // path. Keep the half-float preview only on Android, where
                    // memory pressure is materially higher.
                    let preview_quality = if cfg!(target_os = "android") {
                        ProcessingQuality::Preview
                    } else {
                        ProcessingQuality::High
                    };
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
                        full_raw,
                        preview_raw,
                        pipeline,
                        rendered_exposure: initial_exposure,
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
                loaded
                    .pipeline
                    .register_egui_texture(&render_state.device, &mut renderer);

                let full_width = loaded.full_raw.width;
                let full_height = loaded.full_raw.height;
                let preview_width = loaded.preview_raw.width;
                let preview_height = loaded.preview_raw.height;
                self.image_status = format!(
                    "{} {} — full {}×{}, preview {}×{}",
                    loaded.full_raw.camera_make,
                    loaded.full_raw.camera_model,
                    full_width,
                    full_height,
                    preview_width,
                    preview_height
                );
                self.current_path = loaded.source_path;
                self.current_label = Some(loaded.label.clone());
                self.loaded_raw = Some(loaded.full_raw);
                self.preview_raw = Some(loaded.preview_raw);
                self.gpu_pipeline = Some(loaded.pipeline);
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

    pub(crate) fn mark_mask_adjustments_dirty(&mut self) {
        if self.gpu_pipeline.is_none() {
            return;
        }
        self.pending_stage = Some(match self.pending_stage {
            Some(existing) => existing.min(ProcessingStage::Output),
            None => ProcessingStage::Output,
        });
        self.notice = None;
    }

    pub(crate) fn mark_mask_geometry_dirty(&mut self, layer: usize) {
        if layer < MAX_LOCAL_MASKS {
            self.dirty_mask_layers[layer] = true;
        }
        self.mask_overlay_revision = self.mask_overlay_revision.wrapping_add(1);
        self.mark_mask_adjustments_dirty();
    }

    pub(crate) fn mark_all_mask_layers_dirty(&mut self) {
        self.dirty_mask_layers.fill(true);
        self.mask_overlay_revision = self.mask_overlay_revision.wrapping_add(1);
        self.mark_mask_adjustments_dirty();
    }

    pub(crate) fn activate_mask_tool(&mut self, kind: MaskKind) {
        self.active_mask_tool = kind.is_available().then_some(kind);
        self.mask_drag = None;
        self.last_brush_point = None;
        if kind == MaskKind::Brush {
            self.brush_mode = BrushMode::Paint;
        }
    }

    pub(crate) fn select_mask_tool(&mut self, kind: MaskKind) {
        self.active_mask_tool = kind.is_available().then_some(kind);
        self.mask_drag = None;
        self.last_brush_point = None;
    }

    pub(crate) fn mark_pipeline_dirty(&mut self) {
        if self.gpu_pipeline.is_none() {
            self.target_exposure = self.exposure;
            return;
        }

        if let Some(stage) = affected_stage(&self.target_exposure, &self.exposure) {
            self.pending_stage = Some(match self.pending_stage {
                Some(existing) => existing.min(stage),
                None => stage,
            });
            self.target_exposure = self.exposure;
            self.notice = None;
        }
    }

    fn advance_processing(&mut self, frame: &eframe::Frame) {
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
                let bytes = self.masks.rasterize_layer(
                    layer,
                    edge,
                    edge,
                    raw.width,
                    raw.height,
                );
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

        let params = GpuParams::new(&self.target_exposure, &self.masks, raw);
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
            && self.export_receiver.is_none()
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
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter("PNG image", &["png"])
            .set_file_name(default_name)
            .save_file()
        else {
            return;
        };
        let has_png_extension = matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some(extension) if extension.eq_ignore_ascii_case("png")
        );
        if !has_png_extension {
            path.set_extension("png");
        }

        self.start_export(path, frame);
    }

    #[cfg(target_os = "android")]
    pub(crate) fn export_png(&mut self, frame: &eframe::Frame) {
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
        let path = export_dir.join(format!("AuRaw-{timestamp}.png"));
        self.start_export(path, frame);
    }

    fn start_export(&mut self, path: PathBuf, frame: &eframe::Frame) {
        if !self.can_export() {
            return;
        }

        let (Some(raw), Some(preview_raw)) = (&self.loaded_raw, &self.preview_raw) else {
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            self.notice = Some("eframe is not running with the wgpu backend.".to_owned());
            return;
        };

        let source_file_name = self
            .current_path
            .as_ref()
            .and_then(|source| source.file_name())
            .and_then(|name| name.to_str())
            .map(str::to_owned)
            .or_else(|| self.current_label.clone());
        let metadata = ExportMetadata::from_raw(raw, source_file_name);
        self.export_receiver = Some(spawn_tiled_png_export(
            render_state.device.clone(),
            render_state.queue.clone(),
            Arc::clone(raw),
            Arc::clone(preview_raw),
            self.exposure,
            self.masks.clone(),
            path,
            TileSpec::default(),
            self.export_settings,
            metadata,
        ));
        self.export_progress = Some((0, 0));
        self.notice = None;
    }

    #[cfg(target_os = "android")]
    fn poll_android_export_publish(&mut self) {
        while let Some(result) = crate::android::take_export_publish_result() {
            self.export_publish_pending = false;
            match result {
                crate::android::ExportPublishResult::Published(location) => {
                    self.notice = Some(format!("Exported to {location}"));
                }
                crate::android::ExportPublishResult::Failed(error) => {
                    self.notice = Some(format!("Export failed: {error}"));
                    log::error!("Android export publish failed: {error}");
                }
            }
        }
    }

    fn poll_export_worker(&mut self) {
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
        for event in events {
            match event {
                ExportEvent::Progress {
                    completed_tiles,
                    total_tiles,
                } => self.export_progress = Some((completed_tiles, total_tiles)),
                ExportEvent::Finished(result) => {
                    finished = true;
                    self.export_progress = None;
                    match result {
                        Ok(path) => {
                            #[cfg(not(target_os = "android"))]
                            {
                                self.notice = Some(format!("Exported {}", path.display()));
                            }

                            #[cfg(target_os = "android")]
                            {
                                let display_name = path
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or("AuRaw-export.png")
                                    .to_owned();
                                match crate::android::publish_png(
                                    &self.android_app,
                                    &path,
                                    &display_name,
                                ) {
                                    Ok(()) => {
                                        self.export_publish_pending = true;
                                        self.notice = Some("Saving to Pictures/AuRaw…".to_owned());
                                    }
                                    Err(error) => {
                                        let _ = std::fs::remove_file(&path);
                                        self.notice = Some(format!("Export failed: {error}"));
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            self.notice = Some(format!("Export failed: {error}"));
                            log::error!("export failed: {error}");
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
        }
    }

    fn refresh_status(&mut self) {
        self.status = if let Some(label) = &self.loading_label {
            format!("Decoding and preparing proxy for {label}…")
        } else if let Some((completed, total)) = self.export_progress {
            if total == 0 {
                "Preparing tiled export…".to_owned()
            } else {
                format!("Exporting PNG — tile {completed}/{total}")
            }
        } else if self.export_publish_pending {
            "Saving to Pictures/AuRaw…".to_owned()
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

        // Highlight reconstruction is an application-level processing preference,
        // not one of the Lightroom-style Develop adjustments.
        self.exposure.highlight_method = previous.highlight_method;
        self.exposure.highlight_clip = previous.highlight_clip;
        self.exposure.highlight_reconstruction = previous.highlight_reconstruction;
        self.exposure.highlight_iterations = previous.highlight_iterations;
        self.exposure.highlight_color_adaptation = previous.highlight_color_adaptation;

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
        self.exposure.highlight_iterations = defaults.highlight_iterations;
        self.exposure.highlight_color_adaptation = defaults.highlight_color_adaptation;
        self.mark_pipeline_dirty();
    }
}

impl eframe::App for AurawApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        #[cfg(target_os = "android")]
        {
            self.poll_android_picker(frame);
            self.poll_android_export_publish();
        }

        self.poll_load_worker(frame);
        self.poll_export_worker();
        self.mask_properties_active = false;

        let viewport_size = ui.max_rect().size();
        let layout = ScreenLayout::from_size(viewport_size);
        let sidebar_size = layout.sidebar_default_size(viewport_size);

        self.refresh_status();
        egui::Panel::top("top_bar").show(ui, |ui| TopBar::show(ui, self, frame));

        if self.active_tab == AppTab::Develop {
            match layout {
                ScreenLayout::Horizontal => {
                    egui::Panel::right("develop_sidebar_right")
                        .resizable(true)
                        .min_size(ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH)
                        .default_size(sidebar_size)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| Sidebar::show(ui, self, layout, frame));
                        });
                }
                ScreenLayout::Vertical => {
                    egui::Panel::bottom("develop_sidebar_bottom")
                        .resizable(true)
                        .min_size(ScreenLayout::MIN_VERTICAL_SIDEBAR_HEIGHT)
                        .default_size(sidebar_size)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| Sidebar::show(ui, self, layout, frame));
                        });
                }
            }
        }

        egui::CentralPanel::default().show(ui, |ui| match self.active_tab {
            AppTab::Library => Library::show(ui),
            AppTab::Develop => Preview::show(ui, self),
            AppTab::Settings => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| Settings::show(ui, self));
            }
        });

        self.advance_processing(frame);
        self.refresh_status();

        if self.pending_stage.is_some() {
            ui.ctx().request_repaint();
        }
        if self.export_receiver.is_some() || self.export_publish_pending {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
    }
}
