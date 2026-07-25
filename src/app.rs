use crate::ai_masks::{
    spawn_object_mask, spawn_subject_mask, ObjectInferenceCache, ObjectMaskEvent,
    ObjectMaskRequest, SubjectMaskEvent, BIREFNET_MODEL_BYTES, SAM21_MODEL_BYTES_ESTIMATE,
    VITMATTE_MODEL_BYTES,
};
use crate::inpainting::{
    inpaint_capture_rect, inpaint_patch_rect, spawn_inpaint, InpaintEvent, InpaintRequest,
    PreparedInpaintSource, LAMA_EDGE, LAMA_MODEL_BYTES,
};
use crate::pipeline::{
    affected_stage, apply_lensfun_correction, build_proxy, build_region_proxy,
    compose_inpaint_strokes, crop_raw, lensfun_catalog, load_raw_file_with_profile_selection,
    spawn_tiled_jpeg_export, spawn_tiled_png_export, spawn_tiled_tiff_export, BrushDab, BrushMode,
    CameraProfileMode, ExportEvent, ExportFormat, ExportMetadata, ExportSettings, ExposureParams,
    GeometryTransform, GpuParams, InpaintLayer, InpaintStroke, LensfunCatalog, LensfunLens,
    LoadedRaw, MaskGeometry, MaskImage, MaskKind, MaskRgbImage, MaskStack, ProcessingQuality,
    ProcessingStage, ProxySpec, RawGpuPipeline, TileSpec, EXPORT_TILE_HALO, MAX_LOCAL_MASKS,
};
use crate::sidecar::{
    AdjustmentCopySettings, EditState as SidecarEditState, LensEditState as SidecarLensEditState,
};
use crate::ui::components::adjustment_slider::slider_scroll_locked;
use crate::ui::layout::ScreenLayout;
use crate::ui::library::{Library, LibraryState};
use crate::ui::preview::Preview;
use crate::ui::settings::Settings;
use crate::ui::sidebar::Sidebar;
use crate::ui::top_bar::TopBar;
use eframe::{egui, wgpu};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

mod edit_history;
use edit_history::EditHistory;

#[cfg(not(target_os = "android"))]
pub(crate) enum DesktopPickerEvent {
    RawFile(Option<PathBuf>),
    LibraryFolder(Option<PathBuf>),
    CameraProfileFolder(Option<PathBuf>),
    OnnxRuntime(Result<Option<(PathBuf, String)>, String>),
    DisplayProfile(Option<PathBuf>),
}

#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub(crate) struct AndroidOriginalHold {
    pub start: egui::Pos2,
    pub started_at: Instant,
    pub showing_original: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreviewQuality {
    Fast,
    #[default]
    Balanced,
    High,
}

impl PreviewQuality {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Balanced => "Balanced",
            Self::High => "High",
        }
    }

    pub const fn proxy_edge(self) -> u32 {
        match (self, cfg!(target_os = "android")) {
            (Self::Fast, true) => 960,
            (Self::Balanced, true) => 1280,
            (Self::High, true) => 1600,
            (Self::Fast, false) => 1280,
            (Self::Balanced, false) => 2048,
            (Self::High, false) => 2560,
        }
    }

    pub const fn detail_edge(self) -> u32 {
        match (self, cfg!(target_os = "android")) {
            // Zoom detail coexists with the full preview and navigation proxy.
            // The hybrid branch's additional scene/display working textures make
            // the former 1600/2048 limits exceed the Android GPU budget before a
            // crop can be shown. These still meet or exceed common phone viewport
            // resolution while leaving room for inpainting's 512px work surface.
            (Self::Fast, true) => 960,
            (Self::Balanced, true) => 1152,
            (Self::High, true) => 1280,
            (Self::Fast, false) => 1920,
            (Self::Balanced, false) => 2560,
            (Self::High, false) => 3072,
        }
    }

    pub const fn detail_pixel_scale(self) -> f32 {
        match (self, cfg!(target_os = "android")) {
            (Self::Fast, true) => 0.75,
            (Self::Balanced, true) => 1.00,
            (Self::High, true) => 1.35,
            (Self::Fast, false) => 0.90,
            (Self::Balanced, false) => 1.20,
            (Self::High, false) => 1.50,
        }
    }
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CropHandle {
    Move,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CropDragState {
    pub handle: CropHandle,
    pub start: [f32; 2],
    pub crop: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct StraightenDragState {
    pub start: egui::Pos2,
    pub current: egui::Pos2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreviewUvRect {
    pub min: [f32; 2],
    pub max: [f32; 2],
}

pub(crate) struct PreviewNavigation {
    pub pipeline: RawGpuPipeline,
    /// Very-low-resolution full-frame RAW proxy used as the adjusted backing
    /// image while the high-resolution visible crop is rebuilt or moved.
    raw: Arc<LoadedRaw>,
}

pub(crate) struct PreviewDetail {
    pub pipeline: RawGpuPipeline,
    /// Full-image UV rectangle covered on screen by the detail texture.
    pub uv_rect: PreviewUvRect,
    /// UV rectangle sampled from the padded detail texture. Keeping the padded
    /// processing border outside this rectangle prevents crop-edge seams.
    pub texture_uv_rect: PreviewUvRect,
    pub revision: u64,
    /// Reusable RAW proxy for the padded visible crop. Adjustment interaction
    /// updates this pipeline directly instead of touching the full-frame proxy.
    raw: Arc<LoadedRaw>,
    source_origin: [u32; 2],
    source_size: [u32; 2],
    virtual_origin: [i32; 2],
    virtual_full_size: [u32; 2],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppTab {
    #[default]
    Library,
    Develop,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarTab {
    #[default]
    Adjustments,
    Crop,
    Masks,
    Inpainting,
    Export,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdjustmentSection {
    #[default]
    Light,
    ToneCurve,
    Color,
    ColorGrading,
    Detail,
    Effects,
    ColorMixer,
    Optics,
    AdvancedRendering,
    Raw,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaskSection {
    #[default]
    Properties,
    Light,
    ToneCurve,
    Color,
    ColorGrading,
    Effects,
    ColorMixer,
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

#[derive(Clone, Debug, Default)]
pub(crate) struct LensCorrectionState {
    pub enabled: bool,
    pub applied: bool,
    pub catalog: LensfunCatalog,
    pub selected_maker: String,
    pub selected_model: String,
}

impl LensCorrectionState {
    fn from_catalog(catalog: LensfunCatalog) -> Self {
        let selected = catalog.auto_match.clone();
        Self {
            enabled: catalog.available && selected.is_some(),
            applied: false,
            selected_maker: selected
                .as_ref()
                .map(|lens| lens.maker.clone())
                .unwrap_or_default(),
            selected_model: selected
                .as_ref()
                .map(|lens| lens.model.clone())
                .unwrap_or_default(),
            catalog,
        }
    }

    pub(crate) fn selected_lens(&self) -> Option<LensfunLens> {
        (!self.selected_model.trim().is_empty()).then(|| LensfunLens {
            maker: self.selected_maker.clone(),
            model: self.selected_model.clone(),
        })
    }

    pub(crate) fn makers(&self) -> Vec<String> {
        let mut makers = self
            .catalog
            .lenses
            .iter()
            .map(|lens| lens.maker.clone())
            .collect::<Vec<_>>();
        makers.sort_by_key(|maker| maker.to_lowercase());
        makers.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        makers
    }

    pub(crate) fn models_for_maker(&self, maker: &str) -> Vec<String> {
        let mut models = self
            .catalog
            .lenses
            .iter()
            .filter(|lens| lens.maker.eq_ignore_ascii_case(maker))
            .map(|lens| lens.model.clone())
            .collect::<Vec<_>>();
        models.sort_by_key(|model| model.to_lowercase());
        models.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
        models
    }
}

struct LoadedPreview {
    source_path: Option<PathBuf>,
    raw_cache_key: String,
    label: String,
    original_raw: Arc<LoadedRaw>,
    full_raw: Arc<LoadedRaw>,
    preview_raw: Arc<LoadedRaw>,
    pipeline: RawGpuPipeline,
    rendered_exposure: ExposureParams,
    rendered_masks: MaskStack,
    inpaint_strokes: Vec<InpaintStroke>,
    ai_masks_need_update: bool,
    mask_source: Option<MaskRgbImage>,
    inpaint_source: Option<MaskRgbImage>,
    lens_correction: LensCorrectionState,
    sidecar_target: crate::sidecar::SidecarTarget,
    sidecar_generation: u64,
    sidecar_warning: Option<String>,
    sidecar_needs_rewrite: bool,
    selected_camera_profile: Option<PathBuf>,
    geometry: GeometryTransform,
}

enum LoadEvent {
    Finished(Result<LoadedPreview, String>),
}

#[derive(Clone)]
struct SidecarSaveRequest {
    target: crate::sidecar::SidecarTarget,
    generation: u64,
    revision: u64,
    explicit: bool,
    edits: SidecarEditState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SidecarSaveJob {
    generation: u64,
    revision: u64,
    explicit: bool,
}

struct SidecarSaveEvent {
    job: SidecarSaveJob,
    result: Result<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DevelopedThumbnailJob {
    target: crate::sidecar::SidecarTarget,
    generation: u64,
    revision: u64,
}

struct DevelopedThumbnailEvent {
    job: DevelopedThumbnailJob,
    result: Result<crate::pipeline::RawThumbnail, String>,
}

#[derive(Clone, Copy, Debug)]
struct SidecarAutosaveDeadline {
    generation: u64,
    due_at: Instant,
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
    RotateRadial {
        pointer_angle: f32,
        rotation: f32,
    },
    MoveLinear {
        pointer: [f32; 2],
        start: [f32; 2],
        end: [f32; 2],
    },
    LinearStart,
    LinearEnd,
    RotateLinear {
        pointer_angle: f32,
        start: [f32; 2],
        end: [f32; 2],
    },
}

pub(crate) const MAX_DESKTOP_RAW_CACHE_FILES: usize = 8;
pub(crate) const MAX_ANDROID_RAW_CACHE_FILES: usize = 3;

pub(crate) const fn default_raw_cache_limit() -> usize {
    if cfg!(target_os = "android") {
        1
    } else {
        2
    }
}

pub(crate) const fn maximum_raw_cache_limit() -> usize {
    if cfg!(target_os = "android") {
        MAX_ANDROID_RAW_CACHE_FILES
    } else {
        MAX_DESKTOP_RAW_CACHE_FILES
    }
}

#[derive(Clone)]
struct CachedRawDecode {
    key: String,
    raw: Arc<LoadedRaw>,
}

#[derive(Clone)]
pub(crate) struct MaskTouchGestureBackup {
    mask_index: usize,
    component_index: usize,
    geometry: MaskGeometry,
    object_cache: Option<((usize, usize), ObjectInferenceCache)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MaskOverlayBlink {
    #[default]
    GroupTwice,
    ComponentThenGroup,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Debug)]
struct LibraryBatchExportJob {
    source: PathBuf,
    destination: PathBuf,
}

#[cfg(target_os = "android")]
#[derive(Clone, Debug)]
struct LibraryBatchExportJob {
    uri: String,
    display_name: String,
}

#[derive(Clone, Debug)]
struct LibraryAdjustmentClipboard {
    edits: SidecarEditState,
    settings: AdjustmentCopySettings,
    source_label: String,
}

#[derive(Debug)]
struct LibraryBatchExportState {
    pending: VecDeque<LibraryBatchExportJob>,
    current: Option<LibraryBatchExportJob>,
    total: usize,
    completed: usize,
    failures: Vec<String>,
    cancel_requested: bool,
    format: ExportFormat,
    settings: ExportSettings,
}

pub struct AurawApp {
    pub current_path: Option<PathBuf>,
    pub(crate) original_raw: Option<Arc<LoadedRaw>>,
    pub loaded_raw: Option<Arc<LoadedRaw>>,
    pub preview_raw: Option<Arc<LoadedRaw>>,
    pub gpu_pipeline: Option<RawGpuPipeline>,
    #[cfg(target_os = "android")]
    gpu_preview_prewarm_receiver: Option<mpsc::Receiver<Result<RawGpuPipeline, String>>>,
    pub(crate) preview_quality: PreviewQuality,
    pub(crate) preview_zoom: f32,
    pub(crate) preview_center: [f32; 2],
    pub(crate) preview_visible_uv: PreviewUvRect,
    pub(crate) preview_viewport_pixels: [u32; 2],
    pub(crate) preview_motion_at: Option<Instant>,
    pub(crate) preview_touch_navigation_active: bool,
    pub(crate) preview_revision: u64,
    pub(crate) preview_detail: Option<PreviewDetail>,
    pub(crate) preview_navigation: Option<PreviewNavigation>,
    preview_detail_pending_stage: Option<ProcessingStage>,
    navigation_pending_stage: Option<ProcessingStage>,
    preview_detail_urgent: bool,
    preview_quality_dirty: bool,
    pub(crate) original_preview_exposure: ExposureParams,
    pub(crate) original_preview_requested: bool,
    original_preview_rendered_state: Option<(bool, u64)>,
    pub(crate) android_original_hold: Option<AndroidOriginalHold>,
    pub exposure: ExposureParams,
    pub(crate) library: LibraryState,
    pub(crate) adjustment_copy_settings: AdjustmentCopySettings,
    adjustment_clipboard: Option<LibraryAdjustmentClipboard>,
    raw_cache: VecDeque<CachedRawDecode>,
    raw_cache_limit: usize,
    performance_settings_path: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    pub(crate) display_color_management: bool,
    #[cfg(not(target_os = "android"))]
    pub(crate) display_profile_override: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    pub(crate) display_profile_label: String,
    #[cfg(not(target_os = "android"))]
    display_profile_source: Option<String>,
    #[cfg(not(target_os = "android"))]
    display_profile_fingerprint: Option<u64>,
    #[cfg(not(target_os = "android"))]
    display_profile_last_probe: Option<Instant>,
    #[cfg(not(target_os = "android"))]
    display_profile_last_screen_point: Option<[i32; 2]>,
    #[cfg(not(target_os = "android"))]
    display_output_transform: crate::pipeline::IccOutputTransform,
    pub(crate) camera_profile_mode: CameraProfileMode,
    pub(crate) camera_profile_folder: Option<PathBuf>,
    pub(crate) camera_profile_folder_label: Option<String>,
    pub(crate) camera_profile_auto_detect: bool,
    /// Last manually selected DCP, relative to `camera_profile_folder`. This is
    /// a sticky default only for newly opened RAWs that have no sidecar yet.
    pub(crate) last_camera_profile: Option<PathBuf>,
    /// Explicit DCP chosen for the currently edited image. None keeps automatic selection.
    pub(crate) selected_camera_profile: Option<PathBuf>,
    pub active_tab: AppTab,
    pub sidebar_tab: SidebarTab,
    pub(crate) geometry: GeometryTransform,
    /// Runtime-only crop before automatic rotation/keystone containment. This
    /// lets the crop expand again when the user reduces the straighten angle.
    pub(crate) crop_constraint_reference: Option<[f32; 4]>,
    pub(crate) crop_drag: Option<CropDragState>,
    pub(crate) straighten_tool_active: bool,
    pub(crate) straighten_drag: Option<StraightenDragState>,
    pub(crate) geometry_revision: u64,
    pub adjustment_section: AdjustmentSection,
    pub mask_section: MaskSection,
    pub tone_curve_tab: ToneCurveTab,
    pub color_grade_tab: ColorGradeTab,
    pub export_settings: ExportSettings,
    pub masks: MaskStack,
    pub(crate) active_mask_tool: Option<MaskKind>,
    pub(crate) brush_mode: BrushMode,
    pub(crate) mask_drag: Option<MaskDragState>,
    pub(crate) last_brush_point: Option<[f32; 2]>,
    mask_touch_gesture_backup: Option<MaskTouchGestureBackup>,
    mask_interaction_dirty_layer: Option<usize>,
    mask_interaction_last_upload: Option<Instant>,
    mask_interaction_has_uncommitted_change: bool,
    pub(crate) mask_overlay_revision: u64,
    pub(crate) mask_overlay_texture: Option<egui::TextureHandle>,
    pub(crate) mask_overlay_texture_key: Option<(usize, Option<usize>, u64, u32, u32)>,
    pub(crate) mask_overlay_blink: Option<(std::time::Instant, MaskOverlayBlink)>,
    pub(crate) mask_thumbnail_revision: u64,
    pub(crate) mask_thumbnail_group_textures: Vec<egui::TextureHandle>,
    pub(crate) mask_thumbnail_component_mask: Option<usize>,
    pub(crate) mask_thumbnail_component_textures: Vec<egui::TextureHandle>,
    pub(crate) mask_source_cache: Option<MaskRgbImage>,
    pub(crate) subject_mask_cache: Option<MaskImage>,
    pub(crate) ai_masks_need_update: bool,
    ai_mask_update_active: bool,
    ai_mask_update_subject_pending: bool,
    ai_mask_update_object_queue: VecDeque<(usize, usize)>,
    ai_mask_update_failed: bool,
    #[cfg(not(target_os = "android"))]
    pub(crate) onnx_runtime_path: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    pub(crate) onnx_runtime_sha256: Option<String>,
    #[cfg(not(target_os = "android"))]
    desktop_picker_receiver: Option<mpsc::Receiver<DesktopPickerEvent>>,
    pub status: String,
    /// Reveals low-level darktable/raw controls. The default Lightroom-like
    /// interface intentionally keeps these implementation details hidden.
    pub expert_mode: bool,
    pub(crate) lens_correction: LensCorrectionState,
    edit_history: EditHistory,
    /// A history restore across a lens-geometry change must put back the masks
    /// associated with that historical geometry after the normal lens rebuild.
    history_lens_restore_masks: Option<MaskStack>,
    sidecar_target: Option<crate::sidecar::SidecarTarget>,
    sidecar_generation: u64,
    sidecar_saved_revision: Option<u64>,
    sidecar_failed_revision: Option<u64>,
    sidecar_pending: VecDeque<SidecarSaveRequest>,
    sidecar_in_flight: Option<SidecarSaveJob>,
    sidecar_receiver: Option<mpsc::Receiver<SidecarSaveEvent>>,
    sidecar_autosave_deadline: Option<SidecarAutosaveDeadline>,
    developed_thumbnail_pending: Option<DevelopedThumbnailJob>,
    developed_thumbnail_in_flight: Option<DevelopedThumbnailJob>,
    developed_thumbnail_receiver: Option<mpsc::Receiver<DevelopedThumbnailEvent>>,

    egui_ctx: egui::Context,
    target_exposure: ExposureParams,
    pending_stage: Option<ProcessingStage>,
    lens_correction_dirty: bool,
    load_receiver: Option<mpsc::Receiver<LoadEvent>>,
    loading_label: Option<String>,
    export_receiver: Option<mpsc::Receiver<ExportEvent>>,
    export_progress: Option<(usize, usize)>,
    library_batch_export: Option<LibraryBatchExportState>,
    export_publish_pending: bool,
    image_status: String,
    current_label: Option<String>,
    notice: Option<String>,
    dirty_mask_layers: [bool; MAX_LOCAL_MASKS],
    detail_dirty_mask_layers: [bool; MAX_LOCAL_MASKS],
    navigation_dirty_mask_layers: [bool; MAX_LOCAL_MASKS],
    subject_consent_open: bool,
    subject_receiver: Option<mpsc::Receiver<SubjectMaskEvent>>,
    subject_download_progress: Option<(&'static str, u64, u64)>,
    subject_inferencing: bool,
    object_consent_open: bool,
    object_pending_target: Option<(usize, usize)>,
    object_receiver: Option<mpsc::Receiver<ObjectMaskEvent>>,
    object_download_progress: Option<(&'static str, u64, u64)>,
    object_inferencing: bool,
    object_decoder_only: bool,
    object_error_dialog: Option<String>,
    object_generation: u64,
    object_job_generation: u64,
    object_job_target: Option<(usize, usize)>,
    object_cache: Option<((usize, usize), ObjectInferenceCache)>,

    pub(crate) inpaint_brush_size: f32,
    pub(crate) inpaint_stroke: Vec<crate::pipeline::BrushDab>,
    pub(crate) inpaint_strokes: Vec<InpaintStroke>,
    pub(crate) last_inpaint_brush_point: Option<[f32; 2]>,
    pub(crate) inpaint_layer: Option<InpaintLayer>,
    pub(crate) inpaint_texture: Option<egui::TextureHandle>,
    pub(crate) inpaint_texture_revision: u64,
    pub(crate) inpaint_texture_key: Option<u64>,
    pub(crate) inpaint_stroke_texture: Option<egui::TextureHandle>,
    pub(crate) inpaint_stroke_texture_key: Option<(usize, u32, u32)>,
    pub(crate) inpaint_hovered_stroke: Option<usize>,
    pub(crate) inpaint_selected_stroke: Option<usize>,
    pub(crate) inpaint_focus_texture: Option<egui::TextureHandle>,
    pub(crate) inpaint_focus_texture_key: Option<(usize, u64, u32, u32, bool)>,
    inpaint_source_cache: Option<MaskRgbImage>,
    inpaint_pending_source: Option<PreparedInpaintSource>,
    inpaint_active_dabs: Option<Vec<crate::pipeline::BrushDab>>,
    inpaint_revision: u64,
    inpaint_consent_open: bool,
    inpaint_receiver: Option<mpsc::Receiver<InpaintEvent>>,
    inpaint_download_progress: Option<(u64, u64)>,
    inpaint_inferencing: bool,

    #[cfg(target_os = "android")]
    android_app: android_activity::AndroidApp,
    #[cfg(target_os = "android")]
    pub(crate) picker_pending: bool,
    /// Label of the SAF tree currently being mirrored into app-private DCP storage.
    /// This is UI-only transient state and is never persisted as the active folder.
    #[cfg(target_os = "android")]
    pub(crate) camera_profile_folder_importing_label: Option<String>,
    #[cfg(target_os = "android")]
    pending_android_profile_reload: Option<(Option<PathBuf>, SidecarEditState)>,
}

fn collect_pipeline_update_results(
    operation: &'static str,
    updates: Vec<(&'static str, anyhow::Result<()>)>,
) -> anyhow::Result<()> {
    let failures = updates
        .into_iter()
        .filter_map(|(pipeline, result)| {
            result
                .err()
                .map(|error| format!("{pipeline}: {operation}: {error:#}"))
        })
        .collect::<Vec<_>>();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(failures.join("; ")))
    }
}

impl AurawApp {
    pub(crate) fn activate_tab(&mut self, tab: AppTab) {
        if self.active_tab == tab {
            return;
        }
        if self.active_tab == AppTab::Develop && tab != AppTab::Develop {
            self.set_original_preview_requested(false);
            self.android_original_hold = None;
        }
        if self.active_tab == AppTab::Library && tab != AppTab::Library {
            // Keep thumbnail decoding from competing with Develop rendering.
            self.library.prepare_for_develop();
        }
        self.active_tab = tab;
        #[cfg(target_os = "android")]
        crate::android::set_back_navigation_active(tab != AppTab::Library);
    }

    fn take_preview_pipeline_and_release_textures(
        &mut self,
        renderer: &mut eframe::egui_wgpu::Renderer,
    ) -> Option<RawGpuPipeline> {
        let pipeline = self.gpu_pipeline.take();
        if let Some(texture_id) = pipeline.as_ref().and_then(|pipeline| pipeline.egui_texture_id) {
            renderer.free_texture(&texture_id);
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
            renderer.free_texture(&texture_id);
        }
        pipeline
    }

    #[cfg(target_os = "android")]
    pub(crate) fn copy_text_to_clipboard(&self, label: &str, text: &str) -> Result<(), String> {
        crate::android::copy_text_to_clipboard(&self.android_app, label, text)
    }
}

include!("app/lifecycle.rs");
include!("app/masks_ai.rs");
include!("app/inpainting.rs");
include!("app/processing_export.rs");
include!("app/library_adjustments.rs");
include!("app/sidecar_persistence.rs");
include!("app/eframe_impl.rs");

#[cfg(test)]
mod transactional_pipeline_tests {
    use super::collect_pipeline_update_results;

    #[test]
    fn each_present_pipeline_failure_has_operation_context() {
        for failed in ["main", "detail", "navigation"] {
            let result = collect_pipeline_update_results(
                "install inpaint layer",
                ["main", "detail", "navigation"]
                    .into_iter()
                    .map(|name| {
                        let result = if name == failed {
                            Err(anyhow::anyhow!("injected failure"))
                        } else {
                            Ok(())
                        };
                        (name, result)
                    })
                    .collect(),
            );
            let message = format!("{:#}", result.unwrap_err());
            assert!(message.contains(failed));
            assert!(message.contains("install inpaint layer"));
        }
    }

    #[test]
    fn absent_optional_pipelines_need_no_placeholder_update() {
        assert!(collect_pipeline_update_results(
            "install output transform",
            vec![("main", Ok(()))],
        )
        .is_ok());
    }

    #[test]
    fn a_later_retry_can_succeed_after_partial_failure_without_advancing_revision() {
        let mut rendered_revision = Some(41_u64);
        let requested_revision = 42_u64;
        let first = collect_pipeline_update_results(
            "install output transform",
            vec![
                ("main", Ok(())),
                ("detail", Err(anyhow::anyhow!("injected failure"))),
            ],
        );
        if first.is_ok() {
            rendered_revision = Some(requested_revision);
        }
        assert!(first.is_err());
        assert_eq!(rendered_revision, Some(41));

        let retry = collect_pipeline_update_results(
            "install output transform",
            vec![("main", Ok(())), ("detail", Ok(()))],
        );
        if retry.is_ok() {
            rendered_revision = Some(requested_revision);
        }
        assert!(retry.is_ok());
        assert_eq!(rendered_revision, Some(requested_revision));
    }
}
