use crate::ai_masks::{
    spawn_object_mask, spawn_subject_mask, BiRefNetQuality, ObjectInferenceCache, ObjectMaskEvent,
    ObjectMaskRequest, ObjectMaskWorkerRequest, SubjectMaskEvent, SubjectMaskWorkerRequest,
    SAM21_MODEL_BYTES_ESTIMATE, VITMATTE_MODEL_BYTES,
};
#[cfg(not(target_os = "android"))]
use crate::pipeline::RawThumbnail;
use crate::pipeline::{
    affected_stage, apply_lensfun_correction, build_proxy, build_region_proxy, lensfun_catalog,
    load_raw_file_with_profile_selection, spawn_tiled_export, BrushMode, CameraProfileMode,
    ExportEvent, ExportFormat, ExportMetadata, ExportSettings, ExposureParams, GeometryTransform,
    GpuParams, GpuProgramPrewarm, LensfunCatalog, LensfunLens, LoadedRaw, MaskGeometry, MaskImage,
    MaskKind, MaskRgbImage, MaskStack, ProcessingQuality, ProcessingStage, ProxySpec,
    RawGpuPipeline, RawGpuProgramTemplate, RemoveBrushPoint, RemoveBrushStroke, RemoveEditState,
    RemoveSceneContext, RetouchAlignment, RetouchStroke, RetouchTool, SubjectRefinement, TileSpec,
    TiledExportJob, EXPORT_TILE_HALO, MAX_LOCAL_MASKS,
};
use crate::remove::{spawn_remove, spawn_retouch, RemoveEvent, RemoveRequest, RetouchRequest};
use crate::sidecar::{
    AdjustmentCopySettings, AdjustmentPasteMode, EditState as SidecarEditState,
    LensEditState as SidecarLensEditState,
};
use crate::ui::components::adjustment_slider::slider_scroll_locked;
#[cfg(not(target_os = "android"))]
use crate::ui::develop::Develop;
use crate::ui::layout::ScreenLayout;
use crate::ui::library::{Library, LibraryAdjustmentClipboard, LibraryState};
#[cfg(target_os = "android")]
use crate::ui::preview::Preview;
use crate::ui::settings::Settings;
use crate::ui::sidebar::Sidebar;
use crate::ui::theme::{PreviewBackdrop, UiDesign};
use crate::ui::top_bar::TopBar;
use eframe::{egui, wgpu};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

mod ai_denoise;
#[cfg(not(target_os = "android"))]
mod discord_presence;
#[cfg(not(target_os = "android"))]
use discord_presence::DiscordPresence;
mod edit_history;
use edit_history::EditHistory;
mod worker;
use worker::drain_worker_events;
#[cfg(not(target_os = "android"))]
use worker::spawn_ui_worker;

#[cfg(not(target_os = "android"))]
pub(crate) enum DesktopPickerEvent {
    RawFile(Option<PathBuf>),
    LibraryFolder(Option<PathBuf>),
    CameraProfileFolder(Option<PathBuf>),
    OnnxRuntime(Result<Option<(PathBuf, String)>, String>),
}

#[cfg(target_os = "android")]
#[derive(Clone, Copy, Debug)]
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

#[cfg(not(target_os = "android"))]
type DevelopThumbnailResult = (PathBuf, Result<Option<RawThumbnail>, String>);

#[derive(Default)]
pub(crate) struct DevelopLoadingThumbnailState {
    #[cfg(not(target_os = "android"))]
    pub(crate) path: Option<PathBuf>,
    #[cfg(target_os = "android")]
    pub(crate) source_uri: Option<String>,
    pub(crate) texture: Option<egui::TextureHandle>,
    pub(crate) texture_size: Option<[u32; 2]>,
    #[cfg(not(target_os = "android"))]
    pub(crate) receiver: Option<mpsc::Receiver<DevelopThumbnailResult>>,
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
    pub(crate) const fn pixel_scale(self) -> f32 {
        match self {
            Self::Low => 0.75,
            Self::Medium => 1.00,
            Self::High => 1.25,
            Self::Max => 1.50,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Max => "Max",
        }
    }

    pub(crate) const fn proxy_edge(self) -> u32 {
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

    pub(crate) fn proxy_edge_for_viewport(self, viewport_pixels: [u32; 2]) -> u32 {
        self.edge_for_scale(viewport_pixels, 1.0)
    }

    pub(crate) fn proxy_edge_for_fitted_source(
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

    pub(crate) fn detail_edge_for_viewport(self, viewport_pixels: [u32; 2]) -> u32 {
        self.edge_for_scale(viewport_pixels, 1.35)
    }

    pub(crate) const fn detail_pixel_scale(self) -> f32 {
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
    raw: Arc<LoadedRaw>,
}

pub(crate) struct PreviewDetail {
    pub pipeline: RawGpuPipeline,
    pub uv_rect: PreviewUvRect,
    pub texture_uv_rect: PreviewUvRect,
    pub revision: u64,
    raw: Arc<LoadedRaw>,
    source_origin: [u32; 2],
    source_size: [u32; 2],
    mask_source_region: [u32; 4],
    virtual_origin: [i32; 2],
    virtual_full_size: [u32; 2],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AppTab {
    #[default]
    Library,
    Develop,
    Settings,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SidebarTab {
    #[default]
    Adjustments,
    Crop,
    Masks,
    Inpainting,
    Export,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum InpaintTool {
    #[default]
    Remove,
    Clone,
    Heal,
}

impl InpaintTool {
    pub(crate) const ALL: [Self; 3] = [Self::Remove, Self::Clone, Self::Heal];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Remove => "Remove",
            Self::Clone => "Clone",
            Self::Heal => "Heal",
        }
    }

    pub(crate) const fn retouch(self) -> Option<RetouchTool> {
        match self {
            Self::Remove => None,
            Self::Clone => Some(RetouchTool::Clone),
            Self::Heal => Some(RetouchTool::Heal),
        }
    }

    pub(crate) const fn matches_stroke_tool(self, retouch: Option<RetouchTool>) -> bool {
        matches!(
            (self, retouch),
            (Self::Remove, None)
                | (Self::Clone, Some(RetouchTool::Clone))
                | (Self::Heal, Some(RetouchTool::Heal))
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AdjustmentSection {
    #[default]
    Light,
    ToneCurve,
    Color,
    ColorGrading,
    Detail,
    Effects,
    ColorMixer,
    Optics,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MaskSection {
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
pub(crate) enum ToneCurveTab {
    #[default]
    Rgb,
    Red,
    Green,
    Blue,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ColorGradeTab {
    Shadows,
    #[default]
    Midtones,
    Highlights,
    Global,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(usize)]
pub(crate) enum HslMixerColor {
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
    pub(crate) const ALL: [Self; 8] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Aqua,
        Self::Blue,
        Self::Purple,
        Self::Magenta,
    ];

    pub(crate) const fn index(self) -> usize {
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

pub(crate) struct LoadedPreview {
    source_path: Option<PathBuf>,
    raw_cache_key: String,
    label: String,
    original_raw: Arc<LoadedRaw>,
    full_raw: Arc<LoadedRaw>,
    preview_raw: Arc<LoadedRaw>,
    pipeline: RawGpuPipeline,
    rendered_exposure: ExposureParams,
    rendered_masks: MaskStack,
    remove: RemoveEditState,
    ai_masks_need_update: bool,
    mask_source: Option<MaskRgbImage>,
    lens_correction: LensCorrectionState,
    sidecar_target: crate::sidecar::SidecarTarget,
    sidecar_generation: u64,
    sidecar_warning: Option<String>,
    sidecar_needs_rewrite: bool,
    selected_camera_profile: Option<PathBuf>,
    geometry: GeometryTransform,
}

pub(crate) struct PreparedPreviewRebuild {
    source_raw: Arc<LoadedRaw>,
    preview_raw: Arc<LoadedRaw>,
    quality: PreviewQuality,
    requested_edge: u32,
    ai_enabled: bool,
}

pub(crate) enum PreviewRebuildEvent {
    Finished(Result<PreparedPreviewRebuild, String>),
}

pub(crate) struct PreparedPreviewDetail {
    source_raw: Arc<LoadedRaw>,
    revision: u64,
    quality: PreviewQuality,
    visible: PreviewUvRect,
    texture_uv_rect: PreviewUvRect,
    source_origin: [u32; 2],
    source_size: [u32; 2],
    raw: Arc<LoadedRaw>,
}

pub(crate) enum PreviewDetailRebuildEvent {
    Finished(Result<PreparedPreviewDetail, String>),
}

pub(crate) enum LoadEvent {
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
    Progress(String),
    Finished(Result<PreparedLensCorrection, String>),
}

#[derive(Clone)]
pub(crate) struct SidecarSaveRequest {
    target: crate::sidecar::SidecarTarget,
    generation: u64,
    revision: u64,
    explicit: bool,
    edits: SidecarEditState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SidecarSaveJob {
    generation: u64,
    revision: u64,
    explicit: bool,
}

pub(crate) struct SidecarSaveEvent {
    job: SidecarSaveJob,
    result: Result<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DevelopedThumbnailJob {
    target: crate::sidecar::SidecarTarget,
    generation: u64,
    revision: u64,
}

pub(crate) struct DevelopedThumbnailEvent {
    job: DevelopedThumbnailJob,
    result: Result<crate::pipeline::RawThumbnail, String>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SidecarAutosaveDeadline {
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
pub(crate) struct CachedRawDecode {
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
pub(crate) struct AndroidLibraryExportTarget {
    pub(crate) uri: String,
    pub(crate) display_name: String,
}

#[cfg(target_os = "android")]
impl AndroidLibraryExportTarget {
    fn display_name(&self) -> &str {
        &self.display_name
    }
}

#[cfg(target_os = "android")]
#[derive(Clone, Debug)]
struct LibraryBatchExportJob {
    target: AndroidLibraryExportTarget,
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
pub(crate) struct LibraryAiMaskRefreshState {
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
pub(crate) struct LibraryBatchExportState {
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
    },
    Progress {
        completed_tiles: usize,
        total_tiles: usize,
    },
    ItemFinished {
        error: Option<String>,
    },
    Finished {
        cancelled: bool,
        error: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportTaskKind {
    Single,
    LibraryBatch,
}

enum ExportTaskReceiver {
    Tiled(mpsc::Receiver<ExportEvent>),
    #[cfg(not(target_os = "android"))]
    LibraryBatch(mpsc::Receiver<LibraryBatchExportEvent>),
}

#[derive(Clone, Debug)]
enum ExportDestination {
    #[cfg(not(target_os = "android"))]
    File(PathBuf),
    #[cfg(target_os = "android")]
    AndroidDirect { path: PathBuf },
    #[cfg(target_os = "android")]
    AndroidGallery {
        path: PathBuf,
        display_name: String,
        format: ExportFormat,
    },
}

impl ExportDestination {
    fn path(&self) -> &Path {
        match self {
            #[cfg(not(target_os = "android"))]
            Self::File(path) => path,
            #[cfg(target_os = "android")]
            Self::AndroidDirect { path } | Self::AndroidGallery { path, .. } => path,
        }
    }
}

pub(crate) struct ExportTask {
    kind: ExportTaskKind,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
    receiver: Option<ExportTaskReceiver>,
    destination: Option<ExportDestination>,
    progress: f32,
    phase: String,
    completed: usize,
    total: usize,
    completed_tiles: usize,
    total_tiles: usize,
    minimized: bool,
    cancelling: bool,
}

struct PreparedExportSource {
    raw: Arc<LoadedRaw>,
    geometry: GeometryTransform,
    exposure: ExposureParams,
    masks: MaskStack,
    remove: RemoveEditState,
    source_file_name: Option<String>,
    gpu_export_prewarm: Option<Arc<GpuProgramPrewarm>>,
}

struct ExportItemRequest {
    device: wgpu::Device,
    queue: wgpu::Queue,
    source: PreparedExportSource,
    destination: ExportDestination,
    format: ExportFormat,
    settings: ExportSettings,
}

struct LensCorrectionTaskRequest {
    original_raw: Arc<LoadedRaw>,
    selection: Option<LensfunLens>,
    #[cfg(target_os = "android")]
    preview_quality: PreviewQuality,
    preview_proxy_edge: u32,
    cached_raws: Option<(Arc<LoadedRaw>, Arc<LoadedRaw>)>,
}

type GeneratedAiMaskTargets = (bool, VecDeque<(usize, usize)>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ForegroundOperationKind {
    SubjectMask,
    ObjectMask,
    AiDenoise,
    LensCorrection,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ForegroundProgressValue {
    Indeterminate,
    Units {
        completed: u64,
        total: u64,
        unit: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ForegroundProgress {
    pub(crate) value: ForegroundProgressValue,
    pub(crate) phase: String,
    pub(crate) detail: Option<String>,
}

impl ForegroundProgress {
    pub(crate) fn indeterminate(phase: impl Into<String>) -> Self {
        Self {
            value: ForegroundProgressValue::Indeterminate,
            phase: phase.into(),
            detail: None,
        }
    }

    pub(crate) fn units(
        completed: u64,
        total: u64,
        unit: impl Into<Option<String>>,
        phase: impl Into<String>,
    ) -> Self {
        Self {
            value: ForegroundProgressValue::Units {
                completed,
                total,
                unit: unit.into(),
            },
            phase: phase.into(),
            detail: None,
        }
    }

    pub(crate) fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

enum ForegroundOperationReceiver {
    Subject(mpsc::Receiver<SubjectMaskEvent>),
    Object(mpsc::Receiver<ObjectMaskEvent>),
    AiDenoise(mpsc::Receiver<crate::ai_denoise::AiDenoiseEvent>),
    LensCorrection(mpsc::Receiver<LensCorrectionEvent>),
}

enum ForegroundOperationContext {
    Subject,
    Object {
        target: AiMaskTarget,
        inference_started: bool,
    },
    AiDenoise,
    LensCorrection,
}

pub(crate) struct ForegroundOperation {
    kind: ForegroundOperationKind,
    document_id: u64,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
    progress: ForegroundProgress,
    cancelling: bool,
    receiver: ForegroundOperationReceiver,
    context: ForegroundOperationContext,
}

#[derive(Clone, Debug, PartialEq)]
struct AiMaskTarget {
    mask_index: usize,
    component_index: usize,
    kind: MaskKind,
    geometry: MaskGeometry,
}

pub(crate) struct DevelopState {
    pub(crate) current_path: Option<PathBuf>,
    pub(crate) original_raw: Option<Arc<LoadedRaw>>,
    pub(crate) loaded_raw: Option<Arc<LoadedRaw>>,
    pub(crate) preview_raw: Option<Arc<LoadedRaw>>,
    pub(crate) exposure: ExposureParams,
    pub(crate) target_exposure: ExposureParams,
    pub(crate) geometry: GeometryTransform,
    pub(crate) geometry_revision: u64,
    pub(crate) lens_correction: LensCorrectionState,
    pub(crate) lens_correction_dirty: bool,
    pub(crate) selected_camera_profile: Option<PathBuf>,
    pub(crate) load_receiver: Option<mpsc::Receiver<LoadEvent>>,
    pub(crate) loading_label: Option<String>,
    pub(crate) image_status: String,
    pub(crate) current_label: Option<String>,
    pub(crate) raw_cache: VecDeque<CachedRawDecode>,
    pub(crate) raw_cache_limit: usize,
}

pub(crate) struct PreviewState {
    pub(crate) gpu_pipeline: Option<RawGpuPipeline>,
    pub(crate) program_template: Option<RawGpuProgramTemplate>,
    pub(crate) retired_egui_textures: Vec<egui::TextureId>,
    pub(crate) gpu_prewarm_receiver: Option<mpsc::Receiver<Result<RawGpuPipeline, String>>>,
    pub(crate) quality: PreviewQuality,
    pub(crate) zoom: f32,
    pub(crate) center: [f32; 2],
    pub(crate) visible_uv: PreviewUvRect,
    pub(crate) viewport_pixels: [u32; 2],
    pub(crate) motion_at: Option<Instant>,
    pub(crate) touch_navigation_active: bool,
    pub(crate) revision: u64,
    pub(crate) detail: Option<PreviewDetail>,
    pub(crate) navigation: Option<PreviewNavigation>,
    pub(crate) detail_pending_stage: Option<ProcessingStage>,
    pub(crate) navigation_pending_stage: Option<ProcessingStage>,
    pub(crate) detail_urgent: bool,
    pub(crate) quality_dirty: bool,
    pub(crate) rebuild_receiver: Option<mpsc::Receiver<PreviewRebuildEvent>>,
    pub(crate) detail_rebuild_receiver: Option<mpsc::Receiver<PreviewDetailRebuildEvent>>,
    pub(crate) original_exposure: ExposureParams,
    pub(crate) original_requested: bool,
    pub(crate) original_rendered_state: Option<(bool, u64)>,
    #[cfg(target_os = "android")]
    pub(crate) original_hold: Option<AndroidOriginalHold>,
    pub(crate) pending_stage: Option<ProcessingStage>,
    #[cfg(target_os = "android")]
    pub(crate) lens_original_cache: Option<(PreviewQuality, Arc<LoadedRaw>)>,
    #[cfg(target_os = "android")]
    pub(crate) lens_corrected_cache:
        Option<(LensfunLens, PreviewQuality, Arc<LoadedRaw>, Arc<LoadedRaw>)>,
}

pub(crate) struct DevelopUiState {
    #[cfg(not(target_os = "android"))]
    pub(crate) reference: DevelopReferenceState,
    pub(crate) loading_thumbnail: DevelopLoadingThumbnailState,
    #[cfg(not(target_os = "android"))]
    pub(crate) filmstrip_open: bool,
    #[cfg(not(target_os = "android"))]
    pub(crate) filmstrip_centered_path: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    pub(crate) sidebar_open: bool,
    pub(crate) crop_constraint_reference: Option<[f32; 4]>,
    pub(crate) crop_drag: Option<CropDragState>,
    pub(crate) straighten_tool_active: bool,
    pub(crate) straighten_drag: Option<StraightenDragState>,
    pub(crate) white_balance_picker_active: bool,
    pub(crate) white_balance_picker_drag: Option<[[f32; 2]; 2]>,
    pub(crate) adjustment_section: AdjustmentSection,
    pub(crate) mask_section: MaskSection,
    pub(crate) tone_curve_tab: ToneCurveTab,
    pub(crate) color_grade_tab: ColorGradeTab,
    pub(crate) hsl_mixer_color: HslMixerColor,
}

pub(crate) struct PreferencesState {
    pub(crate) image_relative_brush_size: bool,
    pub(crate) show_develop_navigation_labels: bool,
    #[cfg(not(target_os = "android"))]
    pub(crate) discord_rich_presence: bool,
    pub(crate) ui_design: UiDesign,
    pub(crate) preview_backdrop: PreviewBackdrop,
    pub(crate) onboarding_completed: bool,
    pub(crate) adjustment_copy_settings: AdjustmentCopySettings,
    pub(crate) performance_settings_path: Option<PathBuf>,
    pub(crate) camera_profile_mode: CameraProfileMode,
    pub(crate) camera_profile_folder: Option<PathBuf>,
    pub(crate) camera_profile_folder_label: Option<String>,
    pub(crate) camera_profile_auto_detect: bool,
    pub(crate) last_camera_profile: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum OnboardingStep {
    Appearance,
    Preview,
    CopyPaste,
    #[cfg(not(target_os = "android"))]
    Ai,
}

pub(crate) struct UiState {
    pub(crate) active_tab: AppTab,
    pub(crate) sidebar_tab: SidebarTab,
    pub(crate) status: String,
    pub(crate) adaptive_preview_backdrop: egui::Color32,
    pub(crate) notice: Option<String>,
    pub(crate) onboarding_step: Option<OnboardingStep>,
    pub(crate) thumbnail_cache_size: Option<Result<u64, String>>,
    pub(crate) thumbnail_cache_size_receiver: Option<mpsc::Receiver<Result<u64, String>>>,
    #[cfg(not(target_os = "android"))]
    pub(crate) desktop_picker_receiver: Option<mpsc::Receiver<DesktopPickerEvent>>,
}

pub(crate) struct MaskState {
    pub(crate) stack: MaskStack,
    pub(crate) active_tool: Option<MaskKind>,
    pub(crate) brush_mode: BrushMode,
    pub(crate) subject_refinement_active: bool,
    pub(crate) drag: Option<MaskDragState>,
    pub(crate) last_brush_point: Option<[f32; 2]>,
    pub(crate) touch_gesture_backup: Option<MaskTouchGestureBackup>,
    pub(crate) interaction_dirty_layer: Option<usize>,
    pub(crate) interaction_last_upload: Option<Instant>,
    pub(crate) interaction_has_uncommitted_change: bool,
    pub(crate) overlay_revision: u64,
    pub(crate) overlay_texture: Option<egui::TextureHandle>,
    pub(crate) overlay_texture_key: Option<(usize, Option<usize>, u64, OverlayRasterKey)>,
    pub(crate) overlay_blink: Option<(std::time::Instant, MaskOverlayBlink)>,
    pub(crate) thumbnail_revision: u64,
    pub(crate) thumbnail_group_textures: Vec<egui::TextureHandle>,
    pub(crate) thumbnail_component_mask: Option<usize>,
    pub(crate) thumbnail_component_textures: Vec<egui::TextureHandle>,
    pub(crate) source_cache: Option<MaskRgbImage>,
    pub(crate) subject_cache: Option<MaskImage>,
    pub(crate) dirty_layers: [bool; MAX_LOCAL_MASKS],
    pub(crate) detail_dirty_layers: [bool; MAX_LOCAL_MASKS],
    pub(crate) navigation_dirty_layers: [bool; MAX_LOCAL_MASKS],
}

pub(crate) struct AiState {
    pub(crate) birefnet_quality: BiRefNetQuality,
    #[cfg(not(target_os = "android"))]
    pub(crate) gpu_acceleration: bool,
    pub(crate) masks_need_update: bool,
    pub(crate) mask_update_active: bool,
    pub(crate) mask_update_subject_pending: bool,
    pub(crate) mask_update_object_queue: VecDeque<(usize, usize)>,
    pub(crate) mask_update_failed: bool,
    #[cfg(not(target_os = "android"))]
    pub(crate) runtime_path: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    pub(crate) runtime_sha256: Option<String>,
    pub(crate) library_mask_refresh: Option<LibraryAiMaskRefreshState>,
    pub(crate) subject_consent_open: bool,
    pub(crate) object_consent_open: bool,
    pub(crate) object_pending_target: Option<(usize, usize)>,
    pub(crate) object_error_dialog: Option<String>,
    pub(crate) object_cache: Option<((usize, usize), ObjectInferenceCache)>,
    pub(crate) denoise_consent_open: bool,
    pub(crate) denoise_resume_pending: bool,
}

pub(crate) struct ExportState {
    pub(crate) gpu_prewarm: Option<Arc<GpuProgramPrewarm>>,
    pub(crate) settings: ExportSettings,
    pub(crate) task: Option<ExportTask>,
    pub(crate) batch: Option<LibraryBatchExportState>,
    pub(crate) publish_pending: bool,
    #[cfg(target_os = "android")]
    pub(crate) android_batch_load_pending: bool,
}

pub(crate) struct PersistenceState {
    pub(crate) history: EditHistory,
    pub(crate) lens_restore_masks: Option<MaskStack>,
    pub(crate) sidecar_target: Option<crate::sidecar::SidecarTarget>,
    pub(crate) sidecar_generation: u64,
    pub(crate) sidecar_saved_revision: Option<u64>,
    pub(crate) sidecar_failed_revision: Option<u64>,
    pub(crate) sidecar_pending: VecDeque<SidecarSaveRequest>,
    pub(crate) sidecar_in_flight: Option<SidecarSaveJob>,
    pub(crate) sidecar_receiver: Option<mpsc::Receiver<SidecarSaveEvent>>,
    pub(crate) sidecar_save_feedback_until: Option<Instant>,
    pub(crate) sidecar_save_error_dialog: Option<String>,
    pub(crate) sidecar_autosave_deadline: Option<SidecarAutosaveDeadline>,
    pub(crate) developed_thumbnail_pending: Option<DevelopedThumbnailJob>,
    pub(crate) developed_thumbnail_in_flight: Option<DevelopedThumbnailJob>,
    pub(crate) developed_thumbnail_receiver: Option<mpsc::Receiver<DevelopedThumbnailEvent>>,
}

pub(crate) struct InpaintState {
    pub(crate) tool: InpaintTool,
    pub(crate) brush_size: f32,
    pub(crate) brush_hardness: f32,
    pub(crate) brush_opacity: f32,
    pub(crate) alignment: RetouchAlignment,
    pub(crate) source_point: Option<[f32; 2]>,
    pub(crate) source_pick_active: bool,
    pub(crate) aligned_offset: Option<[f32; 2]>,
    pub(crate) edits: Arc<RemoveEditState>,
    pub(crate) active_points: Vec<RemoveBrushPoint>,
    pub(crate) last_brush_uv: Option<[f32; 2]>,
    pub(crate) pending_brush: Option<RemoveBrushStroke>,
    pub(crate) pending_retouch: Option<RetouchStroke>,
    pub(crate) model_consent_open: bool,
    pub(crate) receiver: Option<mpsc::Receiver<RemoveEvent>>,
    pub(crate) cancellation: Option<Arc<AtomicBool>>,
    pub(crate) processing_label: Option<String>,
    pub(crate) hovered_stroke: Option<usize>,
    pub(crate) selected_stroke: Option<usize>,
    pub(crate) stroke_opacity_edit_pending: bool,
}

#[cfg(target_os = "android")]
pub(crate) struct AndroidState {
    pub(crate) android_app: calibraw_ffi::AndroidApp,
    pub(crate) picker_pending: bool,
    pub(crate) pending_android_library_reset_reload: bool,
    pub(crate) camera_profile_folder_importing_label: Option<String>,
    pub(crate) pending_android_profile_reload: Option<(Option<PathBuf>, SidecarEditState)>,
}

pub struct CalibRawApp {
    pub(crate) develop: DevelopState,
    pub(crate) preview: PreviewState,
    pub(crate) develop_ui: DevelopUiState,
    pub(crate) library: LibraryState,
    pub(crate) masks: MaskState,
    pub(crate) ai: AiState,
    pub(crate) inpaint: InpaintState,
    pub(crate) export: ExportState,
    pub(crate) persistence: PersistenceState,
    pub(crate) preferences: PreferencesState,
    pub(crate) ui: UiState,
    #[cfg(not(target_os = "android"))]
    discord_presence: DiscordPresence,
    egui_ctx: egui::Context,
    foreground_operation: Option<ForegroundOperation>,
    #[cfg(target_os = "android")]
    pub(crate) android: AndroidState,
}

#[cfg(test)]
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

impl CalibRawApp {
    pub(crate) fn sync_ai_model_runtime_context(&mut self) {
        let context = if self.ui.active_tab == AppTab::Develop {
            match self.ui.sidebar_tab {
                SidebarTab::Masks => Some(calibraw_ai::AiRuntimeContext::Masks),
                SidebarTab::Inpainting => Some(calibraw_ai::AiRuntimeContext::Remove),
                SidebarTab::Adjustments | SidebarTab::Crop | SidebarTab::Export => None,
            }
        } else {
            None
        };
        calibraw_ai::set_active_ai_context(context);

        if context != Some(calibraw_ai::AiRuntimeContext::Remove)
            && self.inpaint.cancellation.is_some()
        {
            self.cancel_remove_processing();
        }

        if context != Some(calibraw_ai::AiRuntimeContext::Masks)
            && self.ai.library_mask_refresh.is_none()
            && matches!(
                self.foreground_operation_kind(),
                Some(ForegroundOperationKind::SubjectMask | ForegroundOperationKind::ObjectMask)
            )
        {
            self.cancel_foreground_operation();
        }
    }

    pub(crate) fn activate_tab(&mut self, tab: AppTab) {
        if self.ui.active_tab == tab {
            return;
        }
        if self.ui.active_tab == AppTab::Develop && tab != AppTab::Develop {
            self.set_original_preview_requested(false);
            self.clear_android_original_hold();
        }
        if self.ui.active_tab == AppTab::Library && tab != AppTab::Library {
            self.library.prepare_for_develop();
            #[cfg(target_os = "android")]
            self.library.set_folder_sidebar_open(false);
        }
        if tab == AppTab::Settings {
            self.ui.thumbnail_cache_size = None;
            self.ui.thumbnail_cache_size_receiver = None;
        }
        self.ui.active_tab = tab;
        self.sync_ai_model_runtime_context();
        #[cfg(target_os = "android")]
        crate::android::set_back_navigation_active(tab != AppTab::Library);
    }

    #[cfg(target_os = "android")]
    fn clear_android_original_hold(&mut self) {
        self.preview.original_hold = None;
    }

    #[cfg(not(target_os = "android"))]
    fn clear_android_original_hold(&mut self) {}

    fn retire_egui_texture(&mut self, texture_id: egui::TextureId) {
        if !self.preview.retired_egui_textures.contains(&texture_id) {
            self.preview.retired_egui_textures.push(texture_id);
        }
        self.egui_ctx.request_repaint();
    }

    fn release_retired_egui_textures(&mut self, frame: &eframe::Frame) {
        if self.preview.retired_egui_textures.is_empty() {
            return;
        }
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let retired = std::mem::take(&mut self.preview.retired_egui_textures);
        let mut renderer = render_state.renderer.write();
        for texture_id in retired {
            renderer.free_texture(&texture_id);
        }
    }

    fn take_preview_pipeline_and_release_textures(
        &mut self,
        _renderer: &mut eframe::egui_wgpu::Renderer,
    ) -> Option<RawGpuPipeline> {
        let pipeline = self.preview.gpu_pipeline.take();
        if let Some(pipeline) = pipeline.as_ref() {
            self.preview.program_template = Some(pipeline.program_template());
        }
        if let Some(texture_id) = pipeline
            .as_ref()
            .and_then(|pipeline| pipeline.egui_texture_id)
        {
            self.retire_egui_texture(texture_id);
        }
        for texture_id in [
            self.preview
                .detail
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
            self.preview
                .navigation
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
        crate::android::copy_text_to_clipboard(&self.android.android_app, label, text)
    }
}

mod eframe_impl;
mod foreground;
mod inpainting;
mod library_adjustments;
mod lifecycle;
mod masks_ai;
mod processing_export;
mod sidecar_persistence;

#[cfg(not(target_os = "android"))]
use lifecycle::install_missing_range_sources;
use lifecycle::needs_canonical_mask_source;
use sidecar_persistence::sidecar_interaction_active;

#[cfg(test)]
mod transactional_pipeline_tests {
    use super::{
        collect_pipeline_update_results, AiMaskTarget, MaskGeometry, MaskKind, MaskStack,
        MaskState, PreviewQuality,
    };
    use crate::pipeline::GeometryTransform;

    #[test]
    #[cfg(not(target_os = "android"))]
    fn preview_quality_levels_track_physical_viewport_density() {
        assert_eq!(
            PreviewQuality::Max.proxy_edge_for_viewport([3_000, 2_000]),
            4_506
        );
        assert!(PreviewQuality::Max.detail_edge_for_viewport([3_200, 1_800]) >= 3_200 * 2);
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
    fn preview_quality_density_is_ordered_and_medium_matches_physical_pixels() {
        let viewport = [2_400, 1_600];
        let edges = [
            PreviewQuality::Low.proxy_edge_for_viewport(viewport),
            PreviewQuality::Medium.proxy_edge_for_viewport(viewport),
            PreviewQuality::High.proxy_edge_for_viewport(viewport),
            PreviewQuality::Max.proxy_edge_for_viewport(viewport),
        ];
        assert!(edges.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(edges[1], 2_406);
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
                "install mask atlas",
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
            assert!(message.contains("install mask atlas"));
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
            MaskState::resolve_ai_target_in_stack(&stack, &target),
            Ok((0, 1))
        );

        stack.masks[0].components[1].kind = MaskKind::Brush;
        stack.masks[0].components[1].geometry = MaskGeometry::for_kind(MaskKind::Brush);
        let error = MaskState::resolve_ai_target_in_stack(&stack, &target).unwrap_err();
        assert!(error.contains("changed type"));
    }
}
