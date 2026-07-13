use crate::ai_masks::{spawn_subject_mask, SubjectMaskEvent, BIREFNET_MODEL_BYTES};
use crate::pipeline::{
    affected_stage, build_proxy, load_raw_file, spawn_tiled_png_export, BrushMode, ExportEvent,
    ExportMetadata, ExportSettings, ExposureParams, GpuParams, LoadedRaw, MaskImage, MaskKind,
    MaskRgbImage, MaskStack, ProcessingQuality, ProcessingStage, ProxySpec, RawGpuPipeline,
    TileSpec, MAX_LOCAL_MASKS,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorGradeTab {
    Shadows,
    #[default]
    Midtones,
    Highlights,
    Global,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MaskOverlayBlink {
    #[default]
    GroupTwice,
    ComponentThenGroup,
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
    pub color_grade_tab: ColorGradeTab,
    pub export_settings: ExportSettings,
    pub masks: MaskStack,
    pub(crate) active_mask_tool: Option<MaskKind>,
    pub(crate) brush_mode: BrushMode,
    pub(crate) mask_drag: Option<MaskDragState>,
    pub(crate) last_brush_point: Option<[f32; 2]>,
    pub(crate) mask_overlay_revision: u64,
    pub(crate) mask_overlay_texture: Option<egui::TextureHandle>,
    pub(crate) mask_overlay_texture_key: Option<(usize, Option<usize>, u64, u32, u32)>,
    pub(crate) mask_overlay_blink: Option<(std::time::Instant, MaskOverlayBlink)>,
    pub(crate) mask_source_cache: Option<MaskRgbImage>,
    pub(crate) subject_mask_cache: Option<MaskImage>,
    #[cfg(not(target_os = "android"))]
    pub(crate) onnx_runtime_path: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    pub(crate) onnx_runtime_sha256: Option<String>,
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
    subject_consent_open: bool,
    subject_receiver: Option<mpsc::Receiver<SubjectMaskEvent>>,
    subject_download_progress: Option<(&'static str, u64, u64)>,
    subject_inferencing: bool,

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
            loaded_raw: None,
            preview_raw: None,
            gpu_pipeline: None,
            exposure,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
            tone_curve_tab: ToneCurveTab::default(),
            color_grade_tab: ColorGradeTab::default(),
            export_settings: ExportSettings::default(),
            masks: MaskStack::default(),
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            mask_drag: None,
            last_brush_point: None,
            mask_overlay_revision: 0,
            mask_overlay_texture: None,
            mask_overlay_texture_key: None,
            mask_overlay_blink: None,
            mask_source_cache: None,
            subject_mask_cache: None,
            onnx_runtime_path,
            onnx_runtime_sha256,
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
            loaded_raw: None,
            preview_raw: None,
            gpu_pipeline: None,
            exposure,
            active_tab: AppTab::default(),
            sidebar_tab: SidebarTab::default(),
            tone_curve_tab: ToneCurveTab::default(),
            color_grade_tab: ColorGradeTab::default(),
            export_settings: ExportSettings::default(),
            masks: MaskStack::default(),
            active_mask_tool: None,
            brush_mode: BrushMode::Paint,
            mask_drag: None,
            last_brush_point: None,
            mask_overlay_revision: 0,
            mask_overlay_texture: None,
            mask_overlay_texture_key: None,
            mask_overlay_blink: None,
            mask_source_cache: None,
            subject_mask_cache: None,
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
        self.exposure = initial_exposure;
        self.target_exposure = initial_exposure;
        self.masks.clear();
        self.active_mask_tool = None;
        self.brush_mode = BrushMode::Paint;
        self.mask_drag = None;
        self.last_brush_point = None;
        self.mask_overlay_revision = self.mask_overlay_revision.wrapping_add(1);
        self.mask_overlay_texture = None;
        self.mask_overlay_texture_key = None;
        self.mask_overlay_blink = None;
        self.mask_source_cache = None;
        self.subject_mask_cache = None;
        self.subject_consent_open = false;
        self.subject_receiver = None;
        self.subject_download_progress = None;
        self.subject_inferencing = false;
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
                    let preview_raw =
                        if full_raw.width.max(full_raw.height) <= preview_spec.max_edge {
                            Arc::clone(&full_raw)
                        } else {
                            Arc::new(build_proxy(&full_raw, preview_spec))
                        };
                    let params =
                        GpuParams::new(&initial_exposure, &MaskStack::default(), &preview_raw);
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

    pub(crate) fn blink_selected_mask(&mut self) {
        self.mask_overlay_blink = Some((std::time::Instant::now(), MaskOverlayBlink::GroupTwice));
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn blink_selected_component(&mut self) {
        self.mask_overlay_blink = Some((
            std::time::Instant::now(),
            MaskOverlayBlink::ComponentThenGroup,
        ));
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn capture_mask_source(&mut self, frame: &eframe::Frame) -> Result<(), String> {
        if self.mask_source_cache.is_some() {
            return Ok(());
        }
        let render_state = frame
            .wgpu_render_state()
            .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
        let pipeline = self
            .gpu_pipeline
            .as_ref()
            .ok_or_else(|| "Open an image before creating this mask.".to_owned())?;
        let rgba = pipeline
            .read_output_region_blocking(
                &render_state.device,
                &render_state.queue,
                0,
                0,
                pipeline.width,
                pipeline.height,
            )
            .map_err(|error| format!("Could not read the preview for masking: {error:#}"))?;
        self.mask_source_cache = MaskRgbImage::new(pipeline.width, pipeline.height, rgba);
        Ok(())
    }

    pub(crate) fn request_subject_mask(&mut self, frame: &eframe::Frame) {
        if let Some(mask) = self.subject_mask_cache.clone() {
            self.apply_subject_mask(mask);
            return;
        }
        #[cfg(not(target_os = "android"))]
        if self.onnx_runtime_path.is_none() || self.onnx_runtime_sha256.is_none() {
            self.notice = Some(
                "Choose an ONNX Runtime library under Settings before using Subject or Background masks."
                    .to_owned(),
            );
            return;
        }
        if let Err(error) = self.capture_mask_source(frame) {
            self.notice = Some(error);
            return;
        }
        let path = self.birefnet_model_path();
        if path.exists() {
            self.start_subject_worker(path);
        } else {
            self.subject_consent_open = true;
        }
    }

    fn start_subject_worker(&mut self, model_path: PathBuf) {
        if self.subject_receiver.is_some() {
            return;
        }
        let Some(source) = self.mask_source_cache.clone() else {
            self.notice =
                Some("The preview could not be prepared for subject selection.".to_owned());
            return;
        };
        self.subject_download_progress = None;
        self.subject_inferencing = model_path.exists();
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.onnx_runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.onnx_runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;
        self.subject_receiver = Some(spawn_subject_mask(
            model_path,
            runtime_path,
            runtime_sha256,
            source.width,
            source.height,
            source.rgba.to_vec(),
        ));
        self.egui_ctx.request_repaint();
    }

    fn apply_subject_mask(&mut self, mask: MaskImage) {
        self.subject_mask_cache = Some(mask.clone());
        for local_mask in &mut self.masks.masks {
            for component in &mut local_mask.components {
                if matches!(component.kind, MaskKind::Subject | MaskKind::Background) {
                    if let crate::pipeline::MaskGeometry::Ai { mask: target, .. } =
                        &mut component.geometry
                    {
                        *target = Some(mask.clone());
                    }
                }
            }
        }
        self.mark_all_mask_layers_dirty();
        self.blink_selected_mask();
    }

    fn poll_subject_worker(&mut self) {
        let mut finished = None;
        if let Some(receiver) = &self.subject_receiver {
            while let Ok(event) = receiver.try_recv() {
                match event {
                    SubjectMaskEvent::DownloadProgress {
                        label,
                        downloaded,
                        total,
                    } => {
                        self.subject_download_progress = Some((label, downloaded, total));
                        self.subject_inferencing = false;
                    }
                    SubjectMaskEvent::Inferencing => {
                        self.subject_download_progress = None;
                        self.subject_inferencing = true;
                    }
                    SubjectMaskEvent::Finished(result) => finished = Some(result),
                }
            }
        }
        if let Some(result) = finished {
            self.subject_receiver = None;
            self.subject_download_progress = None;
            self.subject_inferencing = false;
            match result {
                Ok(result) => {
                    if let Some(mask) = MaskImage::new(result.width, result.height, result.mask) {
                        self.apply_subject_mask(mask);
                    }
                }
                Err(error) => self.notice = Some(format!("Subject selection failed: {error}")),
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    fn birefnet_model_path(&self) -> PathBuf {
        let root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("auraw/models/birefnet-general-lite.onnx")
    }

    #[cfg(not(target_os = "android"))]
    fn onnx_runtime_config_path() -> PathBuf {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("auraw/onnx-runtime-path")
    }

    #[cfg(not(target_os = "android"))]
    fn load_onnx_runtime_selection() -> Option<(PathBuf, String)> {
        let configured = std::fs::read_to_string(Self::onnx_runtime_config_path()).ok()?;
        let mut lines = configured.lines();
        let sha256 = lines.next()?.strip_prefix("sha256=")?.to_owned();
        let path = PathBuf::from(lines.next()?.strip_prefix("path=")?);
        if lines.next().is_some()
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || !path.is_file()
        {
            return None;
        }
        Some((path, sha256))
    }

    #[cfg(not(target_os = "android"))]
    fn persist_onnx_runtime_selection(
        selection: Option<(&std::path::Path, &str)>,
    ) -> Result<(), String> {
        let config = Self::onnx_runtime_config_path();
        if let Some((path, sha256)) = selection {
            let parent = config
                .parent()
                .ok_or_else(|| "invalid AuRaw configuration path".to_owned())?;
            let path_text = path
                .to_str()
                .ok_or_else(|| "the ONNX Runtime path is not valid UTF-8".to_owned())?;
            if path_text.contains('\n') || path_text.contains('\r') {
                return Err("the ONNX Runtime path contains a line break".to_owned());
            }
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
            let temporary = config.with_extension(format!("tmp.{}", std::process::id()));
            let payload = format!("sha256={sha256}\npath={path_text}\n");
            std::fs::write(&temporary, payload.as_bytes())
                .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
            #[cfg(windows)]
            if config.exists() {
                std::fs::remove_file(&config)
                    .map_err(|error| format!("could not replace {}: {error}", config.display()))?;
            }
            std::fs::rename(&temporary, &config)
                .map_err(|error| format!("could not publish {}: {error}", config.display()))?;
        } else if let Err(error) = std::fs::remove_file(&config) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("could not remove {}: {error}", config.display()));
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn choose_onnx_runtime(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Select the ONNX Runtime shared library")
            .pick_file()
        else {
            return;
        };
        if !path.is_file() {
            self.notice = Some(format!("{} is not a file.", path.display()));
            return;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let looks_like_runtime = file_name.contains("onnxruntime")
            && (file_name.ends_with(".dll")
                || file_name.ends_with(".dylib")
                || file_name.contains(".so"));
        if !looks_like_runtime {
            self.notice = Some(
                "Select the ONNX Runtime shared library (onnxruntime.dll, libonnxruntime.so, or libonnxruntime.dylib)."
                    .to_owned(),
            );
            return;
        }
        let sha256 = match crate::ai_masks::sha256_file_hex(&path) {
            Ok(sha256) => sha256,
            Err(error) => {
                self.notice = Some(format!("Could not hash selected ONNX Runtime: {error:#}"));
                return;
            }
        };
        match Self::persist_onnx_runtime_selection(Some((&path, &sha256))) {
            Ok(()) => {
                self.onnx_runtime_path = Some(path);
                self.onnx_runtime_sha256 = Some(sha256);
                self.notice = Some(
                    "ONNX Runtime selection and SHA-256 pin saved. Restart AuRaw before generating another subject mask."
                        .to_owned(),
                );
            }
            Err(error) => self.notice = Some(error),
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn clear_onnx_runtime(&mut self) {
        match Self::persist_onnx_runtime_selection(None) {
            Ok(()) => {
                self.onnx_runtime_path = None;
                self.onnx_runtime_sha256 = None;
                self.notice = Some(
                    "ONNX Runtime selection cleared. Restart AuRaw to apply the change.".to_owned(),
                );
            }
            Err(error) => self.notice = Some(error),
        }
    }

    #[cfg(target_os = "android")]
    fn birefnet_model_path(&self) -> PathBuf {
        self.android_app
            .internal_data_path()
            .unwrap_or_else(std::env::temp_dir)
            .join("models/birefnet-general-lite.onnx")
    }

    fn show_subject_dialogs(&mut self, ctx: &egui::Context) {
        if self.subject_consent_open {
            egui::Window::new("Download subject-selection model?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Subject and Background masks use the BiRefNet General Lite ONNX model.");
                    ui.label(format!(
                        "The first use downloads {:.0} MB from the rembg GitHub release and stores it in AuRaw's cache.",
                        BIREFNET_MODEL_BYTES as f64 / 1_000_000.0
                    ));
                    ui.label("Inference is local. No photograph is uploaded.");
                    #[cfg(not(target_os = "android"))]
                    if self.onnx_runtime_path.is_none() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Select a trusted local ONNX Runtime library in Settings before continuing. AuRaw never downloads native runtime code.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Download and continue").clicked() {
                            self.subject_consent_open = false;
                            self.start_subject_worker(self.birefnet_model_path());
                        }
                        if ui.button("Cancel").clicked() {
                            self.subject_consent_open = false;
                        }
                    });
                });
        }
        if self.subject_receiver.is_some() {
            egui::Window::new("Preparing subject mask")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    if let Some((label, downloaded, total)) = self.subject_download_progress {
                        let fraction = downloaded as f32 / total.max(1) as f32;
                        ui.label(format!("Downloading {label}…"));
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .show_percentage()
                                .text(format!(
                                    "{:.1} / {:.1} MB",
                                    downloaded as f64 / 1_000_000.0,
                                    total as f64 / 1_000_000.0
                                )),
                        );
                    } else if self.subject_inferencing {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Running high-quality local subject selection…");
                        });
                    } else {
                        ui.spinner();
                    }
                });
            ctx.request_repaint_after(Duration::from_millis(50));
        }
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
                let bytes = self
                    .masks
                    .rasterize_layer(layer, edge, edge, raw.width, raw.height);
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

        let Some(raw) = &self.loaded_raw else {
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
        self.poll_subject_worker();

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
                        .show(ui, |ui| Sidebar::show(ui, self, layout, frame));
                }
                ScreenLayout::Vertical => {
                    egui::Panel::bottom("develop_sidebar_bottom")
                        .resizable(true)
                        .min_size(ScreenLayout::MIN_VERTICAL_SIDEBAR_HEIGHT)
                        .default_size(sidebar_size)
                        .show(ui, |ui| Sidebar::show(ui, self, layout, frame));
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
        self.show_subject_dialogs(ui.ctx());
    }
}
