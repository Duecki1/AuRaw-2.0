use crate::app::{AppTab, AurawApp};
#[cfg(not(target_os = "android"))]
use crate::pipeline::{
    apply_lensfun_correction, build_proxy, is_supported_raw_path,
    lensfun_catalog, load_raw_display_dimensions, load_raw_file_with_profile_selection,
    load_raw_thumbnail, mask_atlas_edge, GpuParams, LensfunLens, MaskGeometry, MaskRgbImage,
    MaskStack, ProcessingQuality, ProxySpec, RawGpuPipeline, MAX_LOCAL_MASKS,
};
use crate::pipeline::{ExportFormat, ExportSettings, RawThumbnail};
use eframe::egui::{self, Align2, Color32, FontId, Sense, Stroke, StrokeKind, Ui};
use std::cmp::Ordering as CmpOrdering;
use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(not(target_os = "android"))]
use std::ffi::OsString;
use std::fs;
#[cfg(not(target_os = "android"))]
use std::fs::OpenOptions;
#[cfg(not(target_os = "android"))]
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(not(target_os = "android"))]
use std::sync::OnceLock;
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

mod actions;
mod adjustments;
mod catalog;
mod clipboard;
mod dialogs;
mod export;
mod local;
mod platform;
mod state;
mod storage;
mod thumbnails;
mod view;

use actions::*;
use adjustments::*;
use catalog::*;
use clipboard::*;
use dialogs::*;
use export::*;
use platform::*;
use storage::*;
use thumbnails::*;

pub use view::Library;
#[cfg(not(target_os = "android"))]
pub(crate) use actions::{
    apply_library_action, library_image_context_menu, show_library_action_overlays,
};
#[cfg(not(target_os = "android"))]
pub(crate) use thumbnails::{load_desktop_cached_thumbnail, load_desktop_reference_preview};

const THUMBNAIL_EDGE: u32 = 512;
const MAX_LIBRARY_FILES: usize = 20_000;
const MAX_EVENTS_PER_FRAME: usize = 12;
const MAX_PENDING_THUMBNAIL_RESULTS: usize = 32;
const MAX_PENDING_THUMBNAILS: usize = 64;
const DESKTOP_TEXTURE_CACHE_LIMIT: usize = 128;
const ANDROID_TEXTURE_CACHE_LIMIT: usize = 48;
const RESIDENT_THUMBNAIL_EDGE: u32 = 384;
const DESKTOP_RESIDENT_THUMBNAIL_CACHE_LIMIT: usize = 256;
const ANDROID_RESIDENT_THUMBNAIL_CACHE_LIMIT: usize = 72;
#[cfg(target_os = "android")]
const ANDROID_DEVELOP_TEXTURE_CACHE_LIMIT: usize = 10;
const THUMBNAIL_PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(20);
const THUMBNAIL_QUEUE_POLL_INTERVAL: Duration = Duration::from_millis(8);
const THUMBNAIL_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
#[cfg(not(target_os = "android"))]
const DEVELOPED_THUMBNAIL_PROXY_EDGE: u32 = 1024;
pub(crate) const MAX_DESKTOP_THUMBNAIL_WORKERS: usize = 8;
pub(crate) const MAX_ANDROID_THUMBNAIL_WORKERS: usize = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LibrarySortOrder {
    #[default]
    NewestFirst,
    OldestFirst,
    NameAscending,
    NameDescending,
    LargestFirst,
    SmallestFirst,
}

impl LibrarySortOrder {
    const ALL: [Self; 6] = [
        Self::NewestFirst,
        Self::OldestFirst,
        Self::NameAscending,
        Self::NameDescending,
        Self::LargestFirst,
        Self::SmallestFirst,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::NewestFirst => "Newest first",
            Self::OldestFirst => "Oldest first",
            Self::NameAscending => "Name A–Z",
            Self::NameDescending => "Name Z–A",
            Self::LargestFirst => "Largest first",
            Self::SmallestFirst => "Smallest first",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LibraryThumbnailSize {
    Small,
    #[default]
    Medium,
    Large,
    Enormous,
}

impl LibraryThumbnailSize {
    const ALL: [Self; 4] = [Self::Small, Self::Medium, Self::Large, Self::Enormous];

    const fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
            Self::Enormous => "Extra large",
        }
    }

    const fn scale(self) -> f32 {
        match self {
            Self::Small => 1.0,
            Self::Medium => 1.25,
            Self::Large => 1.5,
            Self::Enormous => 1.75,
        }
    }
}

pub(crate) fn default_thumbnail_worker_count() -> usize {
    platform::default_thumbnail_worker_count()
}

pub(crate) fn maximum_thumbnail_worker_count() -> usize {
    platform::maximum_thumbnail_worker_count()
}

/// Stable identity for a Library asset. Identity is intentionally storage-shaped,
/// while all selection and action logic operates on this common type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum LibraryAssetId {
    #[cfg(not(target_os = "android"))]
    Desktop(PathBuf),
    #[cfg(target_os = "android")]
    Android(String),
}

/// Opaque location understood only by the active platform storage backend.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LibraryLocator {
    #[cfg(not(target_os = "android"))]
    Desktop(PathBuf),
    #[cfg(target_os = "android")]
    Android { uri: String },
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LibraryAssetMetadata {
    pub(crate) bytes: u64,
    pub(crate) dimensions_hint: Option<[u32; 2]>,
    /// Unix timestamp seconds when the backend can provide one. Zero means unknown.
    pub(crate) modified_seconds: u64,
}

/// One asset model shared by desktop and Android Library code.
#[derive(Clone, Debug)]
pub(crate) struct LibraryAsset {
    pub(crate) id: LibraryAssetId,
    pub(crate) display_name: String,
    pub(crate) display_path: String,
    pub(crate) locator: LibraryLocator,
    pub(crate) metadata: LibraryAssetMetadata,
}

impl LibraryAsset {
    #[cfg(not(target_os = "android"))]
    pub(crate) fn from_desktop_path(
        path: PathBuf,
        bytes: u64,
        modified_seconds: u64,
        dimensions_hint: Option<[u32; 2]>,
    ) -> Self {
        let display_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        Self {
            id: LibraryAssetId::Desktop(path.clone()),
            display_path: path.display().to_string(),
            display_name,
            locator: LibraryLocator::Desktop(path),
            metadata: LibraryAssetMetadata {
                bytes,
                dimensions_hint,
                modified_seconds,
            },
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn from_android_document(document: crate::android::LibraryDocument) -> Self {
        Self {
            id: LibraryAssetId::Android(document.uri.clone()),
            display_name: document.display_name,
            display_path: document.display_path,
            locator: LibraryLocator::Android { uri: document.uri },
            metadata: LibraryAssetMetadata {
                bytes: document.bytes,
                dimensions_hint: None,
                modified_seconds: document.modified_seconds,
            },
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn desktop_path(&self) -> Option<&Path> {
        match &self.locator {
            LibraryLocator::Desktop(path) => Some(path.as_path()),
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn android_uri(&self) -> Option<&str> {
        match &self.locator {
            LibraryLocator::Android { uri } => Some(uri.as_str()),
        }
    }
}

fn library_import_fab_rect(bounds: egui::Rect) -> egui::Rect {
    crate::ui::theme::floating_action_rect(bounds)
}

fn library_import_icon() -> &'static str {
    egui_phosphor::regular::PLUS
}

pub(crate) struct LibraryEntry {
    asset: LibraryAsset,
    texture: Option<egui::TextureHandle>,
    resident_thumbnail: Option<RawThumbnail>,
    texture_is_resident: bool,
    thumbnail_size: Option<[u32; 2]>,
    layout_size: Option<[u32; 2]>,
    thumbnail_error: Option<String>,
    thumbnail_failures: u8,
    thumbnail_retry_after: Option<Instant>,
    thumbnail_queued: bool,
    developed_thumbnail: bool,
    // The card is deliberately showing the camera/RAW preview while the
    // adjusted replacement is rendered by a thumbnail worker.
    developed_thumbnail_pending: bool,
    last_used: u64,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone)]
pub(crate) struct DesktopFilmstripItem {
    pub(crate) asset: LibraryAsset,
    pub(crate) path: PathBuf,
    pub(crate) texture: Option<egui::TextureHandle>,
    pub(crate) thumbnail_size: Option<[u32; 2]>,
    pub(crate) developed_thumbnail_pending: bool,
}

struct LoadedLibraryThumbnail {
    thumbnail: RawThumbnail,
    resident_thumbnail: RawThumbnail,
    developed: bool,
    developed_render_pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThumbnailLoadStage {
    RawPreview,
    DevelopedPreview,
}

#[derive(Clone)]
struct ThumbnailRequest {
    generation: u64,
    asset_id: LibraryAssetId,
    display_priority: bool,
    stage: ThumbnailLoadStage,
}

struct ThumbnailWorkQueue {
    background: VecDeque<ThumbnailRequest>,
    in_flight: HashMap<LibraryAssetId, bool>,
    initial_completed: HashSet<LibraryAssetId>,
    developed_queued: HashSet<LibraryAssetId>,
}

/// Catalog-wide worker progress. Per-card state still drives placeholders and
/// retries; this separate model gives the rest of the application one compact
/// background-task signal, just like a minimized export.
#[derive(Default)]
struct ThumbnailBackgroundProgress {
    generation: u64,
    total: usize,
    completed_assets: HashSet<LibraryAssetId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ThumbnailProgress {
    pub(crate) completed: usize,
    pub(crate) total: usize,
    pub(crate) paused: bool,
}

impl ThumbnailBackgroundProgress {
    fn begin(&mut self, generation: u64, total: usize) {
        self.generation = generation;
        self.total = total;
        self.completed_assets.clear();
    }

    fn record_completion(&mut self, generation: u64, asset_id: LibraryAssetId) {
        if self.generation == generation {
            self.completed_assets.insert(asset_id);
        }
    }

    fn snapshot(&self, paused: bool) -> Option<ThumbnailProgress> {
        let completed = self.completed_assets.len().min(self.total);
        (self.total > 0 && completed < self.total).then_some(ThumbnailProgress {
            completed,
            total: self.total,
            paused,
        })
    }
}

impl ThumbnailWorkQueue {
    fn new(generation: u64, assets: &[LibraryAsset]) -> Self {
        Self {
            background: assets
                .iter()
                .map(|asset| ThumbnailRequest {
                    generation,
                    asset_id: asset.id.clone(),
                    display_priority: false,
                    stage: ThumbnailLoadStage::RawPreview,
                })
                .collect(),
            in_flight: HashMap::new(),
            initial_completed: HashSet::new(),
            developed_queued: HashSet::new(),
        }
    }

    fn claim(&mut self, request: &ThumbnailRequest, initial_background: bool) -> bool {
        if initial_background
            && request.stage == ThumbnailLoadStage::RawPreview
            && self.initial_completed.contains(&request.asset_id)
        {
            return false;
        }
        if let Some(display_priority) = self.in_flight.get_mut(&request.asset_id) {
            *display_priority |= request.display_priority;
            return false;
        }
        self.in_flight
            .insert(request.asset_id.clone(), request.display_priority);
        true
    }

    fn finish(&mut self, request: &ThumbnailRequest) -> bool {
        if request.stage == ThumbnailLoadStage::RawPreview {
            self.initial_completed.insert(request.asset_id.clone());
        } else {
            self.developed_queued.remove(&request.asset_id);
        }
        self.in_flight.remove(&request.asset_id).unwrap_or(false)
    }

    fn schedule_developed_preview(&mut self, generation: u64, asset_id: LibraryAssetId) {
        if self.developed_queued.insert(asset_id.clone()) {
            self.background.push_back(ThumbnailRequest {
                generation,
                asset_id,
                // The embedded RAW preview was already delivered. Rendering
                // edits remains ordinary background work so Develop keeps
                // priority over it.
                display_priority: false,
                stage: ThumbnailLoadStage::DevelopedPreview,
            });
        }
    }
}

enum ScanEvent {
    Catalog {
        generation: u64,
        assets: Vec<LibraryAsset>,
        warning_count: usize,
        truncated: bool,
    },
    #[cfg(not(target_os = "android"))]
    FolderTree {
        generation: u64,
        tree: LibraryFolderNode,
    },
    #[cfg(target_os = "android")]
    AndroidFolders {
        generation: u64,
        folders: Vec<crate::android::LibraryFolder>,
    },
    Thumbnail {
        generation: u64,
        asset_id: LibraryAssetId,
        display_priority: bool,
        final_thumbnail: bool,
        result: Result<LoadedLibraryThumbnail, String>,
    },
    Failed {
        generation: u64,
        error: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageClipboardMode {
    Copy,
    Cut,
}

#[derive(Clone, Debug)]
struct ImageClipboard {
    mode: ImageClipboardMode,
    assets: Vec<LibraryAsset>,
}

impl ImageClipboard {
    fn count(&self) -> usize {
        self.assets.len()
    }

    fn paste_label(&self) -> String {
        let count = self.count();
        format!("Paste {count} RAW{}", if count == 1 { "" } else { "s" })
    }
}

#[derive(Clone, Debug)]
enum LibraryTransferDestination {
    #[cfg(not(target_os = "android"))]
    LocalFolder(PathBuf),
    #[cfg(target_os = "android")]
    LocalLibrary { path: String },
}

struct AssetTransferCompletion {
    result: Result<String, String>,
    clear_clipboard: bool,
    remaining_clipboard: Option<ImageClipboard>,
}

struct ThumbnailWorker {
    assets: Vec<LibraryAsset>,
    warning_count: usize,
    truncated: bool,
    generation: u64,
    cancellation: Arc<AtomicU64>,
    decoding_paused: Arc<AtomicBool>,
    decode_gate: Arc<RwLock<()>>,
    event_sender: mpsc::SyncSender<ScanEvent>,
    request_receiver: mpsc::Receiver<ThumbnailRequest>,
    repaint: egui::Context,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone)]
struct LibraryExportDialog {
    assets: Vec<LibraryAsset>,
    settings: ExportSettings,
    format: ExportFormat,
}

#[cfg(target_os = "android")]
#[derive(Clone)]
struct LibraryExportDialog {
    assets: Vec<LibraryAsset>,
    settings: ExportSettings,
    format: ExportFormat,
}

#[derive(Clone)]
struct LibraryAdjustmentPasteDialog {
    assets: Vec<LibraryAsset>,
    edited_count: usize,
}

#[derive(Clone)]
struct LibraryAiMaskRefreshPrompt {
    assets: Vec<LibraryAsset>,
}

#[cfg(not(target_os = "android"))]
struct RawImportResult {
    imported: Vec<PathBuf>,
    imported_folders: Vec<PathBuf>,
    already_present: usize,
    ignored: usize,
    failures: Vec<String>,
}

#[cfg(not(target_os = "android"))]
enum RawImportOutcome {
    Imported(PathBuf),
    AlreadyPresent,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LibraryFolderClipboardMode {
    Copy,
    Cut,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct LibraryFolderClipboard {
    path: PathBuf,
    mode: LibraryFolderClipboardMode,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct LibraryFolderDrag(PathBuf);

#[cfg(not(target_os = "android"))]
enum LibraryFolderOperation {
    Create {
        root: PathBuf,
        parent: PathBuf,
        name: String,
    },
    Copy {
        root: PathBuf,
        source: PathBuf,
        destination_parent: PathBuf,
    },
    Move {
        root: PathBuf,
        source: PathBuf,
        destination_parent: PathBuf,
        new_name: Option<String>,
    },
    Delete {
        root: PathBuf,
        target: PathBuf,
    },
}

#[cfg(not(target_os = "android"))]
enum LibraryFolderOperationResult {
    Created(PathBuf),
    Copied { source: PathBuf, destination: PathBuf },
    Moved { source: PathBuf, destination: PathBuf },
    Deleted(PathBuf),
}

#[cfg(not(target_os = "android"))]
struct LibraryFolderOperationCompletion {
    root: PathBuf,
    result: Result<LibraryFolderOperationResult, String>,
}

#[cfg(not(target_os = "android"))]
enum LibraryFolderUiAction {
    New(PathBuf),
    Copy(PathBuf),
    Cut(PathBuf),
    Paste(PathBuf),
    PasteImages(PathBuf),
    Rename(PathBuf),
    Delete(PathBuf),
    Move {
        source: PathBuf,
        destination_parent: PathBuf,
    },
    Refresh,
}

#[cfg(not(target_os = "android"))]
enum LibraryFolderNameDialogKind {
    Create { parent: PathBuf },
    Rename { source: PathBuf },
}

#[cfg(not(target_os = "android"))]
struct LibraryFolderNameDialog {
    kind: LibraryFolderNameDialogKind,
    name: String,
    error: Option<String>,
}

struct LibraryRawNameDialog {
    asset: LibraryAsset,
    name: String,
    error: Option<String>,
    focus_requested: bool,
}

#[cfg(target_os = "android")]
struct AndroidLibraryFolderNameDialog {
    parent: String,
    name: String,
    error: Option<String>,
    focus_requested: bool,
}

#[cfg(target_os = "android")]
struct PlatformLibraryState {
    app: auraw_ffi::AndroidApp,
    root_location: String,
    folder: String,
    folders: Vec<crate::android::LibraryFolder>,
    expanded_folders: HashSet<String>,
    folder_name_dialog: Option<AndroidLibraryFolderNameDialog>,
}

#[cfg(target_os = "android")]
enum AndroidLibraryFolderUiAction {
    Select(String),
    New(String),
    Refresh,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone, Debug, PartialEq, Eq)]
struct LibraryFolderNode {
    path: PathBuf,
    name: String,
    children: Vec<Self>,
}

#[cfg(not(target_os = "android"))]
impl LibraryFolderNode {
    fn empty(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| path.display().to_string());
        Self {
            path,
            name,
            children: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LibraryAdjustmentClipboard {
    pub(crate) edits: crate::sidecar::EditState,
    pub(crate) settings: crate::sidecar::AdjustmentCopySettings,
}

pub(crate) struct LibraryState {
    location: Option<String>,
    #[cfg(not(target_os = "android"))]
    folder: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    root_folder: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    folder_tree: Option<LibraryFolderNode>,
    #[cfg(not(target_os = "android"))]
    expanded_folders: HashSet<PathBuf>,
    folder_sidebar_open: bool,
    #[cfg(target_os = "android")]
    platform: PlatformLibraryState,
    entries: Vec<LibraryEntry>,
    entry_indices: HashMap<LibraryAssetId, usize>,
    event_receiver: Option<mpsc::Receiver<ScanEvent>>,
    request_sender: Option<mpsc::SyncSender<ThumbnailRequest>>,
    generation: Arc<AtomicU64>,
    decoding_paused: Arc<AtomicBool>,
    decode_gate: Arc<RwLock<()>>,
    thumbnail_progress: ThumbnailBackgroundProgress,
    scanning: bool,
    catalog_ready: bool,
    status: String,
    usage_clock: u64,
    thumbnail_workers: usize,
    sort_order: LibrarySortOrder,
    thumbnail_size: LibraryThumbnailSize,
    selected_assets: HashSet<LibraryAssetId>,
    selection_mode: bool,
    image_clipboard: Option<ImageClipboard>,
    pub(crate) adjustment_clipboard: Option<LibraryAdjustmentClipboard>,
    asset_transfer_receiver: Option<mpsc::Receiver<AssetTransferCompletion>>,
    #[cfg(not(target_os = "android"))]
    raw_import_receiver: Option<mpsc::Receiver<RawImportResult>>,
    #[cfg(not(target_os = "android"))]
    folder_operation_receiver: Option<mpsc::Receiver<LibraryFolderOperationCompletion>>,
    #[cfg(not(target_os = "android"))]
    folder_clipboard: Option<LibraryFolderClipboard>,
    #[cfg(not(target_os = "android"))]
    folder_name_dialog: Option<LibraryFolderNameDialog>,
    #[cfg(not(target_os = "android"))]
    folder_delete_confirmation: Option<PathBuf>,
    raw_name_dialog: Option<LibraryRawNameDialog>,
    export_dialog: Option<LibraryExportDialog>,
    adjustment_paste_dialog: Option<LibraryAdjustmentPasteDialog>,
    ai_mask_refresh_prompt: Option<LibraryAiMaskRefreshPrompt>,
}

#[cfg(test)]
mod tests;
