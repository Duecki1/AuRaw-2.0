use crate::ai_masks::{
    spawn_landscape_mask, spawn_object_mask, spawn_subject_mask, BiRefNetQuality,
    LandscapeMaskEvent, LandscapeMaskWorkerRequest, ObjectInferenceCache, ObjectMaskEvent,
    ObjectMaskRequest, SubjectMaskEvent, SubjectMaskWorkerRequest, LANDSCAPE_MODEL_BYTES,
    SAM21_MODEL_BYTES_ESTIMATE, VITMATTE_MODEL_BYTES,
};
use crate::inpainting::{
    inpaint_capture_rect, inpaint_patch_rect, spawn_inpaint, InpaintEvent, InpaintRequest,
    PreparedInpaintSource, LAMA_EDGE, LAMA_MODEL_BYTES,
};
#[cfg(target_os = "android")]
use crate::pipeline::GpuProgramPrewarm;
#[cfg(not(target_os = "android"))]
use crate::pipeline::RawThumbnail;
use crate::pipeline::{
    affected_stage, apply_lensfun_correction, build_proxy, build_region_proxy,
    compose_inpaint_strokes, crop_raw, lensfun_catalog, load_raw_file_with_profile_selection,
    spawn_tiled_jpeg_export_with_program_prewarm, spawn_tiled_png_export_with_program_prewarm,
    spawn_tiled_tiff_export_with_program_prewarm, BrushDab, BrushMode, CameraProfileMode,
    ExportEvent, ExportFormat, ExportMetadata, ExportSettings, ExposureParams, GeometryTransform,
    GpuParams, InpaintLayer, InpaintStroke, LandscapeCategory, LensfunCatalog, LensfunLens,
    LoadedRaw, MaskGeometry, MaskImage, MaskKind, MaskRgbImage, MaskStack, ProcessingQuality,
    ProcessingStage, ProxySpec, RawGpuPipeline, RawGpuProgramTemplate, SubjectRefinement, TileSpec,
    EXPORT_TILE_HALO, MAX_LOCAL_MASKS,
};
use crate::sidecar::{
    AdjustmentCopySettings, AdjustmentPasteMode, EditState as SidecarEditState,
    LensEditState as SidecarLensEditState,
};
use crate::ui::components::adjustment_slider::slider_scroll_locked;
#[cfg(not(target_os = "android"))]
use crate::ui::develop::Develop;
use crate::ui::layout::ScreenLayout;
use crate::ui::library::{Library, LibraryState};
use crate::ui::preview::Preview;
use crate::ui::settings::Settings;
use crate::ui::sidebar::Sidebar;
use crate::ui::top_bar::TopBar;
use eframe::{egui, wgpu};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

mod ai_denoise;
mod background_tasks;
use background_tasks::{
    BackgroundTaskManager, CancelTaskResult, TaskId, TaskKind, TaskProgress, TaskProgressValue,
    TaskSnapshot, TaskStatus,
};
mod edit_history;
use edit_history::EditHistory;

#[cfg(not(target_os = "android"))]
pub(crate) enum DesktopPickerEvent {
    RawFile(Option<PathBuf>),
    CloudRawFiles(Option<Vec<PathBuf>>),
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

#[cfg(not(target_os = "android"))]
pub(crate) struct DevelopReferenceState {
    pub(crate) path: Option<PathBuf>,
    pub(crate) label: Option<String>,
    pub(crate) texture: Option<egui::TextureHandle>,
    pub(crate) texture_size: Option<[u32; 2]>,
    pub(crate) high_quality: bool,
    pub(crate) loading_path: Option<PathBuf>,
    pub(crate) preview_receiver: Option<mpsc::Receiver<(PathBuf, Result<RawThumbnail, String>)>>,
    pub(crate) error: Option<String>,
    pub(crate) split_ratio: f32,
}

#[cfg(not(target_os = "android"))]
impl Default for DevelopReferenceState {
    fn default() -> Self {
        Self {
            path: None,
            label: None,
            texture: None,
            texture_size: None,
            high_quality: false,
            loading_path: None,
            preview_receiver: None,
            error: None,
            split_ratio: 0.5,
        }
    }
}

#[cfg(not(target_os = "android"))]
impl DevelopReferenceState {
    pub(crate) fn clear(&mut self) {
        self.path = None;
        self.label = None;
        self.texture = None;
        self.texture_size = None;
        self.high_quality = false;
        self.loading_path = None;
        self.preview_receiver = None;
        self.error = None;
    }
}

pub(crate) struct DevelopLoadingThumbnailState {
    #[cfg(not(target_os = "android"))]
    pub(crate) path: Option<PathBuf>,
    #[cfg(target_os = "android")]
    pub(crate) source_uri: Option<String>,
    pub(crate) texture: Option<egui::TextureHandle>,
    pub(crate) texture_size: Option<[u32; 2]>,
    #[cfg(not(target_os = "android"))]
    pub(crate) receiver: Option<mpsc::Receiver<(PathBuf, Result<Option<RawThumbnail>, String>)>>,
}

impl Default for DevelopLoadingThumbnailState {
    fn default() -> Self {
        Self {
            #[cfg(not(target_os = "android"))]
            path: None,
            #[cfg(target_os = "android")]
            source_uri: None,
            texture: None,
            texture_size: None,
            #[cfg(not(target_os = "android"))]
            receiver: None,
        }
    }
}

impl DevelopLoadingThumbnailState {
    pub(crate) fn clear(&mut self) {
        #[cfg(not(target_os = "android"))]
        {
            self.path = None;
            self.receiver = None;
        }
        #[cfg(target_os = "android")]
        {
            self.source_uri = None;
        }
        self.texture = None;
        self.texture_size = None;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PreviewQuality {
    #[serde(alias = "fast")]
    Low,
    #[serde(alias = "balanced")]
    #[default]
    Medium,
    High,
    Max,
}

impl PreviewQuality {
    pub const fn pixel_scale(self) -> f32 {
        match self {
            Self::Low => 0.50,
            Self::Medium => 0.67,
            Self::High => 0.84,
            Self::Max => 1.00,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Max => "Max",
        }
    }

    pub const fn proxy_edge(self) -> u32 {
        match self {
            Self::Low => 640,
            Self::Medium => 800,
            Self::High => 1024,
            Self::Max => 1280,
        }
    }

    fn edge_for_scale(self, viewport_pixels: [u32; 2], support_scale: f32) -> u32 {
        const CFA_PHASE_GUARD: u32 = 6;
        let viewport_edge = viewport_pixels[0].max(viewport_pixels[1]).max(1) as f64;
        let requested = (viewport_edge * f64::from(self.pixel_scale() * support_scale)).ceil();
        self.proxy_edge()
            .max(requested.min(f64::from(u32::MAX - CFA_PHASE_GUARD)) as u32 + CFA_PHASE_GUARD)
    }

    pub fn proxy_edge_for_viewport(self, viewport_pixels: [u32; 2]) -> u32 {
        self.edge_for_scale(viewport_pixels, 1.0)
    }

    pub fn proxy_edge_for_fitted_source(
        self,
        viewport_pixels: [u32; 2],
        source_width: u32,
        source_height: u32,
        geometry: GeometryTransform,
    ) -> u32 {
        const CFA_PHASE_GUARD: u32 = 6;
        let source_width = source_width.max(1);
        let source_height = source_height.max(1);
        let source_edge = source_width.max(source_height);
        let (display_width, display_height) =
            geometry.crop_pixel_dimensions(source_width, source_height);
        let fit_scale = (f64::from(viewport_pixels[0].max(1)) / f64::from(display_width.max(1)))
            .min(f64::from(viewport_pixels[1].max(1)) / f64::from(display_height.max(1)));
        let requested = (f64::from(source_edge) * fit_scale * f64::from(self.pixel_scale()))
            .ceil()
            .min(f64::from(u32::MAX - CFA_PHASE_GUARD)) as u32
            + CFA_PHASE_GUARD;
        self.proxy_edge().max(requested).min(source_edge)
    }

    pub fn detail_edge_for_viewport(self, viewport_pixels: [u32; 2]) -> u32 {
        self.edge_for_scale(viewport_pixels, 1.35)
    }

    pub const fn detail_pixel_scale(self) -> f32 {
        self.pixel_scale()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OverlayRasterKey {
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub texture_width: u32,
    pub texture_height: u32,
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
    mask_source_region: [u32; 4],
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(usize)]
pub enum HslMixerColor {
    #[default]
    Red,
    Orange,
    Yellow,
    Green,
    Aqua,
    Blue,
    Purple,
    Magenta,
}

impl HslMixerColor {
    pub const ALL: [Self; 8] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Aqua,
        Self::Blue,
        Self::Purple,
        Self::Magenta,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }
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

struct PreparedPreviewRebuild {
    source_raw: Arc<LoadedRaw>,
    preview_raw: Arc<LoadedRaw>,
    quality: PreviewQuality,
    requested_edge: u32,
    ai_enabled: bool,
}

enum PreviewRebuildEvent {
    Finished(Result<PreparedPreviewRebuild, String>),
}

enum LoadEvent {
    Finished(Result<LoadedPreview, String>),
}

struct PreparedLensCorrection {
    full_raw: Arc<LoadedRaw>,
    preview_raw: Arc<LoadedRaw>,
    applied_label: Option<String>,
    #[cfg(target_os = "android")]
    selection: Option<LensfunLens>,
    #[cfg(target_os = "android")]
    preview_quality: PreviewQuality,
}

enum LensCorrectionEvent {
    Progress {
        task_id: TaskId,
        document_id: u64,
        generation: u64,
        phase: String,
    },
    Finished {
        task_id: TaskId,
        document_id: u64,
        generation: u64,
        result: Result<PreparedLensCorrection, String>,
    },
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
    raw_path: Option<PathBuf>,
    result: Result<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloudSidecarConflictResolution {
    OverwriteServer,
    OverwriteLocal,
}

struct CloudSidecarConflictEvent {
    raw_path: PathBuf,
    generation: u64,
    revision: u64,
    resolution: CloudSidecarConflictResolution,
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
    subject_refinement: Option<SubjectRefinement>,
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
pub(crate) enum AndroidLibraryExportTarget {
    Local { uri: String, display_name: String },
    Cloud { path: PathBuf, display_name: String },
}

#[cfg(target_os = "android")]
impl AndroidLibraryExportTarget {
    fn display_name(&self) -> &str {
        match self {
            Self::Local { display_name, .. } | Self::Cloud { display_name, .. } => display_name,
        }
    }
}

#[cfg(target_os = "android")]
#[derive(Clone, Debug)]
struct LibraryBatchExportJob {
    target: AndroidLibraryExportTarget,
}

#[derive(Clone, Debug)]
struct LibraryAdjustmentClipboard {
    edits: SidecarEditState,
    settings: AdjustmentCopySettings,
    #[cfg(not(target_os = "android"))]
    source_label: String,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Debug)]
struct LibraryAiMaskRefreshJob {
    source: PathBuf,
    mask_targets: usize,
}

#[cfg(target_os = "android")]
#[derive(Clone, Debug)]
struct LibraryAiMaskRefreshJob {
    uri: String,
    display_name: String,
    mask_targets: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryAiMaskRefreshPhase {
    Loading,
    Updating,
    Saving,
}

#[derive(Debug)]
struct LibraryAiMaskRefreshState {
    pending: VecDeque<LibraryAiMaskRefreshJob>,
    current: Option<LibraryAiMaskRefreshJob>,
    phase: LibraryAiMaskRefreshPhase,
    total: usize,
    completed: usize,
    mask_total: usize,
    mask_completed: usize,
    failures: Vec<String>,
    cancel_requested: bool,
}

#[derive(Debug)]
struct LibraryBatchExportState {
    pending: VecDeque<LibraryBatchExportJob>,
    current: Option<LibraryBatchExportJob>,
    total: usize,
    completed: usize,
    failures: Vec<String>,
    cancel_requested: bool,
    #[cfg(target_os = "android")]
    format: ExportFormat,
    #[cfg(target_os = "android")]
    settings: ExportSettings,
}

#[cfg(not(target_os = "android"))]
enum LibraryBatchExportEvent {
    Started {
        job: LibraryBatchExportJob,
        completed: usize,
        total: usize,
    },
    Progress {
        completed: usize,
        total: usize,
        completed_tiles: usize,
        total_tiles: usize,
    },
    ItemFinished {
        completed: usize,
        error: Option<String>,
    },
    Finished {
        cancelled: bool,
    },
}

struct ExportTaskRequest {
    device: wgpu::Device,
    queue: wgpu::Queue,
    raw: Arc<LoadedRaw>,
    geometry: GeometryTransform,
    exposure: ExposureParams,
    masks: MaskStack,
    inpaint: Option<InpaintLayer>,
    path: PathBuf,
    format: ExportFormat,
    settings: ExportSettings,
    metadata: ExportMetadata,
    display_name: String,
    #[cfg(target_os = "android")]
    gpu_export_prewarm: Option<Arc<GpuProgramPrewarm>>,
}

struct LensCorrectionTaskRequest {
    document_id: u64,
    generation: u64,
    original_raw: Arc<LoadedRaw>,
    selection: Option<LensfunLens>,
    #[cfg(target_os = "android")]
    preview_quality: PreviewQuality,
    preview_proxy_edge: u32,
    cached_raws: Option<(Arc<LoadedRaw>, Arc<LoadedRaw>)>,
}

struct SubjectMaskTaskRequest {
    document_id: u64,
    generation: u64,
    quality: BiRefNetQuality,
    source: MaskRgbImage,
    model_path: PathBuf,
    runtime_path: Option<PathBuf>,
    runtime_sha256: Option<String>,
}

struct ObjectMaskTaskRequest {
    document_id: u64,
    generation: u64,
    target: AiMaskTarget,
    encoder_path: PathBuf,
    decoder_path: PathBuf,
    vitmatte_path: PathBuf,
    runtime_path: Option<PathBuf>,
    runtime_sha256: Option<String>,
    request: ObjectMaskRequest,
}

struct LandscapeMaskTaskRequest {
    document_id: u64,
    generation: u64,
    target: AiMaskTarget,
    source: MaskRgbImage,
    model_path: PathBuf,
    vitmatte_path: PathBuf,
    allow_download: bool,
    runtime_path: Option<PathBuf>,
    runtime_sha256: Option<String>,
    category: LandscapeCategory,
}

type GeneratedAiMaskTargets = (bool, VecDeque<(usize, usize)>, VecDeque<(usize, usize)>);

#[cfg(target_os = "android")]
type AndroidAdjustmentPasteResult = (Vec<(String, String)>, Vec<(String, String)>, Vec<String>);

/// A content-aware mask result must be matched to the component that started
/// the request, not merely to the slot it occupied at that time.  Reordering
/// is allowed while inference runs, so the snapshot is deliberately compared
/// before applying a result.  Ambiguous duplicate snapshots are discarded.
#[derive(Clone, Debug, PartialEq)]
struct AiMaskTarget {
    mask_index: usize,
    component_index: usize,
    kind: MaskKind,
    geometry: MaskGeometry,
}

struct InpaintTaskRequest {
    document_id: u64,
    generation: u64,
    model_path: PathBuf,
    runtime_path: Option<PathBuf>,
    runtime_sha256: Option<String>,
    request: InpaintRequest,
    dabs: Vec<BrushDab>,
}

enum BackgroundAction {
    SingleExport(Box<ExportTaskRequest>),
    LibraryBatchExport {
        jobs: VecDeque<LibraryBatchExportJob>,
        settings: ExportSettings,
        format: ExportFormat,
    },
    LensCorrection(LensCorrectionTaskRequest),
    SubjectMask(SubjectMaskTaskRequest),
    ObjectMask(Box<ObjectMaskTaskRequest>),
    LandscapeMask(LandscapeMaskTaskRequest),
    Inpainting(InpaintTaskRequest),
    LibraryAiMaskRefresh {
        jobs: VecDeque<LibraryAiMaskRefreshJob>,
    },
}

pub struct AurawApp {
    pub current_path: Option<PathBuf>,
    pub(crate) original_raw: Option<Arc<LoadedRaw>>,
    pub loaded_raw: Option<Arc<LoadedRaw>>,
    pub preview_raw: Option<Arc<LoadedRaw>>,
    pub gpu_pipeline: Option<RawGpuPipeline>,
    // Compiled preview programs are tiny compared with an image-sized render
    // graph. Retain them independently so replacing or temporarily releasing
    // a preview never forces Android to compile the full graph again.
    preview_program_template: Option<RawGpuProgramTemplate>,
    // Native preview texture IDs cannot be freed during `App::ui`: egui may
    // already have emitted meshes that reference them for the current frame.
    // Retire them now and remove them from the renderer at the start of the
    // next frame, before any new paint meshes are built.
    retired_egui_textures: Vec<egui::TextureId>,
    #[cfg(target_os = "android")]
    gpu_preview_prewarm_receiver: Option<mpsc::Receiver<Result<RawGpuPipeline, String>>>,
    #[cfg(target_os = "android")]
    gpu_export_prewarm: Option<Arc<GpuProgramPrewarm>>,
    pub(crate) preview_quality: PreviewQuality,
    pub(crate) image_relative_brush_size: bool,
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
    preview_rebuild_receiver: Option<mpsc::Receiver<PreviewRebuildEvent>>,
    pub(crate) original_preview_exposure: ExposureParams,
    pub(crate) original_preview_requested: bool,
    original_preview_rendered_state: Option<(bool, u64)>,
    pub(crate) android_original_hold: Option<AndroidOriginalHold>,
    pub exposure: ExposureParams,
    pub(crate) library: LibraryState,
    #[cfg(not(target_os = "android"))]
    pub(crate) develop_reference: DevelopReferenceState,
    pub(crate) develop_loading_thumbnail: DevelopLoadingThumbnailState,
    #[cfg(not(target_os = "android"))]
    pub(crate) develop_filmstrip_open: bool,
    #[cfg(not(target_os = "android"))]
    pub(crate) develop_filmstrip_centered_path: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    pub(crate) develop_sidebar_open: bool,
    pub(crate) adjustment_copy_settings: AdjustmentCopySettings,
    adjustment_clipboard: Option<LibraryAdjustmentClipboard>,
    raw_cache: VecDeque<CachedRawDecode>,
    raw_cache_limit: usize,
    performance_settings_path: Option<PathBuf>,
    thumbnail_cache_size: Option<Result<u64, String>>,
    thumbnail_cache_size_receiver: Option<mpsc::Receiver<Result<u64, String>>>,
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
    pub(crate) white_balance_picker_active: bool,
    /// Native-source UV start/current corners while the neutral-area picker is dragged.
    pub(crate) white_balance_picker_drag: Option<[[f32; 2]; 2]>,
    pub(crate) geometry_revision: u64,
    pub adjustment_section: AdjustmentSection,
    pub mask_section: MaskSection,
    pub tone_curve_tab: ToneCurveTab,
    pub color_grade_tab: ColorGradeTab,
    pub hsl_mixer_color: HslMixerColor,
    pub export_settings: ExportSettings,
    pub masks: MaskStack,
    pub(crate) active_mask_tool: Option<MaskKind>,
    pub(crate) brush_mode: BrushMode,
    pub(crate) subject_refinement_active: bool,
    pub(crate) mask_drag: Option<MaskDragState>,
    pub(crate) last_brush_point: Option<[f32; 2]>,
    mask_touch_gesture_backup: Option<MaskTouchGestureBackup>,
    mask_interaction_dirty_layer: Option<usize>,
    mask_interaction_last_upload: Option<Instant>,
    mask_interaction_has_uncommitted_change: bool,
    pub(crate) mask_overlay_revision: u64,
    pub(crate) mask_overlay_texture: Option<egui::TextureHandle>,
    pub(crate) mask_overlay_texture_key: Option<(usize, Option<usize>, u64, OverlayRasterKey)>,
    pub(crate) mask_overlay_blink: Option<(std::time::Instant, MaskOverlayBlink)>,
    pub(crate) mask_thumbnail_revision: u64,
    pub(crate) mask_thumbnail_group_textures: Vec<egui::TextureHandle>,
    pub(crate) mask_thumbnail_component_mask: Option<usize>,
    pub(crate) mask_thumbnail_component_textures: Vec<egui::TextureHandle>,
    pub(crate) mask_source_cache: Option<MaskRgbImage>,
    pub(crate) subject_mask_cache: Option<MaskImage>,
    pub(crate) birefnet_quality: BiRefNetQuality,
    pub(crate) ai_masks_need_update: bool,
    ai_mask_update_active: bool,
    ai_mask_update_subject_pending: bool,
    ai_mask_update_object_queue: VecDeque<(usize, usize)>,
    ai_mask_update_landscape_queue: VecDeque<(usize, usize)>,
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
    sidecar_save_feedback_until: Option<Instant>,
    sidecar_save_error_dialog: Option<String>,
    sidecar_conflict_receiver: Option<mpsc::Receiver<CloudSidecarConflictEvent>>,
    sidecar_conflict_resolution_error: Option<String>,
    sidecar_autosave_deadline: Option<SidecarAutosaveDeadline>,
    developed_thumbnail_pending: Option<DevelopedThumbnailJob>,
    developed_thumbnail_in_flight: Option<DevelopedThumbnailJob>,
    developed_thumbnail_receiver: Option<mpsc::Receiver<DevelopedThumbnailEvent>>,

    egui_ctx: egui::Context,
    background_tasks: BackgroundTaskManager,
    background_actions: HashMap<TaskId, BackgroundAction>,
    export_task_id: Option<TaskId>,
    library_batch_export_task_id: Option<TaskId>,
    library_ai_mask_refresh_task_id: Option<TaskId>,
    subject_task_id: Option<TaskId>,
    object_task_id: Option<TaskId>,
    landscape_task_id: Option<TaskId>,
    inpaint_task_id: Option<TaskId>,
    target_exposure: ExposureParams,
    pending_stage: Option<ProcessingStage>,
    lens_correction_dirty: bool,
    lens_correction_generation: u64,
    lens_correction_receiver: Option<mpsc::Receiver<LensCorrectionEvent>>,
    lens_correction_task_id: Option<TaskId>,
    #[cfg(target_os = "android")]
    lens_original_preview_cache: Option<(PreviewQuality, Arc<LoadedRaw>)>,
    #[cfg(target_os = "android")]
    lens_corrected_preview_cache:
        Option<(LensfunLens, PreviewQuality, Arc<LoadedRaw>, Arc<LoadedRaw>)>,
    load_receiver: Option<mpsc::Receiver<LoadEvent>>,
    loading_label: Option<String>,
    export_receiver: Option<mpsc::Receiver<ExportEvent>>,
    export_progress: Option<(usize, usize)>,
    #[cfg(not(target_os = "android"))]
    library_batch_export_receiver: Option<mpsc::Receiver<LibraryBatchExportEvent>>,
    #[cfg(not(target_os = "android"))]
    library_batch_export_tile_progress: Option<(usize, usize)>,
    library_batch_export: Option<LibraryBatchExportState>,
    library_ai_mask_refresh: Option<LibraryAiMaskRefreshState>,
    export_publish_pending: bool,
    image_status: String,
    current_label: Option<String>,
    notice: Option<String>,
    dirty_mask_layers: [bool; MAX_LOCAL_MASKS],
    detail_dirty_mask_layers: [bool; MAX_LOCAL_MASKS],
    navigation_dirty_mask_layers: [bool; MAX_LOCAL_MASKS],
    subject_consent_open: bool,
    subject_receiver: Option<mpsc::Receiver<SubjectMaskEvent>>,
    subject_generation: u64,
    subject_job_document_id: u64,
    subject_job_generation: u64,
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
    object_job_document_id: u64,
    object_job_target: Option<AiMaskTarget>,
    object_cache: Option<((usize, usize), ObjectInferenceCache)>,
    landscape_consent_open: bool,
    landscape_pending_target: Option<(usize, usize)>,
    landscape_receiver: Option<mpsc::Receiver<LandscapeMaskEvent>>,
    landscape_download_progress: Option<(u64, u64)>,
    landscape_inferencing: bool,
    landscape_generation: u64,
    landscape_job_generation: u64,
    landscape_job_document_id: u64,
    landscape_job_target: Option<AiMaskTarget>,
    landscape_job_category: Option<LandscapeCategory>,

    pub(crate) inpaint_brush_size: f32,
    pub(crate) inpaint_stroke: Vec<crate::pipeline::BrushDab>,
    pub(crate) inpaint_strokes: Vec<InpaintStroke>,
    pub(crate) last_inpaint_brush_point: Option<[f32; 2]>,
    pub(crate) inpaint_layer: Option<InpaintLayer>,
    pub(crate) inpaint_texture: Option<egui::TextureHandle>,
    pub(crate) inpaint_texture_revision: u64,
    pub(crate) inpaint_texture_key: Option<u64>,
    pub(crate) inpaint_stroke_texture: Option<egui::TextureHandle>,
    pub(crate) inpaint_stroke_texture_key: Option<(usize, OverlayRasterKey)>,
    pub(crate) inpaint_hovered_stroke: Option<usize>,
    pub(crate) inpaint_selected_stroke: Option<usize>,
    pub(crate) inpaint_focus_texture: Option<egui::TextureHandle>,
    pub(crate) inpaint_focus_texture_key: Option<(usize, u64, OverlayRasterKey, bool)>,
    inpaint_source_cache: Option<MaskRgbImage>,
    inpaint_pending_source: Option<PreparedInpaintSource>,
    inpaint_active_dabs: Option<Vec<crate::pipeline::BrushDab>>,
    inpaint_replace_index: Option<usize>,
    inpaint_revision: u64,
    inpaint_job_document_id: u64,
    inpaint_job_generation: u64,
    inpaint_consent_open: bool,
    inpaint_receiver: Option<mpsc::Receiver<InpaintEvent>>,
    inpaint_download_progress: Option<(u64, u64)>,
    inpaint_inferencing: bool,
    ai_denoise_consent_open: bool,
    ai_denoise_receiver: Option<mpsc::Receiver<crate::ai_denoise::AiDenoiseEvent>>,
    ai_denoise_download_progress: Option<(u64, u64)>,
    ai_denoise_apply_progress: Option<(&'static str, usize, usize)>,
    ai_denoise_cancellation: Option<Arc<std::sync::atomic::AtomicBool>>,
    ai_denoise_job_document_id: u64,
    ai_denoise_resume_pending: bool,

    #[cfg(target_os = "android")]
    android_app: auraw_ffi::AndroidApp,
    #[cfg(target_os = "android")]
    pub(crate) picker_pending: bool,
    /// True while the Android SAF result and RAW decode belong to the batch
    /// exporter rather than to an interactive Library open. The batch exporter
    /// shares Android's document bridge, so completion must be routed by owner
    /// instead of merely checking whether a batch happens to exist.
    #[cfg(target_os = "android")]
    android_batch_load_pending: bool,
    /// True while an internal Android RAW reopen belongs to Reset All. The
    /// document is reloaded so Develop cannot retain stale in-memory edits,
    /// but the Library remains the active tab.
    #[cfg(target_os = "android")]
    pending_android_library_reset_reload: bool,
    /// Label of the SAF tree currently being mirrored into app-private DCP storage.
    /// This is UI-only transient state and is never persisted as the active folder.
    #[cfg(target_os = "android")]
    pub(crate) camera_profile_folder_importing_label: Option<String>,
    #[cfg(target_os = "android")]
    pending_android_profile_reload: Option<(Option<PathBuf>, SidecarEditState)>,
}

#[cfg(any(not(target_os = "android"), test))]
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
    pub(crate) fn sync_ai_model_cache_policy(&self) {
        let develop_visible = self.active_tab == AppTab::Develop;
        crate::ai_masks::set_model_cache_enabled(
            develop_visible && self.sidebar_tab == SidebarTab::Masks,
        );
        crate::inpainting::set_model_cache_enabled(
            develop_visible && self.sidebar_tab == SidebarTab::Inpainting,
        );
    }

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
            #[cfg(target_os = "android")]
            self.library.set_folder_sidebar_open(false);
        }
        if tab == AppTab::Settings {
            self.thumbnail_cache_size = None;
            self.thumbnail_cache_size_receiver = None;
        }
        if tab == AppTab::Library && self.library.is_cloud_view() {
            self.library.refresh(&self.egui_ctx);
        }
        self.active_tab = tab;
        self.sync_ai_model_cache_policy();
        #[cfg(target_os = "android")]
        crate::android::set_back_navigation_active(tab != AppTab::Library);
    }

    fn retire_egui_texture(&mut self, texture_id: egui::TextureId) {
        if !self.retired_egui_textures.contains(&texture_id) {
            self.retired_egui_textures.push(texture_id);
        }
        self.egui_ctx.request_repaint();
    }

    fn release_retired_egui_textures(&mut self, frame: &eframe::Frame) {
        if self.retired_egui_textures.is_empty() {
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let retired = std::mem::take(&mut self.retired_egui_textures);
        let mut renderer = render_state.renderer.write();
        for texture_id in retired {
            renderer.free_texture(&texture_id);
        }
    }

    fn take_preview_pipeline_and_release_textures(
        &mut self,
        _renderer: &mut eframe::egui_wgpu::Renderer,
    ) -> Option<RawGpuPipeline> {
        let pipeline = self.gpu_pipeline.take();
        if let Some(pipeline) = pipeline.as_ref() {
            self.preview_program_template = Some(pipeline.program_template());
        }
        if let Some(texture_id) = pipeline
            .as_ref()
            .and_then(|pipeline| pipeline.egui_texture_id)
        {
            self.retire_egui_texture(texture_id);
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
include!("app/background_task_runtime.rs");
include!("app/eframe_impl.rs");

#[cfg(test)]
mod transactional_pipeline_tests {
    use super::{
        collect_pipeline_update_results, AiMaskTarget, AurawApp, BackgroundTaskManager,
        MaskGeometry, MaskKind, MaskStack, PreviewQuality, TaskKind, TaskProgress, TaskStatus,
    };
    use crate::pipeline::GeometryTransform;

    #[test]
    #[cfg(not(target_os = "android"))]
    fn preview_quality_levels_track_physical_viewport_density() {
        assert_eq!(
            PreviewQuality::Max.proxy_edge_for_viewport([3_000, 2_000]),
            3_006
        );
        assert!(PreviewQuality::Max.detail_edge_for_viewport([3_200, 1_800]) >= 3_200 * 135 / 100);
        for quality in [
            PreviewQuality::Low,
            PreviewQuality::Medium,
            PreviewQuality::High,
            PreviewQuality::Max,
        ] {
            assert!(quality.proxy_edge_for_viewport([3_840, 2_160]) > quality.proxy_edge());
        }
    }

    #[test]
    fn preview_quality_density_is_ordered_and_max_matches_physical_pixels() {
        let viewport = [2_400, 1_600];
        let edges = [
            PreviewQuality::Low.proxy_edge_for_viewport(viewport),
            PreviewQuality::Medium.proxy_edge_for_viewport(viewport),
            PreviewQuality::High.proxy_edge_for_viewport(viewport),
            PreviewQuality::Max.proxy_edge_for_viewport(viewport),
        ];
        assert!(edges.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(edges[3], 2_406);
    }

    #[test]
    fn fitted_preview_density_excludes_phone_letterbox_space() {
        let geometry = GeometryTransform::default();
        let edge =
            PreviewQuality::Max.proxy_edge_for_fitted_source([720, 1_500], 7_028, 4_688, geometry);
        assert_eq!(edge, 1_280);

        let mut cropped = geometry;
        cropped.crop = [0.375, 0.0, 0.625, 1.0];
        assert!(
            PreviewQuality::Max.proxy_edge_for_fitted_source([720, 1_500], 7_028, 4_688, cropped,)
                > edge
        );
    }

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

    #[test]
    fn ai_mask_result_target_survives_reordering_but_not_replacement() {
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::Object);
        let target = AiMaskTarget {
            mask_index: 0,
            component_index: 0,
            kind: MaskKind::Object,
            geometry: stack.masks[0].components[0].geometry.clone(),
        };
        stack.add_component(MaskKind::Brush, crate::pipeline::MaskCombineMode::Add);
        assert_eq!(stack.move_submask_component(0, 0, 0, 2), Some((0, 1)));
        assert_eq!(
            AurawApp::resolve_ai_mask_target_in_stack(&stack, &target),
            Ok((0, 1))
        );

        stack.masks[0].components[1].kind = MaskKind::Brush;
        stack.masks[0].components[1].geometry = MaskGeometry::for_kind(MaskKind::Brush);
        let error = AurawApp::resolve_ai_mask_target_in_stack(&stack, &target).unwrap_err();
        assert!(error.contains("changed type"));
    }

    #[test]
    fn subject_landscape_and_object_errors_remain_failed_tasks() {
        let kinds = [
            TaskKind::SubjectMask {
                document_id: 1,
                generation: 1,
            },
            TaskKind::LandscapeMask {
                document_id: 1,
                generation: 1,
            },
            TaskKind::ObjectMask {
                document_id: 1,
                generation: 1,
            },
        ];
        for kind in kinds {
            let mut tasks = BackgroundTaskManager::default();
            let id = tasks.start_nonblocking(
                kind,
                "Generating mask",
                TaskProgress::indeterminate("Running inference"),
                true,
            );
            assert!(tasks.fail(id, "inference failed"));
            assert_eq!(tasks.snapshot(id).unwrap().status, TaskStatus::Failed);
        }
    }
}
