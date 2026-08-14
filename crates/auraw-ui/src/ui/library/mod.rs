use crate::app::{AppTab, AurawApp};
#[cfg(not(target_os = "android"))]
use crate::pipeline::{
    apply_lensfun_correction, build_proxy, compose_inpaint_strokes, is_supported_raw_path,
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
#[cfg(not(target_os = "android"))]
use std::fs::{self, OpenOptions};
#[cfg(not(target_os = "android"))]
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(not(target_os = "android"))]
use std::sync::OnceLock;
use std::sync::{mpsc, Arc, Mutex, RwLock};
#[cfg(not(target_os = "android"))]
use std::time::SystemTime;
use std::time::{Duration, Instant};

mod actions;
mod adjustments;
mod catalog;
mod clipboard;
mod cloud;
mod cloud_view;
mod dialogs;
mod export;
mod local;
mod platform;
mod state;
mod thumbnails;
mod trash;
mod view;

use actions::*;
use adjustments::*;
use catalog::*;
use clipboard::*;
use cloud::*;
use cloud_view::*;
use dialogs::*;
use export::*;
use platform::*;
use thumbnails::*;
use trash::*;

pub use view::Library;
#[cfg(not(target_os = "android"))]
pub(crate) use actions::{
    apply_cloud_image_action, apply_desktop_image_action, cloud_image_context_menu,
    desktop_image_context_menu, show_desktop_image_action_overlays, LibraryCardAction,
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
const CLOUD_DOWNLOAD_PROGRESS_STEP: u64 = 2 * 1024 * 1024;
const MAX_CLOUD_UPLOAD_FILES: usize = 256;
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
            // Small deliberately preserves AuRaw's previous gallery size.
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum LibrarySource {
    #[cfg(not(target_os = "android"))]
    File(PathBuf),
    #[cfg(target_os = "android")]
    Android {
        uri: String,
        display_name: String,
        bytes: u64,
        modified_seconds: u64,
    },
    Cloud(crate::cloud::CloudAsset),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LibraryView {
    #[default]
    Local,
    Cloud,
}

fn library_import_fab_rect(bounds: egui::Rect) -> egui::Rect {
    crate::ui::theme::floating_action_rect(bounds)
}

fn library_import_icon() -> &'static str {
    egui_phosphor::regular::PLUS
}

fn cloud_cache_icon(downloaded: bool) -> &'static str {
    if downloaded {
        egui_phosphor::regular::DOWNLOAD
    } else {
        egui_phosphor::regular::CLOUD
    }
}

fn cloud_sync_badge(
    state: crate::cloud::CloudSyncState,
    downloaded: bool,
) -> (&'static str, Color32, &'static str) {
    match state {
        crate::cloud::CloudSyncState::Synced => (
            cloud_cache_icon(downloaded),
            Color32::WHITE,
            if downloaded {
                "Synced · available offline"
            } else {
                "Synced · cloud only"
            },
        ),
        crate::cloud::CloudSyncState::Queued => (
            egui_phosphor::regular::ARROW_CLOCKWISE,
            Color32::from_rgb(245, 190, 55),
            "Queued for cloud sync",
        ),
        crate::cloud::CloudSyncState::Failed => (
            egui_phosphor::regular::X,
            Color32::from_rgb(240, 78, 78),
            "Cloud sync failed",
        ),
        crate::cloud::CloudSyncState::Conflict => (
            egui_phosphor::regular::INTERSECT,
            Color32::from_rgb(240, 78, 78),
            "Cloud edit conflict",
        ),
    }
}

fn cloud_preview_notice(kind: crate::cloud::CloudThumbnailKind) -> Option<&'static str> {
    match kind {
        crate::cloud::CloudThumbnailKind::Edited => None,
        crate::cloud::CloudThumbnailKind::Placeholder => {
            Some("Temporary unedited RAW preview · full preview is rendering")
        }
        crate::cloud::CloudThumbnailKind::Raw => Some("Unedited RAW preview"),
        crate::cloud::CloudThumbnailKind::Legacy => {
            Some("Legacy preview · edit rendering is unknown")
        }
    }
}

fn cloud_preview_label(kind: crate::cloud::CloudThumbnailKind) -> Option<&'static str> {
    match kind {
        crate::cloud::CloudThumbnailKind::Edited | crate::cloud::CloudThumbnailKind::Legacy => None,
        crate::cloud::CloudThumbnailKind::Placeholder => Some("PREVIEW RENDERING"),
        crate::cloud::CloudThumbnailKind::Raw => Some("UNEDITED PREVIEW"),
    }
}

fn cloud_preview_icon(kind: crate::cloud::CloudThumbnailKind) -> Option<&'static str> {
    matches!(kind, crate::cloud::CloudThumbnailKind::Legacy)
        .then_some(egui_phosphor::regular::CLOCK_COUNTER_CLOCKWISE)
}

#[derive(Clone, Debug)]
struct LibraryFileInfo {
    source: LibrarySource,
    display_path: String,
    name: String,
    bytes: u64,
    dimensions_hint: Option<[u32; 2]>,
    cloud_downloaded: bool,
    cloud_sync_state: crate::cloud::CloudSyncState,
    #[cfg(not(target_os = "android"))]
    modified: Option<SystemTime>,
}

pub(crate) struct LibraryEntry {
    info: LibraryFileInfo,
    texture: Option<egui::TextureHandle>,
    /// Small CPU-side fallback retained across GPU texture eviction so scrolling
    /// back to a previously loaded card can repaint immediately without showing
    /// the loading placeholder or waiting behind newer decode requests.
    resident_thumbnail: Option<RawThumbnail>,
    /// True only when `texture` was rebuilt from the smaller resident fallback.
    /// The card stays visible while a full-resolution refresh is queued.
    texture_is_resident: bool,
    /// Exact decoded preview dimensions, when preview pixels are available.
    thumbnail_size: Option<[u32; 2]>,
    /// Stable geometry used by the justified gallery. This is filled from RAW
    /// header metadata before the catalog is shown and never replaced merely
    /// because a preview texture finishes loading.
    layout_size: Option<[u32; 2]>,
    thumbnail_error: Option<String>,
    thumbnail_failures: u8,
    thumbnail_retry_after: Option<Instant>,
    thumbnail_queued: bool,
    developed_thumbnail: bool,
    last_used: u64,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone)]
pub(crate) enum DesktopFilmstripSource {
    Local(PathBuf),
    Cloud(crate::cloud::CloudAsset),
}

#[cfg(not(target_os = "android"))]
#[derive(Clone)]
pub(crate) struct DesktopFilmstripItem {
    pub(crate) source: DesktopFilmstripSource,
    /// The local RAW path when this item is available on disk. Cloud items
    /// retain their server identity even before this becomes available.
    pub(crate) path: Option<PathBuf>,
    pub(crate) identity: String,
    pub(crate) name: String,
    pub(crate) texture: Option<egui::TextureHandle>,
    pub(crate) thumbnail_size: Option<[u32; 2]>,
}

struct LoadedLibraryThumbnail {
    thumbnail: RawThumbnail,
    resident_thumbnail: RawThumbnail,
    developed: bool,
}

#[derive(Clone)]
struct ThumbnailRequest {
    generation: u64,
    source: LibrarySource,
    display_priority: bool,
}

struct ThumbnailWorkQueue {
    background: VecDeque<ThumbnailRequest>,
    in_flight: HashMap<LibrarySource, bool>,
    initial_completed: HashSet<LibrarySource>,
}

impl ThumbnailWorkQueue {
    fn new(generation: u64, files: &[LibraryFileInfo]) -> Self {
        Self {
            background: files
                .iter()
                .map(|file| ThumbnailRequest {
                    generation,
                    source: file.source.clone(),
                    display_priority: false,
                })
                .collect(),
            in_flight: HashMap::new(),
            initial_completed: HashSet::new(),
        }
    }

    fn claim(&mut self, request: &ThumbnailRequest, initial_background: bool) -> bool {
        if initial_background && self.initial_completed.contains(&request.source) {
            return false;
        }
        if let Some(display_priority) = self.in_flight.get_mut(&request.source) {
            *display_priority |= request.display_priority;
            return false;
        }
        self.in_flight
            .insert(request.source.clone(), request.display_priority);
        true
    }

    fn finish(&mut self, source: &LibrarySource) -> bool {
        self.initial_completed.insert(source.clone());
        self.in_flight.remove(source).unwrap_or(false)
    }
}

enum ScanEvent {
    CloudAvailability {
        generation: u64,
        offline_reason: Option<String>,
        folders: Vec<crate::cloud::CloudFolder>,
        folder_id: String,
        asset_folders: HashMap<String, String>,
    },
    Catalog {
        generation: u64,
        files: Vec<LibraryFileInfo>,
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
        source: LibrarySource,
        display_priority: bool,
        result: Result<LoadedLibraryThumbnail, String>,
    },
    Failed {
        generation: u64,
        error: String,
    },
}

enum CloudOpenEvent {
    Progress { downloaded: u64, total: u64 },
    Finished(Result<crate::cloud::CachedCloudAsset, String>),
}

enum CloudUploadEvent {
    Progress {
        position: usize,
        total: usize,
        label: String,
    },
    Finished {
        target: crate::cloud::CloudConfig,
        uploaded: usize,
        failed: usize,
        errors: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CloudClipboardMode {
    Copy,
    Cut,
}

#[derive(Clone, Debug)]
enum CloudClipboardContent {
    Folder(crate::cloud::CloudFolder),
}

#[derive(Clone, Debug)]
struct CloudClipboard {
    mode: CloudClipboardMode,
    content: CloudClipboardContent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageClipboardMode {
    Copy,
    Cut,
}

#[cfg(target_os = "android")]
#[derive(Clone, Debug)]
struct AndroidImageClipboardItem {
    uri: String,
    display_name: String,
    bytes: u64,
}

#[derive(Clone, Debug)]
enum ImageClipboardContent {
    #[cfg(not(target_os = "android"))]
    Local(Vec<PathBuf>),
    #[cfg(target_os = "android")]
    Local(Vec<AndroidImageClipboardItem>),
    Cloud(Vec<crate::cloud::CloudAsset>),
}

#[derive(Clone, Debug)]
struct ImageClipboard {
    mode: ImageClipboardMode,
    content: ImageClipboardContent,
}

impl ImageClipboard {
    fn count(&self) -> usize {
        match &self.content {
            ImageClipboardContent::Local(items) => items.len(),
            ImageClipboardContent::Cloud(items) => items.len(),
        }
    }

    fn paste_label(&self) -> String {
        let count = self.count();
        format!("Paste {count} RAW{}", if count == 1 { "" } else { "s" })
    }
}

#[derive(Clone, Debug)]
enum ImagePasteDestination {
    #[cfg(not(target_os = "android"))]
    LocalFolder(PathBuf),
    #[cfg(target_os = "android")]
    LocalLibrary,
    CloudFolder(String),
}

struct ImagePasteCompletion {
    result: Result<String, String>,
    clear_clipboard: bool,
    remaining_clipboard: Option<ImageClipboard>,
}

#[derive(Clone, Copy, Debug)]
enum CloudPreparedPurpose {
    Export,
    CopyAdjustments,
    PasteAdjustments,
}

#[derive(Clone, Debug)]
enum CloudActionRequest {
    CreateFolder {
        parent_id: String,
        name: String,
    },
    UpdateFolder {
        folder: crate::cloud::CloudFolder,
        parent_id: String,
        name: String,
        clear_clipboard: bool,
    },
    CopyFolder {
        folder: crate::cloud::CloudFolder,
        destination_parent_id: String,
        clear_clipboard: bool,
    },
    DeleteFolder {
        folder: crate::cloud::CloudFolder,
    },
    CopyAssets {
        assets: Vec<crate::cloud::CloudAsset>,
        destination_folder_id: String,
        clear_clipboard: bool,
    },
    RenameAsset {
        asset: crate::cloud::CloudAsset,
        name: String,
    },
    DeleteAssets {
        assets: Vec<crate::cloud::CloudAsset>,
    },
    RestoreTrash {
        items: Vec<crate::cloud::CloudTrashItem>,
    },
    PermanentlyDeleteTrash {
        items: Vec<crate::cloud::CloudTrashItem>,
    },
    EmptyTrash,
    ResetAssets {
        assets: Vec<crate::cloud::CloudAsset>,
    },
    PrepareAssets {
        assets: Vec<crate::cloud::CloudAsset>,
        purpose: CloudPreparedPurpose,
    },
}

enum CloudActionCompletion {
    Mutation {
        result: Result<String, String>,
        clear_clipboard: bool,
    },
    Prepared {
        purpose: CloudPreparedPurpose,
        result: Result<Vec<crate::cloud::CachedCloudAsset>, String>,
    },
}

#[derive(Clone, Debug)]
enum CloudNameDialogKind {
    CreateFolder { parent_id: String },
    RenameFolder { folder: crate::cloud::CloudFolder },
    RenameAsset { asset: crate::cloud::CloudAsset },
}

#[derive(Clone, Debug)]
struct CloudNameDialog {
    kind: CloudNameDialogKind,
    name: String,
    error: Option<String>,
    focus_requested: bool,
}

#[derive(Clone, Debug)]
enum CloudTrashDeleteTarget {
    Selected(Vec<crate::cloud::CloudTrashItem>),
    Empty,
}

#[derive(Clone, Debug)]
enum CloudDeleteTarget {
    Folder(crate::cloud::CloudFolder),
    Assets(Vec<crate::cloud::CloudAsset>),
}

#[derive(Clone, Debug)]
pub(crate) enum CloudLibraryCardAction {
    Export(Vec<crate::cloud::CloudAsset>),
    CopyAdjustments(crate::cloud::CloudAsset),
    PasteAdjustments(Vec<crate::cloud::CloudAsset>),
    Copy(Vec<crate::cloud::CloudAsset>),
    Cut(Vec<crate::cloud::CloudAsset>),
    Duplicate(Vec<crate::cloud::CloudAsset>),
    Rename(crate::cloud::CloudAsset),
    ResetAdjustments(Vec<crate::cloud::CloudAsset>),
    Delete(Vec<crate::cloud::CloudAsset>),
}

struct ThumbnailWorker {
    files: Vec<LibraryFileInfo>,
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
    paths: Vec<PathBuf>,
    settings: ExportSettings,
    format: ExportFormat,
}

#[cfg(target_os = "android")]
#[derive(Clone)]
struct LibraryExportDialog {
    targets: Vec<crate::app::AndroidLibraryExportTarget>,
    settings: ExportSettings,
    format: ExportFormat,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone)]
struct LibraryAdjustmentPasteDialog {
    paths: Vec<PathBuf>,
    edited_count: usize,
}

#[cfg(target_os = "android")]
#[derive(Clone)]
enum AndroidAdjustmentPasteTargets {
    Local(Vec<(String, String)>),
    Cloud(Vec<PathBuf>),
}

#[cfg(target_os = "android")]
impl AndroidAdjustmentPasteTargets {
    fn len(&self) -> usize {
        match self {
            Self::Local(targets) => targets.len(),
            Self::Cloud(paths) => paths.len(),
        }
    }
}

#[cfg(target_os = "android")]
#[derive(Clone)]
struct LibraryAdjustmentPasteDialog {
    targets: AndroidAdjustmentPasteTargets,
    edited_count: usize,
}

#[cfg(not(target_os = "android"))]
#[derive(Clone)]
struct LibraryAiMaskRefreshPrompt {
    paths: Vec<PathBuf>,
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
#[derive(Clone, Debug)]
struct CloudFolderDrag(String);

enum CloudFolderUiAction {
    Select(String),
    New(String),
    Copy(crate::cloud::CloudFolder),
    Cut(crate::cloud::CloudFolder),
    Paste(String),
    Rename(crate::cloud::CloudFolder),
    Delete(crate::cloud::CloudFolder),
    Move {
        folder: crate::cloud::CloudFolder,
        destination_parent_id: String,
    },
    Refresh,
}

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
    Copied {
        source: PathBuf,
        destination: PathBuf,
    },
    Moved {
        source: PathBuf,
        destination: PathBuf,
    },
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

#[cfg(not(target_os = "android"))]
struct LibraryRawNameDialog {
    source: PathBuf,
    name: String,
    error: Option<String>,
    focus_requested: bool,
}

#[cfg(target_os = "android")]
struct AndroidLibraryRawNameDialog {
    source: AndroidImageClipboardItem,
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

#[cfg(target_os = "android")]
#[derive(Clone)]
struct LibraryAiMaskRefreshPrompt {
    targets: Vec<(String, String)>,
}

pub(crate) struct LibraryState {
    location: Option<String>,
    local_location: Option<String>,
    view: LibraryView,
    cloud_config: crate::cloud::CloudConfig,
    cloud_cache_root: Option<PathBuf>,
    cloud_offline_reason: Option<String>,
    cloud_connection_receiver: Option<mpsc::Receiver<Result<String, String>>>,
    cloud_connection_status: Option<Result<String, String>>,
    cloud_open_receiver: Option<mpsc::Receiver<CloudOpenEvent>>,
    cloud_open_label: Option<String>,
    cloud_upload_receiver: Option<mpsc::Receiver<CloudUploadEvent>>,
    cloud_upload_completion: Option<String>,
    cloud_folders: Vec<crate::cloud::CloudFolder>,
    cloud_asset_folders: HashMap<String, String>,
    cloud_folder_id: String,
    cloud_expanded_folders: HashSet<String>,
    cloud_action_receiver: Option<mpsc::Receiver<CloudActionCompletion>>,
    cloud_clipboard: Option<CloudClipboard>,
    image_clipboard: Option<ImageClipboard>,
    image_paste_receiver: Option<mpsc::Receiver<ImagePasteCompletion>>,
    cloud_name_dialog: Option<CloudNameDialog>,
    cloud_delete_confirmation: Option<CloudDeleteTarget>,
    cloud_trash_open: bool,
    cloud_trash_items: Vec<crate::cloud::CloudTrashItem>,
    cloud_trash_server_time: u64,
    cloud_trash_retention_days: u32,
    cloud_trash_receiver: Option<mpsc::Receiver<Result<crate::cloud::CloudTrashCatalog, String>>>,
    cloud_trash_selection: HashSet<String>,
    cloud_trash_delete_confirmation: Option<CloudTrashDeleteTarget>,
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
    android_app: auraw_ffi::AndroidApp,
    #[cfg(target_os = "android")]
    android_root_location: String,
    #[cfg(target_os = "android")]
    android_folder: String,
    #[cfg(target_os = "android")]
    android_folders: Vec<crate::android::LibraryFolder>,
    #[cfg(target_os = "android")]
    android_expanded_folders: HashSet<String>,
    entries: Vec<LibraryEntry>,
    entry_indices: HashMap<LibrarySource, usize>,
    event_receiver: Option<mpsc::Receiver<ScanEvent>>,
    request_sender: Option<mpsc::SyncSender<ThumbnailRequest>>,
    generation: Arc<AtomicU64>,
    decoding_paused: Arc<AtomicBool>,
    decode_gate: Arc<RwLock<()>>,
    scanning: bool,
    catalog_ready: bool,
    status: String,
    usage_clock: u64,
    thumbnail_workers: usize,
    sort_order: LibrarySortOrder,
    thumbnail_size: LibraryThumbnailSize,
    selected_sources: HashSet<LibrarySource>,
    selection_mode: bool,
    #[cfg(not(target_os = "android"))]
    file_action_receiver: Option<mpsc::Receiver<Result<Vec<PathBuf>, String>>>,
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
    #[cfg(not(target_os = "android"))]
    raw_name_dialog: Option<LibraryRawNameDialog>,
    #[cfg(target_os = "android")]
    android_raw_name_dialog: Option<AndroidLibraryRawNameDialog>,
    #[cfg(target_os = "android")]
    android_folder_name_dialog: Option<AndroidLibraryFolderNameDialog>,
    export_dialog: Option<LibraryExportDialog>,
    adjustment_paste_dialog: Option<LibraryAdjustmentPasteDialog>,
    ai_mask_refresh_prompt: Option<LibraryAiMaskRefreshPrompt>,
}


#[cfg(test)]
mod tests;
