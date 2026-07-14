use crate::ai_masks::{spawn_subject_mask, SubjectMaskEvent, BIREFNET_MODEL_BYTES};
use crate::pipeline::{
    affected_stage, build_proxy, load_raw_file, spawn_tiled_png_export, BrushMode, ExportEvent,
    ExportMetadata, ExportSettings, ExposureParams, GpuParams, LoadedRaw, MaskImage, MaskKind,
    MaskRgbImage, MaskStack, ProcessingQuality, ProcessingStage, ProxySpec, RawGpuPipeline,
    TileSpec, MAX_LOCAL_MASKS,
};
use crate::ui::components::adjustment_slider::slider_scroll_locked;
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
pub enum AdjustmentSection {
    #[default]
    Light,
    ToneCurve,
    Color,
    ColorGrading,
    Effects,
    ColorMixer,
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
    mask_interaction_dirty_layer: Option<usize>,
    mask_interaction_frame_count: u8,
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

include!("app/lifecycle.rs");
include!("app/masks_ai.rs");
include!("app/processing_export.rs");
include!("app/eframe_impl.rs");
