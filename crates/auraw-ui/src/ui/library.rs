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
#[cfg(not(target_os = "android"))]
use std::collections::BinaryHeap;
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
    if cfg!(target_os = "android") {
        1
    } else {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(2)
            .clamp(1, 4)
    }
}

pub(crate) fn maximum_thumbnail_worker_count() -> usize {
    if cfg!(target_os = "android") {
        MAX_ANDROID_THUMBNAIL_WORKERS
    } else {
        MAX_DESKTOP_THUMBNAIL_WORKERS
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(target_os = "android", test))]
enum TouchThumbnailAction {
    Open,
    SelectionChanged { back_navigation_active: bool },
}

#[cfg(any(not(target_os = "android"), test))]
fn desktop_selection_toggle_label(selection_mode: bool) -> &'static str {
    if selection_mode {
        "Cancel"
    } else {
        "Select"
    }
}

const LIBRARY_IMPORT_FAB_EDGE: f32 = 56.0;

fn library_import_fab_rect(bounds: egui::Rect) -> egui::Rect {
    let size = egui::vec2(LIBRARY_IMPORT_FAB_EDGE, LIBRARY_IMPORT_FAB_EDGE);
    egui::Rect::from_min_size(bounds.right_bottom() - size, size)
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
enum CloudLibraryCardAction {
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

#[cfg(any(target_os = "android", test))]
fn android_library_location_label(root: &str, folder: &str) -> String {
    if folder.is_empty() {
        root.to_owned()
    } else {
        format!("{root}/{folder}")
    }
}

#[cfg(any(target_os = "android", test))]
fn android_folder_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

#[cfg(any(target_os = "android", test))]
fn android_folder_ancestors(path: &str) -> HashSet<String> {
    let mut expanded = HashSet::from([String::new()]);
    let mut current = path.to_owned();
    while !current.is_empty() {
        let parent = android_folder_parent(&current).to_owned();
        expanded.insert(parent.clone());
        current = parent;
    }
    expanded
}

fn cloud_batch_summary(
    verb: &str,
    total: usize,
    completed: usize,
    errors: Vec<String>,
) -> Result<String, String> {
    let noun = if total == 1 { "RAW" } else { "RAWs" };
    if errors.is_empty() {
        Ok(format!("{verb} {completed} cloud {noun}."))
    } else {
        Err(format!(
            "{verb} {completed} of {total} cloud {noun}. {}",
            errors.join(" · ")
        ))
    }
}

fn image_paste_summary(
    mode: ImageClipboardMode,
    total: usize,
    completed: usize,
    destination: &str,
    errors: Vec<String>,
) -> Result<String, String> {
    let verb = if mode == ImageClipboardMode::Copy {
        "Copied"
    } else {
        "Moved"
    };
    let noun = if total == 1 { "RAW" } else { "RAWs" };
    if errors.is_empty() {
        Ok(format!("{verb} {completed} {noun} to {destination}."))
    } else {
        Err(format!(
            "{verb} {completed} of {total} {noun} to {destination}. {}",
            errors.join(" · ")
        ))
    }
}

fn run_image_paste(
    config: &crate::cloud::CloudConfig,
    cache_root: Option<&Path>,
    allow_network: bool,
    clipboard: ImageClipboard,
    destination: ImagePasteDestination,
    #[cfg(target_os = "android")] android_app: &auraw_ffi::AndroidApp,
) -> ImagePasteCompletion {
    let mode = clipboard.mode;
    // A Cut can succeed for only part of a multi-selection. Keep a private
    // copy and remove each item only after its complete move has committed so
    // retrying the paste never acts on sources that already moved.
    let mut remaining_cut_clipboard = (mode == ImageClipboardMode::Cut).then(|| clipboard.clone());
    let result = match (clipboard.content, destination) {
        #[cfg(not(target_os = "android"))]
        (ImageClipboardContent::Local(paths), ImagePasteDestination::LocalFolder(folder)) => {
            let total = paths.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for path in paths {
                let result = if mode == ImageClipboardMode::Cut
                    && path.parent() == Some(folder.as_path())
                {
                    Ok(())
                } else {
                    let name = path
                        .file_name()
                        .ok_or_else(|| format!("{} has no usable filename", path.display()));
                    name.and_then(|name| {
                        copy_raw_bundle_to_folder(&path, name, &folder).and_then(|destination| {
                            if mode == ImageClipboardMode::Cut {
                                if let Err(error) = remove_local_raw_bundle(&path) {
                                    let _ = remove_local_raw_bundle(&destination);
                                    return Err(error);
                                }
                            }
                            Ok(())
                        })
                    })
                };
                match result {
                    Ok(_) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Local(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate != &path);
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                }
            }
            image_paste_summary(
                mode,
                total,
                completed,
                &folder.display().to_string(),
                errors,
            )
        }
        #[cfg(not(target_os = "android"))]
        (ImageClipboardContent::Local(paths), ImagePasteDestination::CloudFolder(folder_id)) => {
            let total = paths.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for path in paths {
                let label = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("local RAW")
                    .to_owned();
                let result = crate::cloud::upload_asset_path_to_folder(config, &path, &folder_id)
                    .and_then(|uploaded| {
                        if mode == ImageClipboardMode::Cut {
                            if let Err(error) = remove_local_raw_bundle(&path) {
                                let rollback = crate::cloud::delete_asset(config, &uploaded);
                                return Err(if let Err(rollback) = rollback {
                                    format!("{error} The uploaded rollback also failed: {rollback}")
                                } else {
                                    error
                                });
                            }
                        }
                        Ok(())
                    });
                match result {
                    Ok(()) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Local(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate != &path);
                        }
                    }
                    Err(error) => errors.push(format!("{label}: {error}")),
                }
            }
            image_paste_summary(mode, total, completed, "AuRaw Cloud", errors)
        }
        #[cfg(target_os = "android")]
        (ImageClipboardContent::Local(items), ImagePasteDestination::LocalLibrary) => {
            let total = items.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for item in items {
                let result = if mode == ImageClipboardMode::Cut {
                    Ok(())
                } else {
                    crate::android::duplicate_library_document(
                        android_app,
                        &item.uri,
                        &item.display_name,
                    )
                    .map(|_| ())
                };
                match result {
                    Ok(()) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Local(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate.uri != item.uri);
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", item.display_name)),
                }
            }
            image_paste_summary(mode, total, completed, "the local library", errors)
        }
        #[cfg(target_os = "android")]
        (ImageClipboardContent::Local(items), ImagePasteDestination::CloudFolder(folder_id)) => {
            let total = items.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for item in items {
                let staged_sidecar = crate::android::materialize_raw_sidecar(
                    android_app,
                    &item.uri,
                    &item.display_name,
                );
                let result = staged_sidecar.and_then(|staged_sidecar| {
                    let developed_thumbnail = if staged_sidecar.is_some() {
                        crate::android::developed_thumbnail_cache_file(
                            android_app,
                            &item.uri,
                            &item.display_name,
                        )
                    } else {
                        Ok(None)
                    }?;
                    let upload = (|| {
                        let raw =
                            crate::android::open_document_for_cloud_upload(android_app, &item.uri)?;
                        crate::cloud::upload_asset_file_with_sidecar_and_thumbnail_to_folder(
                            config,
                            raw,
                            &item.display_name,
                            Some(item.bytes),
                            staged_sidecar.as_deref(),
                            developed_thumbnail.as_deref(),
                            &folder_id,
                        )
                    })();
                    if let Some(path) = staged_sidecar.as_deref() {
                        let _ = std::fs::remove_file(path);
                    }
                    upload.and_then(|uploaded| {
                        if mode == ImageClipboardMode::Cut {
                            if let Err(error) = crate::android::delete_library_document(
                                android_app,
                                &item.uri,
                                &item.display_name,
                            ) {
                                let rollback = crate::cloud::delete_asset(config, &uploaded);
                                return Err(if let Err(rollback) = rollback {
                                    format!("{error} The uploaded rollback also failed: {rollback}")
                                } else {
                                    error
                                });
                            }
                        }
                        Ok(())
                    })
                });
                match result {
                    Ok(()) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Local(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate.uri != item.uri);
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", item.display_name)),
                }
            }
            image_paste_summary(mode, total, completed, "AuRaw Cloud", errors)
        }
        (ImageClipboardContent::Cloud(assets), ImagePasteDestination::CloudFolder(folder_id)) => {
            let total = assets.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for asset in assets {
                let result = if mode == ImageClipboardMode::Copy {
                    crate::cloud::copy_asset(config, &asset, &folder_id).map(|_| ())
                } else {
                    crate::cloud::update_asset(config, &asset, &folder_id, &asset.name).map(|_| ())
                };
                match result {
                    Ok(()) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Cloud(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate.id != asset.id);
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", asset.name)),
                }
            }
            image_paste_summary(mode, total, completed, "AuRaw Cloud", errors)
        }
        #[cfg(not(target_os = "android"))]
        (ImageClipboardContent::Cloud(assets), ImagePasteDestination::LocalFolder(folder)) => {
            let total = assets.len();
            let result = (|| {
                let cache_root = cache_root
                    .ok_or_else(|| "AuRaw could not locate its private cloud cache.".to_owned())?;
                let cached = crate::cloud::open_assets(config, cache_root, &assets, allow_network)?;
                let mut completed = 0usize;
                let mut errors = Vec::new();
                for (asset, cached) in assets.iter().zip(cached) {
                    let copied = copy_raw_bundle_to_folder(
                        &cached.raw_path,
                        std::ffi::OsStr::new(&asset.name),
                        &folder,
                    )
                    .and_then(|destination| {
                        let destination_sidecar =
                            crate::sidecar::sidecar_path_for_raw(&destination);
                        let has_developed_thumbnail =
                            crate::sidecar::developed_thumbnail_cache_is_fresh(&destination)?;
                        if destination_sidecar.is_file() && !has_developed_thumbnail {
                            let thumbnail = crate::cloud::load_thumbnail(
                                config,
                                cache_root,
                                asset,
                                THUMBNAIL_EDGE,
                                allow_network,
                            )?;
                            let fingerprint =
                                crate::sidecar::desktop_sidecar_fingerprint(&destination)?
                                    .ok_or_else(|| {
                                        "The copied cloud sidecar disappeared before its thumbnail was saved."
                                            .to_owned()
                                    })?;
                            if let Err(error) = crate::sidecar::save_developed_thumbnail_cache(
                                &destination,
                                &thumbnail,
                                fingerprint,
                            ) {
                                let _ = remove_local_raw_bundle(&destination);
                                return Err(error);
                            }
                        }
                        if mode == ImageClipboardMode::Cut {
                            if let Err(error) = crate::cloud::delete_asset(config, asset) {
                                let _ = remove_local_raw_bundle(&destination);
                                return Err(error);
                            }
                        }
                        Ok(())
                    });
                    match copied {
                        Ok(()) => {
                            completed += 1;
                            if let Some(ImageClipboard {
                                content: ImageClipboardContent::Cloud(remaining),
                                ..
                            }) = remaining_cut_clipboard.as_mut()
                            {
                                remaining.retain(|candidate| candidate.id != asset.id);
                            }
                        }
                        Err(error) => errors.push(format!("{}: {error}", asset.name)),
                    }
                }
                image_paste_summary(
                    mode,
                    total,
                    completed,
                    &folder.display().to_string(),
                    errors,
                )
            })();
            result
        }
        #[cfg(target_os = "android")]
        (ImageClipboardContent::Cloud(assets), ImagePasteDestination::LocalLibrary) => {
            let total = assets.len();
            let result = (|| {
                let cache_root = cache_root
                    .ok_or_else(|| "AuRaw could not locate its private cloud cache.".to_owned())?;
                let cached = crate::cloud::open_assets(config, cache_root, &assets, allow_network)?;
                let mut completed = 0usize;
                let mut errors = Vec::new();
                for (asset, cached) in assets.iter().zip(cached) {
                    let thumbnail =
                        if crate::sidecar::sidecar_path_for_raw(&cached.raw_path).is_file() {
                            crate::cloud::load_thumbnail(
                                config,
                                cache_root,
                                asset,
                                THUMBNAIL_EDGE,
                                allow_network,
                            )
                            .map(Some)
                        } else {
                            Ok(None)
                        };
                    let thumbnail = match thumbnail {
                        Ok(thumbnail) => thumbnail,
                        Err(error) => {
                            errors.push(format!("{}: {error}", asset.name));
                            continue;
                        }
                    };
                    let copied = crate::android::import_cached_library_document(
                        android_app,
                        &cached.raw_path,
                        &asset.name,
                    )
                    .and_then(|imported| {
                        if let Some(thumbnail) = thumbnail.as_ref() {
                            if let Err(error) = crate::android::save_developed_thumbnail_cache(
                                android_app,
                                &imported.uri,
                                &imported.display_name,
                                thumbnail,
                            ) {
                                let rollback = crate::android::delete_imported_library_document(
                                    android_app,
                                    &imported.uri,
                                    &imported.display_name,
                                );
                                return Err(if let Err(rollback) = rollback {
                                    format!(
                                        "{error} The imported-copy rollback also failed: {rollback}"
                                    )
                                } else {
                                    error
                                });
                            }
                        }
                        if mode == ImageClipboardMode::Cut {
                            if let Err(error) = crate::cloud::delete_asset(config, asset) {
                                let rollback = crate::android::delete_imported_library_document(
                                    android_app,
                                    &imported.uri,
                                    &imported.display_name,
                                );
                                return Err(if let Err(rollback) = rollback {
                                    format!(
                                        "{error} The imported-copy rollback also failed: {rollback}"
                                    )
                                } else {
                                    error
                                });
                            }
                        }
                        Ok(())
                    });
                    match copied {
                        Ok(()) => {
                            completed += 1;
                            if let Some(ImageClipboard {
                                content: ImageClipboardContent::Cloud(remaining),
                                ..
                            }) = remaining_cut_clipboard.as_mut()
                            {
                                remaining.retain(|candidate| candidate.id != asset.id);
                            }
                        }
                        Err(error) => errors.push(format!("{}: {error}", asset.name)),
                    }
                }
                image_paste_summary(mode, total, completed, "the local library", errors)
            })();
            result
        }
    };
    let clear_clipboard = remaining_cut_clipboard
        .as_ref()
        .is_some_and(|clipboard| clipboard.count() == 0);
    let remaining_clipboard = remaining_cut_clipboard.filter(|clipboard| clipboard.count() > 0);
    ImagePasteCompletion {
        result,
        clear_clipboard,
        remaining_clipboard,
    }
}

fn cloud_folder_id_for_catalog(
    requested_folder_id: &str,
    folders: &[crate::cloud::CloudFolder],
) -> String {
    if requested_folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID
        || folders
            .iter()
            .any(|folder| folder.id == requested_folder_id)
    {
        requested_folder_id.to_owned()
    } else {
        crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned()
    }
}

fn initial_cloud_expanded_folders() -> HashSet<String> {
    HashSet::from([crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned()])
}

fn run_cloud_action(
    config: &crate::cloud::CloudConfig,
    cache_root: Option<&Path>,
    allow_network: bool,
    request: CloudActionRequest,
) -> CloudActionCompletion {
    match request {
        CloudActionRequest::CreateFolder { parent_id, name } => {
            let result = crate::cloud::create_folder(config, &parent_id, &name)
                .map(|folder| format!("Created cloud folder {}.", folder.name));
            CloudActionCompletion::Mutation {
                result,
                clear_clipboard: false,
            }
        }
        CloudActionRequest::UpdateFolder {
            folder,
            parent_id,
            name,
            clear_clipboard,
        } => {
            let result = crate::cloud::update_folder(config, &folder, &parent_id, &name)
                .map(|updated| format!("Updated cloud folder {}.", updated.name));
            CloudActionCompletion::Mutation {
                clear_clipboard: clear_clipboard && result.is_ok(),
                result,
            }
        }
        CloudActionRequest::CopyFolder {
            folder,
            destination_parent_id,
            clear_clipboard,
        } => {
            let result = crate::cloud::copy_folder(config, &folder, &destination_parent_id)
                .map(|copied| format!("Copied cloud folder as {}.", copied.name));
            CloudActionCompletion::Mutation {
                clear_clipboard: clear_clipboard && result.is_ok(),
                result,
            }
        }
        CloudActionRequest::DeleteFolder { folder } => CloudActionCompletion::Mutation {
            result: crate::cloud::delete_folder(config, &folder.id)
                .map(|()| format!("Deleted cloud folder {}.", folder.name)),
            clear_clipboard: false,
        },
        CloudActionRequest::CopyAssets {
            assets,
            destination_folder_id,
            clear_clipboard,
        } => {
            let total = assets.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for asset in assets {
                match crate::cloud::copy_asset(config, &asset, &destination_folder_id) {
                    Ok(_) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", asset.name)),
                }
            }
            let result = cloud_batch_summary("Copied", total, completed, errors);
            CloudActionCompletion::Mutation {
                clear_clipboard: clear_clipboard && result.is_ok(),
                result,
            }
        }
        CloudActionRequest::RenameAsset { asset, name } => {
            let result = crate::cloud::update_asset(config, &asset, &asset.folder_id, &name)
                .map(|updated| format!("Renamed cloud RAW to {}.", updated.name));
            CloudActionCompletion::Mutation {
                result,
                clear_clipboard: false,
            }
        }
        CloudActionRequest::DeleteAssets { assets } => {
            let total = assets.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for asset in assets {
                match crate::cloud::delete_asset(config, &asset) {
                    Ok(()) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", asset.name)),
                }
            }
            CloudActionCompletion::Mutation {
                result: cloud_batch_summary("Deleted", total, completed, errors),
                clear_clipboard: false,
            }
        }
        CloudActionRequest::RestoreTrash { items } => {
            let total = items.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for item in items {
                match crate::cloud::restore_trash_item(config, &item, None) {
                    Ok(_) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", item.name)),
                }
            }
            CloudActionCompletion::Mutation {
                result: cloud_batch_summary("Restored", total, completed, errors),
                clear_clipboard: false,
            }
        }
        CloudActionRequest::PermanentlyDeleteTrash { items } => {
            let total = items.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for item in items {
                match crate::cloud::permanently_delete_trash_item(config, &item) {
                    Ok(()) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", item.name)),
                }
            }
            CloudActionCompletion::Mutation {
                result: cloud_batch_summary("Permanently deleted", total, completed, errors),
                clear_clipboard: false,
            }
        }
        CloudActionRequest::EmptyTrash => CloudActionCompletion::Mutation {
            result: crate::cloud::empty_trash(config)
                .map(|()| "Emptied AuRaw Cloud Trash.".to_owned()),
            clear_clipboard: false,
        },
        CloudActionRequest::ResetAssets { assets } => {
            let total = assets.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for asset in assets {
                match crate::cloud::reset_asset_sidecar(config, &asset) {
                    Ok(()) => completed += 1,
                    Err(error) => errors.push(format!("{}: {error}", asset.name)),
                }
            }
            CloudActionCompletion::Mutation {
                result: cloud_batch_summary("Reset adjustments for", total, completed, errors),
                clear_clipboard: false,
            }
        }
        CloudActionRequest::PrepareAssets { assets, purpose } => {
            let result = (|| {
                let cache_root = cache_root
                    .ok_or_else(|| "AuRaw could not locate its private cloud cache.".to_owned())?;
                crate::cloud::open_assets(config, cache_root, &assets, allow_network)
            })();
            CloudActionCompletion::Prepared { purpose, result }
        }
    }
}

impl LibraryState {
    /// Refreshes one cached cloud asset after a sidecar/thumbnail worker has
    /// changed its local sync metadata. This avoids a catalog rescan and keeps
    /// queued, failed, conflict, and synced badges live.
    pub(crate) fn update_cloud_sync_state_for_cached_raw(
        &mut self,
        raw_path: &Path,
        context: &egui::Context,
    ) {
        let Some((asset_id, state)) = crate::cloud::cached_asset_sync_state(raw_path) else {
            return;
        };
        let mut changed = false;
        for entry in &mut self.entries {
            let LibrarySource::Cloud(asset) = &entry.info.source else {
                continue;
            };
            if asset.id == asset_id && entry.info.cloud_sync_state != state {
                entry.info.cloud_sync_state = state;
                changed = true;
            }
        }
        if changed {
            context.request_repaint();
        }
    }

    #[cfg(all(not(target_os = "android"), test))]
    pub(crate) fn new(context: &egui::Context) -> Self {
        Self::new_with_workers(
            context,
            default_thumbnail_worker_count(),
            LibraryThumbnailSize::default(),
        )
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn new_with_workers(
        context: &egui::Context,
        workers: usize,
        thumbnail_size: LibraryThumbnailSize,
    ) -> Self {
        Self::new_desktop_with_preferences(
            context,
            workers,
            thumbnail_size,
            LibrarySortOrder::default(),
            true,
        )
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn new_desktop_with_preferences(
        context: &egui::Context,
        workers: usize,
        thumbnail_size: LibraryThumbnailSize,
        sort_order: LibrarySortOrder,
        folder_sidebar_open: bool,
    ) -> Self {
        let _ = context;
        let thumbnail_workers = workers.clamp(1, maximum_thumbnail_worker_count());
        crate::thumbnail_cache::set_rendered_thumbnail_worker_limit(thumbnail_workers);
        Self {
            location: None,
            local_location: None,
            view: LibraryView::Local,
            cloud_config: crate::cloud::CloudConfig::default(),
            cloud_cache_root: None,
            cloud_offline_reason: None,
            cloud_connection_receiver: None,
            cloud_connection_status: None,
            cloud_open_receiver: None,
            cloud_open_label: None,
            cloud_upload_receiver: None,
            cloud_upload_completion: None,
            cloud_folders: Vec::new(),
            cloud_asset_folders: HashMap::new(),
            cloud_folder_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
            cloud_expanded_folders: initial_cloud_expanded_folders(),
            cloud_action_receiver: None,
            cloud_clipboard: None,
            image_clipboard: None,
            image_paste_receiver: None,
            cloud_name_dialog: None,
            cloud_delete_confirmation: None,
            cloud_trash_open: false,
            cloud_trash_items: Vec::new(),
            cloud_trash_server_time: 0,
            cloud_trash_retention_days: 14,
            cloud_trash_receiver: None,
            cloud_trash_selection: HashSet::new(),
            cloud_trash_delete_confirmation: None,
            folder: None,
            root_folder: None,
            folder_tree: None,
            expanded_folders: HashSet::new(),
            folder_sidebar_open,
            entries: Vec::new(),
            entry_indices: HashMap::new(),
            event_receiver: None,
            request_sender: None,
            generation: Arc::new(AtomicU64::new(0)),
            decoding_paused: Arc::new(AtomicBool::new(false)),
            decode_gate: Arc::new(RwLock::new(())),
            scanning: false,
            catalog_ready: false,
            status: "Open a folder to build your RAW library.".to_owned(),
            usage_clock: 0,
            thumbnail_workers,
            sort_order,
            thumbnail_size,
            selected_sources: HashSet::new(),
            selection_mode: false,
            file_action_receiver: None,
            raw_import_receiver: None,
            folder_operation_receiver: None,
            folder_clipboard: None,
            folder_name_dialog: None,
            folder_delete_confirmation: None,
            raw_name_dialog: None,
            export_dialog: None,
            adjustment_paste_dialog: None,
            ai_mask_refresh_prompt: None,
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn new_android_with_workers(
        android_app: auraw_ffi::AndroidApp,
        context: &egui::Context,
        workers: usize,
        thumbnail_size: LibraryThumbnailSize,
        sort_order: LibrarySortOrder,
        selected_folder: String,
    ) -> Self {
        let root_location =
            crate::android::library_location(&android_app).unwrap_or_else(|error| {
                log::warn!("{error}");
                "Android/media/de.duecki.auraw/.library".to_owned()
            });
        let selected_folder =
            match crate::android::select_library_folder(&android_app, &selected_folder) {
                Ok(()) => selected_folder,
                Err(error) => {
                    log::warn!("{error}");
                    if let Err(root_error) = crate::android::select_library_folder(&android_app, "")
                    {
                        log::warn!("{root_error}");
                    }
                    String::new()
                }
            };
        let location = android_library_location_label(&root_location, &selected_folder);
        let thumbnail_workers = workers.clamp(1, maximum_thumbnail_worker_count());
        crate::thumbnail_cache::set_rendered_thumbnail_worker_limit(thumbnail_workers);
        let mut state = Self {
            location: Some(location.clone()),
            local_location: Some(location),
            view: LibraryView::Local,
            cloud_config: crate::cloud::CloudConfig::default(),
            cloud_cache_root: None,
            cloud_offline_reason: None,
            cloud_connection_receiver: None,
            cloud_connection_status: None,
            cloud_open_receiver: None,
            cloud_open_label: None,
            cloud_upload_receiver: None,
            cloud_upload_completion: None,
            cloud_folders: Vec::new(),
            cloud_asset_folders: HashMap::new(),
            cloud_folder_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
            cloud_expanded_folders: initial_cloud_expanded_folders(),
            cloud_action_receiver: None,
            cloud_clipboard: None,
            image_clipboard: None,
            image_paste_receiver: None,
            cloud_name_dialog: None,
            cloud_delete_confirmation: None,
            cloud_trash_open: false,
            cloud_trash_items: Vec::new(),
            cloud_trash_server_time: 0,
            cloud_trash_retention_days: 14,
            cloud_trash_receiver: None,
            cloud_trash_selection: HashSet::new(),
            cloud_trash_delete_confirmation: None,
            folder_sidebar_open: false,
            android_raw_name_dialog: None,
            android_folder_name_dialog: None,
            android_app,
            android_root_location: root_location,
            android_folder: selected_folder.clone(),
            android_folders: Vec::new(),
            android_expanded_folders: android_folder_ancestors(&selected_folder),
            entries: Vec::new(),
            entry_indices: HashMap::new(),
            event_receiver: None,
            request_sender: None,
            generation: Arc::new(AtomicU64::new(0)),
            decoding_paused: Arc::new(AtomicBool::new(false)),
            decode_gate: Arc::new(RwLock::new(())),
            scanning: false,
            catalog_ready: false,
            status: String::new(),
            usage_clock: 0,
            thumbnail_workers,
            sort_order,
            thumbnail_size,
            selected_sources: HashSet::new(),
            selection_mode: false,
            export_dialog: None,
            adjustment_paste_dialog: None,
            ai_mask_refresh_prompt: None,
        };
        state.refresh(context);
        state
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }

    pub(crate) fn cloud_config(&self) -> &crate::cloud::CloudConfig {
        &self.cloud_config
    }

    pub(crate) fn cloud_enabled(&self) -> bool {
        self.cloud_config.enabled
    }

    pub(crate) fn is_cloud_view(&self) -> bool {
        self.view == LibraryView::Cloud
    }

    pub(crate) fn view(&self) -> LibraryView {
        self.view
    }

    pub(crate) fn cloud_folder_id(&self) -> &str {
        &self.cloud_folder_id
    }

    fn cloud_folder(&self, folder_id: &str) -> Option<&crate::cloud::CloudFolder> {
        self.cloud_folders
            .iter()
            .find(|folder| folder.id == folder_id)
    }

    fn cloud_folder_path(&self, folder_id: &str) -> String {
        if folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID {
            return "Cloud".to_owned();
        }
        let mut names = Vec::new();
        let mut current = folder_id;
        let mut remaining = self.cloud_folders.len();
        while current != crate::cloud::CLOUD_ROOT_FOLDER_ID && remaining > 0 {
            let Some(folder) = self.cloud_folder(current) else {
                break;
            };
            names.push(folder.name.clone());
            current = &folder.parent_id;
            remaining -= 1;
        }
        names.reverse();
        if names.is_empty() {
            "Cloud".to_owned()
        } else {
            format!("Cloud / {}", names.join(" / "))
        }
    }

    #[cfg(not(target_os = "android"))]
    fn cloud_breadcrumbs(&self) -> Vec<(String, String)> {
        let mut breadcrumbs = vec![(
            crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
            "Cloud".to_owned(),
        )];
        if self.cloud_folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID {
            return breadcrumbs;
        }
        let mut descendants = Vec::new();
        let mut current = self.cloud_folder_id.as_str();
        let mut remaining = self.cloud_folders.len();
        while current != crate::cloud::CLOUD_ROOT_FOLDER_ID && remaining > 0 {
            let Some(folder) = self.cloud_folder(current) else {
                break;
            };
            descendants.push((folder.id.clone(), folder.name.clone()));
            current = &folder.parent_id;
            remaining -= 1;
        }
        descendants.reverse();
        breadcrumbs.extend(descendants);
        breadcrumbs
    }

    fn update_cloud_location(&mut self) {
        let path = if self.cloud_trash_open {
            "Cloud / Trash".to_owned()
        } else {
            self.cloud_folder_path(&self.cloud_folder_id)
        };
        self.location = self
            .cloud_config
            .normalized()
            .ok()
            .map(|config| format!("AuRaw Cloud · {} · {path}", config.server_url))
            .or_else(|| Some(format!("AuRaw Cloud · {path}")));
    }

    pub(crate) fn select_cloud_folder(
        &mut self,
        folder_id: String,
        context: &egui::Context,
    ) -> bool {
        if folder_id != crate::cloud::CLOUD_ROOT_FOLDER_ID
            && self.cloud_folder(&folder_id).is_none()
        {
            self.status = "That cloud folder no longer exists. Refresh the library.".to_owned();
            return false;
        }
        if self.view == LibraryView::Cloud
            && !self.cloud_trash_open
            && self.cloud_folder_id == folder_id
        {
            return false;
        }
        let mut ancestor_id = folder_id.clone();
        while ancestor_id != crate::cloud::CLOUD_ROOT_FOLDER_ID {
            let Some(parent_id) = self
                .cloud_folder(&ancestor_id)
                .map(|folder| folder.parent_id.clone())
            else {
                break;
            };
            self.cloud_expanded_folders.insert(parent_id.clone());
            ancestor_id = parent_id;
        }
        self.view = LibraryView::Cloud;
        self.cloud_trash_open = false;
        self.cloud_folder_id = folder_id;
        self.update_cloud_location();
        self.entries.clear();
        self.entry_indices.clear();
        self.clear_selection();
        self.catalog_ready = false;
        self.refresh(context);
        true
    }

    pub(crate) fn configure_cloud(
        &mut self,
        config: crate::cloud::CloudConfig,
        cache_root: Option<PathBuf>,
        context: &egui::Context,
    ) {
        let changed = self.cloud_config != config || self.cloud_cache_root != cache_root;
        self.cloud_config = config;
        self.cloud_cache_root = cache_root;
        self.cloud_connection_status = None;
        if changed {
            // Dropping the receiver safely discards any result produced with an
            // old server address or token. The detached worker may finish its
            // bounded cache write, but it can no longer navigate the UI.
            self.cloud_open_receiver = None;
            self.cloud_open_label = None;
            self.cloud_offline_reason = None;
            self.cloud_folders.clear();
            self.cloud_asset_folders.clear();
            self.cloud_folder_id = crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned();
            self.cloud_expanded_folders = initial_cloud_expanded_folders();
            self.cloud_action_receiver = None;
            self.cloud_clipboard = None;
            self.image_clipboard = None;
            self.image_paste_receiver = None;
            self.cloud_name_dialog = None;
            self.cloud_delete_confirmation = None;
            self.cloud_trash_open = false;
            self.cloud_trash_items.clear();
            self.cloud_trash_receiver = None;
            self.cloud_trash_selection.clear();
            self.cloud_trash_delete_confirmation = None;
        }
        if !self.cloud_config.enabled && self.view == LibraryView::Cloud {
            self.show_local(context);
        } else if changed && self.view == LibraryView::Cloud {
            self.refresh(context);
        }
    }

    pub(crate) fn restore_navigation(
        &mut self,
        view: LibraryView,
        cloud_folder_id: String,
        context: &egui::Context,
    ) {
        self.cloud_folder_id = if cloud_folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID
            || (cloud_folder_id.len() == 64
                && cloud_folder_id.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            cloud_folder_id
        } else {
            crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned()
        };
        if view == LibraryView::Cloud && self.cloud_config.enabled {
            self.show_cloud(context);
        }
    }

    pub(crate) fn show_cloud(&mut self, context: &egui::Context) -> bool {
        if !self.cloud_config.enabled {
            self.status = "Enable AuRaw Cloud in Settings first.".to_owned();
            return false;
        }
        let changed_view = self.view != LibraryView::Cloud || self.cloud_trash_open;
        self.cloud_trash_open = false;
        if changed_view {
            self.view = LibraryView::Cloud;
            self.update_cloud_location();
            self.entries.clear();
            self.entry_indices.clear();
            self.clear_selection();
            self.catalog_ready = false;
        }
        self.refresh(context);
        changed_view
    }

    pub(crate) fn show_cloud_trash(&mut self, context: &egui::Context) -> bool {
        if !self.cloud_config.enabled {
            self.status = "Enable AuRaw Cloud in Settings first.".to_owned();
            return false;
        }
        let changed_view = self.view != LibraryView::Cloud || !self.cloud_trash_open;
        self.view = LibraryView::Cloud;
        self.cloud_trash_open = true;
        self.update_cloud_location();
        self.entries.clear();
        self.entry_indices.clear();
        self.clear_selection();
        self.catalog_ready = false;
        self.refresh(context);
        changed_view
    }

    pub(crate) fn show_local(&mut self, context: &egui::Context) -> bool {
        if self.cloud_action_receiver.is_some() {
            self.status = "Wait for the current cloud action to finish.".to_owned();
            return false;
        }
        self.cloud_open_receiver = None;
        self.cloud_open_label = None;
        self.cloud_offline_reason = None;
        let changed_view = self.view != LibraryView::Local;
        if changed_view {
            self.view = LibraryView::Local;
            self.location = self.local_location.clone();
            self.entries.clear();
            self.entry_indices.clear();
            self.clear_selection();
            self.catalog_ready = false;
        }
        self.refresh(context);
        changed_view
    }

    pub(crate) fn remember_cloud_folder_without_refresh(&mut self, folder_id: String) -> bool {
        if self.cloud_folder_id == folder_id {
            return false;
        }
        self.cloud_folder_id = folder_id;
        self.cloud_trash_open = false;
        self.update_cloud_location();
        true
    }

    pub(crate) fn start_cloud_connection_test(&mut self, context: &egui::Context) {
        if self.cloud_connection_receiver.is_some() {
            return;
        }
        let config = self.cloud_config.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_connection_status = None;
        self.cloud_connection_receiver = Some(receiver);
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-test".to_owned())
            .spawn(move || {
                let result = crate::cloud::test_connection(&config);
                let _ = sender.send(result);
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_connection_receiver = None;
            self.cloud_connection_status = Some(Err(format!(
                "Could not start the cloud connection test: {error}"
            )));
        }
    }

    pub(crate) fn cloud_connection_status(&mut self) -> Option<&Result<String, String>> {
        let received = self
            .cloud_connection_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match received {
            Some(Ok(result)) => {
                self.cloud_connection_receiver = None;
                self.cloud_connection_status = Some(result);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.cloud_connection_receiver = None;
                self.cloud_connection_status = Some(Err(
                    "The cloud connection test stopped unexpectedly.".to_owned(),
                ));
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }
        self.cloud_connection_status.as_ref()
    }

    pub(crate) fn cloud_connection_test_in_progress(&self) -> bool {
        self.cloud_connection_receiver.is_some()
    }

    pub(crate) fn start_cloud_open(
        &mut self,
        asset: crate::cloud::CloudAsset,
        context: &egui::Context,
    ) {
        if self.cloud_open_receiver.is_some() {
            self.status = "Wait for the current cloud RAW download to finish.".to_owned();
            return;
        }
        let Some(cache_root) = self.cloud_cache_root.clone() else {
            self.status = "AuRaw could not locate its private cloud cache.".to_owned();
            return;
        };
        let config = self.cloud_config.clone();
        let allow_network = self.cloud_network_available();
        let label = asset.name.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_open_receiver = Some(receiver);
        self.cloud_open_label = Some(label.clone());
        self.status = format!("Preparing {label} from AuRaw Cloud…");
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-raw-download".to_owned())
            .spawn(move || {
                let progress_sender = sender.clone();
                let progress_repaint = repaint.clone();
                let mut last_reported = 0u64;
                let result = crate::cloud::open_asset(
                    &config,
                    &cache_root,
                    &asset,
                    allow_network,
                    move |downloaded, total| {
                        if downloaded == total
                            || downloaded.saturating_sub(last_reported)
                                >= CLOUD_DOWNLOAD_PROGRESS_STEP
                        {
                            last_reported = downloaded;
                            let _ = progress_sender
                                .send(CloudOpenEvent::Progress { downloaded, total });
                            progress_repaint.request_repaint();
                        }
                    },
                );
                let _ = sender.send(CloudOpenEvent::Finished(result));
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_open_receiver = None;
            self.cloud_open_label = None;
            self.status = format!("Could not start the cloud RAW download: {error}");
        }
    }

    #[cfg(target_os = "android")]
    fn cloud_network_available(&self) -> bool {
        crate::android::network_available(&self.android_app).unwrap_or_else(|error| {
            log::warn!("could not inspect Android network state: {error}");
            true
        })
    }

    #[cfg(not(target_os = "android"))]
    fn cloud_network_available(&self) -> bool {
        true
    }

    pub(crate) fn poll_cloud_open(
        &mut self,
    ) -> Option<Result<crate::cloud::CachedCloudAsset, String>> {
        loop {
            let received = self
                .cloud_open_receiver
                .as_ref()
                .map(mpsc::Receiver::try_recv);
            match received {
                Some(Ok(CloudOpenEvent::Progress { downloaded, total })) => {
                    let label = self.cloud_open_label.as_deref().unwrap_or("cloud RAW");
                    self.status = if total > 0 {
                        format!(
                            "Downloading {label} from AuRaw Cloud… {:.0}%",
                            downloaded as f64 * 100.0 / total as f64
                        )
                    } else {
                        format!(
                            "Downloading {label} from AuRaw Cloud… {:.1} MiB",
                            downloaded as f64 / (1024.0 * 1024.0)
                        )
                    };
                }
                Some(Ok(CloudOpenEvent::Finished(result))) => {
                    self.cloud_open_receiver = None;
                    self.cloud_open_label = None;
                    if let Ok(cached) = &result {
                        let sync_state = crate::cloud::cached_asset_sync_state(&cached.raw_path)
                            .map(|(_, state)| state)
                            .unwrap_or(crate::cloud::CloudSyncState::Synced);
                        for entry in &mut self.entries {
                            if matches!(
                                &entry.info.source,
                                LibrarySource::Cloud(asset) if asset.id == cached.asset_id
                            ) {
                                entry.info.cloud_downloaded = true;
                                entry.info.cloud_sync_state = sync_state;
                            }
                        }
                    }
                    return Some(result);
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.cloud_open_receiver = None;
                    self.cloud_open_label = None;
                    return Some(Err(
                        "The cloud RAW download stopped unexpectedly.".to_owned()
                    ));
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => return None,
            }
        }
    }

    pub(crate) fn cloud_upload_in_progress(&self) -> bool {
        self.cloud_upload_receiver.is_some()
    }

    fn cloud_action_in_progress(&self) -> bool {
        self.cloud_action_receiver.is_some()
    }

    fn image_paste_in_progress(&self) -> bool {
        self.image_paste_receiver.is_some()
    }

    fn start_image_paste(&mut self, destination: ImagePasteDestination, context: &egui::Context) {
        if self.image_paste_receiver.is_some()
            || self.cloud_action_receiver.is_some()
            || self.cloud_upload_receiver.is_some()
            || self.cloud_open_receiver.is_some()
            || {
                #[cfg(not(target_os = "android"))]
                {
                    self.file_action_receiver.is_some()
                        || self.raw_import_receiver.is_some()
                        || self.folder_operation_receiver.is_some()
                }
                #[cfg(target_os = "android")]
                {
                    false
                }
            }
        {
            self.status = "Wait for the current library transfer to finish.".to_owned();
            return;
        }
        let Some(clipboard) = self.image_clipboard.clone() else {
            self.status = "Copy or cut one or more RAW files first.".to_owned();
            return;
        };
        #[cfg(not(target_os = "android"))]
        let destination = match destination {
            ImagePasteDestination::LocalFolder(folder) => {
                let Some(root) = self.root_folder.as_deref() else {
                    self.status = "Open a top-level local library folder first.".to_owned();
                    return;
                };
                match canonical_library_directory(root, &folder, false) {
                    Ok(folder) => ImagePasteDestination::LocalFolder(folder),
                    Err(error) => {
                        self.status = error;
                        return;
                    }
                }
            }
            destination => destination,
        };
        if matches!(&destination, ImagePasteDestination::CloudFolder(_))
            && self.cloud_config.normalized().is_err()
        {
            self.status = "Configure AuRaw Cloud before pasting RAW files there.".to_owned();
            return;
        }

        let count = clipboard.count();
        let config = self.cloud_config.clone();
        let cache_root = self.cloud_cache_root.clone();
        let allow_network = self.cloud_network_available();
        #[cfg(target_os = "android")]
        let android_app = self.android_app.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.image_paste_receiver = Some(receiver);
        self.status = format!(
            "{} {count} RAW{}…",
            if clipboard.mode == ImageClipboardMode::Copy {
                "Copying"
            } else {
                "Moving"
            },
            if count == 1 { "" } else { "s" }
        );
        let spawn = std::thread::Builder::new()
            .name("auraw-image-paste".to_owned())
            .spawn(move || {
                let completion = run_image_paste(
                    &config,
                    cache_root.as_deref(),
                    allow_network,
                    clipboard,
                    destination,
                    #[cfg(target_os = "android")]
                    &android_app,
                );
                let _ = sender.send(completion);
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.image_paste_receiver = None;
            self.status = format!("Could not start the image paste: {error}");
        }
    }

    fn start_cloud_action(&mut self, request: CloudActionRequest, context: &egui::Context) {
        if self.cloud_action_receiver.is_some() {
            self.status = "Another cloud action is still running.".to_owned();
            return;
        }
        if self.image_paste_receiver.is_some() {
            self.status = "Wait for the current image paste to finish.".to_owned();
            return;
        }
        if self.cloud_upload_receiver.is_some() || self.cloud_open_receiver.is_some() {
            self.status = "Wait for the current cloud transfer to finish.".to_owned();
            return;
        }
        if self.view != LibraryView::Cloud {
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.event_receiver = None;
            self.request_sender = None;
            self.scanning = false;
            self.catalog_ready = false;
            self.view = LibraryView::Cloud;
            self.update_cloud_location();
            self.entries.clear();
            self.entry_indices.clear();
            self.clear_selection();
        }
        let reveal_parent = match &request {
            CloudActionRequest::CreateFolder { parent_id, .. }
            | CloudActionRequest::UpdateFolder { parent_id, .. } => Some(parent_id),
            CloudActionRequest::CopyFolder {
                destination_parent_id,
                ..
            } => Some(destination_parent_id),
            _ => None,
        };
        if let Some(parent_id) = reveal_parent {
            self.cloud_expanded_folders.insert(parent_id.clone());
        }
        let status = match &request {
            CloudActionRequest::CreateFolder { .. } => "Creating cloud folder…".to_owned(),
            CloudActionRequest::UpdateFolder { .. } => "Updating cloud folder…".to_owned(),
            CloudActionRequest::CopyFolder { .. } => "Copying cloud folder…".to_owned(),
            CloudActionRequest::DeleteFolder { .. } => "Deleting cloud folder…".to_owned(),
            CloudActionRequest::CopyAssets { assets, .. } => {
                format!("Copying {} cloud RAWs…", assets.len())
            }
            CloudActionRequest::RenameAsset { .. } => "Renaming cloud RAW…".to_owned(),
            CloudActionRequest::DeleteAssets { assets } => {
                format!("Deleting {} cloud RAWs…", assets.len())
            }
            CloudActionRequest::RestoreTrash { items } => {
                format!(
                    "Restoring {} Trash item{}…",
                    items.len(),
                    if items.len() == 1 { "" } else { "s" }
                )
            }
            CloudActionRequest::PermanentlyDeleteTrash { items } => format!(
                "Permanently deleting {} Trash item{}…",
                items.len(),
                if items.len() == 1 { "" } else { "s" }
            ),
            CloudActionRequest::EmptyTrash => "Emptying AuRaw Cloud Trash…".to_owned(),
            CloudActionRequest::ResetAssets { assets } => {
                format!("Resetting {} cloud RAWs…", assets.len())
            }
            CloudActionRequest::PrepareAssets { assets, purpose } => format!(
                "Preparing {} cloud RAW{} for {}…",
                assets.len(),
                if assets.len() == 1 { "" } else { "s" },
                match purpose {
                    CloudPreparedPurpose::Export => "export",
                    CloudPreparedPurpose::CopyAdjustments => "copying adjustments",
                    CloudPreparedPurpose::PasteAdjustments => "pasting adjustments",
                }
            ),
        };
        let config = self.cloud_config.clone();
        let cache_root = self.cloud_cache_root.clone();
        let allow_network = self.cloud_network_available();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_action_receiver = Some(receiver);
        self.status = status;
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-action".to_owned())
            .spawn(move || {
                let completion =
                    run_cloud_action(&config, cache_root.as_deref(), allow_network, request);
                let _ = sender.send(completion);
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_action_receiver = None;
            self.status = format!("Could not start the cloud action: {error}");
        }
    }

    fn poll_cloud_action(&mut self) -> Option<CloudActionCompletion> {
        let received = self
            .cloud_action_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match received {
            Some(Ok(completion)) => {
                self.cloud_action_receiver = None;
                Some(completion)
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.cloud_action_receiver = None;
                Some(CloudActionCompletion::Mutation {
                    result: Err("The cloud action stopped unexpectedly.".to_owned()),
                    clear_clipboard: false,
                })
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => None,
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn start_desktop_cloud_upload(
        &mut self,
        paths: Vec<PathBuf>,
        context: &egui::Context,
    ) {
        if self.image_paste_receiver.is_some() {
            self.status = "Wait for the current image paste to finish.".to_owned();
            return;
        }
        if self.cloud_upload_receiver.is_some() {
            self.status = "Wait for the current AuRaw Cloud upload to finish.".to_owned();
            return;
        }
        if paths.is_empty() {
            self.status = "No RAW files selected for cloud upload.".to_owned();
            return;
        }
        if let Err(error) = self.cloud_config.normalized() {
            self.status = error;
            return;
        }

        let selected = paths.len();
        let paths = paths
            .into_iter()
            .take(MAX_CLOUD_UPLOAD_FILES)
            .collect::<Vec<_>>();
        let total = paths.len();
        let config = self.cloud_config.clone();
        let folder_id = self.cloud_folder_id.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_upload_receiver = Some(receiver);
        self.cloud_upload_completion = None;
        self.status = format!(
            "Preparing {} RAW {} for AuRaw Cloud…",
            total,
            if total == 1 { "file" } else { "files" }
        );
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-upload".to_owned())
            .spawn(move || {
                let mut uploaded = 0usize;
                let mut failed = selected.saturating_sub(total);
                let mut errors = Vec::new();
                if selected > total {
                    errors.push(format!(
                        "Only the first {MAX_CLOUD_UPLOAD_FILES} selected RAW files were uploaded."
                    ));
                }
                for (index, path) in paths.into_iter().enumerate() {
                    let label = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                        .unwrap_or_else(|| path.display().to_string());
                    let _ = sender.send(CloudUploadEvent::Progress {
                        position: index + 1,
                        total,
                        label: label.clone(),
                    });
                    repaint.request_repaint();
                    match crate::cloud::upload_asset_path_to_folder(&config, &path, &folder_id) {
                        Ok(_) => uploaded += 1,
                        Err(error) => {
                            failed += 1;
                            if errors.len() < 5 {
                                errors.push(format!("{label}: {error}"));
                            }
                        }
                    }
                }
                let _ = sender.send(CloudUploadEvent::Finished {
                    target: config,
                    uploaded,
                    failed,
                    errors,
                });
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_upload_receiver = None;
            self.status = format!("Could not start the AuRaw Cloud upload: {error}");
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn start_android_cloud_upload(
        &mut self,
        documents: Vec<crate::android::CloudUploadDocument>,
        selection_failed: usize,
        selection_errors: String,
        context: &egui::Context,
    ) {
        if self.image_paste_receiver.is_some() {
            self.status = "Wait for the current image paste to finish.".to_owned();
            return;
        }
        if self.cloud_upload_receiver.is_some() {
            self.status = "Wait for the current AuRaw Cloud upload to finish.".to_owned();
            return;
        }
        if documents.is_empty() {
            self.status = if selection_failed == 0 {
                "No RAW files selected for cloud upload.".to_owned()
            } else {
                format!("No RAW files could be selected for cloud upload. {selection_errors}")
            };
            return;
        }
        if let Err(error) = self.cloud_config.normalized() {
            self.status = error;
            return;
        }

        let selected = documents.len();
        let documents = documents
            .into_iter()
            .take(MAX_CLOUD_UPLOAD_FILES)
            .collect::<Vec<_>>();
        let total = documents.len();
        let config = self.cloud_config.clone();
        let folder_id = self.cloud_folder_id.clone();
        let android_app = self.android_app.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_upload_receiver = Some(receiver);
        self.cloud_upload_completion = None;
        self.status = format!(
            "Preparing {} RAW {} for AuRaw Cloud…",
            total,
            if total == 1 { "file" } else { "files" }
        );
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-upload".to_owned())
            .spawn(move || {
                let mut uploaded = 0usize;
                let mut failed = selection_failed + selected.saturating_sub(total);
                let mut errors = selection_errors
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .take(5)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if selected > total && errors.len() < 5 {
                    errors.push(format!(
                        "Only the first {MAX_CLOUD_UPLOAD_FILES} selected RAW files were uploaded."
                    ));
                }
                for (index, document) in documents.into_iter().enumerate() {
                    let label = document.display_name.clone();
                    let _ = sender.send(CloudUploadEvent::Progress {
                        position: index + 1,
                        total,
                        label: label.clone(),
                    });
                    repaint.request_repaint();
                    let result =
                        crate::android::open_document_for_cloud_upload(&android_app, &document.uri)
                            .and_then(|raw| {
                                crate::cloud::upload_asset_file_to_folder(
                                    &config,
                                    raw,
                                    &document.display_name,
                                    document.bytes,
                                    &folder_id,
                                )
                            });
                    match result {
                        Ok(_) => uploaded += 1,
                        Err(error) => {
                            failed += 1;
                            if errors.len() < 5 {
                                errors.push(format!("{label}: {error}"));
                            }
                        }
                    }
                }
                let _ = sender.send(CloudUploadEvent::Finished {
                    target: config,
                    uploaded,
                    failed,
                    errors,
                });
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_upload_receiver = None;
            self.status = format!("Could not start the AuRaw Cloud upload: {error}");
        }
    }

    pub(crate) fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    #[cfg(target_os = "android")]
    pub(crate) fn has_selection(&self) -> bool {
        !self.selected_sources.is_empty()
    }

    pub(crate) fn selection_mode(&self) -> bool {
        self.selection_mode
    }

    pub(crate) fn begin_selection(&mut self) {
        self.selection_mode = true;
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selected_sources.clear();
        self.selection_mode = false;
    }

    #[cfg(any(target_os = "android", test))]
    fn handle_touch_thumbnail_activation(
        &mut self,
        source: &LibrarySource,
        secondary_clicked: bool,
    ) -> TouchThumbnailAction {
        if secondary_clicked {
            self.begin_selection();
            self.selected_sources.insert(source.clone());
            return TouchThumbnailAction::SelectionChanged {
                back_navigation_active: true,
            };
        }

        if !self.selection_mode() {
            return TouchThumbnailAction::Open;
        }

        if !self.selected_sources.remove(source) {
            self.selected_sources.insert(source.clone());
        }
        if self.selected_sources.is_empty() {
            self.clear_selection();
        }

        TouchThumbnailAction::SelectionChanged {
            back_navigation_active: self.selection_mode(),
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn root_folder(&self) -> Option<&Path> {
        self.root_folder.as_deref()
    }

    pub(crate) fn folder_sidebar_open(&self) -> bool {
        self.folder_sidebar_open
    }

    pub(crate) fn set_folder_sidebar_open(&mut self, open: bool) -> bool {
        if self.folder_sidebar_open == open {
            return false;
        }
        self.folder_sidebar_open = open;
        true
    }

    #[cfg(target_os = "android")]
    pub(crate) fn android_folder(&self) -> &str {
        &self.android_folder
    }

    #[cfg(target_os = "android")]
    pub(crate) fn select_android_folder(
        &mut self,
        folder: String,
        context: &egui::Context,
    ) -> bool {
        if self.view == LibraryView::Local && self.android_folder == folder {
            return false;
        }
        if let Err(error) = crate::android::select_library_folder(&self.android_app, &folder) {
            self.status = error;
            return false;
        }
        self.view = LibraryView::Local;
        self.android_folder = folder;
        self.android_expanded_folders
            .extend(android_folder_ancestors(&self.android_folder));
        let location =
            android_library_location_label(&self.android_root_location, &self.android_folder);
        self.location = Some(location.clone());
        self.local_location = Some(location);
        self.entries.clear();
        self.entry_indices.clear();
        self.clear_selection();
        self.catalog_ready = false;
        self.refresh(context);
        true
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn filmstrip_len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn filmstrip_item(&self, index: usize) -> Option<DesktopFilmstripItem> {
        let entry = self.entries.get(index)?;
        let (source, path, identity) = match &entry.info.source {
            LibrarySource::File(path) => (
                DesktopFilmstripSource::Local(path.clone()),
                Some(path.clone()),
                format!("local:{}", path.display()),
            ),
            LibrarySource::Cloud(asset) => {
                let cached_path = self.cloud_cache_root.as_deref().and_then(|cache_root| {
                    crate::cloud::cached_asset_path(&self.cloud_config, cache_root, asset)
                });
                (
                    DesktopFilmstripSource::Cloud(asset.clone()),
                    cached_path,
                    format!("cloud:{}", asset.id),
                )
            }
        };
        Some(DesktopFilmstripItem {
            source,
            path,
            identity,
            name: entry.info.name.clone(),
            texture: entry.texture.clone(),
            thumbnail_size: entry.thumbnail_size,
        })
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn filmstrip_index_for_path(&self, path: &Path) -> Option<usize> {
        if let Some(index) = self
            .entry_indices
            .get(&LibrarySource::File(path.to_owned()))
            .copied()
        {
            return Some(index);
        }
        let asset_id = crate::cloud::cached_asset_id_for_raw(path)?;
        self.entries.iter().position(|entry| {
            matches!(
                &entry.info.source,
                LibrarySource::Cloud(asset) if asset.id == asset_id
            )
        })
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn desktop_loading_thumbnail_for_path(
        &mut self,
        path: &Path,
        context: &egui::Context,
    ) -> Option<(egui::TextureHandle, [u32; 2])> {
        let index = self.filmstrip_index_for_path(path)?;
        self.restore_resident_thumbnail_texture(index, context);
        self.loading_thumbnail_for_index(index)
    }

    #[cfg(target_os = "android")]
    pub(crate) fn android_loading_thumbnail_for_uri(
        &mut self,
        uri: &str,
        context: &egui::Context,
    ) -> Option<(egui::TextureHandle, [u32; 2])> {
        let index = self.entries.iter().position(|entry| {
            matches!(
                &entry.info.source,
                LibrarySource::Android { uri: entry_uri, .. } if entry_uri == uri
            )
        })?;
        self.restore_resident_thumbnail_texture(index, context);
        self.loading_thumbnail_for_index(index)
    }

    fn loading_thumbnail_for_index(&self, index: usize) -> Option<(egui::TextureHandle, [u32; 2])> {
        let entry = self.entries.get(index)?;
        let texture = entry.texture.clone()?;
        let size = entry.thumbnail_size.unwrap_or_else(|| {
            let [width, height] = texture.size();
            [width as u32, height as u32]
        });
        Some((texture, size))
    }

    pub(crate) fn thumbnail_worker_count(&self) -> usize {
        self.thumbnail_workers
    }

    pub(crate) fn thumbnail_size(&self) -> LibraryThumbnailSize {
        self.thumbnail_size
    }

    pub(crate) fn set_thumbnail_size(&mut self, thumbnail_size: LibraryThumbnailSize) -> bool {
        if self.thumbnail_size == thumbnail_size {
            return false;
        }
        self.thumbnail_size = thumbnail_size;
        true
    }

    pub(crate) fn sort_order(&self) -> LibrarySortOrder {
        self.sort_order
    }

    pub(crate) fn set_sort_order(&mut self, sort_order: LibrarySortOrder) -> bool {
        if self.sort_order == sort_order {
            return false;
        }
        self.sort_order = sort_order;
        self.sort_entries();
        true
    }

    fn sort_entries(&mut self) {
        let sort_order = self.sort_order;
        self.entries
            .sort_by(|left, right| compare_library_entries(left, right, sort_order));
        self.rebuild_entry_indices();
    }

    fn rebuild_entry_indices(&mut self) {
        self.entry_indices = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.info.source.clone(), index))
            .collect();
    }

    pub(crate) fn set_thumbnail_worker_count(&mut self, workers: usize, context: &egui::Context) {
        let workers = workers.clamp(1, maximum_thumbnail_worker_count());
        if self.thumbnail_workers == workers {
            return;
        }
        self.thumbnail_workers = workers;
        crate::thumbnail_cache::set_rendered_thumbnail_worker_limit(workers);
        if self.location.is_some() {
            self.refresh(context);
        }
    }

    /// Stops catalog-wide background thumbnail decoding while Develop owns the
    /// full RAW. Explicit display-priority requests from the desktop filmstrip
    /// may still run through the shared decode gate, which preserves full-RAW
    /// decode exclusivity while keeping visible Develop thumbnails responsive.
    pub(crate) fn prepare_for_develop(&mut self) {
        self.decoding_paused.store(true, Ordering::Release);
        #[cfg(target_os = "android")]
        self.evict_textures_to_limit(ANDROID_DEVELOP_TEXTURE_CACHE_LIMIT);
    }

    /// Returns the gate shared by all thumbnail generations. Full RAW workers
    /// use the same gate so a sensor decode cannot overlap a preview decode.
    pub(crate) fn decode_gate(&self) -> Arc<RwLock<()>> {
        Arc::clone(&self.decode_gate)
    }

    /// Cancels the current generation and drops every in-memory preview before
    /// the platform cache directory is cleared. The caller takes the shared
    /// decode gate's writer lock before deleting files, then calls `refresh`.
    pub(crate) fn prepare_for_thumbnail_cache_clear(&mut self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.event_receiver = None;
        self.request_sender = None;
        self.scanning = false;
        self.usage_clock = 0;
        for entry in &mut self.entries {
            entry.texture = None;
            entry.resident_thumbnail = None;
            entry.texture_is_resident = false;
            entry.thumbnail_size = None;
            entry.thumbnail_error = None;
            entry.thumbnail_failures = 0;
            entry.thumbnail_retry_after = None;
            entry.thumbnail_queued = false;
            entry.developed_thumbnail = false;
            entry.last_used = 0;
        }
    }

    fn resume_thumbnail_decoding(&self) {
        self.decoding_paused.store(false, Ordering::Release);
    }

    #[cfg(not(target_os = "android"))]
    fn file_action_in_progress(&self) -> bool {
        self.file_action_receiver.is_some()
            || self.raw_import_receiver.is_some()
            || self.folder_operation_receiver.is_some()
            || self.cloud_action_receiver.is_some()
            || self.cloud_upload_receiver.is_some()
            || self.cloud_open_receiver.is_some()
            || self.image_paste_receiver.is_some()
    }

    #[cfg(not(target_os = "android"))]
    fn start_folder_operation(
        &mut self,
        operation: LibraryFolderOperation,
        context: &egui::Context,
    ) {
        if self.file_action_in_progress() {
            self.status = "Another library file action is still running.".to_owned();
            return;
        }

        let status = folder_operation_progress_status(&operation);
        let operation_root = match &operation {
            LibraryFolderOperation::Create { root, .. }
            | LibraryFolderOperation::Copy { root, .. }
            | LibraryFolderOperation::Move { root, .. }
            | LibraryFolderOperation::Delete { root, .. } => root.clone(),
        };
        let (sender, receiver) = mpsc::channel();
        self.folder_operation_receiver = Some(receiver);
        self.status = status;
        let repaint = context.clone();
        let spawn = std::thread::Builder::new()
            .name("auraw-library-folder-operation".to_owned())
            .spawn(move || {
                let result = run_folder_operation(operation);
                let _ = sender.send(LibraryFolderOperationCompletion {
                    root: operation_root,
                    result,
                });
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.folder_operation_receiver = None;
            self.status = format!("Could not start folder operation: {error}");
        }
    }

    #[cfg(not(target_os = "android"))]
    fn apply_folder_operation_result(
        &mut self,
        result: LibraryFolderOperationResult,
        context: &egui::Context,
    ) {
        match result {
            LibraryFolderOperationResult::Created(path) => {
                if let Some(parent) = path.parent() {
                    self.expanded_folders.insert(parent.to_path_buf());
                }
                self.status = format!("Created folder {}", path.display());
            }
            LibraryFolderOperationResult::Copied {
                source,
                destination,
            } => {
                if let Some(parent) = destination.parent() {
                    self.expanded_folders.insert(parent.to_path_buf());
                }
                self.status = format!("Copied {} to {}", source.display(), destination.display());
            }
            LibraryFolderOperationResult::Moved {
                source,
                destination,
            } => {
                if let Some(folder) = self.folder.as_mut() {
                    if let Ok(suffix) = folder.strip_prefix(&source) {
                        *folder = destination.join(suffix);
                        self.location = Some(folder.display().to_string());
                    }
                }
                let remapped_expanded = self
                    .expanded_folders
                    .drain()
                    .map(|folder| {
                        folder
                            .strip_prefix(&source)
                            .map(|suffix| destination.join(suffix))
                            .unwrap_or(folder)
                    })
                    .collect();
                self.expanded_folders = remapped_expanded;
                if let Some(parent) = destination.parent() {
                    self.expanded_folders.insert(parent.to_path_buf());
                }
                if self.folder_clipboard.as_ref().is_some_and(|clipboard| {
                    clipboard.mode == LibraryFolderClipboardMode::Cut && clipboard.path == source
                }) {
                    self.folder_clipboard = None;
                } else if let Some(clipboard) = self.folder_clipboard.as_mut() {
                    if let Ok(suffix) = clipboard.path.strip_prefix(&source) {
                        clipboard.path = destination.join(suffix);
                    }
                }
                self.status = format!("Moved {} to {}", source.display(), destination.display());
            }
            LibraryFolderOperationResult::Deleted(path) => {
                if let Some(folder) = self.folder.as_ref() {
                    if folder.starts_with(&path) {
                        let root = self.root_folder.as_deref();
                        let fallback = path
                            .parent()
                            .filter(|parent| root.is_some_and(|root| parent.starts_with(root)))
                            .map(Path::to_path_buf)
                            .or_else(|| self.root_folder.clone());
                        self.folder = fallback;
                        self.location = self
                            .folder
                            .as_ref()
                            .map(|folder| folder.display().to_string());
                        self.entries.clear();
                        self.entry_indices.clear();
                        self.clear_selection();
                        self.catalog_ready = false;
                    }
                }
                self.expanded_folders
                    .retain(|folder| !folder.starts_with(&path));
                if self
                    .folder_clipboard
                    .as_ref()
                    .is_some_and(|clipboard| clipboard.path.starts_with(&path))
                {
                    self.folder_clipboard = None;
                }
                self.status = format!("Deleted folder {}", path.display());
            }
        }
        self.refresh(context);
    }

    #[cfg(not(target_os = "android"))]
    fn duplicate_raws_with_sidecars(&mut self, raw_paths: Vec<PathBuf>, context: &egui::Context) {
        if self.file_action_in_progress() {
            self.status = "Another library file action is still running.".to_owned();
            return;
        }
        if raw_paths.is_empty() {
            return;
        }

        let (sender, receiver) = mpsc::channel();
        self.file_action_receiver = Some(receiver);
        self.status = if raw_paths.len() == 1 {
            format!("Duplicating {}…", raw_paths[0].display())
        } else {
            format!("Duplicating {} selected RAW files…", raw_paths.len())
        };
        let repaint = context.clone();
        let spawn = std::thread::Builder::new()
            .name("auraw-library-duplicate".to_owned())
            .spawn(move || {
                let total = raw_paths.len();
                let mut destinations = Vec::with_capacity(total);
                let mut failures = Vec::new();
                for raw_path in raw_paths {
                    match duplicate_raw_and_sidecar(&raw_path) {
                        Ok(destination) => destinations.push(destination),
                        Err(error) => failures.push(error),
                    }
                }
                let result = if failures.is_empty() {
                    Ok(destinations)
                } else {
                    Err(format!(
                        "Duplicated {} of {total} selected RAW files. {}",
                        destinations.len(),
                        failures.join(" · ")
                    ))
                };
                let _ = sender.send(result);
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.file_action_receiver = None;
            self.status = format!("Could not start duplicate operation: {error}");
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn import_dropped_raws(
        &mut self,
        source_paths: Vec<PathBuf>,
        context: &egui::Context,
    ) {
        if self.file_action_in_progress() {
            self.status = "Another library file action is still running.".to_owned();
            return;
        }
        let Some(folder) = self.folder.clone() else {
            self.status = "Open a library folder before dropping RAW files.".to_owned();
            return;
        };
        let Some(root_folder) = self.root_folder.clone() else {
            self.status = "Open a top-level library folder before dropping items.".to_owned();
            return;
        };
        if source_paths.is_empty() {
            return;
        }

        let item_count = source_paths.len();
        let (sender, receiver) = mpsc::channel();
        self.raw_import_receiver = Some(receiver);
        self.status = format!(
            "Importing {item_count} dropped {} into {}…",
            if item_count == 1 { "item" } else { "items" },
            folder.display()
        );
        let repaint = context.clone();
        let spawn = std::thread::Builder::new()
            .name("auraw-library-drop-import".to_owned())
            .spawn(move || {
                let mut imported = Vec::new();
                let mut imported_folders = Vec::new();
                let mut already_present = 0usize;
                let mut ignored = 0usize;
                let mut failures = Vec::new();
                let mut seen_sources = HashSet::new();

                if let Err(error) = canonical_library_directory(&root_folder, &folder, true) {
                    let _ = sender.send(RawImportResult {
                        imported,
                        imported_folders,
                        already_present,
                        ignored,
                        failures: vec![error],
                    });
                    repaint.request_repaint();
                    return;
                }

                for source in source_paths {
                    let source_key = fs::canonicalize(&source).unwrap_or_else(|_| source.clone());
                    if !seen_sources.insert(source_key) {
                        ignored += 1;
                        continue;
                    }
                    if source.is_dir() {
                        match import_folder_into_library(&source, &folder) {
                            Ok(destination) => imported_folders.push(destination),
                            Err(error) => failures.push(error),
                        }
                        continue;
                    }
                    if !source.is_file() || !is_supported_raw_path(&source) {
                        ignored += 1;
                        continue;
                    }

                    match import_raw_into_folder(&source, &folder) {
                        Ok(RawImportOutcome::Imported(destination)) => imported.push(destination),
                        Ok(RawImportOutcome::AlreadyPresent) => already_present += 1,
                        Err(error) => failures.push(error),
                    }
                }

                let _ = sender.send(RawImportResult {
                    imported,
                    imported_folders,
                    already_present,
                    ignored,
                    failures,
                });
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.raw_import_receiver = None;
            self.status = format!("Could not start RAW import: {error}");
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn open_folder(&mut self, folder: PathBuf, context: &egui::Context) {
        self.folder_sidebar_open = true;
        self.open_folder_at(folder.clone(), folder, context);
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn restore_folder(
        &mut self,
        root: PathBuf,
        selected: Option<PathBuf>,
        context: &egui::Context,
    ) {
        let selected = selected
            .filter(|folder| folder.is_dir() && folder.starts_with(&root))
            .unwrap_or_else(|| root.clone());
        self.open_folder_at(root, selected, context);
    }

    #[cfg(not(target_os = "android"))]
    fn open_folder_at(&mut self, root: PathBuf, folder: PathBuf, context: &egui::Context) {
        self.view = LibraryView::Local;
        let folder_changed = self.folder.as_ref() != Some(&folder);
        let root_changed = self.root_folder.as_ref() != Some(&root);
        if root_changed {
            self.folder_clipboard = None;
            self.folder_name_dialog = None;
            self.folder_delete_confirmation = None;
        }
        self.root_folder = Some(root.clone());
        self.location = Some(folder.display().to_string());
        self.local_location = self.location.clone();
        self.folder = Some(folder.clone());
        self.folder_tree = Some(LibraryFolderNode::empty(root.clone()));
        self.expanded_folders.clear();
        let mut ancestor = Some(folder.as_path());
        while let Some(path) = ancestor.filter(|path| path.starts_with(&root)) {
            self.expanded_folders.insert(path.to_path_buf());
            if path == root {
                break;
            }
            ancestor = path.parent();
        }
        if folder_changed {
            self.entries.clear();
            self.entry_indices.clear();
            self.clear_selection();
            self.catalog_ready = false;
        }
        self.refresh(context);
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn select_folder(&mut self, folder: PathBuf, context: &egui::Context) -> bool {
        let Some(root) = self.root_folder.as_ref() else {
            return false;
        };
        if !folder.starts_with(root)
            || (self.view == LibraryView::Local && self.folder.as_ref() == Some(&folder))
        {
            return false;
        }

        self.view = LibraryView::Local;
        self.location = Some(folder.display().to_string());
        self.local_location = self.location.clone();
        self.folder = Some(folder);
        self.entries.clear();
        self.entry_indices.clear();
        self.clear_selection();
        self.catalog_ready = false;
        self.refresh(context);
        true
    }

    fn refresh_cloud_trash(&mut self, context: &egui::Context) {
        if self.cloud_action_receiver.is_some() {
            self.status = "Wait for the current Trash action to finish.".to_owned();
            return;
        }
        let config = self.cloud_config.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_trash_receiver = Some(receiver);
        self.catalog_ready = false;
        self.status = "Refreshing AuRaw Cloud Trash…".to_owned();
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-trash".to_owned())
            .spawn(move || {
                let _ = sender.send(crate::cloud::list_trash(&config));
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_trash_receiver = None;
            self.catalog_ready = true;
            self.status = format!("Could not start the Trash refresh: {error}");
        }
    }

    fn poll_cloud_trash(&mut self) {
        let received = self
            .cloud_trash_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match received {
            Some(Ok(Ok(catalog))) => {
                self.cloud_trash_receiver = None;
                self.cloud_trash_server_time = catalog.server_time;
                self.cloud_trash_retention_days = catalog.retention_days;
                self.cloud_trash_items = catalog.items;
                self.cloud_trash_selection
                    .retain(|id| self.cloud_trash_items.iter().any(|item| &item.id == id));
                self.catalog_ready = true;
                self.status = format!(
                    "Trash · {} item{} · retained for {} days",
                    self.cloud_trash_items.len(),
                    if self.cloud_trash_items.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    catalog.retention_days
                );
            }
            Some(Ok(Err(error))) => {
                self.cloud_trash_receiver = None;
                self.catalog_ready = true;
                self.status = error;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.cloud_trash_receiver = None;
                self.catalog_ready = true;
                self.status = "The AuRaw Cloud Trash refresh stopped unexpectedly.".to_owned();
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }
    }

    pub(crate) fn refresh(&mut self, context: &egui::Context) {
        if self.view == LibraryView::Cloud && self.cloud_trash_open {
            self.refresh_cloud_trash(context);
            return;
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let cancellation = Arc::clone(&self.generation);
        let decoding_paused = Arc::clone(&self.decoding_paused);
        let decode_gate = Arc::clone(&self.decode_gate);
        let thumbnail_workers = self.thumbnail_workers;
        let repaint = context.clone();
        let (event_sender, event_receiver) = mpsc::sync_channel(MAX_PENDING_THUMBNAIL_RESULTS);
        let (request_sender, request_receiver) = mpsc::sync_channel(MAX_PENDING_THUMBNAILS);
        self.event_receiver = Some(event_receiver);
        self.request_sender = Some(request_sender);
        // Keep already decoded GPU textures visible while the same folder is
        // rescanned. Catalog reconciliation below reuses only entries whose
        // RAW identity is unchanged, so reopening/refreshing a folder does not
        // flash every card back to a placeholder or decode cached previews again.
        for entry in &mut self.entries {
            entry.thumbnail_queued = false;
            entry.thumbnail_error = None;
            entry.thumbnail_failures = 0;
            entry.thumbnail_retry_after = None;
        }
        self.scanning = true;
        self.catalog_ready = !self.entries.is_empty();
        self.usage_clock = 0;

        if self.view == LibraryView::Cloud {
            let config = self.cloud_config.clone();
            let folder_id = self.cloud_folder_id.clone();
            let allow_network = self.cloud_network_available();
            let Some(cache_root) = self.cloud_cache_root.clone() else {
                self.event_receiver = None;
                self.request_sender = None;
                self.scanning = false;
                self.catalog_ready = true;
                self.status = "AuRaw could not locate its private cloud cache.".to_owned();
                return;
            };
            self.status = "Refreshing AuRaw Cloud…".to_owned();
            let worker = std::thread::Builder::new()
                .name("auraw-cloud-library".to_owned())
                .spawn(move || {
                    let snapshot =
                        match crate::cloud::list_assets_cached(&config, &cache_root, allow_network)
                        {
                            Ok(snapshot) => snapshot,
                            Err(error) => {
                                send_scan_failure(&event_sender, generation, error, &repaint);
                                return;
                            }
                        };
                    let thumbnail_network_available = snapshot.offline_reason.is_none();
                    let folder_id = cloud_folder_id_for_catalog(&folder_id, &snapshot.folders);
                    let asset_folders = snapshot
                        .items
                        .iter()
                        .map(|asset| (asset.id.clone(), asset.folder_id.clone()))
                        .collect();
                    if event_sender
                        .send(ScanEvent::CloudAvailability {
                            generation,
                            offline_reason: snapshot.offline_reason,
                            folders: snapshot.folders,
                            folder_id: folder_id.clone(),
                            asset_folders,
                        })
                        .is_err()
                    {
                        return;
                    }
                    let files = snapshot
                        .items
                        .into_iter()
                        .filter(|asset| asset.folder_id == folder_id)
                        .map(|asset| {
                            let cloud_downloaded =
                                crate::cloud::asset_available_offline(&config, &cache_root, &asset);
                            let cloud_sync_state =
                                crate::cloud::asset_sync_state(&config, &cache_root, &asset);
                            LibraryFileInfo {
                                display_path: format!("AuRaw Cloud / {}", asset.name),
                                name: asset.name.clone(),
                                bytes: asset.bytes,
                                dimensions_hint: Some([asset.width, asset.height]),
                                cloud_downloaded,
                                cloud_sync_state,
                                #[cfg(not(target_os = "android"))]
                                modified: Some(crate::cloud::modified_time(asset.modified_seconds)),
                                source: LibrarySource::Cloud(asset),
                            }
                        })
                        .collect::<Vec<_>>();
                    let thumbnail_config = config.clone();
                    let thumbnail_cache = cache_root.clone();
                    run_thumbnail_workers(
                        ThumbnailWorker {
                            files,
                            warning_count: 0,
                            truncated: false,
                            generation,
                            cancellation,
                            decoding_paused,
                            decode_gate,
                            event_sender,
                            request_receiver,
                            repaint,
                        },
                        thumbnail_workers,
                        Arc::new(move |source| match source {
                            LibrarySource::Cloud(asset) => crate::cloud::load_thumbnail(
                                &thumbnail_config,
                                &thumbnail_cache,
                                asset,
                                THUMBNAIL_EDGE,
                                thumbnail_network_available,
                            )
                            .map(|thumbnail| loaded_library_thumbnail(thumbnail, false)),
                            _ => Err("invalid cloud thumbnail request".to_owned()),
                        }),
                    );
                });
            if let Err(error) = worker {
                self.event_receiver = None;
                self.request_sender = None;
                self.scanning = false;
                self.catalog_ready = true;
                self.status = format!("Could not start the cloud library scanner: {error}");
            }
            return;
        }

        #[cfg(not(target_os = "android"))]
        let worker = {
            let Some(folder) = self.folder.clone() else {
                self.event_receiver = None;
                self.request_sender = None;
                self.scanning = false;
                self.status = "Open a folder to build your RAW library.".to_owned();
                return;
            };
            let Some(root_folder) = self.root_folder.clone() else {
                self.event_receiver = None;
                self.request_sender = None;
                self.scanning = false;
                self.status = "Open a top-level folder to build your RAW library.".to_owned();
                return;
            };
            self.status = format!("Scanning {}…", folder.display());
            std::thread::Builder::new()
                .name("auraw-library".to_owned())
                .spawn(move || {
                    let tree_sender = event_sender.clone();
                    let tree_repaint = repaint.clone();
                    let tree_cancellation = Arc::clone(&cancellation);
                    let tree_worker = std::thread::Builder::new()
                        .name("auraw-library-folders".to_owned())
                        .spawn(move || {
                            if let Some(tree) = scan_folder_tree(&root_folder, || {
                                tree_cancellation.load(Ordering::Acquire) != generation
                            }) {
                                let _ =
                                    tree_sender.send(ScanEvent::FolderTree { generation, tree });
                                tree_repaint.request_repaint();
                            }
                        });
                    if let Err(error) = tree_worker {
                        log::warn!("could not start the library folder scanner: {error}");
                    }
                    let scan = match scan_folder(&folder, || {
                        cancellation.load(Ordering::Acquire) != generation
                    }) {
                        Ok(result) => result,
                        Err(error) => {
                            send_scan_failure(&event_sender, generation, error, &repaint);
                            return;
                        }
                    };
                    let Some((files, warning_count, truncated)) = scan else {
                        return;
                    };
                    run_thumbnail_workers(
                        ThumbnailWorker {
                            files,
                            warning_count,
                            truncated,
                            generation,
                            cancellation,
                            decoding_paused,
                            decode_gate,
                            event_sender,
                            request_receiver,
                            repaint,
                        },
                        thumbnail_workers,
                        Arc::new(load_desktop_library_thumbnail),
                    );
                })
        };

        #[cfg(target_os = "android")]
        let worker = {
            let android_app = self.android_app.clone();
            self.status = "Refreshing AuRaw library…".to_owned();
            std::thread::Builder::new()
                .name("auraw-library".to_owned())
                .spawn(move || {
                    let folders = match crate::android::list_library_folders(&android_app) {
                        Ok(folders) => folders,
                        Err(error) => {
                            send_scan_failure(&event_sender, generation, error, &repaint);
                            return;
                        }
                    };
                    if event_sender
                        .send(ScanEvent::AndroidFolders {
                            generation,
                            folders,
                        })
                        .is_err()
                    {
                        return;
                    }
                    let documents = match crate::android::list_library_documents(&android_app) {
                        Ok(documents) => documents,
                        Err(error) => {
                            send_scan_failure(&event_sender, generation, error, &repaint);
                            return;
                        }
                    };
                    let truncated = documents.len() > MAX_LIBRARY_FILES;
                    let files = documents
                        .into_iter()
                        .take(MAX_LIBRARY_FILES)
                        .map(|document| {
                            let dimensions_hint = crate::android::load_library_display_dimensions(
                                &android_app,
                                &document.uri,
                            )
                            .ok();
                            LibraryFileInfo {
                                source: LibrarySource::Android {
                                    uri: document.uri,
                                    display_name: document.display_name.clone(),
                                    bytes: document.bytes,
                                    modified_seconds: document.modified_seconds,
                                },
                                display_path: document.display_path,
                                name: document.display_name,
                                bytes: document.bytes,
                                dimensions_hint,
                                cloud_downloaded: false,
                                cloud_sync_state: crate::cloud::CloudSyncState::Synced,
                            }
                        })
                        .collect();
                    let thumbnail_app = android_app.clone();
                    run_thumbnail_workers(
                        ThumbnailWorker {
                            files,
                            warning_count: 0,
                            truncated,
                            generation,
                            cancellation,
                            decoding_paused,
                            decode_gate,
                            event_sender,
                            request_receiver,
                            repaint,
                        },
                        thumbnail_workers,
                        Arc::new(move |source| {
                            load_android_library_thumbnail(&thumbnail_app, source)
                        }),
                    );
                })
        };

        if let Err(error) = worker {
            self.event_receiver = None;
            self.request_sender = None;
            self.scanning = false;
            self.catalog_ready = true;
            self.status = format!("Could not start the library scanner: {error}");
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn poll_dropped_raw_import(&mut self, context: &egui::Context) {
        let imported = self
            .raw_import_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match imported {
            Some(Ok(result)) => {
                self.raw_import_receiver = None;
                if !result.imported.is_empty() || !result.imported_folders.is_empty() {
                    self.refresh(context);
                }
                self.status = raw_import_status(&result);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.raw_import_receiver = None;
                self.status = "The dropped RAW import stopped unexpectedly.".to_owned();
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }
    }

    pub(crate) fn poll(&mut self, context: &egui::Context) {
        let pasted = self
            .image_paste_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match pasted {
            Some(Ok(completion)) => {
                self.image_paste_receiver = None;
                if completion.clear_clipboard {
                    self.image_clipboard = None;
                } else if let Some(remaining) = completion.remaining_clipboard {
                    self.image_clipboard = Some(remaining);
                }
                self.clear_selection();
                #[cfg(target_os = "android")]
                crate::android::set_back_navigation_active(false);
                self.status = completion.result.unwrap_or_else(|error| error);
                self.refresh(context);
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.image_paste_receiver = None;
                self.status = "The image paste stopped unexpectedly.".to_owned();
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }

        loop {
            let received = self
                .cloud_upload_receiver
                .as_ref()
                .map(mpsc::Receiver::try_recv);
            match received {
                Some(Ok(CloudUploadEvent::Progress {
                    position,
                    total,
                    label,
                })) => {
                    self.status =
                        format!("Uploading {position} of {total} to AuRaw Cloud · {label}…");
                }
                Some(Ok(CloudUploadEvent::Finished {
                    target,
                    uploaded,
                    failed,
                    errors,
                })) => {
                    self.cloud_upload_receiver = None;
                    let mut summary = match (uploaded, failed) {
                        (0, 0) => "No RAW files were uploaded to AuRaw Cloud.".to_owned(),
                        (_, 0) => format!(
                            "Uploaded {uploaded} RAW {} to AuRaw Cloud.",
                            if uploaded == 1 { "file" } else { "files" }
                        ),
                        _ => format!(
                            "Uploaded {uploaded} RAW {}; {failed} failed.",
                            if uploaded == 1 { "file" } else { "files" }
                        ),
                    };
                    if !errors.is_empty() {
                        summary.push_str("\n");
                        summary.push_str(&errors.join("\n"));
                    }
                    if uploaded > 0
                        && self.view == LibraryView::Cloud
                        && self.cloud_config == target
                    {
                        self.cloud_upload_completion = Some(summary);
                        self.refresh(context);
                    } else {
                        self.status = summary;
                    }
                    break;
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.cloud_upload_receiver = None;
                    self.status = "The AuRaw Cloud upload stopped unexpectedly.".to_owned();
                    break;
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => break,
            }
        }

        #[cfg(not(target_os = "android"))]
        {
            self.poll_dropped_raw_import(context);

            let completed = self
                .file_action_receiver
                .as_ref()
                .map(mpsc::Receiver::try_recv);
            match completed {
                Some(Ok(Ok(destinations))) => {
                    self.file_action_receiver = None;
                    self.refresh(context);
                    self.status = if destinations.len() == 1 {
                        format!("Duplicated as {}", destinations[0].display())
                    } else {
                        format!("Duplicated {} selected RAW files", destinations.len())
                    };
                }
                Some(Ok(Err(error))) => {
                    self.file_action_receiver = None;
                    self.refresh(context);
                    self.status = self
                        .cloud_upload_completion
                        .take()
                        .map(|summary| format!("{summary}\nCloud refresh failed: {error}"))
                        .unwrap_or(error);
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.file_action_receiver = None;
                    self.status = "The library file operation stopped unexpectedly.".to_owned();
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => {}
            }

            let folder_completed = self
                .folder_operation_receiver
                .as_ref()
                .map(mpsc::Receiver::try_recv);
            match folder_completed {
                Some(Ok(completion)) => {
                    self.folder_operation_receiver = None;
                    if self.root_folder.as_ref() != Some(&completion.root) {
                        self.status = match completion.result {
                            Ok(_) => format!(
                                "Folder operation completed in the previous library root {}.",
                                completion.root.display()
                            ),
                            Err(error) => error,
                        };
                    } else {
                        match completion.result {
                            Ok(result) => self.apply_folder_operation_result(result, context),
                            Err(error) => {
                                self.refresh(context);
                                self.status = error;
                            }
                        }
                    }
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.folder_operation_receiver = None;
                    self.status = "The folder operation stopped unexpectedly.".to_owned();
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => {}
            }
        }

        for _ in 0..MAX_EVENTS_PER_FRAME {
            let received = self.event_receiver.as_ref().map(mpsc::Receiver::try_recv);
            let event = match received {
                Some(Ok(event)) => event,
                Some(Err(mpsc::TryRecvError::Empty)) | None => break,
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.event_receiver = None;
                    self.request_sender = None;
                    self.scanning = false;
                    if !self.catalog_ready {
                        self.status = "The library scanner stopped unexpectedly.".to_owned();
                    }
                    break;
                }
            };

            match event {
                ScanEvent::CloudAvailability {
                    generation,
                    offline_reason,
                    folders,
                    folder_id,
                    asset_folders,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    self.cloud_offline_reason = offline_reason;
                    self.cloud_folders = folders;
                    self.cloud_asset_folders = asset_folders;
                    self.cloud_expanded_folders.retain(|folder_id| {
                        folder_id == crate::cloud::CLOUD_ROOT_FOLDER_ID
                            || self
                                .cloud_folders
                                .iter()
                                .any(|folder| &folder.id == folder_id)
                    });
                    self.cloud_folder_id = folder_id;
                    self.update_cloud_location();
                }
                #[cfg(not(target_os = "android"))]
                ScanEvent::FolderTree { generation, tree }
                    if generation == self.generation.load(Ordering::Acquire) =>
                {
                    self.folder_tree = Some(tree);
                }
                #[cfg(target_os = "android")]
                ScanEvent::AndroidFolders {
                    generation,
                    folders,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    self.android_folders = folders;
                    self.android_expanded_folders.retain(|path| {
                        path.is_empty()
                            || self
                                .android_folders
                                .iter()
                                .any(|folder| &folder.path == path)
                    });
                    self.android_expanded_folders
                        .extend(android_folder_ancestors(&self.android_folder));
                }
                ScanEvent::Catalog {
                    generation,
                    files,
                    warning_count,
                    truncated,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    let mut previous = std::mem::take(&mut self.entries)
                        .into_iter()
                        .map(|entry| (entry.info.source.clone(), entry))
                        .collect::<HashMap<_, _>>();
                    self.entries = files
                        .into_iter()
                        .map(|info| {
                            if let Some(mut entry) = previous.remove(&info.source) {
                                if same_library_file_identity(&entry.info, &info) {
                                    entry.info = info;
                                    entry.thumbnail_error = None;
                                    entry.thumbnail_queued = false;
                                    entry.last_used = 0;
                                    return entry;
                                }
                            }
                            new_library_entry(info)
                        })
                        .collect();
                    self.sort_entries();
                    self.selected_sources
                        .retain(|source| self.entry_indices.contains_key(source));
                    if self.selected_sources.is_empty() && !self.selection_mode {
                        #[cfg(target_os = "android")]
                        crate::android::set_back_navigation_active(false);
                    }
                    self.scanning = false;
                    self.catalog_ready = true;
                    let mut catalog_status = catalog_status(warning_count, truncated);
                    if self.view == LibraryView::Cloud {
                        if let Some(reason) = &self.cloud_offline_reason {
                            let prefix = "Offline · cached cloud library";
                            catalog_status = if catalog_status.is_empty() {
                                format!("{prefix}\n{reason}")
                            } else {
                                format!("{prefix} · {catalog_status}\n{reason}")
                            };
                        }
                    }
                    self.status = match (
                        self.cloud_upload_completion.take(),
                        catalog_status.is_empty(),
                    ) {
                        (Some(summary), true) => summary,
                        (Some(summary), false) => format!("{summary}\n{catalog_status}"),
                        (None, _) => catalog_status,
                    };
                }
                ScanEvent::Thumbnail {
                    generation,
                    source,
                    display_priority,
                    result,
                } if generation == self.generation.load(Ordering::Acquire) => {
                    let Some(index) = self.entry_indices.get(&source).copied() else {
                        continue;
                    };
                    self.entries[index].thumbnail_queued = false;
                    match result {
                        Ok(loaded) => {
                            if self.entries[index].developed_thumbnail && !loaded.developed {
                                continue;
                            }
                            let LoadedLibraryThumbnail {
                                thumbnail,
                                resident_thumbnail,
                                developed,
                            } = loaded;
                            let decoded_size = [thumbnail.width, thumbnail.height];
                            let install_pixels =
                                display_priority || self.entries[index].texture.is_some();
                            self.entries[index].resident_thumbnail = Some(resident_thumbnail);
                            self.entries[index].texture_is_resident = false;
                            if install_pixels {
                                let image = egui::ColorImage::from_rgba_unmultiplied(
                                    [thumbnail.width as usize, thumbnail.height as usize],
                                    &thumbnail.rgba,
                                );
                                self.entries[index].texture = Some(context.load_texture(
                                    format!("library-thumbnail-{generation}-{index}"),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ));
                            }
                            self.entries[index].thumbnail_size = Some(decoded_size);
                            self.entries[index].layout_size.get_or_insert(decoded_size);
                            self.entries[index].thumbnail_error = None;
                            self.entries[index].thumbnail_failures = 0;
                            self.entries[index].thumbnail_retry_after = None;
                            self.entries[index].developed_thumbnail = developed;
                        }
                        Err(error) => {
                            if !self.entries[index].developed_thumbnail {
                                let entry = &mut self.entries[index];
                                entry.thumbnail_failures =
                                    entry.thumbnail_failures.saturating_add(1);
                                let exponent =
                                    u32::from(entry.thumbnail_failures.saturating_sub(1).min(5));
                                let delay = Duration::from_secs(1_u64 << exponent)
                                    .min(THUMBNAIL_RETRY_MAX_DELAY);
                                entry.thumbnail_error = Some(error);
                                entry.thumbnail_retry_after = Some(Instant::now() + delay);
                                context.request_repaint_after(delay);
                            }
                        }
                    }
                }
                ScanEvent::Failed { generation, error }
                    if generation == self.generation.load(Ordering::Acquire) =>
                {
                    self.scanning = false;
                    self.catalog_ready = true;
                    self.event_receiver = None;
                    self.request_sender = None;
                    self.status = self
                        .cloud_upload_completion
                        .take()
                        .map(|summary| format!("{summary}\nCloud refresh failed: {error}"))
                        .unwrap_or(error);
                    break;
                }
                _ => {}
            }
        }
        let _ = context;
    }

    pub(crate) fn touch_and_request_thumbnail(&mut self, index: usize, context: &egui::Context) {
        self.restore_resident_thumbnail_texture(index, context);

        let generation = self.generation.load(Ordering::Acquire);
        let request_sender = self.request_sender.clone();

        let Some(entry) = self.entries.get_mut(index) else {
            return;
        };

        // A full GPU texture needs no work. A resident fallback remains visible
        // while we opportunistically queue the full thumbnail again, so revisiting
        // an evicted card never falls back to the loading placeholder.
        if entry.texture.is_some() && !entry.texture_is_resident || entry.thumbnail_queued {
            return;
        }
        if entry.thumbnail_error.is_some() {
            if entry
                .thumbnail_retry_after
                .is_some_and(|retry_after| Instant::now() < retry_after)
            {
                return;
            }
            entry.thumbnail_error = None;
            entry.thumbnail_retry_after = None;
        }
        let request = ThumbnailRequest {
            generation,
            source: entry.info.source.clone(),
            display_priority: true,
        };
        if request_sender
            .as_ref()
            .is_some_and(|sender| sender.try_send(request).is_ok())
        {
            entry.thumbnail_queued = true;
        }
    }

    fn restore_resident_thumbnail_texture(&mut self, index: usize, context: &egui::Context) {
        let generation = self.generation.load(Ordering::Acquire);
        self.usage_clock = self.usage_clock.wrapping_add(1).max(1);
        let usage_clock = self.usage_clock;
        let Some(entry) = self.entries.get_mut(index) else {
            return;
        };
        entry.last_used = usage_clock;

        if entry.texture.is_none() {
            if let Some(resident) = entry.resident_thumbnail.as_ref() {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [resident.width as usize, resident.height as usize],
                    &resident.rgba,
                );
                entry.texture = Some(context.load_texture(
                    format!("library-resident-thumbnail-{generation}-{index}-{usage_clock}"),
                    image,
                    egui::TextureOptions::LINEAR,
                ));
                entry.texture_is_resident = entry
                    .thumbnail_size
                    .is_some_and(|size| size != [resident.width, resident.height]);
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn install_developed_thumbnail(
        &mut self,
        raw_path: &Path,
        thumbnail: RawThumbnail,
        context: &egui::Context,
        revision: u64,
    ) {
        let source = LibrarySource::File(raw_path.to_owned());
        let Some(index) = self.entry_indices.get(&source).copied() else {
            return;
        };
        self.install_developed_thumbnail_at(index, thumbnail, context, revision);
    }

    #[cfg(target_os = "android")]
    pub(crate) fn install_android_developed_thumbnail(
        &mut self,
        raw_uri: &str,
        thumbnail: RawThumbnail,
        context: &egui::Context,
        revision: u64,
    ) {
        let Some(index) = self.entries.iter().position(|entry| {
            matches!(
                &entry.info.source,
                LibrarySource::Android { uri, .. } if uri == raw_uri
            )
        }) else {
            return;
        };
        self.install_developed_thumbnail_at(index, thumbnail, context, revision);
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn invalidate_adjustment_thumbnail_for_path(&mut self, raw_path: &Path) {
        let source = LibrarySource::File(raw_path.to_owned());
        if let Some(index) = self.entry_indices.get(&source).copied() {
            self.invalidate_adjustment_thumbnail_at(index);
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn invalidate_android_adjustment_thumbnail(&mut self, raw_uri: &str) {
        if let Some(index) = self.entries.iter().position(|entry| {
            matches!(
                &entry.info.source,
                LibrarySource::Android { uri, .. } if uri == raw_uri
            )
        }) {
            self.invalidate_adjustment_thumbnail_at(index);
        }
    }

    fn invalidate_adjustment_thumbnail_at(&mut self, index: usize) {
        let Some(entry) = self.entries.get_mut(index) else {
            return;
        };
        if !entry.developed_thumbnail {
            return;
        }

        // A reset removes the developed cache, so the next valid result is an
        // unedited RAW thumbnail. Clear the developed marker immediately;
        // otherwise poll_events treats that RAW result as a stale downgrade and
        // can leave the card blank after its old texture is evicted.
        entry.texture = None;
        entry.resident_thumbnail = None;
        entry.texture_is_resident = false;
        entry.thumbnail_size = None;
        entry.thumbnail_error = None;
        entry.thumbnail_failures = 0;
        entry.thumbnail_retry_after = None;
        entry.thumbnail_queued = false;
        entry.developed_thumbnail = false;
    }

    fn install_developed_thumbnail_at(
        &mut self,
        index: usize,
        thumbnail: RawThumbnail,
        context: &egui::Context,
        revision: u64,
    ) {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [thumbnail.width as usize, thumbnail.height as usize],
            &thumbnail.rgba,
        );
        self.entries[index].texture = Some(context.load_texture(
            format!("library-developed-thumbnail-{index}-{revision}"),
            image,
            egui::TextureOptions::LINEAR,
        ));
        let decoded_size = [thumbnail.width, thumbnail.height];
        let resident_thumbnail = make_resident_thumbnail(&thumbnail);
        self.entries[index].thumbnail_size = Some(decoded_size);
        self.entries[index].layout_size.get_or_insert(decoded_size);
        self.entries[index].resident_thumbnail = Some(resident_thumbnail);
        self.entries[index].texture_is_resident = false;
        self.entries[index].thumbnail_error = None;
        self.entries[index].thumbnail_failures = 0;
        self.entries[index].thumbnail_retry_after = None;
        self.entries[index].thumbnail_queued = false;
        self.entries[index].developed_thumbnail = true;
    }

    pub(crate) fn evict_old_textures(&mut self, protected_indices: &HashSet<usize>) {
        let limit = if cfg!(target_os = "android") {
            ANDROID_TEXTURE_CACHE_LIMIT
        } else {
            DESKTOP_TEXTURE_CACHE_LIMIT
        };
        self.evict_textures_to_limit_protecting(limit, protected_indices);
        let resident_limit = if cfg!(target_os = "android") {
            ANDROID_RESIDENT_THUMBNAIL_CACHE_LIMIT
        } else {
            DESKTOP_RESIDENT_THUMBNAIL_CACHE_LIMIT
        };
        self.evict_resident_thumbnails_to_limit_protecting(resident_limit, protected_indices);
    }

    #[cfg(target_os = "android")]
    fn evict_textures_to_limit(&mut self, limit: usize) {
        self.evict_textures_to_limit_protecting(limit, &HashSet::new());
    }

    fn evict_textures_to_limit_protecting(
        &mut self,
        limit: usize,
        protected_indices: &HashSet<usize>,
    ) {
        let texture_count = self
            .entries
            .iter()
            .filter(|entry| entry.texture.is_some())
            .count();
        if texture_count <= limit {
            return;
        }
        // Never evict thumbnails that are currently visible or inside the preload
        // margin. On a desktop resize/fullscreen transition the number of active
        // thumbnails can temporarily exceed the nominal cache limit. Evicting by
        // LRU alone would repeatedly remove the first (top) visible rows because
        // they are touched first every frame, making them oscillate between the
        // texture and the "Loading preview…" placeholder.
        let protected_texture_count = protected_indices
            .iter()
            .filter(|&&index| {
                self.entries
                    .get(index)
                    .is_some_and(|entry| entry.texture.is_some())
            })
            .count();
        let effective_limit = limit.max(protected_texture_count);
        if texture_count <= effective_limit {
            return;
        }

        let mut candidates = self
            .entries
            .iter()
            .enumerate()
            .filter(|(index, entry)| entry.texture.is_some() && !protected_indices.contains(index))
            .map(|(index, entry)| (entry.last_used, index))
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        for (_, index) in candidates.into_iter().take(texture_count - effective_limit) {
            self.entries[index].texture = None;
            self.entries[index].texture_is_resident = false;
            // Keep decoded dimensions and a bounded resident pixel fallback after GPU
            // eviction. Returning to this card can rebuild a texture synchronously.
        }
    }

    fn evict_resident_thumbnails_to_limit_protecting(
        &mut self,
        limit: usize,
        protected_indices: &HashSet<usize>,
    ) {
        let resident_count = self
            .entries
            .iter()
            .filter(|entry| entry.resident_thumbnail.is_some())
            .count();
        if resident_count <= limit {
            return;
        }

        let protected_resident_count = protected_indices
            .iter()
            .filter(|&&index| {
                self.entries
                    .get(index)
                    .is_some_and(|entry| entry.resident_thumbnail.is_some())
            })
            .count();
        let effective_limit = limit.max(protected_resident_count);
        if resident_count <= effective_limit {
            return;
        }

        let mut candidates = self
            .entries
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                entry.resident_thumbnail.is_some() && !protected_indices.contains(index)
            })
            .map(|(index, entry)| (entry.last_used, index))
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        for (_, index) in candidates
            .into_iter()
            .take(resident_count - effective_limit)
        {
            self.entries[index].resident_thumbnail = None;
        }
    }
}

fn new_library_entry(info: LibraryFileInfo) -> LibraryEntry {
    // Keep gallery geometry immutable for the lifetime of the catalog entry.
    // Header probing supplies the real display ratio for normal supported RAWs;
    // 3:2 is only a last-resort fallback when metadata cannot be inspected.
    let layout_size = Some(info.dimensions_hint.unwrap_or([3, 2]));
    LibraryEntry {
        info,
        texture: None,
        resident_thumbnail: None,
        texture_is_resident: false,
        thumbnail_size: None,
        layout_size,
        thumbnail_error: None,
        thumbnail_failures: 0,
        thumbnail_retry_after: None,
        thumbnail_queued: false,
        developed_thumbnail: false,
        last_used: 0,
    }
}

fn same_library_file_identity(left: &LibraryFileInfo, right: &LibraryFileInfo) -> bool {
    if left.source != right.source || left.bytes != right.bytes {
        return false;
    }
    #[cfg(not(target_os = "android"))]
    {
        left.modified == right.modified
    }
    #[cfg(target_os = "android")]
    {
        true
    }
}

fn compare_library_entries(
    left: &LibraryEntry,
    right: &LibraryEntry,
    sort_order: LibrarySortOrder,
) -> CmpOrdering {
    let name_order = compare_library_names(&left.info, &right.info);

    match sort_order {
        LibrarySortOrder::NewestFirst => library_modified_key(&right.info)
            .cmp(&library_modified_key(&left.info))
            .then(name_order),
        LibrarySortOrder::OldestFirst => library_modified_key(&left.info)
            .cmp(&library_modified_key(&right.info))
            .then(name_order),
        LibrarySortOrder::NameAscending => name_order,
        LibrarySortOrder::NameDescending => name_order.reverse(),
        LibrarySortOrder::LargestFirst => right.info.bytes.cmp(&left.info.bytes).then(name_order),
        LibrarySortOrder::SmallestFirst => left.info.bytes.cmp(&right.info.bytes).then(name_order),
    }
}

fn compare_library_names(left: &LibraryFileInfo, right: &LibraryFileInfo) -> CmpOrdering {
    left.name
        .to_lowercase()
        .cmp(&right.name.to_lowercase())
        .then_with(|| left.display_path.cmp(&right.display_path))
}

#[cfg(not(target_os = "android"))]
fn library_modified_key(info: &LibraryFileInfo) -> Option<SystemTime> {
    info.modified
}

#[cfg(target_os = "android")]
fn library_modified_key(info: &LibraryFileInfo) -> u64 {
    match &info.source {
        LibrarySource::Android {
            modified_seconds, ..
        } => *modified_seconds,
        LibrarySource::Cloud(asset) => asset.modified_seconds,
    }
}

fn make_resident_thumbnail(thumbnail: &RawThumbnail) -> RawThumbnail {
    if thumbnail.width <= RESIDENT_THUMBNAIL_EDGE && thumbnail.height <= RESIDENT_THUMBNAIL_EDGE {
        return thumbnail.clone();
    }

    let Some(image) =
        image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.rgba.clone())
    else {
        return thumbnail.clone();
    };
    let image = image::DynamicImage::ImageRgba8(image)
        .thumbnail(RESIDENT_THUMBNAIL_EDGE, RESIDENT_THUMBNAIL_EDGE)
        .to_rgba8();
    let (width, height) = image.dimensions();
    RawThumbnail {
        width,
        height,
        rgba: image.into_raw(),
    }
}

fn loaded_library_thumbnail(thumbnail: RawThumbnail, developed: bool) -> LoadedLibraryThumbnail {
    let resident_thumbnail = make_resident_thumbnail(&thumbnail);
    LoadedLibraryThumbnail {
        thumbnail,
        resident_thumbnail,
        developed,
    }
}

type ThumbnailLoader =
    Arc<dyn Fn(&LibrarySource) -> Result<LoadedLibraryThumbnail, String> + Send + Sync + 'static>;

#[cfg(not(target_os = "android"))]
struct DevelopedThumbnailGpu {
    device: eframe::wgpu::Device,
    queue: eframe::wgpu::Queue,
}

#[cfg(not(target_os = "android"))]
static DEVELOPED_THUMBNAIL_GPU: OnceLock<Result<Mutex<DevelopedThumbnailGpu>, String>> =
    OnceLock::new();

#[cfg(not(target_os = "android"))]
fn developed_thumbnail_gpu() -> Result<&'static Mutex<DevelopedThumbnailGpu>, String> {
    let initialized = DEVELOPED_THUMBNAIL_GPU.get_or_init(|| {
        let instance = eframe::wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(
            &eframe::wgpu::RequestAdapterOptions {
                power_preference: eframe::wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
        ))
        .or_else(|_| {
            pollster::block_on(instance.request_adapter(
                &eframe::wgpu::RequestAdapterOptions {
                    power_preference: eframe::wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: true,
                },
            ))
        })
        .map_err(|error| format!("could not find a GPU for edited thumbnails: {error}"))?;
        let adapter_info = adapter.get_info();
        let adapter_limits = adapter.limits();
        let required_dimension = DEVELOPED_THUMBNAIL_PROXY_EDGE.max(mask_atlas_edge());
        if required_dimension > adapter_limits.max_texture_dimension_2d {
            return Err(format!(
                "edited thumbnails require a {required_dimension}-pixel GPU texture, but this adapter supports {}",
                adapter_limits.max_texture_dimension_2d
            ));
        }
        let mut required_limits = if adapter_info.backend == eframe::wgpu::Backend::Gl {
            eframe::wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            eframe::wgpu::Limits::default()
        };
        required_limits.max_texture_dimension_2d = required_dimension;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &eframe::wgpu::DeviceDescriptor {
                label: Some("auraw library edited-thumbnail device"),
                required_limits,
                ..Default::default()
            },
        ))
        .map_err(|error| format!("could not create the edited-thumbnail GPU device: {error}"))?;
        Ok(Mutex::new(DevelopedThumbnailGpu { device, queue }))
    });

    match initialized {
        Ok(gpu) => Ok(gpu),
        Err(error) => Err(error.clone()),
    }
}

#[cfg(not(target_os = "android"))]
fn masks_need_canonical_source(masks: &MaskStack) -> bool {
    masks.masks.iter().any(|mask| {
        mask.components.iter().any(|component| {
            matches!(
                &component.geometry,
                MaskGeometry::LuminanceRange { source: None, .. }
                    | MaskGeometry::ColorRange { source: None, .. }
            )
        })
    })
}

#[cfg(not(target_os = "android"))]
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

#[cfg(not(target_os = "android"))]
fn render_uncached_developed_thumbnail(
    path: &Path,
    maximum_edge: u32,
) -> Result<Option<RawThumbnail>, String> {
    let loaded_sidecar = match crate::sidecar::load_desktop(path) {
        Ok(Some(sidecar)) => sidecar,
        Ok(None) => return Ok(None),
        // Match RAW opening: malformed edit JSON is recoverable. Returning
        // None lets the library use its normal cached/embedded RAW thumbnail
        // instead of leaving the card without any preview.
        Err(crate::sidecar::SidecarError::Invalid(error)) => {
            log::warn!(
                "ignoring invalid sidecar while rendering library thumbnail for {}: {error}",
                path.display()
            );
            return Ok(None);
        }
        Err(error) => {
            return Err(format!(
                "could not load edits for {}: {error}",
                path.display()
            ))
        }
    };
    let sidecar_fingerprint = crate::sidecar::desktop_sidecar_fingerprint(path)?
        .ok_or_else(|| "edit sidecar disappeared before thumbnail rendering".to_owned())?;

    // Edited rebuilds and preview-less RAW fallbacks share the same user-set
    // concurrency budget because both unpack full sensors. The headless GPU
    // phase below remains serialized on its device while RAW preparation for
    // other edited cards can proceed concurrently.
    let _render_permit = crate::thumbnail_cache::acquire_rendered_thumbnail_worker();

    // A different worker may have completed the cache while this request was
    // waiting for a rendered-thumbnail permit.
    if let Some(thumbnail) = crate::sidecar::load_developed_thumbnail_cache(path, maximum_edge)? {
        let cached_edge = thumbnail.width.max(thumbnail.height);
        let minimum_edge = maximum_edge.saturating_mul(3) / 4;
        if maximum_edge <= THUMBNAIL_EDGE || cached_edge >= minimum_edge {
            return Ok(Some(thumbnail));
        }
    }

    let performance =
        crate::performance_settings::load(crate::performance_settings::desktop_path().as_deref());
    let mut camera_profile_folder = performance.camera_profile_folder;
    if performance.camera_profile_auto_detect
        && camera_profile_folder
            .as_ref()
            .is_none_or(|folder| !folder.is_dir())
    {
        camera_profile_folder = crate::performance_settings::detected_adobe_camera_profile_folder();
    }
    let requested_camera_profile =
        loaded_sidecar
            .edits
            .camera_profile
            .as_ref()
            .and_then(|relative| {
                camera_profile_folder
                    .as_ref()
                    .map(|root| root.join(relative))
            });
    let full_raw = load_raw_file_with_profile_selection(
        path,
        performance.camera_profile_mode,
        camera_profile_folder.as_deref(),
        requested_camera_profile.as_deref(),
    )
    .map_err(|error| format!("could not decode edited RAW {}: {error:#}", path.display()))?;
    let render_proxy_edge = DEVELOPED_THUMBNAIL_PROXY_EDGE.max(maximum_edge);
    let mut preview_raw = if full_raw.width.max(full_raw.height) > render_proxy_edge {
        build_proxy(
            &full_raw,
            ProxySpec {
                max_edge: render_proxy_edge,
            },
        )
    } else {
        full_raw
    };

    let edits = loaded_sidecar.edits;
    let geometry = edits.geometry;
    if edits.lens.enabled {
        let catalog = lensfun_catalog(&preview_raw);
        let selected = catalog
            .lenses
            .iter()
            .find(|lens| lens.maker == edits.lens.maker && lens.model == edits.lens.model)
            .cloned()
            .or_else(|| {
                (!edits.lens.maker.is_empty() || !edits.lens.model.is_empty()).then(|| {
                    LensfunLens {
                        maker: edits.lens.maker.clone(),
                        model: edits.lens.model.clone(),
                    }
                })
            })
            .or(catalog.auto_match);
        if let Some(selected) = selected {
            match apply_lensfun_correction(&preview_raw, &selected) {
                Ok(corrected) => preview_raw = corrected,
                Err(error) => log::warn!(
                    "could not apply saved lens correction to library thumbnail {}: {error:#}",
                    path.display()
                ),
            }
        }
    }

    let mut masks = Arc::unwrap_or_clone(edits.masks);
    let inpaint_strokes = Arc::unwrap_or_clone(edits.inpainting);
    let composed_inpaint = compose_inpaint_strokes(&inpaint_strokes);
    let initial_params =
        GpuParams::new(&edits.exposure, &masks, &preview_raw).with_vignette_geometry(geometry);
    let gpu = developed_thumbnail_gpu()?;
    let gpu = gpu
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pipeline = RawGpuPipeline::new_headless_with_quality(
        &gpu.device,
        &gpu.queue,
        &preview_raw,
        &initial_params,
        ProcessingQuality::Preview,
    )
    .map_err(|error| format!("could not prepare edited thumbnail rendering: {error:#}"))?;
    pipeline
        .update_inpaint_layer(
            &gpu.queue,
            composed_inpaint.as_ref(),
            0,
            0,
            preview_raw.width,
            preview_raw.height,
        )
        .map_err(|error| format!("could not apply thumbnail inpainting: {error:#}"))?;

    if masks_need_canonical_source(&masks) {
        let neutral_exposure = crate::pipeline::ExposureParams::scene_referred_default();
        let neutral_masks = MaskStack::default();
        let neutral_params = GpuParams::new(&neutral_exposure, &neutral_masks, &preview_raw);
        pipeline.recompute(&gpu.queue, &gpu.device, &neutral_params);
        let rgba = pipeline
            .read_output_region_blocking(
                &gpu.device,
                &gpu.queue,
                0,
                0,
                preview_raw.width,
                preview_raw.height,
            )
            .map_err(|error| format!("could not build range-mask thumbnail source: {error:#}"))?;
        let source = MaskRgbImage::new(preview_raw.width, preview_raw.height, rgba)
            .ok_or_else(|| "range-mask thumbnail source has invalid dimensions".to_owned())?;
        install_missing_range_sources(&mut masks, &source);
    }

    for layer in 0..masks.masks.len().min(MAX_LOCAL_MASKS) {
        let edge = pipeline.mask_atlas_edge();
        let values =
            masks.rasterize_layer_f16(layer, edge, edge, preview_raw.width, preview_raw.height);
        pipeline
            .update_mask_layer(&gpu.queue, layer, &values)
            .map_err(|error| format!("could not apply thumbnail local mask: {error:#}"))?;
    }
    let params =
        GpuParams::new(&edits.exposure, &masks, &preview_raw).with_vignette_geometry(geometry);
    pipeline.recompute(&gpu.queue, &gpu.device, &params);
    let thumbnail = pipeline
        .output_snapshot()
        .read_thumbnail_blocking(&gpu.device, &gpu.queue, maximum_edge)
        .map_err(|error| format!("could not read edited thumbnail pixels: {error:#}"))?;
    let thumbnail = crate::pipeline::transform_thumbnail_geometry_with_lens(
        &thumbnail,
        geometry,
        preview_raw.lens_geometry.as_deref(),
    );
    crate::sidecar::save_developed_thumbnail_cache(path, &thumbnail, sidecar_fingerprint)?;
    Ok(Some(thumbnail))
}

#[cfg(not(target_os = "android"))]
pub(crate) fn load_desktop_reference_preview(
    path: &Path,
    maximum_edge: u32,
) -> Result<RawThumbnail, String> {
    if maximum_edge == 0 {
        return Err("reference preview edge must be non-zero".to_owned());
    }

    // Preserve developed edits when a sidecar exists. The reference request is
    // allowed to ask for a larger render than the 512 px catalog card;
    // `render_uncached_developed_thumbnail` regenerates an undersized cache.
    match render_uncached_developed_thumbnail(path, maximum_edge) {
        Ok(Some(thumbnail)) => return Ok(thumbnail),
        Ok(None) => {}
        Err(error) => {
            log::warn!(
                "could not render developed reference preview for {}: {error}",
                path.display()
            );
        }
    }

    // Unedited references use the same RAW preview loader as the catalog but
    // at a much larger edge. Most cameras embed a full-resolution JPEG; when
    // they do not, the loader retains its LibRaw processed fallback.
    load_raw_thumbnail(path, maximum_edge)
        .map_err(|error| format!("could not render reference preview: {error:#}"))
}

/// Loads only an already-rendered desktop thumbnail. This deliberately avoids
/// the embedded-preview and sensor-decode fallbacks used to populate Library
/// cards, so it is safe to run alongside the full RAW open worker.
#[cfg(not(target_os = "android"))]
pub(crate) fn load_desktop_cached_thumbnail(
    path: &Path,
    maximum_edge: u32,
) -> Result<Option<RawThumbnail>, String> {
    match crate::sidecar::load_developed_thumbnail_cache(path, maximum_edge) {
        Ok(Some(thumbnail)) => return Ok(Some(thumbnail)),
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use developed loading thumbnail for {}: {error}",
            path.display()
        ),
    }
    crate::thumbnail_cache::load_desktop_raw_thumbnail(path, maximum_edge)
}

#[cfg(not(target_os = "android"))]
fn load_desktop_library_thumbnail(
    source: &LibrarySource,
) -> Result<LoadedLibraryThumbnail, String> {
    let LibrarySource::File(path) = source else {
        return Err("invalid local thumbnail request".to_owned());
    };
    match crate::sidecar::load_developed_thumbnail_cache(path, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => return Ok(loaded_library_thumbnail(thumbnail, true)),
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use developed thumbnail cache for {}: {error}",
            path.display()
        ),
    }
    match render_uncached_developed_thumbnail(path, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => return Ok(loaded_library_thumbnail(thumbnail, true)),
        Ok(None) => {}
        Err(error) => {
            return Err(format!(
                "could not render the edited RAW thumbnail for {}: {error}",
                path.display()
            ))
        }
    }
    match crate::thumbnail_cache::load_desktop_raw_thumbnail(path, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => return Ok(loaded_library_thumbnail(thumbnail, false)),
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use RAW thumbnail cache for {}: {error}",
            path.display()
        ),
    }

    // Prefer the camera-generated JPEG/bitmap, but a missing or unsupported
    // embedded preview must never make an otherwise valid RAW card permanent.
    // `load_raw_thumbnail` falls back to LibRaw's half-size sensor render.
    let thumbnail = load_raw_thumbnail(path, THUMBNAIL_EDGE)
        .map_err(|error| format!("could not render a RAW preview: {error:#}"))?;
    if let Err(error) = crate::thumbnail_cache::save_desktop_raw_thumbnail(path, &thumbnail) {
        log::warn!(
            "could not persist RAW thumbnail cache for {}: {error}",
            path.display()
        );
    }
    Ok(loaded_library_thumbnail(thumbnail, false))
}

#[cfg(target_os = "android")]
fn load_android_library_thumbnail(
    app: &auraw_ffi::AndroidApp,
    source: &LibrarySource,
) -> Result<LoadedLibraryThumbnail, String> {
    let LibrarySource::Android {
        uri,
        display_name,
        bytes,
        modified_seconds,
    } = source
    else {
        return Err("invalid Android thumbnail request".to_owned());
    };
    match crate::android::load_developed_thumbnail_cache(app, uri, display_name, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => return Ok(loaded_library_thumbnail(thumbnail, true)),
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use Android developed-thumbnail cache for {display_name}: {error}"
        ),
    }
    let mut thumbnail = crate::android::load_library_thumbnail(
        app,
        uri,
        display_name,
        *bytes,
        *modified_seconds,
        THUMBNAIL_EDGE,
    )?;
    // Android cannot headlessly rebuild all adjustments while browsing the
    // library, but geometry is cheap and important for composition. Apply the
    // saved crop/orientation even when a developed cache has not been captured
    // yet; opening/saving the image later replaces this with the fully developed
    // geometry-aware thumbnail.
    if let Ok(Some(sidecar)) = crate::sidecar::load_android(app, uri, display_name) {
        thumbnail =
            crate::pipeline::transform_thumbnail_geometry(&thumbnail, sidecar.edits.geometry);
    }
    Ok(loaded_library_thumbnail(thumbnail, false))
}

fn run_thumbnail_workers(worker: ThumbnailWorker, worker_count: usize, load: ThumbnailLoader) {
    let ThumbnailWorker {
        files,
        warning_count,
        truncated,
        generation,
        cancellation,
        decoding_paused,
        decode_gate,
        event_sender,
        request_receiver,
        repaint,
    } = worker;
    if cancellation.load(Ordering::Acquire) != generation {
        return;
    }
    let work_queue = Arc::new(Mutex::new(ThumbnailWorkQueue::new(generation, &files)));
    if event_sender
        .send(ScanEvent::Catalog {
            generation,
            files,
            warning_count,
            truncated,
        })
        .is_err()
    {
        return;
    }
    repaint.request_repaint();

    let request_receiver = Arc::new(Mutex::new(request_receiver));
    let worker_count = worker_count.clamp(1, maximum_thumbnail_worker_count());
    let mut handles = Vec::with_capacity(worker_count);
    for worker_index in 0..worker_count {
        let cancellation = Arc::clone(&cancellation);
        let decoding_paused = Arc::clone(&decoding_paused);
        let decode_gate = Arc::clone(&decode_gate);
        let event_sender = event_sender.clone();
        let request_receiver = Arc::clone(&request_receiver);
        let work_queue = Arc::clone(&work_queue);
        let repaint = repaint.clone();
        let load = Arc::clone(&load);
        let spawn = std::thread::Builder::new()
            .name(format!("auraw-thumbnail-{worker_index}"))
            .spawn(move || {
                run_one_thumbnail_worker(
                    generation,
                    cancellation,
                    decoding_paused,
                    decode_gate,
                    event_sender,
                    request_receiver,
                    work_queue,
                    repaint,
                    load,
                )
            });
        match spawn {
            Ok(handle) => handles.push(handle),
            Err(error) => log::warn!("could not start thumbnail worker {worker_index}: {error}"),
        }
    }

    if handles.is_empty() {
        send_scan_failure(
            &event_sender,
            generation,
            "Could not start any thumbnail workers.".to_owned(),
            &repaint,
        );
        return;
    }
    for handle in handles {
        if handle.join().is_err() {
            log::warn!("a thumbnail worker panicked");
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_one_thumbnail_worker(
    generation: u64,
    cancellation: Arc<AtomicU64>,
    decoding_paused: Arc<AtomicBool>,
    decode_gate: Arc<RwLock<()>>,
    event_sender: mpsc::SyncSender<ScanEvent>,
    request_receiver: Arc<Mutex<mpsc::Receiver<ThumbnailRequest>>>,
    work_queue: Arc<Mutex<ThumbnailWorkQueue>>,
    repaint: egui::Context,
    load: ThumbnailLoader,
) {
    while cancellation.load(Ordering::Acquire) == generation {
        let received = request_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_recv();
        let (request, initial_background) = match received {
            Ok(request) => (request, false),
            Err(mpsc::TryRecvError::Empty) => {
                // Develop pauses catalog-wide background decoding, but visible
                // filmstrip/reference requests still arrive through the explicit
                // request channel. Do not let workers get stuck holding ordinary
                // background entries while those display-priority requests wait.
                if decoding_paused.load(Ordering::Acquire) {
                    std::thread::sleep(THUMBNAIL_PAUSE_POLL_INTERVAL);
                    continue;
                }
                let background = work_queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .background
                    .pop_front();
                let Some(request) = background else {
                    std::thread::sleep(THUMBNAIL_QUEUE_POLL_INTERVAL);
                    continue;
                };
                (request, true)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                if decoding_paused.load(Ordering::Acquire) {
                    break;
                }
                let background = work_queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .background
                    .pop_front();
                let Some(request) = background else {
                    break;
                };
                (request, true)
            }
        };
        if request.generation != generation {
            continue;
        }
        if !work_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .claim(&request, initial_background)
        {
            continue;
        }
        let result = loop {
            // Ordinary catalog requests remain paused in Develop. Explicit
            // display-priority requests (filmstrip/reference) may proceed, but
            // still take the shared decode gate so an active full RAW open keeps
            // exclusive priority and the application's peak memory stays bounded.
            while decoding_paused.load(Ordering::Acquire) && !request.display_priority {
                if cancellation.load(Ordering::Acquire) != generation {
                    return;
                }
                std::thread::sleep(THUMBNAIL_PAUSE_POLL_INTERVAL);
            }

            let decode_guard = decode_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cancellation.load(Ordering::Acquire) != generation {
                return;
            }
            if decoding_paused.load(Ordering::Acquire) && !request.display_priority {
                drop(decode_guard);
                continue;
            }
            break load(&request.source);
        };
        let display_priority = work_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish(&request.source);
        if event_sender
            .send(ScanEvent::Thumbnail {
                generation,
                source: request.source,
                display_priority,
                result,
            })
            .is_err()
        {
            break;
        }
        repaint.request_repaint();
    }
}

fn send_scan_failure(
    sender: &mpsc::SyncSender<ScanEvent>,
    generation: u64,
    error: String,
    repaint: &egui::Context,
) {
    let _ = sender.send(ScanEvent::Failed { generation, error });
    repaint.request_repaint();
}

fn catalog_status(warning_count: usize, truncated: bool) -> String {
    let mut notices = Vec::new();
    if truncated {
        notices.push(format!("Newest {MAX_LIBRARY_FILES} RAW files shown"));
    }
    if warning_count > 0 {
        notices.push(format!(
            "{warning_count} unreadable {}",
            if warning_count == 1 { "item" } else { "items" }
        ));
    }
    notices.join(" · ")
}

fn cloud_image_context_menu(
    ui: &mut Ui,
    app: &AurawApp,
    assets: &[crate::cloud::CloudAsset],
) -> Option<CloudLibraryCardAction> {
    let selected_count = assets.len();
    let action_enabled = !app.library.cloud_action_in_progress()
        && !app.library.cloud_upload_in_progress()
        && !app.library.image_paste_in_progress()
        && app.library.cloud_open_receiver.is_none()
        && app.library_batch_export_progress().is_none()
        && !assets.is_empty();
    let mut action = None;

    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(if selected_count > 1 {
                "Export selected…"
            } else {
                "Export…"
            }),
        )
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Export(assets.to_vec()));
        ui.close();
    }

    ui.separator();
    if ui
        .add_enabled(
            action_enabled && selected_count == 1,
            egui::Button::new("Copy adjustments"),
        )
        .clicked()
    {
        action = assets
            .first()
            .cloned()
            .map(CloudLibraryCardAction::CopyAdjustments);
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled && app.has_copied_adjustments(),
            egui::Button::new(if selected_count > 1 {
                "Paste adjustments to selected"
            } else {
                "Paste adjustments"
            }),
        )
        .on_disabled_hover_text("Copy adjustments from an image first")
        .clicked()
    {
        action = Some(CloudLibraryCardAction::PasteAdjustments(assets.to_vec()));
        ui.close();
    }

    ui.separator();
    if ui
        .add_enabled(action_enabled, egui::Button::new("Copy"))
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Copy(assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(action_enabled, egui::Button::new("Cut"))
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Cut(assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(if selected_count > 1 {
                "Duplicate selected"
            } else {
                "Duplicate"
            }),
        )
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Duplicate(assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled && selected_count == 1,
            egui::Button::new("Rename…"),
        )
        .clicked()
    {
        action = assets.first().cloned().map(CloudLibraryCardAction::Rename);
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(format!(
                "{}  {}",
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                if selected_count > 1 {
                    "Reset adjustments for selected"
                } else {
                    "Reset all adjustments"
                }
            )),
        )
        .clicked()
    {
        action = Some(CloudLibraryCardAction::ResetAdjustments(assets.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(if selected_count > 1 {
                "Delete selected…"
            } else {
                "Delete…"
            }),
        )
        .clicked()
    {
        action = Some(CloudLibraryCardAction::Delete(assets.to_vec()));
        ui.close();
    }
    action
}

fn detach_current_cloud_asset_if_selected(app: &mut AurawApp, assets: &[crate::cloud::CloudAsset]) {
    let current = app.current_path.clone();
    let selected_current = current.as_ref().is_some_and(|path| {
        crate::cloud::cached_asset_id_for_raw(path)
            .is_some_and(|asset_id| assets.iter().any(|asset| asset.id == asset_id))
    });
    if selected_current {
        if let Some(path) = current.as_deref() {
            app.detach_current_file_for_library_action(path);
        }
        app.current_path = None;
    }
}

fn detach_current_cloud_asset_if_inside_folder(app: &mut AurawApp, folder_id: &str) {
    let current = app.current_path.clone();
    let current_folder_id = current
        .as_deref()
        .and_then(crate::cloud::cached_asset_id_for_raw)
        .and_then(|asset_id| app.library.cloud_asset_folders.get(&asset_id))
        .cloned();
    let inside_folder = current_folder_id
        .as_deref()
        .is_some_and(|current_folder_id| {
            cloud_folder_contains(&app.library.cloud_folders, folder_id, current_folder_id)
        });
    if inside_folder {
        if let Some(path) = current.as_deref() {
            app.detach_current_file_for_library_action(path);
        }
        app.current_path = None;
    }
}

fn apply_cloud_image_action(
    app: &mut AurawApp,
    action: CloudLibraryCardAction,
    context: &egui::Context,
) {
    match action {
        CloudLibraryCardAction::Export(assets) => app.library.start_cloud_action(
            CloudActionRequest::PrepareAssets {
                assets,
                purpose: CloudPreparedPurpose::Export,
            },
            context,
        ),
        CloudLibraryCardAction::CopyAdjustments(asset) => app.library.start_cloud_action(
            CloudActionRequest::PrepareAssets {
                assets: vec![asset],
                purpose: CloudPreparedPurpose::CopyAdjustments,
            },
            context,
        ),
        CloudLibraryCardAction::PasteAdjustments(assets) => app.library.start_cloud_action(
            CloudActionRequest::PrepareAssets {
                assets,
                purpose: CloudPreparedPurpose::PasteAdjustments,
            },
            context,
        ),
        CloudLibraryCardAction::Copy(assets) => {
            let count = assets.len();
            app.library.image_clipboard = Some(ImageClipboard {
                mode: ImageClipboardMode::Copy,
                content: ImageClipboardContent::Cloud(assets),
            });
            app.library.cloud_clipboard = None;
            #[cfg(not(target_os = "android"))]
            {
                app.library.folder_clipboard = None;
            }
            app.library.status = format!(
                "Copied {count} cloud RAW{}. Choose Paste in any local or cloud folder.",
                if count == 1 { "" } else { "s" }
            );
        }
        CloudLibraryCardAction::Cut(assets) => {
            let count = assets.len();
            app.library.image_clipboard = Some(ImageClipboard {
                mode: ImageClipboardMode::Cut,
                content: ImageClipboardContent::Cloud(assets),
            });
            app.library.cloud_clipboard = None;
            #[cfg(not(target_os = "android"))]
            {
                app.library.folder_clipboard = None;
            }
            app.library.status = format!(
                "Cut {count} cloud RAW{}. Choose Paste in any local or cloud folder.",
                if count == 1 { "" } else { "s" }
            );
        }
        CloudLibraryCardAction::Duplicate(assets) => app.library.start_cloud_action(
            CloudActionRequest::CopyAssets {
                assets,
                destination_folder_id: app.library.cloud_folder_id.clone(),
                clear_clipboard: false,
            },
            context,
        ),
        CloudLibraryCardAction::Rename(asset) => {
            app.library.cloud_name_dialog = Some(CloudNameDialog {
                name: asset.name.clone(),
                kind: CloudNameDialogKind::RenameAsset { asset },
                error: None,
                focus_requested: false,
            });
        }
        CloudLibraryCardAction::ResetAdjustments(assets) => {
            detach_current_cloud_asset_if_selected(app, &assets);
            app.library
                .start_cloud_action(CloudActionRequest::ResetAssets { assets }, context);
        }
        CloudLibraryCardAction::Delete(assets) => {
            app.library.cloud_delete_confirmation = Some(CloudDeleteTarget::Assets(assets));
        }
    }
}

#[cfg(not(target_os = "android"))]
pub(crate) enum LibraryCardAction {
    Export(Vec<PathBuf>),
    CopyAdjustments(PathBuf),
    PasteAdjustments(Vec<PathBuf>),
    Copy(Vec<PathBuf>),
    Cut(Vec<PathBuf>),
    Duplicate(Vec<PathBuf>),
    Rename(PathBuf),
    ResetAdjustments(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
}

#[cfg(not(target_os = "android"))]
pub(crate) fn desktop_image_context_menu(
    ui: &mut Ui,
    app: &AurawApp,
    context_source_path: &Path,
    context_paths: &[PathBuf],
) -> Option<LibraryCardAction> {
    let selected_count = context_paths.len();
    let action_enabled = !app.library.file_action_in_progress()
        && app.library_batch_export_progress().is_none()
        && app.library_ai_mask_refresh_status().is_none()
        && !context_paths.is_empty();
    let can_paste_adjustments = action_enabled && app.has_copied_adjustments();
    let mut action = None;

    let export_label = if selected_count > 1 {
        "Export selected…"
    } else {
        "Export…"
    };
    if ui
        .add_enabled(action_enabled, egui::Button::new(export_label))
        .clicked()
    {
        action = Some(LibraryCardAction::Export(context_paths.to_vec()));
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(
            action_enabled && selected_count == 1,
            egui::Button::new("Copy adjustments"),
        )
        .clicked()
    {
        action = Some(LibraryCardAction::CopyAdjustments(
            context_source_path.to_path_buf(),
        ));
        ui.close();
    }
    let paste_label = if selected_count > 1 {
        "Paste adjustments to selected"
    } else {
        "Paste adjustments"
    };
    if ui
        .add_enabled(can_paste_adjustments, egui::Button::new(paste_label))
        .on_disabled_hover_text("Copy adjustments from an image first")
        .clicked()
    {
        action = Some(LibraryCardAction::PasteAdjustments(context_paths.to_vec()));
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(action_enabled, egui::Button::new("Copy"))
        .clicked()
    {
        action = Some(LibraryCardAction::Copy(context_paths.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(action_enabled, egui::Button::new("Cut"))
        .clicked()
    {
        action = Some(LibraryCardAction::Cut(context_paths.to_vec()));
        ui.close();
    }
    let duplicate_label = if selected_count > 1 {
        "Duplicate selected (RAW + sidecars)"
    } else {
        "Duplicate (RAW + sidecar)"
    };
    if ui
        .add_enabled(action_enabled, egui::Button::new(duplicate_label))
        .clicked()
    {
        action = Some(LibraryCardAction::Duplicate(context_paths.to_vec()));
        ui.close();
    }
    if ui
        .add_enabled(
            action_enabled && selected_count == 1,
            egui::Button::new("Rename…"),
        )
        .clicked()
    {
        action = context_paths
            .first()
            .cloned()
            .map(LibraryCardAction::Rename);
        ui.close();
    }
    let reset_label = if selected_count > 1 {
        "Reset adjustments for selected"
    } else {
        "Reset all adjustments"
    };
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(format!(
                "{}  {reset_label}",
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE
            )),
        )
        .clicked()
    {
        action = Some(LibraryCardAction::ResetAdjustments(context_paths.to_vec()));
        ui.close();
    }
    ui.separator();
    let delete_label = if selected_count > 1 {
        "Delete selected"
    } else {
        "Delete"
    };
    if ui
        .add_enabled(action_enabled, egui::Button::new(delete_label))
        .clicked()
    {
        action = Some(LibraryCardAction::Delete(context_paths.to_vec()));
        ui.close();
    }

    action
}

#[cfg(not(target_os = "android"))]
pub(crate) fn apply_desktop_image_action(
    ui: &mut Ui,
    app: &mut AurawApp,
    frame: &eframe::Frame,
    action: LibraryCardAction,
) {
    match action {
        LibraryCardAction::Export(paths) => {
            if !paths.is_empty() {
                app.library.export_dialog = Some(LibraryExportDialog {
                    paths,
                    settings: app.export_settings.clone(),
                    format: ExportFormat::Jpeg,
                });
            }
        }
        LibraryCardAction::CopyAdjustments(path) => {
            let status = match app.copy_library_adjustments_from_path(&path) {
                Ok(()) => format!(
                    "Copied adjustments from {}",
                    app.copied_adjustments_source_label().unwrap_or("image")
                ),
                Err(error) => format!("Could not copy adjustments: {error}"),
            };
            app.library.status = status;
        }
        LibraryCardAction::PasteAdjustments(paths) => {
            let (edited_count, failures) = app.library_adjustment_edit_count_paths(&paths);
            if failures.is_empty() {
                if edited_count > 0 {
                    app.library.adjustment_paste_dialog = Some(LibraryAdjustmentPasteDialog {
                        paths,
                        edited_count,
                    });
                } else {
                    apply_library_adjustment_paste(
                        app,
                        paths,
                        crate::sidecar::AdjustmentPasteMode::Merge,
                        ui.ctx(),
                        frame,
                    );
                }
            } else {
                app.library.status = format!(
                    "Could not inspect selected adjustments. {}",
                    failures.join(" · ")
                );
            }
        }
        LibraryCardAction::Copy(paths) => {
            let mode = ImageClipboardMode::Copy;
            let count = paths.len();
            app.library.image_clipboard = Some(ImageClipboard {
                mode,
                content: ImageClipboardContent::Local(paths),
            });
            app.library.cloud_clipboard = None;
            app.library.folder_clipboard = None;
            app.library.status = format!(
                "{} {count} local RAW{}. Choose Paste in any local or cloud folder.",
                if mode == ImageClipboardMode::Copy {
                    "Copied"
                } else {
                    "Cut"
                },
                if count == 1 { "" } else { "s" }
            );
        }
        LibraryCardAction::Cut(paths) => {
            let mode = ImageClipboardMode::Cut;
            let count = paths.len();
            app.library.image_clipboard = Some(ImageClipboard {
                mode,
                content: ImageClipboardContent::Local(paths),
            });
            app.library.cloud_clipboard = None;
            app.library.folder_clipboard = None;
            app.library.status = format!(
                "Cut {count} local RAW{}. Choose Paste in any local or cloud folder.",
                if count == 1 { "" } else { "s" }
            );
        }
        LibraryCardAction::Duplicate(paths) => {
            app.library.clear_selection();
            app.library.duplicate_raws_with_sidecars(paths, ui.ctx());
        }
        LibraryCardAction::Rename(path) => {
            let Some(name) = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
            else {
                app.library.status = "This RAW filename cannot be edited as text.".to_owned();
                return;
            };
            app.library.raw_name_dialog = Some(LibraryRawNameDialog {
                source: path,
                name,
                error: None,
                focus_requested: false,
            });
        }
        LibraryCardAction::ResetAdjustments(paths) => {
            let current_to_reopen = app
                .current_path
                .as_ref()
                .and_then(|current| paths.iter().find(|path| *path == current).cloned());
            if let Some(path) = current_to_reopen.as_deref() {
                app.detach_current_file_for_library_action(path);
            }

            let total = paths.len();
            let mut failures = Vec::new();
            let mut reset_count = 0usize;
            for path in &paths {
                match crate::sidecar::reset_desktop_adjustments(path) {
                    Ok(reset) => {
                        app.library.invalidate_adjustment_thumbnail_for_path(path);
                        if reset {
                            reset_count += 1;
                        }
                    }
                    Err(error) => failures.push(format!("{}: {error}", path.display())),
                }
            }
            app.library.clear_selection();
            app.library.refresh(ui.ctx());
            app.library.status = if failures.is_empty() {
                format!(
                    "Cleared all adjustments for {total} selected {} ({reset_count} changed)",
                    if total == 1 { "image" } else { "images" }
                )
            } else {
                format!(
                    "Cleared all adjustments for {} of {total} selected images. {}",
                    total.saturating_sub(failures.len()),
                    failures.join(" · ")
                )
            };
            if let Some(path) = current_to_reopen {
                app.reload_desktop_library_document_after_reset(path, frame);
            }
        }
        LibraryCardAction::Delete(paths) => {
            let current_target = app
                .current_path
                .as_ref()
                .and_then(|current| paths.iter().find(|path| *path == current).cloned());
            if let Some(path) = current_target.as_deref() {
                app.detach_current_file_for_library_action(path);
            }

            let total = paths.len();
            let mut failures = Vec::new();
            let mut cleanup_warnings = Vec::new();
            let mut deleted_current = false;
            for path in &paths {
                match fs::remove_file(path) {
                    Ok(()) => {
                        if current_target.as_ref() == Some(path) {
                            deleted_current = true;
                        }
                        if let Err(error) = crate::sidecar::remove_desktop_edits(path) {
                            cleanup_warnings.push(format!("{}: {error}", path.display()));
                        }
                    }
                    Err(error) => failures.push(format!("{}: {error}", path.display())),
                }
            }
            if deleted_current {
                app.current_path = None;
            }
            app.library.clear_selection();
            app.library.refresh(ui.ctx());
            let deleted = total.saturating_sub(failures.len());
            app.library.status = if failures.is_empty() && cleanup_warnings.is_empty() {
                format!(
                    "Deleted {deleted} selected {}",
                    if deleted == 1 { "image" } else { "images" }
                )
            } else {
                let mut details = failures;
                details.extend(cleanup_warnings);
                format!(
                    "Deleted {deleted} of {total} selected images. {}",
                    details.join(" · ")
                )
            };
            if !deleted_current {
                if let Some(path) = current_target {
                    app.open_path(path, frame);
                }
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
pub(crate) fn show_desktop_image_action_overlays(
    ui: &mut Ui,
    app: &mut AurawApp,
    frame: &eframe::Frame,
) {
    // These overlays normally live in `Library::show`. Develop's filmstrip now
    // exposes the same image actions, so keep their modal follow-up UI available
    // without forcing a tab switch back to Library.
    let mut paste_action = 0u8;
    if let Some(dialog) = app.library.adjustment_paste_dialog.as_ref() {
        let target_count = dialog.paths.len();
        crate::ui::responsive_popup(egui::Window::new("Paste adjustments"), ui.ctx(), 480.0)
            .id(egui::Id::new("library-adjustment-paste-conflict-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "{} of the {} selected {} already contain edits.",
                    dialog.edited_count,
                    target_count,
                    if target_count == 1 { "image" } else { "images" }
                ));
                ui.add_space(4.0);
                ui.label(
                    "Merge overwrites only the copied categories and preserves every unchecked category already on the destination.",
                );
                ui.label(
                    "Replace clears the destination edit state first, then applies the categories stored in the adjustment clipboard.",
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        paste_action = 1;
                    }
                    if ui.button("Merge").clicked() {
                        paste_action = 2;
                    }
                    if ui.button("Replace").clicked() {
                        paste_action = 3;
                    }
                });
            });
    }
    if paste_action != 0 {
        if let Some(dialog) = app.library.adjustment_paste_dialog.take() {
            match paste_action {
                2 => apply_library_adjustment_paste(
                    app,
                    dialog.paths,
                    crate::sidecar::AdjustmentPasteMode::Merge,
                    ui.ctx(),
                    frame,
                ),
                3 => apply_library_adjustment_paste(
                    app,
                    dialog.paths,
                    crate::sidecar::AdjustmentPasteMode::Replace,
                    ui.ctx(),
                    frame,
                ),
                _ => {}
            }
        }
    }

    let mut refresh_action = 0u8;
    let can_regenerate = app.can_start_library_ai_mask_refresh();
    if let Some(prompt) = app.library.ai_mask_refresh_prompt.as_ref() {
        let target_count = prompt.paths.len();
        crate::ui::responsive_popup(egui::Window::new("Regenerate AI masks?"), ui.ctx(), 460.0)
            .id(egui::Id::new("library-ai-mask-refresh-prompt"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "{} pasted {} contain content-aware masks that belong to the source image.",
                    target_count,
                    if target_count == 1 { "image" } else { "images" }
                ));
                ui.label(
                    "Regenerate them now for each destination image? Mask groups, settings, object strokes, and local adjustments are preserved.",
                );
                if !can_regenerate {
                    ui.label(
                        egui::RichText::new(
                            "Waiting for the current RAW load or edit save to finish…",
                        )
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Not now").clicked() {
                        refresh_action = 1;
                    }
                    if ui
                        .add_enabled(can_regenerate, egui::Button::new("Regenerate"))
                        .clicked()
                    {
                        refresh_action = 2;
                    }
                });
            });
    }
    if refresh_action != 0 {
        if let Some(prompt) = app.library.ai_mask_refresh_prompt.take() {
            if refresh_action == 2 {
                app.start_library_ai_mask_refresh_paths(prompt.paths, frame);
            }
        }
    }

    if let Some((completed, total, failed, current_name)) = app.library_ai_mask_refresh_status() {
        if app.library_ai_mask_refresh_progress_open() {
            let fraction = if total == 0 {
                0.0
            } else {
                (completed as f32 / total as f32).clamp(0.0, 1.0)
            };
            let mut minimize = false;
            let mut cancel = false;
            crate::ui::responsive_popup(
                egui::Window::new("Regenerating AI masks"),
                ui.ctx(),
                360.0,
            )
            .id(egui::Id::new("library-ai-mask-refresh-progress"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(
                    egui::RichText::new(format!("{completed} / {total} AI masks updated")).strong(),
                );
                ui.add_space(6.0);
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .show_percentage()
                        .animate(completed < total),
                );
                if let Some(name) = current_name.as_deref() {
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!("Refreshing {name}…"));
                    });
                }
                if failed > 0 {
                    ui.label(
                        egui::RichText::new(format!(
                            "{failed} {} failed",
                            if failed == 1 { "image" } else { "images" }
                        ))
                        .small()
                        .color(ui.visuals().warn_fg_color),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    minimize = ui.button("Minimize").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            if minimize {
                app.minimize_library_ai_mask_refresh_progress();
            }
            if cancel {
                app.cancel_library_ai_mask_refresh();
            }
        }
    }

    let mut close_export_dialog = false;
    let mut confirm_export = false;
    if let Some(dialog) = app.library.export_dialog.as_mut() {
        let count = dialog.paths.len();
        let export_picker_directory = dialog
            .paths
            .first()
            .and_then(|path| path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf);
        let title = if count == 1 {
            "Export image".to_owned()
        } else {
            format!("Export {count} images")
        };
        crate::ui::responsive_popup(egui::Window::new(title), ui.ctx(), 480.0)
            .id(egui::Id::new("library-export-dialog"))
            .collapsible(false)
            .resizable(true)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    ui.label("Format");
                    ui.selectable_value(&mut dialog.format, ExportFormat::Jpeg, "JPEG");
                    ui.selectable_value(&mut dialog.format, ExportFormat::Png, "PNG");
                    ui.selectable_value(&mut dialog.format, ExportFormat::Tiff, "TIFF");
                });
                match dialog.format {
                    ExportFormat::Jpeg => {
                        dialog.settings.bit_depth = crate::pipeline::ExportBitDepth::Eight;
                    }
                    ExportFormat::Png
                        if dialog.settings.bit_depth
                            == crate::pipeline::ExportBitDepth::Float32Linear =>
                    {
                        dialog.settings.bit_depth = crate::pipeline::ExportBitDepth::Sixteen;
                    }
                    _ => {}
                }
                ui.add_space(6.0);
                crate::ui::sidebar::export_settings_controls(
                    ui,
                    &mut dialog.settings,
                    None,
                    false,
                    export_picker_directory.as_deref(),
                );
                match dialog.format {
                    ExportFormat::Jpeg => {
                        dialog.settings.bit_depth = crate::pipeline::ExportBitDepth::Eight;
                    }
                    ExportFormat::Png
                        if dialog.settings.bit_depth
                            == crate::pipeline::ExportBitDepth::Float32Linear =>
                    {
                        dialog.settings.bit_depth = crate::pipeline::ExportBitDepth::Sixteen;
                    }
                    _ => {}
                }
                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new(if count > 1 {
                        "A destination folder will be selected for the batch. File names are generated from each RAW name."
                    } else {
                        "Choose the output file after pressing Export."
                    })
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_export_dialog = true;
                    }
                    let label = if count == 1 {
                        "Export 1 image…".to_owned()
                    } else {
                        format!("Export {count} images…")
                    };
                    if ui.button(label).clicked() {
                        confirm_export = true;
                    }
                });
            });
    }

    if confirm_export {
        if let Some(dialog) = app.library.export_dialog.clone() {
            if let Some(jobs) = library_export_jobs(&dialog.paths, dialog.format) {
                app.library.clear_selection();
                app.library.export_dialog = None;
                app.start_library_exports(jobs, dialog.settings.clone(), dialog.format, frame);
            }
        }
    } else if close_export_dialog {
        app.library.export_dialog = None;
    }

    show_library_batch_export_progress(ui, app);
}

#[cfg(target_os = "android")]
enum LibraryCardAction {
    Export(Vec<(String, String)>),
    CopyAdjustments((String, String)),
    PasteAdjustments(Vec<(String, String)>),
    Copy(Vec<AndroidImageClipboardItem>),
    Cut(Vec<AndroidImageClipboardItem>),
    Duplicate(Vec<(String, String)>),
    Rename(AndroidImageClipboardItem),
    ResetAdjustments(Vec<(String, String)>),
    Delete(Vec<(String, String)>),
}

#[cfg(target_os = "android")]
fn android_selection_menu(
    ui: &mut Ui,
    selected: &[(LibrarySource, String)],
    action_enabled: bool,
    can_paste_adjustments: bool,
    library_action: &mut Option<LibraryCardAction>,
) {
    let targets = || {
        selected
            .iter()
            .filter_map(|(source, _)| match source {
                LibrarySource::Android {
                    uri, display_name, ..
                } => Some((uri.clone(), display_name.clone())),
                LibrarySource::Cloud(_) => None,
            })
            .collect::<Vec<_>>()
    };
    let clipboard_targets = || {
        selected
            .iter()
            .filter_map(|(source, _)| match source {
                LibrarySource::Android {
                    uri,
                    display_name,
                    bytes,
                    ..
                } => Some(AndroidImageClipboardItem {
                    uri: uri.clone(),
                    display_name: display_name.clone(),
                    bytes: *bytes,
                }),
                LibrarySource::Cloud(_) => None,
            })
            .collect::<Vec<_>>()
    };
    let selected_count = selected.len();

    let export_label = if selected_count > 1 {
        "Export selected…"
    } else {
        "Export…"
    };
    if ui
        .add_enabled(action_enabled, egui::Button::new(export_label))
        .clicked()
    {
        *library_action = Some(LibraryCardAction::Export(targets()));
        ui.close();
    }
    ui.separator();
    if selected_count == 1
        && ui
            .add_enabled(action_enabled, egui::Button::new("Copy adjustments"))
            .clicked()
    {
        if let Some((
            LibrarySource::Android {
                uri, display_name, ..
            },
            _,
        )) = selected.first()
        {
            *library_action = Some(LibraryCardAction::CopyAdjustments((
                uri.clone(),
                display_name.clone(),
            )));
        }
        ui.close();
    }
    let paste_label = if selected_count > 1 {
        "Paste adjustments to selected"
    } else {
        "Paste adjustments"
    };
    if ui
        .add_enabled(
            action_enabled && can_paste_adjustments,
            egui::Button::new(paste_label),
        )
        .on_disabled_hover_text("Copy adjustments from an image first")
        .clicked()
    {
        *library_action = Some(LibraryCardAction::PasteAdjustments(targets()));
        ui.close();
    }
    ui.separator();
    if ui
        .add_enabled(action_enabled, egui::Button::new("Copy"))
        .clicked()
    {
        *library_action = Some(LibraryCardAction::Copy(clipboard_targets()));
        ui.close();
    }
    if ui
        .add_enabled(action_enabled, egui::Button::new("Cut"))
        .clicked()
    {
        *library_action = Some(LibraryCardAction::Cut(clipboard_targets()));
        ui.close();
    }
    let duplicate_label = if selected_count > 1 {
        "Duplicate selected (RAW + sidecars)"
    } else {
        "Duplicate (RAW + sidecar)"
    };
    if ui
        .add_enabled(action_enabled, egui::Button::new(duplicate_label))
        .clicked()
    {
        *library_action = Some(LibraryCardAction::Duplicate(targets()));
        ui.close();
    }
    if selected_count == 1
        && ui
            .add_enabled(action_enabled, egui::Button::new("Rename…"))
            .clicked()
    {
        *library_action = clipboard_targets()
            .into_iter()
            .next()
            .map(LibraryCardAction::Rename);
        ui.close();
    }
    let reset_label = if selected_count > 1 {
        "Reset adjustments for selected"
    } else {
        "Reset all adjustments"
    };
    if ui
        .add_enabled(
            action_enabled,
            egui::Button::new(format!(
                "{}  {reset_label}",
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE
            )),
        )
        .clicked()
    {
        *library_action = Some(LibraryCardAction::ResetAdjustments(targets()));
        ui.close();
    }
    ui.separator();
    let delete_label = if selected_count > 1 {
        "Delete selected"
    } else {
        "Delete"
    };
    if ui
        .add_enabled(action_enabled, egui::Button::new(delete_label))
        .clicked()
    {
        *library_action = Some(LibraryCardAction::Delete(targets()));
        ui.close();
    }
}

#[cfg(not(target_os = "android"))]
fn duplicate_raw_and_sidecar(raw_path: &Path) -> Result<PathBuf, String> {
    let parent = raw_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = raw_path
        .file_stem()
        .map(OsString::from)
        .or_else(|| raw_path.file_name().map(OsString::from))
        .ok_or_else(|| "RAW path has no file name".to_owned())?;
    let extension = raw_path.extension().map(OsString::from);

    for number in 1..=10_000usize {
        let mut file_name = stem.clone();
        if number == 1 {
            file_name.push(" copy");
        } else {
            file_name.push(format!(" copy {number}"));
        }
        if let Some(extension) = &extension {
            file_name.push(".");
            file_name.push(extension);
        }
        let destination = parent.join(file_name);
        if crate::sidecar::sidecar_path_for_raw(&destination).exists() {
            continue;
        }

        match copy_file_create_new(raw_path, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Could not duplicate {}: {error}",
                    raw_path.display()
                ));
            }
        }

        let source_sidecar = crate::sidecar::sidecar_path_for_raw(raw_path);
        if source_sidecar.is_file() {
            let destination_sidecar = crate::sidecar::sidecar_path_for_raw(&destination);
            if let Err(error) = copy_file_create_new(&source_sidecar, &destination_sidecar) {
                let _ = fs::remove_file(&destination);
                let _ = fs::remove_file(&destination_sidecar);
                return Err(format!(
                    "Duplicated RAW cleanup completed after the sidecar copy failed: {error}"
                ));
            }
        }
        if let Err(error) = crate::sidecar::copy_developed_thumbnail_cache(raw_path, &destination) {
            let _ = fs::remove_file(&destination);
            let _ = fs::remove_file(crate::sidecar::sidecar_path_for_raw(&destination));
            let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&destination);
            return Err(format!("Could not copy the developed thumbnail: {error}"));
        }
        return Ok(destination);
    }

    Err("Could not find an unused duplicate file name.".to_owned())
}

#[cfg(not(target_os = "android"))]
fn copy_file_create_new(source: &Path, destination: &Path) -> io::Result<()> {
    let mut input = OpenOptions::new().read(true).open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let result = io::copy(&mut input, &mut output).and_then(|_| output.sync_all());
    if result.is_err() {
        drop(output);
        let _ = fs::remove_file(destination);
    } else if let Ok(metadata) = fs::metadata(source) {
        let _ = fs::set_permissions(destination, metadata.permissions());
    }
    result.map(|_| ())
}

#[cfg(not(target_os = "android"))]
fn copy_raw_bundle_to_folder(
    source_raw: &Path,
    requested_name: &std::ffi::OsStr,
    destination_folder: &Path,
) -> Result<PathBuf, String> {
    let requested_path = Path::new(requested_name);
    if requested_path.file_name() != Some(requested_name)
        || !crate::pipeline::is_supported_raw_path(requested_path)
    {
        return Err("The RAW has an unsafe or unsupported filename.".to_owned());
    }
    if !source_raw.is_file() {
        return Err(format!("{} is no longer a file.", source_raw.display()));
    }
    if !destination_folder.is_dir() {
        return Err(format!(
            "The destination folder {} no longer exists.",
            destination_folder.display()
        ));
    }

    let stem = requested_path
        .file_stem()
        .map(OsString::from)
        .unwrap_or_else(|| requested_name.to_os_string());
    let extension = requested_path.extension().map(OsString::from);
    let source_sidecar = crate::sidecar::sidecar_path_for_raw(source_raw);
    for number in 0..=10_000usize {
        let file_name = if number == 0 {
            requested_name.to_os_string()
        } else {
            let mut candidate = stem.clone();
            candidate.push(format!(" ({number})"));
            if let Some(extension) = &extension {
                candidate.push(".");
                candidate.push(extension);
            }
            candidate
        };
        let destination_raw = destination_folder.join(file_name);
        let destination_sidecar = crate::sidecar::sidecar_path_for_raw(&destination_raw);
        if destination_raw.exists() || destination_sidecar.exists() {
            continue;
        }
        match copy_file_create_new(source_raw, &destination_raw) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("Could not copy {}: {error}", source_raw.display()));
            }
        }
        if source_sidecar.is_file() {
            if let Err(error) = copy_file_create_new(&source_sidecar, &destination_sidecar) {
                let _ = fs::remove_file(&destination_raw);
                let _ = fs::remove_file(&destination_sidecar);
                return Err(format!("Could not copy the matching sidecar: {error}"));
            }
        }
        if let Err(error) =
            crate::sidecar::copy_developed_thumbnail_cache(source_raw, &destination_raw)
        {
            let _ = fs::remove_file(&destination_raw);
            let _ = fs::remove_file(&destination_sidecar);
            let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&destination_raw);
            return Err(format!("Could not copy the developed thumbnail: {error}"));
        }
        if let Err(error) = crate::file_ops::sync_parent_directory(destination_folder) {
            log::warn!(
                "could not sync image paste folder {} after copying {}: {error}",
                destination_folder.display(),
                destination_raw.display()
            );
        }
        return Ok(destination_raw);
    }
    Err(format!(
        "Could not find an unused name for {:?} in {}.",
        requested_name,
        destination_folder.display()
    ))
}

#[cfg(not(target_os = "android"))]
fn remove_local_raw_bundle(raw_path: &Path) -> Result<(), String> {
    fs::remove_file(raw_path).map_err(|error| {
        format!(
            "Could not remove {} after copying it: {error}",
            raw_path.display()
        )
    })?;
    let sidecar = crate::sidecar::sidecar_path_for_raw(raw_path);
    if let Err(error) = fs::remove_file(&sidecar) {
        if error.kind() != io::ErrorKind::NotFound {
            log::warn!(
                "could not remove the old sidecar {} after moving its RAW: {error}",
                sidecar.display()
            );
        }
    }
    if let Err(error) = crate::sidecar::invalidate_developed_thumbnail_cache(raw_path) {
        log::warn!("could not remove the old developed thumbnail after moving a RAW: {error}");
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn rename_raw_bundle(source_raw: &Path, requested_name: &str) -> Result<PathBuf, String> {
    validate_cloud_item_name(requested_name, true)?;
    let parent = source_raw
        .parent()
        .ok_or_else(|| "The RAW has no parent folder.".to_owned())?;
    let destination_raw = parent.join(requested_name);
    if destination_raw == source_raw {
        return Ok(destination_raw);
    }
    let source_sidecar = crate::sidecar::sidecar_path_for_raw(source_raw);
    let destination_sidecar = crate::sidecar::sidecar_path_for_raw(&destination_raw);
    if destination_raw.exists() || destination_sidecar.exists() {
        return Err(format!("{} already exists.", destination_raw.display()));
    }
    let developed_thumbnail = crate::sidecar::load_developed_thumbnail_cache(source_raw, 8192)?;
    fs::rename(source_raw, &destination_raw).map_err(|error| {
        format!(
            "Could not rename {} to {}: {error}",
            source_raw.display(),
            destination_raw.display()
        )
    })?;
    if source_sidecar.is_file() {
        if let Err(error) = fs::rename(&source_sidecar, &destination_sidecar) {
            let rollback = fs::rename(&destination_raw, source_raw);
            return Err(if rollback.is_ok() {
                format!("Could not rename the matching sidecar: {error}")
            } else {
                format!(
                    "The RAW was renamed to {}, but its sidecar could not be renamed: {error}",
                    destination_raw.display()
                )
            });
        }
    }
    if let Some(thumbnail) = developed_thumbnail {
        let thumbnail_result = crate::sidecar::desktop_sidecar_fingerprint(&destination_raw)
            .and_then(|fingerprint| {
                fingerprint.ok_or_else(|| "The renamed RAW's edit sidecar disappeared.".to_owned())
            })
            .and_then(|fingerprint| {
                crate::sidecar::save_developed_thumbnail_cache(
                    &destination_raw,
                    &thumbnail,
                    fingerprint,
                )
                .map(|_| ())
            });
        if let Err(error) = thumbnail_result {
            let sidecar_rollback = if destination_sidecar.is_file() {
                fs::rename(&destination_sidecar, &source_sidecar)
            } else {
                Ok(())
            };
            let raw_rollback = fs::rename(&destination_raw, source_raw);
            let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&destination_raw);
            return Err(if sidecar_rollback.is_ok() && raw_rollback.is_ok() {
                format!("Could not preserve the developed thumbnail while renaming: {error}")
            } else {
                format!(
                    "The RAW was renamed to {}, but its developed thumbnail and rename rollback failed: {error}",
                    destination_raw.display()
                )
            });
        }
    }
    if let Err(error) = crate::sidecar::invalidate_developed_thumbnail_cache(source_raw) {
        log::warn!("could not clear the old thumbnail after renaming a RAW: {error}");
    }
    if let Err(error) = crate::file_ops::sync_parent_directory(parent) {
        log::warn!(
            "could not sync RAW folder {} after renaming {}: {error}",
            parent.display(),
            destination_raw.display()
        );
    }
    Ok(destination_raw)
}

#[cfg(not(target_os = "android"))]
fn import_raw_into_folder(source: &Path, folder: &Path) -> Result<RawImportOutcome, String> {
    let original_name = source
        .file_name()
        .map(OsString::from)
        .ok_or_else(|| format!("{} has no file name", source.display()))?;
    let stem = source
        .file_stem()
        .map(OsString::from)
        .unwrap_or_else(|| original_name.clone());
    let extension = source.extension().map(OsString::from);

    for number in 0..=10_000usize {
        let file_name = if number == 0 {
            original_name.clone()
        } else {
            let mut file_name = stem.clone();
            file_name.push(format!(" ({number})"));
            if let Some(extension) = &extension {
                file_name.push(".");
                file_name.push(extension);
            }
            file_name
        };
        let destination = folder.join(file_name);

        if destination.exists() {
            if same_existing_file(source, &destination) {
                return Ok(RawImportOutcome::AlreadyPresent);
            }
            continue;
        }

        match copy_file_create_new(source, &destination) {
            Ok(()) => {
                if let Err(error) = crate::file_ops::sync_parent_directory(folder) {
                    log::warn!(
                        "could not sync RAW import folder {} after copying {}: {error}",
                        folder.display(),
                        destination.display()
                    );
                }
                return Ok(RawImportOutcome::Imported(destination));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Could not import {} into {}: {error}",
                    source.display(),
                    folder.display()
                ));
            }
        }
    }

    Err(format!(
        "Could not find an unused name for {} in {}",
        source.display(),
        folder.display()
    ))
}

#[cfg(not(target_os = "android"))]
fn same_existing_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(not(target_os = "android"))]
fn validate_folder_name(name: &str) -> Result<OsString, String> {
    if name.is_empty() || name.trim() != name {
        return Err("Folder names cannot be empty or start/end with whitespace.".to_owned());
    }
    let path = Path::new(name);
    let mut components = path.components();
    let Some(std::path::Component::Normal(component)) = components.next() else {
        return Err("Enter a single folder name, without a path.".to_owned());
    };
    if components.next().is_some() {
        return Err("Enter a single folder name, without a path.".to_owned());
    }
    Ok(component.to_os_string())
}

#[cfg(not(target_os = "android"))]
fn canonical_library_directory(
    root: &Path,
    path: &Path,
    allow_root: bool,
) -> Result<PathBuf, String> {
    let root = fs::canonicalize(root)
        .map_err(|error| format!("Could not resolve library root {}: {error}", root.display()))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect folder {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{} is not a real folder", path.display()));
    }
    let resolved = fs::canonicalize(path)
        .map_err(|error| format!("Could not resolve folder {}: {error}", path.display()))?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "Refusing to operate outside the library root: {}",
            path.display()
        ));
    }
    if !allow_root && resolved == root {
        return Err(
            "The top-level library folder cannot be moved, renamed, or deleted.".to_owned(),
        );
    }
    Ok(resolved)
}

#[cfg(not(target_os = "android"))]
fn path_entry_exists(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

#[cfg(not(target_os = "android"))]
fn unique_folder_destination(parent: &Path, name: &std::ffi::OsStr) -> Result<PathBuf, String> {
    for number in 0..=10_000usize {
        let file_name = if number == 0 {
            OsString::from(name)
        } else {
            let mut copy_name = OsString::from(name);
            if number == 1 {
                copy_name.push(" copy");
            } else {
                copy_name.push(format!(" copy {number}"));
            }
            copy_name
        };
        let destination = parent.join(file_name);
        if !path_entry_exists(&destination) {
            return Ok(destination);
        }
    }
    Err(format!(
        "Could not find an unused folder name in {}",
        parent.display()
    ))
}

#[cfg(not(target_os = "android"))]
fn copy_directory_create_new(source: &Path, destination: &Path) -> Result<(), String> {
    let source_metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("Could not inspect {}: {error}", source.display()))?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(format!("{} is not a real folder", source.display()));
    }
    let source_resolved = fs::canonicalize(source)
        .map_err(|error| format!("Could not resolve {}: {error}", source.display()))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| format!("{} has no parent folder", destination.display()))?;
    let destination_parent_resolved = fs::canonicalize(destination_parent).map_err(|error| {
        format!(
            "Could not resolve destination {}: {error}",
            destination_parent.display()
        )
    })?;
    if destination_parent_resolved.starts_with(&source_resolved) {
        return Err("A folder cannot be copied into itself or one of its subfolders.".to_owned());
    }

    fn copy_contents(source: &Path, destination: &Path) -> Result<(), String> {
        for entry in fs::read_dir(source)
            .map_err(|error| format!("Could not read {}: {error}", source.display()))?
        {
            let entry = entry.map_err(|error| {
                format!("Could not read an entry in {}: {error}", source.display())
            })?;
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect {}: {error}", source_path.display()))?;
            if file_type.is_symlink() {
                return Err(format!(
                    "Refusing to follow symbolic link {} while copying a folder",
                    source_path.display()
                ));
            }
            if file_type.is_dir() {
                fs::create_dir(&destination_path).map_err(|error| {
                    format!("Could not create {}: {error}", destination_path.display())
                })?;
                copy_contents(&source_path, &destination_path)?;
                if let Err(error) = crate::file_ops::sync_parent_directory(&destination_path) {
                    log::warn!(
                        "could not sync copied folder {}: {error}",
                        destination_path.display()
                    );
                }
            } else if file_type.is_file() {
                copy_file_create_new(&source_path, &destination_path).map_err(|error| {
                    format!("Could not copy {}: {error}", source_path.display())
                })?;
            } else {
                return Err(format!(
                    "Refusing to copy special filesystem entry {}",
                    source_path.display()
                ));
            }
        }
        Ok(())
    }

    fs::create_dir(destination)
        .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;
    if let Err(error) = copy_contents(source, destination) {
        if let Err(cleanup_error) = fs::remove_dir_all(destination) {
            return Err(format!(
                "{error} Cleanup of incomplete folder {} also failed: {cleanup_error}",
                destination.display()
            ));
        }
        return Err(error);
    }
    if let Err(error) = crate::file_ops::sync_parent_directory(destination_parent) {
        log::warn!(
            "could not sync folder {} after copying {}: {error}",
            destination_parent.display(),
            destination.display()
        );
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn import_folder_into_library(source: &Path, folder: &Path) -> Result<PathBuf, String> {
    let source_name = source
        .file_name()
        .ok_or_else(|| format!("{} has no folder name", source.display()))?;
    let destination = unique_folder_destination(folder, source_name)?;
    copy_directory_create_new(source, &destination).map_err(|error| {
        format!(
            "Could not import folder {} into {}: {error}",
            source.display(),
            folder.display()
        )
    })?;
    Ok(destination)
}

#[cfg(not(target_os = "android"))]
fn folder_operation_progress_status(operation: &LibraryFolderOperation) -> String {
    match operation {
        LibraryFolderOperation::Create { parent, .. } => {
            format!("Creating a folder in {}…", parent.display())
        }
        LibraryFolderOperation::Copy { source, .. } => {
            format!("Copying folder {}…", source.display())
        }
        LibraryFolderOperation::Move { source, .. } => {
            format!("Moving folder {}…", source.display())
        }
        LibraryFolderOperation::Delete { target, .. } => {
            format!("Deleting folder {}…", target.display())
        }
    }
}

#[cfg(not(target_os = "android"))]
fn run_folder_operation(
    operation: LibraryFolderOperation,
) -> Result<LibraryFolderOperationResult, String> {
    match operation {
        LibraryFolderOperation::Create { root, parent, name } => {
            canonical_library_directory(&root, &parent, true)?;
            let name = validate_folder_name(&name)?;
            let destination = parent.join(name);
            fs::create_dir(&destination).map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    format!("A folder named {} already exists.", destination.display())
                } else {
                    format!("Could not create folder {}: {error}", destination.display())
                }
            })?;
            if let Err(error) = crate::file_ops::sync_parent_directory(&parent) {
                log::warn!("could not sync folder {}: {error}", parent.display());
            }
            Ok(LibraryFolderOperationResult::Created(destination))
        }
        LibraryFolderOperation::Copy {
            root,
            source,
            destination_parent,
        } => {
            canonical_library_directory(&root, &source, false)?;
            canonical_library_directory(&root, &destination_parent, true)?;
            let name = source
                .file_name()
                .ok_or_else(|| format!("{} has no folder name", source.display()))?;
            let destination = unique_folder_destination(&destination_parent, name)?;
            copy_directory_create_new(&source, &destination)?;
            Ok(LibraryFolderOperationResult::Copied {
                source,
                destination,
            })
        }
        LibraryFolderOperation::Move {
            root,
            source,
            destination_parent,
            new_name,
        } => {
            let source_resolved = canonical_library_directory(&root, &source, false)?;
            let destination_parent_resolved =
                canonical_library_directory(&root, &destination_parent, true)?;
            if destination_parent_resolved.starts_with(&source_resolved) {
                return Err(
                    "A folder cannot be moved into itself or one of its subfolders.".to_owned(),
                );
            }
            let name = match new_name {
                Some(name) => validate_folder_name(&name)?,
                None => source
                    .file_name()
                    .map(OsString::from)
                    .ok_or_else(|| format!("{} has no folder name", source.display()))?,
            };
            let destination = destination_parent.join(name);
            if destination == source {
                return Err("The folder is already in that location.".to_owned());
            }
            if path_entry_exists(&destination) {
                return Err(format!(
                    "A folder named {} already exists; nothing was overwritten.",
                    destination.display()
                ));
            }
            fs::rename(&source, &destination).map_err(|error| {
                format!(
                    "Could not move {} to {}: {error}",
                    source.display(),
                    destination.display()
                )
            })?;
            if let Some(parent) = source.parent() {
                if let Err(error) = crate::file_ops::sync_parent_directory(parent) {
                    log::warn!("could not sync source folder {}: {error}", parent.display());
                }
            }
            if let Err(error) = crate::file_ops::sync_parent_directory(&destination_parent) {
                log::warn!(
                    "could not sync destination folder {}: {error}",
                    destination_parent.display()
                );
            }
            Ok(LibraryFolderOperationResult::Moved {
                source,
                destination,
            })
        }
        LibraryFolderOperation::Delete { root, target } => {
            canonical_library_directory(&root, &target, false)?;
            fs::remove_dir_all(&target).map_err(|error| {
                format!("Could not delete folder {}: {error}", target.display())
            })?;
            if let Some(parent) = target.parent() {
                if let Err(error) = crate::file_ops::sync_parent_directory(parent) {
                    log::warn!(
                        "could not sync folder {} after deletion: {error}",
                        parent.display()
                    );
                }
            }
            Ok(LibraryFolderOperationResult::Deleted(target))
        }
    }
}

#[cfg(not(target_os = "android"))]
fn raw_import_status(result: &RawImportResult) -> String {
    let mut parts = Vec::new();
    if !result.imported.is_empty() {
        parts.push(format!(
            "Imported {} RAW {}",
            result.imported.len(),
            if result.imported.len() == 1 {
                "file"
            } else {
                "files"
            }
        ));
    }
    if !result.imported_folders.is_empty() {
        parts.push(format!(
            "Imported {} {}",
            result.imported_folders.len(),
            if result.imported_folders.len() == 1 {
                "folder"
            } else {
                "folders"
            }
        ));
    }
    if result.already_present > 0 {
        parts.push(format!("{} already in this folder", result.already_present));
    }
    if result.ignored > 0 {
        parts.push(format!(
            "ignored {} unsupported or inaccessible {}",
            result.ignored,
            if result.ignored == 1 { "item" } else { "items" }
        ));
    }
    if !result.failures.is_empty() {
        parts.push(format!("{} failed", result.failures.len()));
        parts.push(result.failures.join(" · "));
    }
    if parts.is_empty() {
        "No RAW files or folders were imported.".to_owned()
    } else {
        format!("{}.", parts.join("; "))
    }
}

#[cfg(not(target_os = "android"))]
fn unique_library_export_path(
    folder: &Path,
    source: &Path,
    format: ExportFormat,
    reserved: &mut HashSet<PathBuf>,
) -> PathBuf {
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("auraw-export");
    let base = format!("{stem}-auraw");
    let mut index = 1usize;
    loop {
        let name = if index == 1 {
            format!("{base}.{}", format.extension())
        } else {
            format!("{base}-{index}.{}", format.extension())
        };
        let candidate = folder.join(name);
        if !candidate.exists() && reserved.insert(candidate.clone()) {
            return candidate;
        }
        index += 1;
    }
}

#[cfg(not(target_os = "android"))]
fn library_export_jobs(paths: &[PathBuf], format: ExportFormat) -> Option<Vec<(PathBuf, PathBuf)>> {
    if paths.is_empty() {
        return None;
    }
    if paths.len() == 1 {
        let source = &paths[0];
        let default_name = source
            .file_stem()
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}-auraw.{}", format.extension()))
            .unwrap_or_else(|| format!("auraw-export.{}", format.extension()));
        let mut dialog = rfd::FileDialog::new().set_file_name(default_name);
        if let Some(parent) = source
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            dialog = dialog.set_directory(parent);
        }
        dialog = match format {
            ExportFormat::Png => dialog.add_filter("PNG image", &["png"]),
            ExportFormat::Jpeg => dialog.add_filter("JPEG image", &["jpg", "jpeg"]),
            ExportFormat::Tiff => dialog.add_filter("TIFF image", &["tif", "tiff"]),
        };
        let mut destination = dialog.save_file()?;
        let valid_extension = destination
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| match format {
                ExportFormat::Png => extension.eq_ignore_ascii_case("png"),
                ExportFormat::Jpeg => {
                    extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
                }
                ExportFormat::Tiff => {
                    extension.eq_ignore_ascii_case("tif") || extension.eq_ignore_ascii_case("tiff")
                }
            });
        if !valid_extension {
            destination.set_extension(format.extension());
        }
        return Some(vec![(source.clone(), destination)]);
    }

    let mut dialog = rfd::FileDialog::new();
    if let Some(parent) = paths
        .first()
        .and_then(|path| path.parent())
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        dialog = dialog.set_directory(parent);
    }
    let folder = dialog.pick_folder()?;
    let mut reserved = HashSet::new();
    Some(
        paths
            .iter()
            .map(|source| {
                let destination =
                    unique_library_export_path(&folder, source, format, &mut reserved);
                (source.clone(), destination)
            })
            .collect(),
    )
}

#[cfg(not(target_os = "android"))]
fn apply_library_adjustment_paste(
    app: &mut AurawApp,
    paths: Vec<PathBuf>,
    mode: crate::sidecar::AdjustmentPasteMode,
    context: &egui::Context,
    frame: &eframe::Frame,
) {
    let total = paths.len();
    let (completed, ai_refresh, failures) =
        app.paste_library_adjustments_to_paths(&paths, mode, frame);
    app.library.clear_selection();
    app.library.refresh(context);
    app.library.status = if failures.is_empty() {
        format!(
            "Pasted adjustments to {} selected {}",
            completed.len(),
            if completed.len() == 1 {
                "image"
            } else {
                "images"
            }
        )
    } else {
        format!(
            "Pasted adjustments to {} of {total} selected images. {}",
            completed.len(),
            failures.join(" · ")
        )
    };
    app.library.ai_mask_refresh_prompt =
        (!ai_refresh.is_empty()).then_some(LibraryAiMaskRefreshPrompt { paths: ai_refresh });
}

#[cfg(target_os = "android")]
fn apply_library_adjustment_paste(
    app: &mut AurawApp,
    targets: Vec<(String, String)>,
    mode: crate::sidecar::AdjustmentPasteMode,
    context: &egui::Context,
    frame: &eframe::Frame,
) {
    let total = targets.len();
    let (completed, ai_refresh, failures) =
        app.paste_library_adjustments_to_android(&targets, mode, frame);
    app.library.clear_selection();
    crate::android::set_back_navigation_active(false);
    app.library.refresh(context);
    app.library.status = if failures.is_empty() {
        format!(
            "Pasted adjustments to {} selected {}",
            completed.len(),
            if completed.len() == 1 {
                "image"
            } else {
                "images"
            }
        )
    } else {
        format!(
            "Pasted adjustments to {} of {total} selected images. {}",
            completed.len(),
            failures.join(" · ")
        )
    };
    app.library.ai_mask_refresh_prompt =
        (!ai_refresh.is_empty()).then_some(LibraryAiMaskRefreshPrompt {
            targets: ai_refresh,
        });
}

#[cfg(target_os = "android")]
fn prepare_android_cloud_adjustment_paste(
    ui: &mut Ui,
    app: &mut AurawApp,
    paths: Vec<PathBuf>,
    frame: &eframe::Frame,
) {
    let (edited_count, failures) = app.library_adjustment_edit_count_paths(&paths);
    if !failures.is_empty() {
        app.library.status = format!(
            "Could not inspect selected cloud adjustments. {}",
            failures.join(" · ")
        );
    } else if edited_count > 0 {
        app.library.adjustment_paste_dialog = Some(LibraryAdjustmentPasteDialog {
            targets: AndroidAdjustmentPasteTargets::Cloud(paths),
            edited_count,
        });
    } else {
        apply_android_cloud_adjustment_paste(
            app,
            paths,
            crate::sidecar::AdjustmentPasteMode::Merge,
            ui.ctx(),
            frame,
        );
    }
}

#[cfg(target_os = "android")]
fn apply_android_cloud_adjustment_paste(
    app: &mut AurawApp,
    paths: Vec<PathBuf>,
    mode: crate::sidecar::AdjustmentPasteMode,
    context: &egui::Context,
    frame: &eframe::Frame,
) {
    let total = paths.len();
    let (completed, ai_refresh, failures) =
        app.paste_library_adjustments_to_paths(&paths, mode, frame);
    app.library.clear_selection();
    crate::android::set_back_navigation_active(false);
    app.library.refresh(context);
    let mut status = if failures.is_empty() {
        format!(
            "Pasted adjustments to {} selected {}",
            completed.len(),
            if completed.len() == 1 {
                "cloud image"
            } else {
                "cloud images"
            }
        )
    } else {
        format!(
            "Pasted adjustments to {} of {total} selected cloud images. {}",
            completed.len(),
            failures.join(" · ")
        )
    };
    if !ai_refresh.is_empty() {
        status.push_str(
            " Content-aware masks were marked for regeneration and can be refreshed when each cloud RAW is opened.",
        );
    }
    app.library.status = status;
}

fn show_library_batch_export_progress(ui: &mut Ui, app: &mut AurawApp) {
    let mut cancel = false;
    #[cfg(not(target_os = "android"))]
    let mut minimize = false;

    if let Some((completed, total, failed, current_name, cancelling)) =
        app.library_batch_export_status()
    {
        if app.library_batch_export_progress_open() {
            let exported = completed.saturating_sub(failed);
            let overall_fraction = app.library_batch_export_overall_fraction().unwrap_or(0.0);

            crate::ui::responsive_popup(egui::Window::new("Exporting images"), ui.ctx(), 420.0)
                .id(egui::Id::new("library-batch-export-progress-dialog"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{exported} / {total} exported")).strong(),
                    );
                    ui.add_space(6.0);
                    ui.add(
                        egui::ProgressBar::new(overall_fraction)
                            .show_percentage()
                            .animate(!cancelling),
                    );
                    ui.add_space(6.0);

                    if let Some(name) = current_name.as_deref() {
                        let phase = match app.library_batch_export_tile_progress() {
                            Some((tiles_done, tiles_total))
                                if tiles_total > 0 && tiles_done >= tiles_total =>
                            {
                                "Finalizing"
                            }
                            Some((_, tiles_total)) if tiles_total > 0 => "Exporting",
                            _ => "Preparing",
                        };
                        ui.label(format!("{phase} {name}…"));
                    }
                    if failed > 0 {
                        ui.label(
                            egui::RichText::new(format!(
                                "{failed} {} failed",
                                if failed == 1 { "image" } else { "images" }
                            ))
                            .small()
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                    if cancelling {
                        ui.label(
                            egui::RichText::new("Cancelling after the current image finishes…")
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        #[cfg(not(target_os = "android"))]
                        if ui.button("Minimize").clicked() {
                            minimize = true;
                        }
                        if ui
                            .add_enabled(!cancelling, egui::Button::new("Cancel"))
                            .clicked()
                        {
                            cancel = true;
                        }
                    });
                });
        }
    }

    #[cfg(not(target_os = "android"))]
    if minimize {
        app.minimize_library_batch_export_progress();
    }
    if cancel {
        app.cancel_library_batch_export();
    }
}

fn cloud_folder_contains(
    folders: &[crate::cloud::CloudFolder],
    ancestor_id: &str,
    candidate_id: &str,
) -> bool {
    let mut current = candidate_id;
    let mut remaining = folders.len();
    while current != crate::cloud::CLOUD_ROOT_FOLDER_ID && remaining > 0 {
        if current == ancestor_id {
            return true;
        }
        let Some(folder) = folders.iter().find(|folder| folder.id == current) else {
            return false;
        };
        current = &folder.parent_id;
        remaining -= 1;
    }
    ancestor_id == crate::cloud::CLOUD_ROOT_FOLDER_ID
}

#[allow(clippy::too_many_arguments)]
fn show_cloud_folder_node(
    ui: &mut Ui,
    folder: Option<&crate::cloud::CloudFolder>,
    folders: &[crate::cloud::CloudFolder],
    selected_folder_id: &str,
    clipboard: Option<&CloudClipboard>,
    image_clipboard: Option<&ImageClipboard>,
    action_in_progress: bool,
    expanded_folders: &mut HashSet<String>,
    requested_action: &mut Option<CloudFolderUiAction>,
) {
    let folder_id = folder
        .map(|folder| folder.id.as_str())
        .unwrap_or(crate::cloud::CLOUD_ROOT_FOLDER_ID);
    let name = folder.map(|folder| folder.name.as_str()).unwrap_or("Cloud");
    let children = folders
        .iter()
        .filter(|candidate| candidate.parent_id == folder_id)
        .collect::<Vec<_>>();
    let has_children = !children.is_empty();
    let expanded = expanded_folders.contains(folder_id);
    let selected = selected_folder_id == folder_id;
    let is_root = folder.is_none();

    ui.push_id(("cloud-folder", folder_id), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if has_children {
                let caret = if expanded {
                    egui_phosphor::regular::CARET_DOWN
                } else {
                    egui_phosphor::regular::CARET_RIGHT
                };
                if ui
                    .add_sized(
                        egui::vec2(24.0, 28.0),
                        egui::Button::new(egui::RichText::new(caret).size(12.0)).frame(false),
                    )
                    .clicked()
                {
                    if expanded {
                        expanded_folders.remove(folder_id);
                    } else {
                        expanded_folders.insert(folder_id.to_owned());
                    }
                }
            } else {
                ui.allocate_space(egui::vec2(24.0, 28.0));
            }

            let icon = if is_root {
                egui_phosphor::regular::CLOUD
            } else if expanded && has_children {
                egui_phosphor::regular::FOLDER_OPEN
            } else {
                egui_phosphor::regular::FOLDER
            };
            let response = ui.add(
                egui::Button::selectable(selected, egui::RichText::new(format!("{icon}  {name}")))
                    .sense(Sense::click_and_drag()),
            );
            if response.clicked() {
                *requested_action = Some(CloudFolderUiAction::Select(folder_id.to_owned()));
            }
            if let Some(folder) = folder {
                response.dnd_set_drag_payload(CloudFolderDrag(folder.id.clone()));
            }

            response.context_menu(|ui| {
                let enabled = !action_in_progress;
                if ui
                    .add_enabled(enabled, egui::Button::new("New Folder…"))
                    .clicked()
                {
                    *requested_action = Some(CloudFolderUiAction::New(folder_id.to_owned()));
                    ui.close();
                }
                let paste_label = if let Some(clipboard) = image_clipboard {
                    clipboard.paste_label()
                } else {
                    match clipboard.map(|clipboard| &clipboard.content) {
                        Some(CloudClipboardContent::Folder(folder)) => {
                            format!("Paste “{}”", folder.name)
                        }
                        None => "Paste".to_owned(),
                    }
                };
                if ui
                    .add_enabled(
                        enabled && (clipboard.is_some() || image_clipboard.is_some()),
                        egui::Button::new(paste_label),
                    )
                    .clicked()
                {
                    *requested_action = Some(CloudFolderUiAction::Paste(folder_id.to_owned()));
                    ui.close();
                }
                ui.separator();
                if let Some(folder) = folder {
                    if ui
                        .add_enabled(enabled, egui::Button::new("Copy Folder"))
                        .clicked()
                    {
                        *requested_action = Some(CloudFolderUiAction::Copy(folder.clone()));
                        ui.close();
                    }
                    if ui
                        .add_enabled(enabled, egui::Button::new("Cut Folder"))
                        .clicked()
                    {
                        *requested_action = Some(CloudFolderUiAction::Cut(folder.clone()));
                        ui.close();
                    }
                    if ui
                        .add_enabled(enabled, egui::Button::new("Rename Folder…"))
                        .clicked()
                    {
                        *requested_action = Some(CloudFolderUiAction::Rename(folder.clone()));
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .add_enabled(enabled, egui::Button::new("Delete Folder…"))
                        .clicked()
                    {
                        *requested_action = Some(CloudFolderUiAction::Delete(folder.clone()));
                        ui.close();
                    }
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Refresh Cloud"))
                    .clicked()
                {
                    *requested_action = Some(CloudFolderUiAction::Refresh);
                    ui.close();
                }
            });

            if let Some(payload) = response.dnd_hover_payload::<CloudFolderDrag>() {
                let source_id = &payload.0;
                let can_drop = !action_in_progress
                    && source_id != folder_id
                    && !cloud_folder_contains(folders, source_id, folder_id);
                if can_drop {
                    ui.painter().rect_stroke(
                        response.rect.expand(2.0),
                        3.0,
                        Stroke::new(2.0, ui.visuals().selection.stroke.color),
                        StrokeKind::Outside,
                    );
                    if let Some(payload) = response.dnd_release_payload::<CloudFolderDrag>() {
                        if let Some(source) = folders
                            .iter()
                            .find(|candidate| candidate.id == payload.0)
                            .cloned()
                        {
                            *requested_action = Some(CloudFolderUiAction::Move {
                                folder: source,
                                destination_parent_id: folder_id.to_owned(),
                            });
                        }
                    }
                }
            }
        });

        if expanded {
            ui.indent("cloud-children", |ui| {
                for child in children {
                    show_cloud_folder_node(
                        ui,
                        Some(child),
                        folders,
                        selected_folder_id,
                        clipboard,
                        image_clipboard,
                        action_in_progress,
                        expanded_folders,
                        requested_action,
                    );
                }
            });
        }
    });
}

fn apply_cloud_folder_ui_action(
    app: &mut AurawApp,
    action: CloudFolderUiAction,
    context: &egui::Context,
) {
    match action {
        CloudFolderUiAction::Select(folder_id) => {
            app.select_cloud_library_folder(folder_id);
        }
        CloudFolderUiAction::New(parent_id) => {
            app.library.cloud_name_dialog = Some(CloudNameDialog {
                kind: CloudNameDialogKind::CreateFolder { parent_id },
                name: String::new(),
                error: None,
                focus_requested: false,
            });
        }
        CloudFolderUiAction::Copy(folder) => {
            app.library.cloud_clipboard = Some(CloudClipboard {
                mode: CloudClipboardMode::Copy,
                content: CloudClipboardContent::Folder(folder.clone()),
            });
            app.library.image_clipboard = None;
            #[cfg(not(target_os = "android"))]
            {
                app.library.folder_clipboard = None;
            }
            app.library.status = format!(
                "Copied cloud folder {}. Choose Paste in a destination.",
                folder.name
            );
        }
        CloudFolderUiAction::Cut(folder) => {
            app.library.cloud_clipboard = Some(CloudClipboard {
                mode: CloudClipboardMode::Cut,
                content: CloudClipboardContent::Folder(folder.clone()),
            });
            app.library.image_clipboard = None;
            #[cfg(not(target_os = "android"))]
            {
                app.library.folder_clipboard = None;
            }
            app.library.status = format!(
                "Cut cloud folder {}. Choose Paste in a destination.",
                folder.name
            );
        }
        CloudFolderUiAction::Paste(destination_folder_id) => {
            paste_cloud_clipboard(app, destination_folder_id, context);
        }
        CloudFolderUiAction::Rename(folder) => {
            app.library.cloud_name_dialog = Some(CloudNameDialog {
                name: folder.name.clone(),
                kind: CloudNameDialogKind::RenameFolder { folder },
                error: None,
                focus_requested: false,
            });
        }
        CloudFolderUiAction::Delete(folder) => {
            app.library.cloud_delete_confirmation = Some(CloudDeleteTarget::Folder(folder));
        }
        CloudFolderUiAction::Move {
            folder,
            destination_parent_id,
        } => app.library.start_cloud_action(
            CloudActionRequest::UpdateFolder {
                name: folder.name.clone(),
                folder,
                parent_id: destination_parent_id,
                clear_clipboard: false,
            },
            context,
        ),
        CloudFolderUiAction::Refresh => {
            if app.library.cloud_trash_open {
                app.library.refresh(context);
            } else {
                app.show_library_view(LibraryView::Cloud);
            }
        }
    }
}

#[cfg(target_os = "android")]
#[allow(clippy::too_many_arguments)]
fn show_android_library_folder_node(
    ui: &mut Ui,
    path: &str,
    name: &str,
    children_by_parent: &HashMap<&str, Vec<&crate::android::LibraryFolder>>,
    selected_folder: &str,
    action_in_progress: bool,
    expanded_folders: &mut HashSet<String>,
    requested_action: &mut Option<AndroidLibraryFolderUiAction>,
) {
    let children = children_by_parent
        .get(path)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let has_children = !children.is_empty();
    let expanded = expanded_folders.contains(path);
    let selected = selected_folder == path;

    ui.push_id(("android-library-folder", path), |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if has_children {
                let caret = if expanded {
                    egui_phosphor::regular::CARET_DOWN
                } else {
                    egui_phosphor::regular::CARET_RIGHT
                };
                if ui
                    .add_sized(
                        crate::ui::theme::toolbar_icon_size(),
                        egui::Button::new(egui::RichText::new(caret).size(13.0)).frame(false),
                    )
                    .clicked()
                {
                    if expanded {
                        expanded_folders.remove(path);
                    } else {
                        expanded_folders.insert(path.to_owned());
                    }
                }
            } else {
                ui.allocate_space(crate::ui::theme::toolbar_icon_size());
            }

            let icon = if expanded && has_children {
                egui_phosphor::regular::FOLDER_OPEN
            } else {
                egui_phosphor::regular::FOLDER
            };
            let response = ui.add_enabled_ui(!action_in_progress, |ui| {
                ui.add_sized(
                    [
                        ui.available_width().max(80.0),
                        crate::ui::theme::CONTROL_HEIGHT,
                    ],
                    egui::Button::selectable(
                        selected,
                        egui::RichText::new(format!("{icon}  {name}")),
                    ),
                )
            });
            if response.inner.clicked() {
                *requested_action = Some(AndroidLibraryFolderUiAction::Select(path.to_owned()));
            }
            response.inner.context_menu(|ui| {
                if ui.button("New folder here…").clicked() {
                    *requested_action = Some(AndroidLibraryFolderUiAction::New(path.to_owned()));
                    ui.close();
                }
                if ui.button("Refresh folders").clicked() {
                    *requested_action = Some(AndroidLibraryFolderUiAction::Refresh);
                    ui.close();
                }
            });
        });

        if expanded {
            ui.indent("android-library-children", |ui| {
                for child in children {
                    show_android_library_folder_node(
                        ui,
                        &child.path,
                        &child.name,
                        children_by_parent,
                        selected_folder,
                        action_in_progress,
                        expanded_folders,
                        requested_action,
                    );
                }
            });
        }
    });
}

#[cfg(target_os = "android")]
fn apply_android_library_folder_ui_action(
    app: &mut AurawApp,
    action: AndroidLibraryFolderUiAction,
    context: &egui::Context,
) {
    match action {
        AndroidLibraryFolderUiAction::Select(folder) => {
            app.select_android_library_folder(folder);
            app.set_library_folder_sidebar_open(false);
        }
        AndroidLibraryFolderUiAction::New(parent) => {
            app.library.android_folder_name_dialog = Some(AndroidLibraryFolderNameDialog {
                parent,
                name: String::new(),
                error: None,
                focus_requested: false,
            });
        }
        AndroidLibraryFolderUiAction::Refresh => app.library.refresh(context),
    }
}

#[cfg(target_os = "android")]
fn show_android_library_folder_dialog(ui: &mut Ui, app: &mut AurawApp) {
    let mut close = false;
    let mut create = None;
    if let Some(dialog) = app.library.android_folder_name_dialog.as_mut() {
        crate::ui::responsive_popup(egui::Window::new("New folder"), ui.ctx(), 380.0)
            .id(egui::Id::new("android-library-folder-name-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label("Folder name");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(f32::INFINITY)
                        .id_source("android-library-folder-name-input"),
                );
                if !dialog.focus_requested {
                    response.request_focus();
                    dialog.focus_requested = true;
                }
                if let Some(error) = dialog.error.as_deref() {
                    ui.label(
                        egui::RichText::new(error)
                            .small()
                            .color(ui.visuals().error_fg_color),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let enter = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if ui.button("Create").clicked() || enter {
                        create = Some((dialog.parent.clone(), dialog.name.clone()));
                    }
                });
            });
    }
    if close {
        app.library.android_folder_name_dialog = None;
    }
    if let Some((parent, name)) = create {
        match crate::android::create_library_folder(&app.library.android_app, &parent, &name) {
            Ok(folder) => {
                app.library.android_folder_name_dialog = None;
                app.library.android_expanded_folders.insert(parent);
                app.library.status = format!("Created folder {folder}");
                app.library.refresh(ui.ctx());
            }
            Err(error) => {
                if let Some(dialog) = app.library.android_folder_name_dialog.as_mut() {
                    dialog.error = Some(error);
                }
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
fn show_library_folder_node(
    ui: &mut Ui,
    node: &LibraryFolderNode,
    root_folder: &Path,
    selected_folder: Option<&Path>,
    clipboard: Option<&LibraryFolderClipboard>,
    image_clipboard: Option<&ImageClipboard>,
    action_in_progress: bool,
    expanded_folders: &mut HashSet<PathBuf>,
    requested_folder: &mut Option<PathBuf>,
    requested_action: &mut Option<LibraryFolderUiAction>,
) {
    let has_children = !node.children.is_empty();
    let expanded = expanded_folders.contains(&node.path);
    let selected = selected_folder == Some(node.path.as_path());

    ui.push_id(&node.path, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if has_children {
                let caret = if expanded {
                    egui_phosphor::regular::CARET_DOWN
                } else {
                    egui_phosphor::regular::CARET_RIGHT
                };
                if ui
                    .add_sized(
                        egui::vec2(24.0, 28.0),
                        egui::Button::new(egui::RichText::new(caret).size(12.0)).frame(false),
                    )
                    .on_hover_text(if expanded {
                        "Collapse folder"
                    } else {
                        "Expand folder"
                    })
                    .clicked()
                {
                    if expanded {
                        expanded_folders.remove(&node.path);
                    } else {
                        expanded_folders.insert(node.path.clone());
                    }
                }
            } else {
                ui.allocate_space(egui::vec2(24.0, 28.0));
            }

            let folder_icon = if expanded && has_children {
                egui_phosphor::regular::FOLDER_OPEN
            } else {
                egui_phosphor::regular::FOLDER
            };
            // Sensing both clicks and drags makes egui wait until the pointer
            // crosses its real drag threshold. A simple press/release remains
            // normal folder navigation and never creates a drag payload.
            let response = ui
                .add(
                    egui::Button::selectable(
                        selected,
                        egui::RichText::new(format!("{folder_icon}  {}", node.name)),
                    )
                    .sense(Sense::click_and_drag()),
                )
                .on_hover_text(format!(
                    "{}\nDrag onto another folder to move",
                    node.path.display()
                ));
            response.dnd_set_drag_payload(LibraryFolderDrag(node.path.clone()));
            if response.clicked() {
                *requested_folder = Some(node.path.clone());
            }

            let is_root = node.path == root_folder;
            response.context_menu(|ui| {
                let enabled = !action_in_progress;
                if ui
                    .add_enabled(enabled, egui::Button::new("New Folder…"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::New(node.path.clone()));
                    ui.close();
                }

                let paste_label = image_clipboard.map_or_else(
                    || {
                        clipboard
                            .and_then(|clipboard| clipboard.path.file_name())
                            .map(|name| format!("Paste “{}”", name.to_string_lossy()))
                            .unwrap_or_else(|| "Paste".to_owned())
                    },
                    ImageClipboard::paste_label,
                );
                if ui
                    .add_enabled(
                        enabled && (clipboard.is_some() || image_clipboard.is_some()),
                        egui::Button::new(paste_label),
                    )
                    .clicked()
                {
                    *requested_action = Some(if image_clipboard.is_some() {
                        LibraryFolderUiAction::PasteImages(node.path.clone())
                    } else {
                        LibraryFolderUiAction::Paste(node.path.clone())
                    });
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(enabled && !is_root, egui::Button::new("Copy Folder"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Copy(node.path.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(enabled && !is_root, egui::Button::new("Cut Folder"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Cut(node.path.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(enabled && !is_root, egui::Button::new("Rename Folder…"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Rename(node.path.clone()));
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(enabled && !is_root, egui::Button::new("Delete Folder…"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Delete(node.path.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Refresh Folders"))
                    .clicked()
                {
                    *requested_action = Some(LibraryFolderUiAction::Refresh);
                    ui.close();
                }
            });

            if let Some(payload) = response.dnd_hover_payload::<LibraryFolderDrag>() {
                let source = &payload.0;
                let can_drop = !action_in_progress
                    && source != root_folder
                    && source != &node.path
                    && !node.path.starts_with(source);
                if can_drop {
                    ui.painter().rect_stroke(
                        response.rect.expand(2.0),
                        3.0,
                        Stroke::new(2.0, ui.visuals().selection.stroke.color),
                        StrokeKind::Outside,
                    );
                    if let Some(payload) = response.dnd_release_payload::<LibraryFolderDrag>() {
                        *requested_action = Some(LibraryFolderUiAction::Move {
                            source: payload.0.clone(),
                            destination_parent: node.path.clone(),
                        });
                    }
                }
            }
        });

        if expanded {
            ui.indent("children", |ui| {
                for child in &node.children {
                    show_library_folder_node(
                        ui,
                        child,
                        root_folder,
                        selected_folder,
                        clipboard,
                        image_clipboard,
                        action_in_progress,
                        expanded_folders,
                        requested_folder,
                        requested_action,
                    );
                }
            });
        }
    });
}

#[cfg(not(target_os = "android"))]
fn apply_library_folder_ui_action(
    app: &mut AurawApp,
    action: LibraryFolderUiAction,
    context: &egui::Context,
) {
    let Some(root) = app.library.root_folder.clone() else {
        return;
    };
    match action {
        LibraryFolderUiAction::New(parent) => {
            app.library.folder_name_dialog = Some(LibraryFolderNameDialog {
                kind: LibraryFolderNameDialogKind::Create { parent },
                name: String::new(),
                error: None,
            });
        }
        LibraryFolderUiAction::Copy(path) => {
            match canonical_library_directory(&root, &path, false) {
                Ok(_) => {
                    app.library.folder_clipboard = Some(LibraryFolderClipboard {
                        path: path.clone(),
                        mode: LibraryFolderClipboardMode::Copy,
                    });
                    app.library.image_clipboard = None;
                    app.library.cloud_clipboard = None;
                    app.library.status = format!(
                        "Copied folder {}. Choose Paste Folder in a destination.",
                        path.display()
                    );
                }
                Err(error) => app.library.status = error,
            }
        }
        LibraryFolderUiAction::Cut(path) => {
            match canonical_library_directory(&root, &path, false) {
                Ok(_) => {
                    app.library.folder_clipboard = Some(LibraryFolderClipboard {
                        path: path.clone(),
                        mode: LibraryFolderClipboardMode::Cut,
                    });
                    app.library.image_clipboard = None;
                    app.library.cloud_clipboard = None;
                    app.library.status = format!(
                        "Cut folder {}. Choose Paste Folder in a destination.",
                        path.display()
                    );
                }
                Err(error) => app.library.status = error,
            }
        }
        LibraryFolderUiAction::Paste(destination_parent) => {
            let Some(clipboard) = app.library.folder_clipboard.clone() else {
                app.library.status = "Copy or cut a folder first.".to_owned();
                return;
            };
            if clipboard.mode == LibraryFolderClipboardMode::Cut
                && app
                    .current_path
                    .as_ref()
                    .is_some_and(|path| path.starts_with(&clipboard.path))
            {
                app.library.status =
                    "Open an image outside this folder before moving it.".to_owned();
                return;
            }
            let operation = match clipboard.mode {
                LibraryFolderClipboardMode::Copy => LibraryFolderOperation::Copy {
                    root,
                    source: clipboard.path,
                    destination_parent,
                },
                LibraryFolderClipboardMode::Cut => LibraryFolderOperation::Move {
                    root,
                    source: clipboard.path,
                    destination_parent,
                    new_name: None,
                },
            };
            app.library.start_folder_operation(operation, context);
        }
        LibraryFolderUiAction::PasteImages(destination_parent) => {
            start_image_clipboard_paste(
                app,
                ImagePasteDestination::LocalFolder(destination_parent),
                context,
            );
        }
        LibraryFolderUiAction::Rename(source) => {
            if app
                .current_path
                .as_ref()
                .is_some_and(|path| path.starts_with(&source))
            {
                app.library.status =
                    "Open an image outside this folder before renaming it.".to_owned();
                return;
            }
            let Some(name) = source
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
            else {
                app.library.status = "This folder name cannot be edited as text.".to_owned();
                return;
            };
            app.library.folder_name_dialog = Some(LibraryFolderNameDialog {
                kind: LibraryFolderNameDialogKind::Rename { source },
                name,
                error: None,
            });
        }
        LibraryFolderUiAction::Delete(path) => {
            app.library.folder_delete_confirmation = Some(path);
        }
        LibraryFolderUiAction::Move {
            source,
            destination_parent,
        } => {
            if app
                .current_path
                .as_ref()
                .is_some_and(|path| path.starts_with(&source))
            {
                app.library.status =
                    "Open an image outside this folder before moving it.".to_owned();
                return;
            }
            app.library.start_folder_operation(
                LibraryFolderOperation::Move {
                    root,
                    source,
                    destination_parent,
                    new_name: None,
                },
                context,
            );
        }
        LibraryFolderUiAction::Refresh => app.library.refresh(context),
    }
}

#[cfg(not(target_os = "android"))]
fn show_library_folder_dialogs(ui: &mut Ui, app: &mut AurawApp) {
    let mut close_name_dialog = false;
    let mut name_operation = None;
    if let Some(dialog) = app.library.folder_name_dialog.as_mut() {
        let title = match dialog.kind {
            LibraryFolderNameDialogKind::Create { .. } => "New folder",
            LibraryFolderNameDialogKind::Rename { .. } => "Rename folder",
        };
        egui::Window::new(title)
            .id(egui::Id::new("library-folder-name-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label("Folder name");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(320.0)
                        .id_source("library-folder-name-input"),
                );
                response.request_focus();
                if let Some(error) = dialog.error.as_deref() {
                    ui.label(
                        egui::RichText::new(error)
                            .small()
                            .color(ui.visuals().error_fg_color),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_name_dialog = true;
                    }
                    let enter = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    let confirm_label = match dialog.kind {
                        LibraryFolderNameDialogKind::Create { .. } => "Create",
                        LibraryFolderNameDialogKind::Rename { .. } => "Rename",
                    };
                    if ui.button(confirm_label).clicked() || enter {
                        match validate_folder_name(&dialog.name) {
                            Ok(_) => {
                                let Some(root) = app.library.root_folder.clone() else {
                                    close_name_dialog = true;
                                    return;
                                };
                                name_operation = Some(match &dialog.kind {
                                    LibraryFolderNameDialogKind::Create { parent } => {
                                        LibraryFolderOperation::Create {
                                            root,
                                            parent: parent.clone(),
                                            name: dialog.name.clone(),
                                        }
                                    }
                                    LibraryFolderNameDialogKind::Rename { source } => {
                                        let Some(parent) = source.parent() else {
                                            dialog.error = Some(
                                                "This folder has no parent folder.".to_owned(),
                                            );
                                            return;
                                        };
                                        LibraryFolderOperation::Move {
                                            root,
                                            source: source.clone(),
                                            destination_parent: parent.to_path_buf(),
                                            new_name: Some(dialog.name.clone()),
                                        }
                                    }
                                });
                                close_name_dialog = true;
                            }
                            Err(error) => dialog.error = Some(error),
                        }
                    }
                });
            });
    }
    if close_name_dialog {
        app.library.folder_name_dialog = None;
    }
    if let Some(operation) = name_operation {
        app.library.start_folder_operation(operation, ui.ctx());
    }

    let delete_target = app.library.folder_delete_confirmation.clone();
    let mut close_delete = false;
    let mut confirm_delete = false;
    if let Some(target) = delete_target.as_ref() {
        egui::Window::new("Delete folder?")
            .id(egui::Id::new("library-folder-delete-confirmation"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "Delete {} and everything inside it?",
                    target.display()
                ));
                ui.label(
                    egui::RichText::new("This cannot be undone.")
                        .strong()
                        .color(ui.visuals().warn_fg_color),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_delete = true;
                    }
                    if ui.button("Delete Folder").clicked() {
                        confirm_delete = true;
                        close_delete = true;
                    }
                });
            });
    }
    if close_delete {
        app.library.folder_delete_confirmation = None;
    }
    if confirm_delete {
        if let (Some(root), Some(target)) = (app.library.root_folder.clone(), delete_target) {
            if let Some(current) = app
                .current_path
                .clone()
                .filter(|current| current.starts_with(&target))
            {
                app.detach_current_file_for_library_action(&current);
                app.current_path = None;
            }
            app.library
                .start_folder_operation(LibraryFolderOperation::Delete { root, target }, ui.ctx());
        }
    }
}

fn validate_cloud_item_name(name: &str, raw: bool) -> Result<(), String> {
    if name.is_empty()
        || name.trim() != name
        || name.contains(['/', '\\'])
        || name.contains('"')
        || name.chars().any(char::is_control)
    {
        return Err("Enter a single safe name without leading or trailing spaces.".to_owned());
    }
    if raw && !crate::pipeline::is_supported_raw_path(Path::new(name)) {
        return Err("Keep a supported RAW filename extension.".to_owned());
    }
    Ok(())
}

fn paste_cloud_clipboard(
    app: &mut AurawApp,
    destination_folder_id: String,
    context: &egui::Context,
) {
    if app.library.image_clipboard.is_some() {
        start_image_clipboard_paste(
            app,
            ImagePasteDestination::CloudFolder(destination_folder_id),
            context,
        );
        return;
    }
    let Some(clipboard) = app.library.cloud_clipboard.clone() else {
        app.library.status = "Copy or cut images or a cloud folder first.".to_owned();
        return;
    };
    let request = match clipboard.content {
        CloudClipboardContent::Folder(folder) => match clipboard.mode {
            CloudClipboardMode::Copy => CloudActionRequest::CopyFolder {
                folder,
                destination_parent_id: destination_folder_id,
                clear_clipboard: false,
            },
            CloudClipboardMode::Cut => CloudActionRequest::UpdateFolder {
                name: folder.name.clone(),
                folder,
                parent_id: destination_folder_id,
                clear_clipboard: true,
            },
        },
    };
    app.library.start_cloud_action(request, context);
}

fn start_image_clipboard_paste(
    app: &mut AurawApp,
    destination: ImagePasteDestination,
    context: &egui::Context,
) {
    let Some(clipboard) = app.library.image_clipboard.clone() else {
        app.library.status = "Copy or cut one or more RAW files first.".to_owned();
        return;
    };
    let busy = app.library.image_paste_receiver.is_some()
        || app.library.cloud_action_receiver.is_some()
        || app.library.cloud_upload_receiver.is_some()
        || app.library.cloud_open_receiver.is_some()
        || {
            #[cfg(not(target_os = "android"))]
            {
                app.library.file_action_receiver.is_some()
                    || app.library.raw_import_receiver.is_some()
                    || app.library.folder_operation_receiver.is_some()
            }
            #[cfg(target_os = "android")]
            {
                false
            }
        };
    if busy {
        app.library.status = "Wait for the current library transfer to finish.".to_owned();
        return;
    }
    if matches!(&destination, ImagePasteDestination::CloudFolder(_))
        && app.library.cloud_config.normalized().is_err()
    {
        app.library.status = "Configure AuRaw Cloud before pasting RAW files there.".to_owned();
        return;
    }
    if clipboard.mode == ImageClipboardMode::Cut {
        match &clipboard.content {
            #[cfg(not(target_os = "android"))]
            ImageClipboardContent::Local(paths) => {
                let moves_current = app.current_path.as_ref().is_some_and(|current| {
                    paths.iter().any(|path| path == current)
                        && match &destination {
                            ImagePasteDestination::LocalFolder(folder) => {
                                current.parent() != Some(folder.as_path())
                            }
                            ImagePasteDestination::CloudFolder(_) => true,
                        }
                });
                if moves_current {
                    if let Some(current) = app.current_path.clone() {
                        app.detach_current_file_for_library_action(&current);
                        app.current_path = None;
                    }
                }
            }
            #[cfg(target_os = "android")]
            ImageClipboardContent::Local(items) => {
                if matches!(&destination, ImagePasteDestination::CloudFolder(_)) {
                    for item in items {
                        app.detach_current_android_document_for_library_action(
                            &item.uri,
                            &item.display_name,
                        );
                    }
                }
            }
            ImageClipboardContent::Cloud(assets) => {
                let deletes_server_copy = match &destination {
                    ImagePasteDestination::CloudFolder(_) => false,
                    #[cfg(not(target_os = "android"))]
                    ImagePasteDestination::LocalFolder(_) => true,
                    #[cfg(target_os = "android")]
                    ImagePasteDestination::LocalLibrary => true,
                };
                if deletes_server_copy {
                    detach_current_cloud_asset_if_selected(app, assets);
                }
            }
        }
    }
    app.library.start_image_paste(destination, context);
}

#[cfg(not(target_os = "android"))]
fn show_cloud_folder_bar(ui: &mut Ui, app: &mut AurawApp) {
    if !app.library.is_cloud_view() {
        return;
    }
    if app.library.cloud_trash_open {
        let action_enabled =
            !app.library.cloud_action_in_progress() && app.library.cloud_trash_receiver.is_none();
        let mut back = false;
        let mut refresh = false;
        ui.horizontal_wrapped(|ui| {
            if ui.button("Cloud").clicked() {
                back = true;
            }
            ui.label(egui_phosphor::regular::CARET_RIGHT);
            ui.strong(format!("{} Trash", egui_phosphor::regular::TRASH));
            ui.separator();
            if ui
                .add_enabled(action_enabled, egui::Button::new("Refresh"))
                .clicked()
            {
                refresh = true;
            }
        });
        if back {
            app.show_library_view(LibraryView::Cloud);
        } else if refresh {
            app.library.refresh(ui.ctx());
        }
        return;
    }
    let breadcrumbs = app.library.cloud_breadcrumbs();
    let children = app
        .library
        .cloud_folders
        .iter()
        .filter(|folder| folder.parent_id == app.library.cloud_folder_id)
        .cloned()
        .collect::<Vec<_>>();
    let current_folder = app
        .library
        .cloud_folder(&app.library.cloud_folder_id)
        .cloned();
    let action_enabled = !app.library.cloud_action_in_progress()
        && !app.library.cloud_upload_in_progress()
        && !app.library.image_paste_in_progress()
        && app.library.cloud_open_receiver.is_none();
    let has_clipboard =
        app.library.cloud_clipboard.is_some() || app.library.image_clipboard.is_some();
    let mut navigate_to = None;
    let mut create_folder = false;
    let mut paste = false;
    let mut folder_action = None;
    let mut open_trash = false;

    ui.horizontal_wrapped(|ui| {
        for (index, (folder_id, name)) in breadcrumbs.iter().enumerate() {
            if index > 0 {
                ui.label(egui_phosphor::regular::CARET_RIGHT);
            }
            if ui
                .add_enabled(
                    folder_id != &app.library.cloud_folder_id,
                    egui::Button::new(name).frame(false),
                )
                .clicked()
            {
                navigate_to = Some(folder_id.clone());
            }
        }
        ui.separator();
        if has_clipboard
            && ui
                .add_enabled(
                    action_enabled,
                    egui::Button::new(
                        app.library
                            .image_clipboard
                            .as_ref()
                            .map(ImageClipboard::paste_label)
                            .unwrap_or_else(|| "Paste here".to_owned()),
                    ),
                )
                .clicked()
        {
            paste = true;
        }
        ui.menu_button(egui_phosphor::regular::DOTS_THREE, |ui| {
            if ui
                .add_enabled(action_enabled, egui::Button::new("New folder…"))
                .clicked()
            {
                create_folder = true;
                ui.close();
            }
            if let Some(folder) = current_folder.as_ref() {
                ui.separator();
                if ui
                    .add_enabled(action_enabled, egui::Button::new("Copy folder"))
                    .clicked()
                {
                    folder_action = Some((CloudClipboardMode::Copy, folder.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(action_enabled, egui::Button::new("Cut folder"))
                    .clicked()
                {
                    folder_action = Some((CloudClipboardMode::Cut, folder.clone()));
                    ui.close();
                }
                if ui
                    .add_enabled(action_enabled, egui::Button::new("Rename folder…"))
                    .clicked()
                {
                    app.library.cloud_name_dialog = Some(CloudNameDialog {
                        name: folder.name.clone(),
                        kind: CloudNameDialogKind::RenameFolder {
                            folder: folder.clone(),
                        },
                        error: None,
                        focus_requested: false,
                    });
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(action_enabled, egui::Button::new("Delete folder…"))
                    .clicked()
                {
                    app.library.cloud_delete_confirmation =
                        Some(CloudDeleteTarget::Folder(folder.clone()));
                    ui.close();
                }
            }
        });
        if ui
            .button(format!("{} Trash", egui_phosphor::regular::TRASH))
            .clicked()
        {
            open_trash = true;
        }
    });

    if !children.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for folder in &children {
                if ui
                    .button(format!(
                        "{}  {}",
                        egui_phosphor::regular::FOLDER,
                        folder.name
                    ))
                    .clicked()
                {
                    navigate_to = Some(folder.id.clone());
                }
            }
        });
    }
    if let Some(folder_id) = navigate_to {
        app.select_cloud_library_folder(folder_id);
    }
    if open_trash {
        app.show_cloud_library_trash();
    }
    if create_folder {
        app.library.cloud_name_dialog = Some(CloudNameDialog {
            kind: CloudNameDialogKind::CreateFolder {
                parent_id: app.library.cloud_folder_id.clone(),
            },
            name: String::new(),
            error: None,
            focus_requested: false,
        });
    }
    if paste {
        paste_cloud_clipboard(app, app.library.cloud_folder_id.clone(), ui.ctx());
    }
    if let Some((mode, folder)) = folder_action {
        app.library.cloud_clipboard = Some(CloudClipboard {
            mode,
            content: CloudClipboardContent::Folder(folder.clone()),
        });
        app.library.image_clipboard = None;
        #[cfg(not(target_os = "android"))]
        {
            app.library.folder_clipboard = None;
        }
        app.library.status = format!(
            "{} cloud folder {}. Choose Paste in a destination.",
            if mode == CloudClipboardMode::Copy {
                "Copied"
            } else {
                "Cut"
            },
            folder.name
        );
    }
}

fn show_local_image_paste_bar(ui: &mut Ui, app: &mut AurawApp) {
    if app.library.is_cloud_view() {
        return;
    }
    let Some(clipboard) = app.library.image_clipboard.as_ref() else {
        return;
    };
    let label = format!("{} here", clipboard.paste_label());
    let enabled = {
        #[cfg(not(target_os = "android"))]
        {
            !app.library.file_action_in_progress() && app.library.folder.is_some()
        }
        #[cfg(target_os = "android")]
        {
            !app.library.image_paste_in_progress()
                && !app.library.cloud_action_in_progress()
                && !app.library.cloud_upload_in_progress()
                && app.library.cloud_open_receiver.is_none()
        }
    };
    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
        #[cfg(not(target_os = "android"))]
        if let Some(folder) = app.library.folder.clone() {
            start_image_clipboard_paste(app, ImagePasteDestination::LocalFolder(folder), ui.ctx());
        }
        #[cfg(target_os = "android")]
        start_image_clipboard_paste(app, ImagePasteDestination::LocalLibrary, ui.ctx());
    }
}

fn trash_age_label(seconds: u64) -> String {
    if seconds < 60 {
        "just now".to_owned()
    } else if seconds < 60 * 60 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{} h ago", seconds / (60 * 60))
    } else {
        let days = seconds / (24 * 60 * 60);
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    }
}

fn trash_remaining_label(seconds: u64) -> String {
    if seconds == 0 {
        "expires now".to_owned()
    } else if seconds < 24 * 60 * 60 {
        let hours = seconds.div_ceil(60 * 60);
        format!("{hours} h remaining")
    } else {
        let days = seconds.div_ceil(24 * 60 * 60);
        format!("{days} day{} remaining", if days == 1 { "" } else { "s" })
    }
}

fn trash_size_label(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn show_cloud_trash_panel(ui: &mut Ui, app: &mut AurawApp) {
    let action_enabled =
        !app.library.cloud_action_in_progress() && app.library.cloud_trash_receiver.is_none();
    let items = app.library.cloud_trash_items.clone();
    let selected = items
        .iter()
        .filter(|item| app.library.cloud_trash_selection.contains(&item.id))
        .cloned()
        .collect::<Vec<_>>();
    let mut request = None;
    let mut request_delete = None;
    ui.horizontal_wrapped(|ui| {
        ui.heading("Trash");
        ui.label(
            egui::RichText::new(format!(
                "Deleted bundles are retained for {} days.",
                app.library.cloud_trash_retention_days
            ))
            .small()
            .color(ui.visuals().weak_text_color()),
        );
        ui.separator();
        if ui
            .add_enabled(
                action_enabled && !selected.is_empty(),
                egui::Button::new(format!("Restore selected ({})", selected.len())),
            )
            .clicked()
        {
            request = Some(CloudActionRequest::RestoreTrash {
                items: selected.clone(),
            });
        }
        if ui
            .add_enabled(
                action_enabled && !selected.is_empty(),
                egui::Button::new("Permanently delete selected…"),
            )
            .clicked()
        {
            request_delete = Some(CloudTrashDeleteTarget::Selected(selected.clone()));
        }
        if ui
            .add_enabled(
                action_enabled && !items.is_empty(),
                egui::Button::new("Empty Trash…"),
            )
            .clicked()
        {
            request_delete = Some(CloudTrashDeleteTarget::Empty);
        }
    });
    ui.separator();

    if app.library.catalog_ready && items.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Trash is empty");
                ui.label("Deleted cloud RAWs and folders will appear here.");
            });
        });
    } else {
        egui::ScrollArea::vertical().show(ui, |ui| {
            for item in &items {
                let mut checked = app.library.cloud_trash_selection.contains(&item.id);
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut checked, "").changed() {
                            if checked {
                                app.library.cloud_trash_selection.insert(item.id.clone());
                            } else {
                                app.library.cloud_trash_selection.remove(&item.id);
                            }
                        }
                        let icon = if item.kind == "folder" {
                            egui_phosphor::regular::FOLDER
                        } else {
                            egui_phosphor::regular::IMAGE
                        };
                        ui.strong(format!("{icon}  {}", item.name));
                        ui.label(trash_size_label(item.bytes));
                        if item.kind == "folder" {
                            ui.label(format!(
                                "{} bundled item{}",
                                item.item_count,
                                if item.item_count == 1 { "" } else { "s" }
                            ));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(action_enabled, egui::Button::new("Restore"))
                                .clicked()
                            {
                                request = Some(CloudActionRequest::RestoreTrash {
                                    items: vec![item.clone()],
                                });
                            }
                        });
                    });
                    let age = app
                        .library
                        .cloud_trash_server_time
                        .saturating_sub(item.deleted_seconds);
                    let remaining = item
                        .expires_seconds
                        .saturating_sub(app.library.cloud_trash_server_time);
                    ui.label(
                        egui::RichText::new(format!(
                            "Deleted {} · {}",
                            trash_age_label(age),
                            trash_remaining_label(remaining)
                        ))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                });
                ui.add_space(4.0);
            }
        });
    }
    if let Some(target) = request_delete {
        app.library.cloud_trash_delete_confirmation = Some(target);
    }

    let confirmation = app.library.cloud_trash_delete_confirmation.clone();
    let mut close_confirmation = false;
    if let Some(target) = confirmation {
        let (count, empty) = match &target {
            CloudTrashDeleteTarget::Selected(items) => (items.len(), false),
            CloudTrashDeleteTarget::Empty => (items.len(), true),
        };
        egui::Window::new(if empty {
            "Empty Cloud Trash?"
        } else {
            "Permanently delete selected items?"
        })
        .id(egui::Id::new("cloud-trash-permanent-confirmation"))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label(format!(
                "Permanently delete {count} Trash item{}?",
                if count == 1 { "" } else { "s" }
            ));
            ui.label(
                egui::RichText::new("This cannot be undone.")
                    .strong()
                    .color(ui.visuals().warn_fg_color),
            );
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close_confirmation = true;
                }
                if ui.button("Permanently delete").clicked() {
                    request = Some(match target.clone() {
                        CloudTrashDeleteTarget::Selected(items) => {
                            CloudActionRequest::PermanentlyDeleteTrash { items }
                        }
                        CloudTrashDeleteTarget::Empty => CloudActionRequest::EmptyTrash,
                    });
                    close_confirmation = true;
                }
            });
        });
    }
    if close_confirmation {
        app.library.cloud_trash_delete_confirmation = None;
    }
    if let Some(request) = request {
        app.library.cloud_trash_selection.clear();
        app.library.start_cloud_action(request, ui.ctx());
    }
}

fn show_cloud_dialogs(ui: &mut Ui, app: &mut AurawApp) {
    let mut close_name_dialog = false;
    let mut name_operation = None;
    if let Some(dialog) = app.library.cloud_name_dialog.as_mut() {
        let title = match dialog.kind {
            CloudNameDialogKind::CreateFolder { .. } => "New cloud folder",
            CloudNameDialogKind::RenameFolder { .. } => "Rename cloud folder",
            CloudNameDialogKind::RenameAsset { .. } => "Rename cloud RAW",
        };
        egui::Window::new(title)
            .id(egui::Id::new("cloud-item-name-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(
                    if matches!(dialog.kind, CloudNameDialogKind::RenameAsset { .. }) {
                        "RAW filename"
                    } else {
                        "Folder name"
                    },
                );
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(320.0)
                        .id_source("cloud-item-name-input"),
                );
                if !dialog.focus_requested {
                    response.request_focus();
                    dialog.focus_requested = true;
                }
                if let Some(error) = dialog.error.as_deref() {
                    ui.label(
                        egui::RichText::new(error)
                            .small()
                            .color(ui.visuals().error_fg_color),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_name_dialog = true;
                    }
                    let enter = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    let confirm_label = match dialog.kind {
                        CloudNameDialogKind::CreateFolder { .. } => "Create",
                        CloudNameDialogKind::RenameFolder { .. }
                        | CloudNameDialogKind::RenameAsset { .. } => "Rename",
                    };
                    if ui.button(confirm_label).clicked() || enter {
                        let raw = matches!(dialog.kind, CloudNameDialogKind::RenameAsset { .. });
                        match validate_cloud_item_name(&dialog.name, raw) {
                            Ok(()) => {
                                name_operation = Some(match &dialog.kind {
                                    CloudNameDialogKind::CreateFolder { parent_id } => {
                                        CloudActionRequest::CreateFolder {
                                            parent_id: parent_id.clone(),
                                            name: dialog.name.clone(),
                                        }
                                    }
                                    CloudNameDialogKind::RenameFolder { folder } => {
                                        CloudActionRequest::UpdateFolder {
                                            folder: folder.clone(),
                                            parent_id: folder.parent_id.clone(),
                                            name: dialog.name.clone(),
                                            clear_clipboard: false,
                                        }
                                    }
                                    CloudNameDialogKind::RenameAsset { asset } => {
                                        CloudActionRequest::RenameAsset {
                                            asset: asset.clone(),
                                            name: dialog.name.clone(),
                                        }
                                    }
                                });
                                close_name_dialog = true;
                            }
                            Err(error) => {
                                dialog.error = Some(error);
                                dialog.focus_requested = false;
                            }
                        }
                    }
                });
            });
    }
    if close_name_dialog {
        app.library.cloud_name_dialog = None;
    }
    if let Some(operation) = name_operation {
        app.library.start_cloud_action(operation, ui.ctx());
    }

    let delete_target = app.library.cloud_delete_confirmation.clone();
    let mut close_delete = false;
    let mut confirm_delete = false;
    if let Some(target) = delete_target.as_ref() {
        let (title, message) = match target {
            CloudDeleteTarget::Folder(folder) => (
                "Delete cloud folder?",
                format!("Delete {} and everything inside it?", folder.name),
            ),
            CloudDeleteTarget::Assets(assets) => (
                "Delete cloud RAWs?",
                format!(
                    "Delete {} selected cloud RAW{}?",
                    assets.len(),
                    if assets.len() == 1 { "" } else { "s" }
                ),
            ),
        };
        egui::Window::new(title)
            .id(egui::Id::new("cloud-delete-confirmation"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(message);
                ui.label(
                    egui::RichText::new(
                        "This moves the complete server copy to Trash for its retention period.",
                    )
                    .strong(),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close_delete = true;
                    }
                    if ui.button("Delete").clicked() {
                        confirm_delete = true;
                        close_delete = true;
                    }
                });
            });
    }
    if close_delete {
        app.library.cloud_delete_confirmation = None;
    }
    if confirm_delete {
        if let Some(target) = delete_target {
            let request = match target {
                CloudDeleteTarget::Folder(folder) => {
                    detach_current_cloud_asset_if_inside_folder(app, &folder.id);
                    if cloud_folder_contains(
                        &app.library.cloud_folders,
                        &folder.id,
                        &app.library.cloud_folder_id,
                    ) {
                        app.remember_cloud_library_folder(folder.parent_id.clone());
                    }
                    CloudActionRequest::DeleteFolder { folder }
                }
                CloudDeleteTarget::Assets(assets) => {
                    detach_current_cloud_asset_if_selected(app, &assets);
                    CloudActionRequest::DeleteAssets { assets }
                }
            };
            app.library.start_cloud_action(request, ui.ctx());
        }
    }
}

#[cfg(not(target_os = "android"))]
fn show_local_raw_name_dialog(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
    let mut close = false;
    let mut rename = None;
    if let Some(dialog) = app.library.raw_name_dialog.as_mut() {
        egui::Window::new("Rename local RAW")
            .id(egui::Id::new("local-raw-name-dialog"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label("RAW filename");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(320.0)
                        .id_source("local-raw-name-input"),
                );
                if !dialog.focus_requested {
                    response.request_focus();
                    dialog.focus_requested = true;
                }
                if let Some(error) = dialog.error.as_deref() {
                    ui.label(
                        egui::RichText::new(error)
                            .small()
                            .color(ui.visuals().error_fg_color),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let enter = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if ui.button("Rename").clicked() || enter {
                        match validate_cloud_item_name(&dialog.name, true) {
                            Ok(()) => {
                                rename = Some((dialog.source.clone(), dialog.name.clone()));
                            }
                            Err(error) => {
                                dialog.error = Some(error);
                                dialog.focus_requested = false;
                            }
                        }
                    }
                });
            });
    }
    if close {
        app.library.raw_name_dialog = None;
    }
    if let Some((source, name)) = rename {
        let was_current = app.detach_current_file_for_library_action(&source);
        if was_current {
            app.current_path = None;
        }
        match rename_raw_bundle(&source, &name) {
            Ok(destination) => {
                if let Some(ImageClipboard {
                    content: ImageClipboardContent::Local(paths),
                    ..
                }) = app.library.image_clipboard.as_mut()
                {
                    for path in paths {
                        if path == &source {
                            *path = destination.clone();
                        }
                    }
                }
                app.library.raw_name_dialog = None;
                app.library.clear_selection();
                app.library.refresh(ui.ctx());
                app.library.status = format!("Renamed local RAW to {}.", destination.display());
                if was_current {
                    app.open_path_labeled(
                        destination.clone(),
                        name,
                        false,
                        crate::sidecar::SidecarTarget::Desktop {
                            raw_path: destination,
                        },
                        frame,
                        None,
                    );
                }
            }
            Err(error) => {
                if let Some(dialog) = app.library.raw_name_dialog.as_mut() {
                    dialog.error = Some(error);
                    dialog.focus_requested = false;
                }
                if was_current && source.is_file() {
                    let label = source
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("local RAW")
                        .to_owned();
                    app.open_path_labeled(
                        source.clone(),
                        label,
                        false,
                        crate::sidecar::SidecarTarget::Desktop { raw_path: source },
                        frame,
                        None,
                    );
                }
            }
        }
    }
}

#[cfg(target_os = "android")]
fn show_android_local_raw_name_dialog(ui: &mut Ui, app: &mut AurawApp) {
    let mut close = false;
    let mut rename = None;
    if let Some(dialog) = app.library.android_raw_name_dialog.as_mut() {
        crate::ui::responsive_popup(egui::Window::new("Rename local RAW"), ui.ctx(), 420.0)
            .id(egui::Id::new("android-local-raw-name-dialog"))
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.label("RAW filename");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut dialog.name)
                        .desired_width(320.0)
                        .id_source("android-local-raw-name-input"),
                );
                if !dialog.focus_requested {
                    response.request_focus();
                    dialog.focus_requested = true;
                }
                if let Some(error) = dialog.error.as_deref() {
                    ui.label(
                        egui::RichText::new(error)
                            .small()
                            .color(ui.visuals().error_fg_color),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    let enter = response.has_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if ui.button("Rename").clicked() || enter {
                        match validate_cloud_item_name(&dialog.name, true) {
                            Ok(()) => {
                                rename = Some((dialog.source.clone(), dialog.name.clone()));
                            }
                            Err(error) => {
                                dialog.error = Some(error);
                                dialog.focus_requested = false;
                            }
                        }
                    }
                });
            });
    }
    if close {
        app.library.android_raw_name_dialog = None;
    }
    if let Some((source, name)) = rename {
        match app.rename_android_library_item(&source.uri, &source.display_name, &name) {
            Ok(renamed_uri) => {
                if let Some(ImageClipboard {
                    content: ImageClipboardContent::Local(items),
                    ..
                }) = app.library.image_clipboard.as_mut()
                {
                    for item in items {
                        if item.uri == source.uri {
                            item.uri = renamed_uri.clone();
                            item.display_name = name.clone();
                        }
                    }
                }
                app.library.android_raw_name_dialog = None;
                app.library.clear_selection();
                crate::android::set_back_navigation_active(false);
                app.library.refresh(ui.ctx());
                app.library.status = format!("Renamed local RAW to {name}.");
            }
            Err(error) => {
                if let Some(dialog) = app.library.android_raw_name_dialog.as_mut() {
                    dialog.error = Some(error);
                    dialog.focus_requested = false;
                }
            }
        }
    }
}

pub struct Library;

impl Library {
    pub(crate) fn show_folder_sidebar(ui: &mut Ui, app: &mut AurawApp) {
        #[cfg(not(target_os = "android"))]
        let action_in_progress = app.library.file_action_in_progress();
        #[cfg(target_os = "android")]
        let action_in_progress = app.library.cloud_action_in_progress()
            || app.library.cloud_upload_in_progress()
            || app.library.image_paste_in_progress()
            || app.library.cloud_open_receiver.is_some()
            || app.picker_pending;
        #[cfg(not(target_os = "android"))]
        let folders_available = app.library.root_folder.is_some() || app.library.is_cloud_view();
        #[cfg(target_os = "android")]
        let folders_available = true;
        #[cfg(not(target_os = "android"))]
        let can_create_folder = app.library.folder.is_some()
            || (app.library.is_cloud_view() && !app.library.cloud_trash_open);
        #[cfg(target_os = "android")]
        let can_create_folder = !app.library.cloud_trash_open;
        #[cfg(not(target_os = "android"))]
        let mut requested_action = None;
        #[cfg(target_os = "android")]
        let mut requested_android_action = None;
        let mut requested_cloud_action = None;
        let mut requested_cloud_trash = false;
        // The folder header and Library toolbar share the same dimensions so
        // their controls and separators stay aligned across the split view.
        crate::ui::theme::toolbar_row(ui, |ui| {
            ui.strong("Folders");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::ui::icons::phosphor_icon_button(
                    ui,
                    egui_phosphor::regular::X,
                    crate::ui::theme::toolbar_icon_size(),
                    "Close folder sidebar",
                )
                .clicked()
                {
                    app.set_library_folder_sidebar_open(false);
                }
                if crate::ui::icons::phosphor_icon_button_enabled(
                    ui,
                    folders_available && !action_in_progress,
                    egui_phosphor::regular::ARROW_CLOCKWISE,
                    crate::ui::theme::toolbar_icon_size(),
                    "Refresh folders",
                )
                .clicked()
                {
                    if app.library.is_cloud_view() {
                        requested_cloud_action = Some(CloudFolderUiAction::Refresh);
                    } else {
                        #[cfg(not(target_os = "android"))]
                        {
                            requested_action = Some(LibraryFolderUiAction::Refresh);
                        }
                        #[cfg(target_os = "android")]
                        {
                            requested_android_action = Some(AndroidLibraryFolderUiAction::Refresh);
                        }
                    }
                }
                if crate::ui::icons::phosphor_icon_button_enabled(
                    ui,
                    can_create_folder && !action_in_progress,
                    egui_phosphor::regular::FOLDER_PLUS,
                    crate::ui::theme::toolbar_icon_size(),
                    "Create folder here",
                )
                .clicked()
                {
                    if app.library.is_cloud_view() {
                        requested_cloud_action = Some(CloudFolderUiAction::New(
                            app.library.cloud_folder_id.clone(),
                        ));
                    } else {
                        #[cfg(not(target_os = "android"))]
                        if let Some(folder) = app.library.folder.clone() {
                            requested_action = Some(LibraryFolderUiAction::New(folder));
                        }
                        #[cfg(target_os = "android")]
                        {
                            requested_android_action = Some(AndroidLibraryFolderUiAction::New(
                                app.library.android_folder.clone(),
                            ));
                        }
                    }
                }
            });
        });

        let cloud_view = app.library.is_cloud_view();
        let mut requested_view = None;
        #[cfg(not(target_os = "android"))]
        let navigation_enabled = true;
        #[cfg(target_os = "android")]
        let navigation_enabled = !action_in_progress;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let width = ((ui.available_width() - 6.0) * 0.5).max(72.0);
            if ui
                .add_enabled(
                    navigation_enabled,
                    egui::Button::selectable(!cloud_view, "Local")
                        .min_size(egui::vec2(width, crate::ui::theme::CONTROL_HEIGHT)),
                )
                .clicked()
                && cloud_view
            {
                requested_view = Some(LibraryView::Local);
            }
            let cloud_tab = ui.add_enabled(
                app.library.cloud_enabled() && navigation_enabled,
                egui::Button::selectable(cloud_view, "Cloud")
                    .min_size(egui::vec2(width, crate::ui::theme::CONTROL_HEIGHT)),
            );
            if cloud_tab.clicked() && !cloud_view {
                requested_view = Some(LibraryView::Cloud);
            }
            if !app.library.cloud_enabled() {
                cloud_tab.on_disabled_hover_text("Enable AuRaw Cloud in Settings first.");
            }
        });
        if let Some(view) = requested_view {
            app.show_library_view(view);
        }

        ui.separator();

        #[cfg(not(target_os = "android"))]
        let mut requested_folder = None;
        let tree_height = ui.available_height().max(32.0);
        if app.library.is_cloud_view() {
            egui::ScrollArea::both()
                .max_height(tree_height)
                .min_scrolled_height(tree_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    show_cloud_folder_node(
                        ui,
                        None,
                        &app.library.cloud_folders,
                        &app.library.cloud_folder_id,
                        app.library.cloud_clipboard.as_ref(),
                        app.library.image_clipboard.as_ref(),
                        action_in_progress,
                        &mut app.library.cloud_expanded_folders,
                        &mut requested_cloud_action,
                    );
                    if ui
                        .selectable_label(
                            app.library.cloud_trash_open,
                            format!("{}  Trash", egui_phosphor::regular::TRASH),
                        )
                        .clicked()
                    {
                        requested_cloud_trash = true;
                    }
                });
        } else {
            #[cfg(not(target_os = "android"))]
            {
                let tree = app.library.folder_tree.as_ref();
                let root_folder = app.library.root_folder.as_deref();
                let selected_folder = app.library.folder.as_deref();
                let clipboard = app.library.folder_clipboard.as_ref();
                let image_clipboard = app.library.image_clipboard.as_ref();
                let expanded_folders = &mut app.library.expanded_folders;
                egui::ScrollArea::both()
                    .max_height(tree_height)
                    .min_scrolled_height(tree_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if let (Some(tree), Some(root_folder)) = (tree, root_folder) {
                            show_library_folder_node(
                                ui,
                                tree,
                                root_folder,
                                selected_folder,
                                clipboard,
                                image_clipboard,
                                action_in_progress,
                                expanded_folders,
                                &mut requested_folder,
                                &mut requested_action,
                            );
                        } else {
                            ui.label(
                                egui::RichText::new(
                                    "Open a top-level folder to browse its hierarchy.",
                                )
                                .small()
                                .color(ui.visuals().weak_text_color()),
                            );
                        }
                    });
            }
            #[cfg(target_os = "android")]
            {
                let folders = &app.library.android_folders;
                let mut children_by_parent =
                    HashMap::<&str, Vec<&crate::android::LibraryFolder>>::new();
                for folder in folders {
                    children_by_parent
                        .entry(android_folder_parent(&folder.path))
                        .or_default()
                        .push(folder);
                }
                let selected_folder = &app.library.android_folder;
                let expanded_folders = &mut app.library.android_expanded_folders;
                egui::ScrollArea::both()
                    .max_height(tree_height)
                    .min_scrolled_height(tree_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        show_android_library_folder_node(
                            ui,
                            "",
                            "Library",
                            &children_by_parent,
                            selected_folder,
                            action_in_progress,
                            expanded_folders,
                            &mut requested_android_action,
                        );
                    });
            }
        }

        #[cfg(not(target_os = "android"))]
        if let Some(folder) = requested_folder {
            app.select_library_folder(folder);
        }
        #[cfg(not(target_os = "android"))]
        if let Some(action) = requested_action {
            apply_library_folder_ui_action(app, action, ui.ctx());
        }
        if let Some(action) = requested_cloud_action {
            #[cfg(target_os = "android")]
            let close_drawer = matches!(action, CloudFolderUiAction::Select(_));
            apply_cloud_folder_ui_action(app, action, ui.ctx());
            #[cfg(target_os = "android")]
            if close_drawer {
                app.set_library_folder_sidebar_open(false);
            }
        }
        if requested_cloud_trash {
            app.show_cloud_library_trash();
            #[cfg(target_os = "android")]
            app.set_library_folder_sidebar_open(false);
        }
        #[cfg(target_os = "android")]
        if let Some(action) = requested_android_action {
            apply_android_library_folder_ui_action(app, action, ui.ctx());
        }
        #[cfg(not(target_os = "android"))]
        show_library_folder_dialogs(ui, app);
    }

    pub fn show(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        app.library.resume_thumbnail_decoding();
        app.library.poll(ui.ctx());
        app.library.poll_cloud_trash();
        if let Some(completion) = app.library.poll_cloud_action() {
            match completion {
                CloudActionCompletion::Mutation {
                    result,
                    clear_clipboard,
                } => {
                    if clear_clipboard {
                        app.library.cloud_clipboard = None;
                    }
                    app.library.clear_selection();
                    #[cfg(target_os = "android")]
                    crate::android::set_back_navigation_active(false);
                    app.library.cloud_upload_completion =
                        Some(result.unwrap_or_else(|error| error));
                    app.library.refresh(ui.ctx());
                }
                CloudActionCompletion::Prepared { purpose, result } => match result {
                    Err(error) => app.library.status = error,
                    Ok(cached) => {
                        let paths = cached
                            .iter()
                            .map(|asset| asset.raw_path.clone())
                            .collect::<Vec<_>>();
                        match purpose {
                            CloudPreparedPurpose::Export => {
                                if !paths.is_empty() {
                                    #[cfg(not(target_os = "android"))]
                                    {
                                        app.library.export_dialog = Some(LibraryExportDialog {
                                            paths,
                                            settings: app.export_settings.clone(),
                                            format: ExportFormat::Jpeg,
                                        });
                                    }
                                    #[cfg(target_os = "android")]
                                    {
                                        app.library.export_dialog = Some(LibraryExportDialog {
                                            targets: cached
                                                .into_iter()
                                                .map(|asset| {
                                                    crate::app::AndroidLibraryExportTarget::Cloud {
                                                        path: asset.raw_path,
                                                        display_name: asset.label,
                                                    }
                                                })
                                                .collect(),
                                            settings: app.export_settings.clone(),
                                            format: ExportFormat::Jpeg,
                                        });
                                    }
                                }
                            }
                            CloudPreparedPurpose::CopyAdjustments => {
                                if let Some(path) = paths.first() {
                                    let status = match app.copy_library_adjustments_from_path(path)
                                    {
                                        Ok(()) => format!(
                                            "Copied adjustments from {}",
                                            cached
                                                .first()
                                                .map(|asset| asset.label.as_str())
                                                .unwrap_or("cloud RAW")
                                        ),
                                        Err(error) => {
                                            format!("Could not copy adjustments: {error}")
                                        }
                                    };
                                    app.library.status = status;
                                }
                            }
                            CloudPreparedPurpose::PasteAdjustments => {
                                #[cfg(not(target_os = "android"))]
                                apply_desktop_image_action(
                                    ui,
                                    app,
                                    frame,
                                    LibraryCardAction::PasteAdjustments(paths),
                                );
                                #[cfg(target_os = "android")]
                                prepare_android_cloud_adjustment_paste(ui, app, paths, frame);
                            }
                        }
                    }
                },
            }
        }
        if let Some(result) = app.library.poll_cloud_open() {
            match result {
                Ok(cached) => {
                    app.open_cloud_cached_asset(cached, frame);
                    return;
                }
                Err(error) => app.library.set_status(error),
            }
        }

        let mut refresh = false;
        let mut import_raw = false;
        let mut open_source = None;
        let mut library_action = None;
        let mut cloud_library_action = None;

        #[cfg(target_os = "android")]
        let selected_android_items = app
            .library
            .entries
            .iter()
            .filter(|entry| app.library.selected_sources.contains(&entry.info.source))
            .map(|entry| (entry.info.source.clone(), entry.info.name.clone()))
            .collect::<Vec<_>>();

        let compact_header = ui.available_width() < 520.0;
        crate::ui::theme::toolbar_row(ui, |ui| {
            if compact_header {
                ui.spacing_mut().item_spacing.x = 4.0;
            }
            if !app.library.folder_sidebar_open()
                && crate::ui::icons::phosphor_icon_button(
                    ui,
                    egui_phosphor::regular::SIDEBAR_SIMPLE,
                    crate::ui::theme::toolbar_icon_size(),
                    "Open folder sidebar",
                )
                .clicked()
            {
                app.set_library_folder_sidebar_open(true);
            }

            #[cfg(target_os = "android")]
            if !selected_android_items.is_empty() {
                ui.strong(format!("{} selected", selected_android_items.len()));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        app.library.clear_selection();
                        crate::android::set_back_navigation_active(false);
                    }
                    let anchor = ui.allocate_response(egui::vec2(48.0, 42.0), Sense::hover());
                    let menu_id = ui.make_persistent_id("android-library-selection-overflow");
                    crate::ui::android_overflow_menu(ui, anchor.rect, menu_id, 36.0, |ui| {
                        if app.library.is_cloud_view() {
                            let assets = selected_android_items
                                .iter()
                                .filter_map(|(source, _)| match source {
                                    LibrarySource::Cloud(asset) => Some(asset.clone()),
                                    _ => None,
                                })
                                .collect::<Vec<_>>();
                            if let Some(action) = cloud_image_context_menu(ui, app, &assets) {
                                cloud_library_action = Some(action);
                            }
                        } else {
                            let action_enabled = app.library_batch_export_progress().is_none()
                                && app.library_ai_mask_refresh_status().is_none()
                                && !app.library.image_paste_in_progress()
                                && !app.library.cloud_action_in_progress()
                                && !app.library.cloud_upload_in_progress()
                                && app.library.cloud_open_receiver.is_none();
                            android_selection_menu(
                                ui,
                                &selected_android_items,
                                action_enabled,
                                action_enabled && app.has_copied_adjustments(),
                                &mut library_action,
                            );
                        }
                    });
                });
                return;
            }

            #[cfg(not(target_os = "android"))]
            let desktop_selection_mode = app.library.selection_mode();
            #[cfg(not(target_os = "android"))]
            let desktop_selection_count = app.library.selected_sources.len();
            #[cfg(not(target_os = "android"))]
            let desktop_selection_available = true;

            #[cfg(not(target_os = "android"))]
            if app.library.cloud_trash_open {
                let count = app.library.cloud_trash_items.len();
                ui.strong(format!(
                    "{count} Trash item{}",
                    if count == 1 { "" } else { "s" }
                ));
            } else if desktop_selection_mode {
                ui.strong(format!("{desktop_selection_count} selected"));
            } else {
                let count = app.library.entries.len();
                ui.strong(format!(
                    "{count} RAW {}",
                    if count == 1 { "file" } else { "files" }
                ));
            }
            #[cfg(target_os = "android")]
            {
                if app.library.cloud_trash_open {
                    let count = app.library.cloud_trash_items.len();
                    ui.strong(format!(
                        "{count} Trash item{}",
                        if count == 1 { "" } else { "s" }
                    ));
                } else {
                    let count = app.library.entries.len();
                    ui.strong(format!(
                        "{count} RAW {}",
                        if count == 1 { "file" } else { "files" }
                    ));
                }
            }

            let mut selected_sort = app.library.sort_order();
            let mut selected_size = app.library.thumbnail_size();

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                #[cfg(target_os = "android")]
                if (if compact_header {
                    crate::ui::icons::phosphor_icon_button(
                        ui,
                        egui_phosphor::regular::GEAR,
                        crate::ui::theme::toolbar_icon_size(),
                        "Settings",
                    )
                } else {
                    crate::ui::theme::toolbar_button(ui, "Settings", 82.0)
                })
                .clicked()
                {
                    app.activate_tab(AppTab::Settings);
                }

                if crate::ui::icons::phosphor_icon_button_enabled(
                    ui,
                    app.library.location.is_some() && !app.library.scanning,
                    egui_phosphor::regular::ARROW_CLOCKWISE,
                    crate::ui::theme::toolbar_icon_size(),
                    "Refresh library",
                )
                .clicked()
                {
                    refresh = true;
                }

                if compact_header {
                    ui.menu_button(
                        egui::RichText::new(egui_phosphor::regular::SLIDERS_HORIZONTAL).size(17.0),
                        |ui| {
                            ui.set_min_width(220.0);
                            #[cfg(not(target_os = "android"))]
                            {
                                if desktop_selection_available
                                    && ui
                                        .button(desktop_selection_toggle_label(
                                            desktop_selection_mode,
                                        ))
                                        .clicked()
                                {
                                    if desktop_selection_mode {
                                        app.library.clear_selection();
                                    } else {
                                        app.library.begin_selection();
                                    }
                                    ui.close();
                                }
                                ui.separator();
                            }

                            ui.strong("Thumbnail size");
                            for thumbnail_size in LibraryThumbnailSize::ALL {
                                ui.selectable_value(
                                    &mut selected_size,
                                    thumbnail_size,
                                    thumbnail_size.label(),
                                );
                            }
                            ui.separator();
                            ui.strong("Sort order");
                            for sort_order in LibrarySortOrder::ALL {
                                ui.selectable_value(
                                    &mut selected_sort,
                                    sort_order,
                                    sort_order.label(),
                                );
                            }
                        },
                    )
                    .response
                    .on_hover_text("Library view options");
                } else {
                    #[cfg(not(target_os = "android"))]
                    if desktop_selection_available
                        && crate::ui::theme::toolbar_button(
                            ui,
                            desktop_selection_toggle_label(desktop_selection_mode),
                            76.0,
                        )
                        .clicked()
                    {
                        if desktop_selection_mode {
                            app.library.clear_selection();
                        } else {
                            app.library.begin_selection();
                        }
                    }

                    egui::ComboBox::from_id_salt("library-sort-order")
                        .selected_text(format!("Sort: {}", selected_sort.label()))
                        .width(154.0)
                        .show_ui(ui, |ui| {
                            for sort_order in LibrarySortOrder::ALL {
                                ui.selectable_value(
                                    &mut selected_sort,
                                    sort_order,
                                    sort_order.label(),
                                );
                            }
                        });

                    egui::ComboBox::from_id_salt("library-thumbnail-size")
                        .selected_text(format!("Size: {}", selected_size.label()))
                        .width(118.0)
                        .show_ui(ui, |ui| {
                            for thumbnail_size in LibraryThumbnailSize::ALL {
                                ui.selectable_value(
                                    &mut selected_size,
                                    thumbnail_size,
                                    thumbnail_size.label(),
                                );
                            }
                        });
                }
            });
            app.set_library_sort_order(selected_sort);
            app.set_library_thumbnail_size(selected_size);
        });

        #[cfg(not(target_os = "android"))]
        if !app.library.is_cloud_view() {
            if let Some(location) = app.library.location() {
                ui.label(
                    egui::RichText::new(location)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                );
            }
        }
        #[cfg(target_os = "android")]
        if !app.library.is_cloud_view() {
            let folder_label = if app.library.android_folder.is_empty() {
                "Local / Library".to_owned()
            } else {
                format!("Local / {}", app.library.android_folder)
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(folder_label)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .truncate(),
            );
        }
        #[cfg(not(target_os = "android"))]
        show_cloud_folder_bar(ui, app);
        show_local_image_paste_bar(ui, app);
        if !app.library.status.is_empty() {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&app.library.status)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .wrap(),
            );
        }
        ui.separator();

        if app.library.cloud_trash_open {
            show_cloud_trash_panel(ui, app);
            return;
        }

        if app.library.location.is_none() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("Choose your top-level photo folder");
                    ui.label("AuRaw builds a folder hierarchy in the desktop sidebar. Select any folder there to show the RAW files directly inside the selected folder.");
                    ui.add_space(8.0);
                    #[cfg(not(target_os = "android"))]
                    if ui.button("Open Top Folder…").clicked() {
                        app.open_library_folder_dialog();
                    }
                });
            });
        } else if app.library.catalog_ready && app.library.entries.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading(if app.library.is_cloud_view() {
                        "No cloud RAW files yet"
                    } else {
                        "No RAW files here yet"
                    });
                    #[cfg(not(target_os = "android"))]
                    if app.library.is_cloud_view() {
                        ui.label("Click + to upload one or more RAW files.");
                    } else {
                        ui.label("Choose another folder or add RAW files to this folder.");
                    }
                    #[cfg(target_os = "android")]
                    if app.library.is_cloud_view() {
                        ui.label("Tap + to upload one or more RAW files.");
                    } else {
                        ui.label("Tap + to import one or more RAW files.");
                    }
                });
            });
        } else {
            #[cfg(not(target_os = "android"))]
            let current_path = app.current_path.clone();
            let available = ui.available_width().max(1.0);
            let available_height = ui.available_height().max(1.0);
            let gap = 6.0;
            let target_thumbnail_height = responsive_thumbnail_target_height(
                available,
                available_height,
                ui.ctx().pixels_per_point(),
                cfg!(target_os = "android"),
            ) * app.library.thumbnail_size().scale();
            let (placements, grid_height) = justified_thumbnail_layout(
                &app.library.entries,
                available,
                target_thumbnail_height,
                gap,
            );

            let mut protected_thumbnail_indices = HashSet::new();
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show_viewport(ui, |ui, viewport| {
                    let (content_rect, _) = ui.allocate_exact_size(
                        egui::vec2(available, grid_height.max(1.0)),
                        Sense::hover(),
                    );
                    let preload_viewport = viewport.expand(600.0);

                    for (index, relative_rect) in placements.iter().copied().enumerate() {
                        if !relative_rect.intersects(preload_viewport) {
                            continue;
                        }

                        // Protect the complete preload window from cache eviction, not
                        // only the currently painted rows. This keeps resize-driven layout
                        // changes from immediately discarding thumbnails we are about to use.
                        protected_thumbnail_indices.insert(index);
                        app.library.touch_and_request_thumbnail(index, ui.ctx());
                        if !relative_rect.intersects(viewport) {
                            continue;
                        }
                        let item_rect = relative_rect.translate(content_rect.min.to_vec2());

                        let entry = &app.library.entries[index];
                        let source = entry.info.source.clone();
                        let name = entry.info.name.clone();
                        let selected = if app.library.selection_mode() {
                            app.library.selected_sources.contains(&source)
                        } else {
                            match &source {
                                #[cfg(not(target_os = "android"))]
                                LibrarySource::File(path) => current_path.as_deref() == Some(path),
                                #[cfg(target_os = "android")]
                                LibrarySource::Android { .. } => false,
                                LibrarySource::Cloud(_) => false,
                            }
                        };
                        let response = thumbnail_tile(ui, entry, item_rect, selected);

                        #[cfg(target_os = "android")]
                        {
                            // egui maps a touch long-press to a secondary click. Enter
                            // selection mode instead of opening a per-thumbnail menu.
                            if response.secondary_clicked() || response.clicked() {
                                match app.library.handle_touch_thumbnail_activation(
                                    &source,
                                    response.secondary_clicked(),
                                ) {
                                    TouchThumbnailAction::Open => {
                                        open_source = Some((source.clone(), name.clone()));
                                    }
                                    TouchThumbnailAction::SelectionChanged {
                                        back_navigation_active,
                                    } => {
                                        crate::android::set_back_navigation_active(
                                            back_navigation_active,
                                        );
                                    }
                                }
                            }
                        }

                        #[cfg(not(target_os = "android"))]
                        {
                            let path = match &source {
                                LibrarySource::File(path) => Some(path.clone()),
                                LibrarySource::Cloud(_) => None,
                            };

                            if response.clicked() && !response.secondary_clicked() {
                                if app.library.selection_mode() {
                                    if !app.library.selected_sources.remove(&source) {
                                        app.library.selected_sources.insert(source.clone());
                                    }
                                } else {
                                    open_source = Some((source.clone(), name.clone()));
                                }
                            }

                            // In desktop selection mode, right-click keeps the familiar
                            // context menu but targets the complete selection. Right-clicking
                            // an unselected thumbnail first adds it to that selection.
                            if response.secondary_clicked()
                                && app.library.selection_mode()
                                && !app.library.selected_sources.contains(&source)
                            {
                                app.library.selected_sources.insert(source.clone());
                            }

                            let context_paths = if app.library.selection_mode() {
                                app.library
                                    .entries
                                    .iter()
                                    .filter(|candidate| {
                                        app.library
                                            .selected_sources
                                            .contains(&candidate.info.source)
                                    })
                                    .filter_map(|candidate| match &candidate.info.source {
                                        LibrarySource::File(path) => Some(path.clone()),
                                        LibrarySource::Cloud(_) => None,
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                path.clone().into_iter().collect()
                            };
                            let context_assets = if app.library.selection_mode() {
                                app.library
                                    .entries
                                    .iter()
                                    .filter(|candidate| {
                                        app.library
                                            .selected_sources
                                            .contains(&candidate.info.source)
                                    })
                                    .filter_map(|candidate| match &candidate.info.source {
                                        LibrarySource::Cloud(asset) => Some(asset.clone()),
                                        LibrarySource::File(_) => None,
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                match &source {
                                    LibrarySource::Cloud(asset) => vec![asset.clone()],
                                    LibrarySource::File(_) => Vec::new(),
                                }
                            };
                            response.context_menu(|ui| match &source {
                                LibrarySource::File(context_source_path) => {
                                    if let Some(action) = desktop_image_context_menu(
                                        ui,
                                        app,
                                        context_source_path,
                                        &context_paths,
                                    ) {
                                        library_action = Some(action);
                                    }
                                }
                                LibrarySource::Cloud(_) => {
                                    if let Some(action) =
                                        cloud_image_context_menu(ui, app, &context_assets)
                                    {
                                        cloud_library_action = Some(action);
                                    }
                                }
                            });
                        }
                    }
                });
            app.library.evict_old_textures(&protected_thumbnail_indices);
        }

        if let Some(action) = cloud_library_action {
            apply_cloud_image_action(app, action, ui.ctx());
        }

        #[cfg(not(target_os = "android"))]
        if let Some(action) = library_action {
            apply_desktop_image_action(ui, app, frame, action);
        }

        show_cloud_dialogs(ui, app);
        #[cfg(not(target_os = "android"))]
        show_local_raw_name_dialog(ui, app, frame);
        #[cfg(target_os = "android")]
        show_android_local_raw_name_dialog(ui, app);
        #[cfg(target_os = "android")]
        show_android_library_folder_dialog(ui, app);

        #[cfg(target_os = "android")]
        if let Some(action) = library_action {
            match action {
                LibraryCardAction::Export(targets) => {
                    if !targets.is_empty() {
                        app.library.export_dialog = Some(LibraryExportDialog {
                            targets: targets
                                .into_iter()
                                .map(|(uri, display_name)| {
                                    crate::app::AndroidLibraryExportTarget::Local {
                                        uri,
                                        display_name,
                                    }
                                })
                                .collect(),
                            settings: app.export_settings.clone(),
                            format: ExportFormat::Jpeg,
                        });
                    }
                }
                LibraryCardAction::CopyAdjustments((uri, display_name)) => {
                    let status =
                        match app.copy_library_adjustments_from_android(&uri, &display_name) {
                            Ok(()) => format!("Copied adjustments from {display_name}"),
                            Err(error) => format!("Could not copy adjustments: {error}"),
                        };
                    app.library.status = status;
                }
                LibraryCardAction::PasteAdjustments(targets) => {
                    let (edited_count, failures) =
                        app.library_adjustment_edit_count_android(&targets);
                    if failures.is_empty() {
                        if edited_count > 0 {
                            app.library.adjustment_paste_dialog =
                                Some(LibraryAdjustmentPasteDialog {
                                    targets: AndroidAdjustmentPasteTargets::Local(targets),
                                    edited_count,
                                });
                        } else {
                            apply_library_adjustment_paste(
                                app,
                                targets,
                                crate::sidecar::AdjustmentPasteMode::Merge,
                                ui.ctx(),
                                frame,
                            );
                        }
                    } else {
                        app.library.status = format!(
                            "Could not inspect selected adjustments. {}",
                            failures.join(" · ")
                        );
                    }
                }
                LibraryCardAction::Copy(items) => {
                    let count = items.len();
                    app.library.image_clipboard = Some(ImageClipboard {
                        mode: ImageClipboardMode::Copy,
                        content: ImageClipboardContent::Local(items),
                    });
                    app.library.cloud_clipboard = None;
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.status = format!(
                        "Copied {count} local RAW{}. Paste the selection in Local or any cloud folder.",
                        if count == 1 { "" } else { "s" }
                    );
                }
                LibraryCardAction::Cut(items) => {
                    let count = items.len();
                    app.library.image_clipboard = Some(ImageClipboard {
                        mode: ImageClipboardMode::Cut,
                        content: ImageClipboardContent::Local(items),
                    });
                    app.library.cloud_clipboard = None;
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.status = format!(
                        "Cut {count} local RAW{}. Paste the selection in Local or any cloud folder.",
                        if count == 1 { "" } else { "s" }
                    );
                }
                LibraryCardAction::Duplicate(targets) => {
                    let total = targets.len();
                    let mut failures = Vec::new();
                    for (uri, display_name) in targets {
                        if let Err(error) = app.duplicate_android_library_item(&uri, &display_name)
                        {
                            failures.push(format!("{display_name}: {error}"));
                        }
                    }
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.refresh(ui.ctx());
                    app.library.status = if failures.is_empty() {
                        format!(
                            "Duplicated {total} selected {}",
                            if total == 1 { "image" } else { "images" }
                        )
                    } else {
                        format!(
                            "Duplicated {} of {total} selected images. {}",
                            total.saturating_sub(failures.len()),
                            failures.join(" · ")
                        )
                    };
                }
                LibraryCardAction::Rename(source) => {
                    app.library.android_raw_name_dialog = Some(AndroidLibraryRawNameDialog {
                        name: source.display_name.clone(),
                        source,
                        error: None,
                        focus_requested: false,
                    });
                }
                LibraryCardAction::ResetAdjustments(targets) => {
                    let total = targets.len();
                    let mut failures = Vec::new();
                    for (uri, display_name) in targets {
                        match app.reset_android_library_adjustments(&uri, &display_name) {
                            Ok(()) => app.library.invalidate_android_adjustment_thumbnail(&uri),
                            Err(error) => failures.push(format!("{display_name}: {error}")),
                        }
                    }
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.refresh(ui.ctx());
                    app.library.status = if failures.is_empty() {
                        format!(
                            "Cleared all adjustments for {total} selected {}",
                            if total == 1 { "image" } else { "images" }
                        )
                    } else {
                        format!(
                            "Cleared all adjustments for {} of {total} selected images. {}",
                            total.saturating_sub(failures.len()),
                            failures.join(" · ")
                        )
                    };
                }
                LibraryCardAction::Delete(targets) => {
                    let total = targets.len();
                    let mut failures = Vec::new();
                    for (uri, display_name) in targets {
                        if let Err(error) = app.delete_android_library_item(&uri, &display_name) {
                            failures.push(format!("{display_name}: {error}"));
                        }
                    }
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.refresh(ui.ctx());
                    app.library.status = if failures.is_empty() {
                        format!(
                            "Deleted {total} selected {}",
                            if total == 1 { "image" } else { "images" }
                        )
                    } else {
                        format!(
                            "Completed {} of {total} selected actions. {}",
                            total.saturating_sub(failures.len()),
                            failures.join(" · ")
                        )
                    };
                }
            }
        }

        #[cfg(not(target_os = "android"))]
        show_desktop_image_action_overlays(ui, app, frame);

        #[cfg(target_os = "android")]
        {
            let mut paste_action = 0u8;
            if let Some(dialog) = app.library.adjustment_paste_dialog.as_ref() {
                let target_count = dialog.targets.len();
                crate::ui::responsive_popup(egui::Window::new("Paste adjustments"), ui.ctx(), 480.0)
                    .id(egui::Id::new(
                        "android-library-adjustment-paste-conflict-dialog",
                    ))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(ui.ctx(), |ui| {
                        ui.label(format!(
                            "{} of the {} selected {} already contain edits.",
                            dialog.edited_count,
                            target_count,
                            if target_count == 1 { "image" } else { "images" }
                        ));
                        ui.add_space(4.0);
                        ui.label(
                            "Merge overwrites only the copied categories and preserves every unchecked category already on the destination.",
                        );
                        ui.label(
                            "Replace clears the destination edit state first, then applies the categories stored in the adjustment clipboard.",
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                paste_action = 1;
                            }
                            if ui.button("Merge").clicked() {
                                paste_action = 2;
                            }
                            if ui.button("Replace").clicked() {
                                paste_action = 3;
                            }
                        });
                    });
            }
            if paste_action != 0 {
                if let Some(dialog) = app.library.adjustment_paste_dialog.take() {
                    let mode = match paste_action {
                        2 => Some(crate::sidecar::AdjustmentPasteMode::Merge),
                        3 => Some(crate::sidecar::AdjustmentPasteMode::Replace),
                        _ => None,
                    };
                    match (mode, dialog.targets) {
                        (Some(mode), AndroidAdjustmentPasteTargets::Local(targets)) => {
                            apply_library_adjustment_paste(app, targets, mode, ui.ctx(), frame);
                        }
                        (Some(mode), AndroidAdjustmentPasteTargets::Cloud(paths)) => {
                            apply_android_cloud_adjustment_paste(app, paths, mode, ui.ctx(), frame);
                        }
                        (None, _) => {}
                    }
                }
            }

            let mut refresh_action = 0u8;
            let can_regenerate = app.can_start_library_ai_mask_refresh();
            if let Some(prompt) = app.library.ai_mask_refresh_prompt.as_ref() {
                let target_count = prompt.targets.len();
                crate::ui::responsive_popup(egui::Window::new("Regenerate AI masks?"), ui.ctx(), 460.0)
                    .id(egui::Id::new("android-library-ai-mask-refresh-prompt"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(ui.ctx(), |ui| {
                        ui.label(format!(
                            "{} pasted {} contain content-aware masks that belong to the source image.",
                            target_count,
                            if target_count == 1 { "image" } else { "images" }
                        ));
                        ui.label(
                            "Regenerate them now for each destination image? Mask groups, settings, object strokes, and local adjustments are preserved.",
                        );
                        if !can_regenerate {
                            ui.label(
                                egui::RichText::new(
                                    "Waiting for the current RAW load or edit save to finish…",
                                )
                                .small()
                                .color(ui.visuals().weak_text_color()),
                            );
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Not now").clicked() {
                                refresh_action = 1;
                            }
                            if ui
                                .add_enabled(can_regenerate, egui::Button::new("Regenerate"))
                                .clicked()
                            {
                                refresh_action = 2;
                            }
                        });
                    });
            }
            if refresh_action != 0 {
                if let Some(prompt) = app.library.ai_mask_refresh_prompt.take() {
                    if refresh_action == 2 {
                        app.start_library_ai_mask_refresh_android(prompt.targets, frame);
                    }
                }
            }
        }

        #[cfg(target_os = "android")]
        if let Some((completed, total, failed, current_name)) = app.library_ai_mask_refresh_status()
        {
            if app.library_ai_mask_refresh_progress_open() {
                let fraction = if total == 0 {
                    0.0
                } else {
                    (completed as f32 / total as f32).clamp(0.0, 1.0)
                };
                #[cfg(not(target_os = "android"))]
                let mut minimize = false;
                let mut cancel = false;
                crate::ui::responsive_popup(
                    egui::Window::new("Regenerating AI masks"),
                    ui.ctx(),
                    360.0,
                )
                .id(egui::Id::new("library-ai-mask-refresh-progress"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{completed} / {total} AI masks updated"))
                            .strong(),
                    );
                    ui.add_space(6.0);
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .show_percentage()
                            .animate(completed < total),
                    );
                    if let Some(name) = current_name.as_deref() {
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Refreshing {name}…"));
                        });
                    }
                    if failed > 0 {
                        ui.label(
                            egui::RichText::new(format!(
                                "{failed} {} failed",
                                if failed == 1 { "image" } else { "images" }
                            ))
                            .small()
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        #[cfg(not(target_os = "android"))]
                        {
                            minimize = ui.button("Minimize").clicked();
                        }
                        cancel = ui.button("Cancel").clicked();
                    });
                });
                #[cfg(not(target_os = "android"))]
                if minimize {
                    app.minimize_library_ai_mask_refresh_progress();
                }
                if cancel {
                    app.cancel_library_ai_mask_refresh();
                }
            }
        }

        #[cfg(target_os = "android")]
        {
            let mut close_export_dialog = false;
            let mut confirm_export = false;
            if let Some(dialog) = app.library.export_dialog.as_mut() {
                let count = dialog.targets.len();
                let title = if count == 1 {
                    "Export image".to_owned()
                } else {
                    format!("Export {count} images")
                };
                crate::ui::responsive_popup(egui::Window::new(title), ui.ctx(), 480.0)
                    .id(egui::Id::new("android-library-export-dialog"))
                    .collapsible(false)
                    .resizable(true)
                    .show(ui.ctx(), |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Format");
                            ui.selectable_value(&mut dialog.format, ExportFormat::Jpeg, "JPEG");
                            ui.selectable_value(&mut dialog.format, ExportFormat::Png, "PNG");
                            ui.selectable_value(&mut dialog.format, ExportFormat::Tiff, "TIFF");
                        });
                        match dialog.format {
                            ExportFormat::Jpeg => {
                                dialog.settings.bit_depth = crate::pipeline::ExportBitDepth::Eight;
                            }
                            ExportFormat::Png
                                if dialog.settings.bit_depth
                                    == crate::pipeline::ExportBitDepth::Float32Linear =>
                            {
                                dialog.settings.bit_depth = crate::pipeline::ExportBitDepth::Sixteen;
                            }
                            _ => {}
                        }
                        ui.add_space(6.0);
                        crate::ui::sidebar::export_settings_controls(
                            ui,
                            &mut dialog.settings,
                            None,
                            false,
                            None,
                        );
                        match dialog.format {
                            ExportFormat::Jpeg => {
                                dialog.settings.bit_depth = crate::pipeline::ExportBitDepth::Eight;
                            }
                            ExportFormat::Png
                                if dialog.settings.bit_depth
                                    == crate::pipeline::ExportBitDepth::Float32Linear =>
                            {
                                dialog.settings.bit_depth = crate::pipeline::ExportBitDepth::Sixteen;
                            }
                            _ => {}
                        }
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(
                                "Exports are saved to Pictures/AuRaw. File names are generated from each RAW name.",
                            )
                            .small()
                            .color(ui.visuals().weak_text_color()),
                        );
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close_export_dialog = true;
                            }
                            let label = if count == 1 {
                                "Export 1 image".to_owned()
                            } else {
                                format!("Export {count} images")
                            };
                            if ui.button(label).clicked() {
                                confirm_export = true;
                            }
                        });
                    });
            }

            if confirm_export {
                if let Some(dialog) = app.library.export_dialog.clone() {
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.library.export_dialog = None;
                    app.start_android_library_exports(
                        dialog.targets,
                        dialog.settings.clone(),
                        dialog.format,
                    );
                }
            } else if close_export_dialog {
                app.library.export_dialog = None;
            }

            show_library_batch_export_progress(ui, app);
        }

        #[cfg(target_os = "android")]
        let show_import_fab = !app.library.has_selection();
        #[cfg(not(target_os = "android"))]
        let show_import_fab = app.library.is_cloud_view() && !app.library.selection_mode();
        if show_import_fab && !app.library.cloud_upload_in_progress() {
            let cloud_upload = app.library.is_cloud_view();
            let bounds = ui.max_rect().shrink(16.0);
            let rect = library_import_fab_rect(bounds);
            let response = ui.put(
                rect,
                egui::Button::new(egui::RichText::new(library_import_icon()).size(26.0))
                    .min_size(rect.size())
                    .corner_radius(LIBRARY_IMPORT_FAB_EDGE * 0.5)
                    .fill(ui.visuals().selection.bg_fill),
            );
            if response.clicked() {
                import_raw = true;
            }
            response.on_hover_text(if cloud_upload {
                "Upload RAW files to AuRaw Cloud"
            } else {
                "Import RAW"
            });
        }

        if refresh {
            app.library.refresh(ui.ctx());
        }
        if import_raw {
            if app.library.is_cloud_view() {
                app.open_cloud_upload_dialog(frame);
            } else {
                #[cfg(target_os = "android")]
                app.open_file_dialog(frame);
            }
        }
        if let Some((source, display_name)) = open_source {
            match source {
                #[cfg(not(target_os = "android"))]
                LibrarySource::File(path) => {
                    let _ = display_name;
                    app.active_tab = AppTab::Develop;
                    app.open_path(path, frame);
                }
                #[cfg(target_os = "android")]
                LibrarySource::Android { uri, .. } => {
                    app.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                    app.open_android_library_document(&uri, &display_name);
                }
                LibrarySource::Cloud(asset) => {
                    app.library.start_cloud_open(asset, ui.ctx());
                }
            }
        }
    }
}

fn responsive_thumbnail_target_height(
    available_width: f32,
    available_height: f32,
    pixels_per_point: f32,
    android: bool,
) -> f32 {
    if android {
        // Android's logical-point coordinate system already follows device density.
        // Keep touch targets predictable instead of making them balloon on very dense phones.
        return 120.0;
    }

    // egui sizes are already expressed in DPI-aware logical points, so using the raw
    // pixels-per-point value as a direct multiplier would double-apply OS display scaling.
    // Scale primarily with usable window area: a 4K/full-screen workspace should show
    // substantially larger rows than a small laptop window, while preserving similar
    // gallery density as the app is resized.
    const REFERENCE_WIDTH: f32 = 1280.0;
    const REFERENCE_HEIGHT: f32 = 720.0;
    const BASE_HEIGHT: f32 = 140.0;

    let width = if available_width.is_finite() {
        available_width.max(1.0)
    } else {
        REFERENCE_WIDTH
    };
    let height = if available_height.is_finite() {
        available_height.max(1.0)
    } else {
        REFERENCE_HEIGHT
    };
    let viewport_scale = ((width * height) / (REFERENCE_WIDTH * REFERENCE_HEIGHT))
        .sqrt()
        .clamp(0.90, 1.70);

    // A restrained density adjustment helps very high-DPI desktop displays without
    // fighting egui/OS scaling. sqrt keeps 150–200% scaling from becoming excessive.
    let density_scale = if pixels_per_point.is_finite() {
        pixels_per_point.max(1.0).sqrt().clamp(1.0, 1.20)
    } else {
        1.0
    };

    (BASE_HEIGHT * viewport_scale * density_scale).clamp(126.0, 270.0)
}

fn balanced_justified_row_ranges(
    aspects: &[f32],
    available_width: f32,
    target_height: f32,
    gap: f32,
) -> Vec<(usize, usize)> {
    if aspects.is_empty() {
        return Vec::new();
    }

    // Treat every item as its aspect-ratio width plus the gap that follows it.
    // This lets us estimate the ideal number of rows for the whole gallery
    // before choosing any breaks, instead of greedily leaving a tiny orphan
    // row at the end.
    let gap_weight = gap / target_height.max(1.0);
    let weights = aspects
        .iter()
        .map(|aspect| aspect.max(f32::EPSILON) + gap_weight)
        .collect::<Vec<_>>();
    let total_weight = weights.iter().sum::<f32>();
    let target_row_weight = (available_width + gap) / target_height.max(1.0);
    let row_count = (total_weight / target_row_weight.max(f32::EPSILON))
        .round()
        .clamp(1.0, aspects.len() as f32) as usize;

    let mut ranges = Vec::with_capacity(row_count);
    let mut start = 0usize;
    let mut remaining_weight = total_weight;

    for row_index in 0..row_count {
        let rows_left = row_count - row_index;
        if rows_left == 1 {
            ranges.push((start, aspects.len()));
            break;
        }

        let max_end = aspects.len() - (rows_left - 1);
        let desired_weight = remaining_weight / rows_left as f32;
        let mut end = start + 1;
        let mut row_weight = weights[start];

        // Pick the break closest to an equal share of the gallery's total
        // visual width. Because every future row is reserved at least one
        // image, the final row cannot collapse into a few oversized leftovers.
        while end < max_end {
            let with_next = row_weight + weights[end];
            if (row_weight - desired_weight).abs() <= (with_next - desired_weight).abs() {
                break;
            }
            row_weight = with_next;
            end += 1;
        }

        ranges.push((start, end));
        remaining_weight = (remaining_weight - row_weight).max(0.0);
        start = end;
    }

    ranges
}

fn justified_thumbnail_layout(
    entries: &[LibraryEntry],
    available_width: f32,
    target_height: f32,
    gap: f32,
) -> (Vec<egui::Rect>, f32) {
    let available_width = available_width.max(1.0);
    let target_height = target_height.max(1.0);
    let gap = gap.max(0.0);
    let aspects: Vec<f32> = entries
        .iter()
        .map(|entry| {
            entry
                .layout_size
                .or(entry.thumbnail_size)
                .and_then(|[width, source_height]| {
                    (width > 0 && source_height > 0).then_some(width as f32 / source_height as f32)
                })
                .filter(|aspect| aspect.is_finite() && *aspect > 0.0)
                .unwrap_or(1.5)
        })
        .collect();

    let row_ranges = balanced_justified_row_ranges(&aspects, available_width, target_height, gap);
    let mut placements = Vec::with_capacity(entries.len());
    let mut y = 0.0;

    for (row_start, row_end) in row_ranges {
        let row_aspects = &aspects[row_start..row_end];
        let item_count = row_aspects.len();
        let aspect_sum = row_aspects.iter().sum::<f32>();
        let gaps_width = gap * (item_count.saturating_sub(1) as f32);
        let justified_height =
            ((available_width - gaps_width).max(1.0) / aspect_sum.max(f32::EPSILON)).max(1.0);
        // A sparse row must not inflate a handful of thumbnails to fill the
        // entire viewport. Keep such rows at the same responsive target height
        // as a full gallery row and leave the unused space on the right. Rows
        // that need to shrink still justify normally so they never overflow a
        // narrow phone or window.
        let row_is_justified = justified_height <= target_height;
        let row_height = justified_height.min(target_height);
        let mut x = 0.0;

        for (row_offset, aspect) in row_aspects.iter().copied().enumerate() {
            // Give the final item the exact remaining width to absorb floating-
            // point rounding when this is a justified row. Sparse rows retain
            // every thumbnail's natural aspect width instead of stretching the
            // final item across all remaining space.
            let width = if row_is_justified && row_offset + 1 == item_count {
                (available_width - x).max(1.0)
            } else {
                row_height * aspect
            };
            placements.push(egui::Rect::from_min_size(
                egui::pos2(x, y),
                egui::vec2(width, row_height),
            ));
            x += width + gap;
        }

        y += row_height + gap;
    }

    let total_height = if placements.is_empty() {
        0.0
    } else {
        (y - gap).max(0.0)
    };
    (placements, total_height)
}

fn thumbnail_cover_uv(source_size: Option<[u32; 2]>, target_size: egui::Vec2) -> egui::Rect {
    let Some([width, height]) = source_size.filter(|[width, height]| *width > 0 && *height > 0)
    else {
        return egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    };
    if target_size.x <= 0.0 || target_size.y <= 0.0 {
        return egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    }

    let source_aspect = width as f32 / height as f32;
    let target_aspect = target_size.x / target_size.y;
    if !source_aspect.is_finite() || !target_aspect.is_finite() {
        return egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
    }

    if source_aspect > target_aspect {
        let visible = (target_aspect / source_aspect).clamp(0.0, 1.0);
        let inset = (1.0 - visible) * 0.5;
        egui::Rect::from_min_max(egui::pos2(inset, 0.0), egui::pos2(1.0 - inset, 1.0))
    } else if source_aspect < target_aspect {
        let visible = (source_aspect / target_aspect).clamp(0.0, 1.0);
        let inset = (1.0 - visible) * 0.5;
        egui::Rect::from_min_max(egui::pos2(0.0, inset), egui::pos2(1.0, 1.0 - inset))
    } else {
        egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0))
    }
}

fn thumbnail_tile(
    ui: &mut Ui,
    entry: &LibraryEntry,
    rect: egui::Rect,
    selected: bool,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("library-thumbnail-tile", entry.info.display_path.as_str())),
        Sense::click(),
    );
    let visuals = ui.visuals();

    ui.painter()
        .rect_filled(rect, 0.0, Color32::from_rgb(17, 18, 20));
    if let Some(texture) = &entry.texture {
        let uv = thumbnail_cover_uv(entry.thumbnail_size, rect.size());
        ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
    } else {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            if entry.thumbnail_error.is_some() {
                "Retrying preview…"
            } else if entry.thumbnail_queued {
                "Loading preview…"
            } else {
                "RAW"
            },
            FontId::proportional(11.0),
            visuals.weak_text_color(),
        );
    }

    if response.hovered() {
        ui.painter()
            .rect_filled(rect, 0.0, Color32::from_white_alpha(14));
    }
    if selected {
        ui.painter().rect_stroke(
            rect,
            0.0,
            Stroke::new(2.0, visuals.selection.bg_fill),
            StrokeKind::Inside,
        );
    }

    let overlay_height = 32.0_f32.min(rect.height());
    let overlay = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.bottom() - overlay_height),
        rect.right_bottom(),
    );
    ui.painter()
        .rect_filled(overlay, 0.0, Color32::from_black_alpha(116));
    let max_chars = ((rect.width() - 16.0) / 7.0).floor().max(8.0) as usize;
    ui.painter().text(
        egui::pos2(rect.left() + 8.0, rect.bottom() - 7.0),
        Align2::LEFT_BOTTOM,
        elide_middle(&entry.info.name, max_chars),
        FontId::proportional(12.5),
        Color32::WHITE,
    );

    if let LibrarySource::Cloud(asset) = &entry.info.source {
        let (icon, color, _) =
            cloud_sync_badge(entry.info.cloud_sync_state, entry.info.cloud_downloaded);
        let badge = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 37.0, rect.top() + 7.0),
            egui::vec2(29.0, 25.0),
        );
        ui.painter()
            .rect_filled(badge, 4.0, Color32::from_black_alpha(176));
        ui.painter().text(
            badge.center(),
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(16.0),
            color,
        );
        if let Some(icon) = cloud_preview_icon(asset.thumbnail_kind) {
            let preview_badge = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 7.0, rect.top() + 7.0),
                egui::vec2(29.0, 25.0),
            );
            ui.painter()
                .rect_filled(preview_badge, 4.0, Color32::from_black_alpha(176));
            ui.painter().text(
                preview_badge.center(),
                Align2::CENTER_CENTER,
                icon,
                FontId::proportional(16.0),
                Color32::from_rgb(170, 205, 245),
            );
        }
        if let Some(label) = cloud_preview_label(asset.thumbnail_kind) {
            let label_width =
                (label.chars().count() as f32 * 6.4 + 14.0).min((rect.width() - 53.0).max(0.0));
            if label_width >= 58.0 {
                let preview_badge = egui::Rect::from_min_size(
                    egui::pos2(rect.left() + 7.0, rect.top() + 7.0),
                    egui::vec2(label_width, 25.0),
                );
                ui.painter()
                    .rect_filled(preview_badge, 4.0, Color32::from_black_alpha(176));
                ui.painter().text(
                    preview_badge.center(),
                    Align2::CENTER_CENTER,
                    label,
                    FontId::proportional(10.5),
                    Color32::from_rgb(170, 205, 245),
                );
            }
        }
    }

    let mut tooltip = entry.info.display_path.clone();
    if let LibrarySource::Cloud(asset) = &entry.info.source {
        let (_, _, sync_text) =
            cloud_sync_badge(entry.info.cloud_sync_state, entry.info.cloud_downloaded);
        tooltip.push('\n');
        tooltip.push_str(sync_text);
        if let Some(preview_notice) = cloud_preview_notice(asset.thumbnail_kind) {
            tooltip.push('\n');
            tooltip.push_str(preview_notice);
        }
    }
    if let Some(error) = &entry.thumbnail_error {
        tooltip.push_str("\nPreview: ");
        tooltip.push_str(error);
    }
    response.on_hover_text(tooltip)
}

#[cfg(not(target_os = "android"))]
struct RankedLibraryFile {
    info: LibraryFileInfo,
    lowercase_name: String,
}

#[cfg(not(target_os = "android"))]
impl RankedLibraryFile {
    fn new(info: LibraryFileInfo) -> Self {
        let lowercase_name = info.name.to_lowercase();
        Self {
            info,
            lowercase_name,
        }
    }
}

#[cfg(not(target_os = "android"))]
impl PartialEq for RankedLibraryFile {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == CmpOrdering::Equal
    }
}

#[cfg(not(target_os = "android"))]
impl Eq for RankedLibraryFile {}

#[cfg(not(target_os = "android"))]
impl PartialOrd for RankedLibraryFile {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

#[cfg(not(target_os = "android"))]
impl Ord for RankedLibraryFile {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // This is also the final display order: newest first, then stable
        // lexical tie-breakers. BinaryHeap therefore keeps the worst retained
        // item at its root, where a better candidate can replace it.
        other
            .info
            .modified
            .cmp(&self.info.modified)
            .then_with(|| self.lowercase_name.cmp(&other.lowercase_name))
            .then_with(|| self.info.display_path.cmp(&other.info.display_path))
            .then_with(|| match (&self.info.source, &other.info.source) {
                (LibrarySource::File(left), LibrarySource::File(right)) => left.cmp(right),
                _ => self.info.display_path.cmp(&other.info.display_path),
            })
    }
}

#[cfg(not(target_os = "android"))]
type FolderScan = (Vec<LibraryFileInfo>, usize, bool);

#[cfg(not(target_os = "android"))]
fn scan_folder_tree(root: &Path, is_cancelled: impl Fn() -> bool) -> Option<LibraryFolderNode> {
    fn visit(path: PathBuf, is_cancelled: &impl Fn() -> bool) -> Option<LibraryFolderNode> {
        if is_cancelled() {
            return None;
        }

        let mut node = LibraryFolderNode::empty(path.clone());
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                log::warn!(
                    "could not read library folder hierarchy at {}: {error}",
                    path.display()
                );
                return Some(node);
            }
        };

        for entry in entries {
            if is_cancelled() {
                return None;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    log::warn!("could not read a library folder entry: {error}");
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    log::warn!("could not inspect {}: {error}", entry.path().display());
                    continue;
                }
            };
            // Directory symlinks are not followed, so a cycle cannot make the
            // desktop folder hierarchy recurse forever.
            if !file_type.is_dir() {
                continue;
            }
            if let Some(child) = visit(entry.path(), is_cancelled) {
                node.children.push(child);
            } else {
                return None;
            }
        }

        node.children.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.path.cmp(&right.path))
        });
        Some(node)
    }

    visit(root.to_path_buf(), &is_cancelled)
}

#[cfg(not(target_os = "android"))]
fn scan_folder(
    folder: &Path,
    is_cancelled: impl Fn() -> bool,
) -> Result<Option<FolderScan>, String> {
    scan_folder_with_limit(folder, MAX_LIBRARY_FILES, is_cancelled)
}

#[cfg(not(target_os = "android"))]
fn scan_folder_with_limit(
    folder: &Path,
    maximum_files: usize,
    is_cancelled: impl Fn() -> bool,
) -> Result<Option<FolderScan>, String> {
    if is_cancelled() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(folder)
        .map_err(|error| format!("Could not read {}: {error}", folder.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }

    let mut files = BinaryHeap::with_capacity(maximum_files);
    let mut warning_count = 0usize;
    let mut truncated = false;
    let entries = std::fs::read_dir(folder)
        .map_err(|error| format!("Could not scan {}: {error}", folder.display()))?;
    for entry in entries {
        if is_cancelled() {
            return Ok(None);
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warning_count += 1;
                log::warn!("could not read a library directory entry: {error}");
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                warning_count += 1;
                log::warn!("could not inspect {}: {error}", entry.path().display());
                continue;
            }
        };
        // Only direct children of the selected folder belong to this view.
        // Symlinks and subdirectories are deliberately ignored.
        if !file_type.is_file() || !is_supported_raw_path(&entry.path()) {
            continue;
        }
        let path = entry.path();
        let file_metadata = entry.metadata().ok();
        let candidate = RankedLibraryFile::new(LibraryFileInfo {
            source: LibrarySource::File(path.clone()),
            display_path: path.display().to_string(),
            name: entry.file_name().to_string_lossy().into_owned(),
            bytes: file_metadata.as_ref().map_or(0, std::fs::Metadata::len),
            dimensions_hint: None,
            cloud_downloaded: false,
            cloud_sync_state: crate::cloud::CloudSyncState::Synced,
            modified: file_metadata.and_then(|metadata| metadata.modified().ok()),
        });
        if files.len() < maximum_files {
            files.push(candidate);
        } else {
            truncated = true;
            if files
                .peek()
                .is_some_and(|worst_retained| candidate < *worst_retained)
            {
                files.pop();
                files.push(candidate);
            }
        }
    }
    let mut files = files.into_vec();
    files.sort();
    let mut files = files
        .into_iter()
        .map(|ranked| ranked.info)
        .collect::<Vec<_>>();

    // Reserve the final gallery geometry before any preview pixels arrive.
    // LibRaw can expose display-oriented active dimensions from the header
    // after open_file/identify without unpacking the sensor or decoding a
    // thumbnail, so placeholders start at the same aspect ratio as the image.
    for info in &mut files {
        if is_cancelled() {
            return Ok(None);
        }
        if let LibrarySource::File(path) = &info.source {
            info.dimensions_hint = load_raw_display_dimensions(path).ok();
        }
    }

    Ok(Some((files, warning_count, truncated)))
}

#[cfg(test)]
fn format_file_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    match bytes {
        0..=1023 => format!("{bytes} B"),
        1024..=1_048_575 => format!("{:.1} KiB", bytes as f64 / KIB),
        1_048_576..=1_073_741_823 => format!("{:.1} MiB", bytes as f64 / MIB),
        _ => format!("{:.1} GiB", bytes as f64 / GIB),
    }
}

fn elide_middle(value: &str, maximum_chars: usize) -> String {
    let count = value.chars().count();
    if count <= maximum_chars || maximum_chars < 5 {
        return value.to_owned();
    }
    let left = (maximum_chars - 1) / 2;
    let right = maximum_chars - 1 - left;
    let prefix = value.chars().take(left).collect::<String>();
    let suffix = value.chars().skip(count - right).collect::<String>();
    format!("{prefix}…{suffix}")
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_os = "android"))]
    use super::LibrarySource;
    use super::{
        android_folder_ancestors, android_folder_parent, android_library_location_label,
        balanced_justified_row_ranges, catalog_status, cloud_cache_icon,
        cloud_folder_id_for_catalog, cloud_preview_icon, cloud_preview_label, cloud_preview_notice,
        cloud_sync_badge, copy_directory_create_new, desktop_selection_toggle_label,
        duplicate_raw_and_sidecar, elide_middle, format_file_size, import_folder_into_library,
        import_raw_into_folder, justified_thumbnail_layout, library_import_fab_rect,
        library_import_icon, loaded_library_thumbnail, make_resident_thumbnail, new_library_entry,
        rename_raw_bundle, run_folder_operation, run_image_paste, run_thumbnail_workers,
        scan_folder, scan_folder_tree, scan_folder_with_limit, trash_age_label,
        trash_remaining_label, trash_size_label, validate_folder_name, ImageClipboard,
        ImageClipboardContent, ImageClipboardMode, ImagePasteDestination, LibraryFileInfo,
        LibraryFolderOperation, LibraryState, LibraryThumbnailSize, LibraryView, RawImportOutcome,
        ScanEvent, ThumbnailRequest, ThumbnailWorker, TouchThumbnailAction,
        LIBRARY_IMPORT_FAB_EDGE,
    };
    use crate::pipeline::RawThumbnail;
    use eframe::egui::Color32;
    use std::collections::HashSet;
    use std::fs;
    #[cfg(not(target_os = "android"))]
    use std::io::{Read, Write};
    #[cfg(not(target_os = "android"))]
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, RwLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn android_folder_navigation_uses_normalized_relative_paths() {
        assert_eq!(android_folder_parent("2026/Trip"), "2026");
        assert_eq!(android_folder_parent("2026"), "");
        assert_eq!(
            android_folder_ancestors("2026/Trip"),
            HashSet::from([String::new(), "2026".to_owned()])
        );
        assert_eq!(
            android_library_location_label("/media/.library", "2026/Trip"),
            "/media/.library/2026/Trip"
        );
    }

    #[test]
    fn successful_catalog_status_does_not_repeat_the_header_file_count() {
        assert_eq!(catalog_status(0, false), "");
        assert_eq!(catalog_status(1, false), "1 unreadable item");
        assert!(catalog_status(0, true).contains("RAW files shown"));
    }

    #[cfg(not(target_os = "android"))]
    fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "client closed before sending HTTP headers");
            request.extend_from_slice(&buffer[..count]);
            if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let content_length = String::from_utf8_lossy(&request[..header_end])
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "client closed before sending HTTP body");
            request.extend_from_slice(&buffer[..count]);
        }
        request
    }

    #[cfg(not(target_os = "android"))]
    fn write_http_response(stream: &mut std::net::TcpStream, content_type: &str, body: &[u8]) {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

    #[cfg(not(target_os = "android"))]
    fn sha256_hex(bytes: &[u8]) -> String {
        ring::digest::digest(&ring::digest::SHA256, bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[cfg(not(target_os = "android"))]
    fn test_developed_thumbnail() -> RawThumbnail {
        RawThumbnail {
            width: 16,
            height: 12,
            rgba: [28, 74, 196, 255].repeat(16 * 12),
        }
    }

    #[cfg(not(target_os = "android"))]
    fn install_test_developed_thumbnail(raw: &std::path::Path) {
        let fingerprint = crate::sidecar::desktop_sidecar_fingerprint(raw)
            .unwrap()
            .expect("test RAW should have an edit sidecar");
        crate::sidecar::save_developed_thumbnail_cache(
            raw,
            &test_developed_thumbnail(),
            fingerprint,
        )
        .unwrap();
    }

    #[cfg(not(target_os = "android"))]
    fn assert_test_developed_thumbnail(raw: &std::path::Path) {
        let thumbnail = crate::sidecar::load_developed_thumbnail_cache(raw, 512)
            .unwrap()
            .expect("copied RAW should retain its developed thumbnail");
        assert_eq!([thumbnail.width, thumbnail.height], [16, 12]);
        let pixel = &thumbnail.rgba[..4];
        assert!(pixel[2] > pixel[1] && pixel[1] > pixel[0], "{pixel:?}");
    }

    #[cfg(not(target_os = "android"))]
    fn test_developed_thumbnail_jpeg() -> Vec<u8> {
        let thumbnail = test_developed_thumbnail();
        let rgba =
            image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.rgba).unwrap();
        let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92)
            .encode(
                rgb.as_raw(),
                thumbnail.width,
                thumbnail.height,
                image::ExtendedColorType::Rgb8,
            )
            .unwrap();
        jpeg
    }

    #[test]
    fn missing_cloud_catalog_folder_falls_back_to_root() {
        let folder = crate::cloud::CloudFolder {
            id: "a".repeat(64),
            parent_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
            name: "Trips".to_owned(),
        };
        assert_eq!(
            cloud_folder_id_for_catalog(&folder.id, std::slice::from_ref(&folder)),
            folder.id.clone()
        );
        assert_eq!(
            cloud_folder_id_for_catalog(&"b".repeat(64), &[folder]),
            crate::cloud::CLOUD_ROOT_FOLDER_ID
        );
    }

    #[test]
    fn middle_elision_preserves_both_ends() {
        assert_eq!(elide_middle("abcdefghijklmnop", 9), "abcd…mnop");
        assert_eq!(elide_middle("short", 9), "short");
    }

    #[test]
    fn file_sizes_are_readable() {
        assert_eq!(format_file_size(500), "500 B");
        assert_eq!(format_file_size(1024), "1.0 KiB");
        assert_eq!(format_file_size(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn cloud_trash_retention_metadata_is_readable() {
        assert_eq!(trash_age_label(0), "just now");
        assert_eq!(trash_age_label(2 * 24 * 60 * 60), "2 days ago");
        assert_eq!(trash_remaining_label(25 * 60 * 60), "2 days remaining");
        assert_eq!(trash_size_label(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn desktop_selection_toggle_label_matches_the_next_action() {
        assert_eq!(desktop_selection_toggle_label(false), "Select");
        assert_eq!(desktop_selection_toggle_label(true), "Cancel");
    }

    #[test]
    fn thumbnail_size_defaults_to_average_with_small_preserving_the_old_scale() {
        assert_eq!(
            LibraryThumbnailSize::default(),
            LibraryThumbnailSize::Medium
        );
        assert_eq!(LibraryThumbnailSize::Small.scale(), 1.0);
        assert_eq!(LibraryThumbnailSize::Medium.scale(), 1.25);
        assert_eq!(LibraryThumbnailSize::Large.scale(), 1.5);
        assert_eq!(LibraryThumbnailSize::Enormous.scale(), 1.75);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn touch_thumbnail_activation_enters_toggles_and_exits_selection() {
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);
        let source = LibrarySource::File(PathBuf::from("selection.dng"));

        let action = library.handle_touch_thumbnail_activation(&source, true);
        assert_eq!(
            action,
            TouchThumbnailAction::SelectionChanged {
                back_navigation_active: true
            }
        );
        assert!(library.selection_mode());
        assert!(library.selected_sources.contains(&source));

        let action = library.handle_touch_thumbnail_activation(&source, false);
        assert_eq!(
            action,
            TouchThumbnailAction::SelectionChanged {
                back_navigation_active: false
            }
        );
        assert!(!library.selection_mode());
        assert!(library.selected_sources.is_empty());

        assert_eq!(
            library.handle_touch_thumbnail_activation(&source, false),
            TouchThumbnailAction::Open
        );
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn cloud_sources_support_multi_selection_and_nested_breadcrumbs() {
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);
        let parent = crate::cloud::CloudFolder {
            id: "a".repeat(64),
            parent_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
            name: "Trips".to_owned(),
        };
        let child = crate::cloud::CloudFolder {
            id: "b".repeat(64),
            parent_id: parent.id.clone(),
            name: "Day 1".to_owned(),
        };
        library.cloud_folders = vec![parent.clone(), child.clone()];
        library.cloud_folder_id = child.id.clone();
        assert_eq!(
            library.cloud_folder_path(&child.id),
            "Cloud / Trips / Day 1"
        );
        assert_eq!(
            library.cloud_breadcrumbs(),
            vec![
                (
                    crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
                    "Cloud".to_owned()
                ),
                (parent.id, parent.name),
                (child.id, child.name),
            ]
        );

        let asset = crate::cloud::CloudAsset {
            id: "c".repeat(64),
            name: "photo.dng".to_owned(),
            bytes: 10,
            modified_seconds: 1,
            width: 10,
            height: 10,
            raw_etag: "d".repeat(64),
            sidecar_etag: None,
            thumbnail_etag: "e".repeat(64),
            thumbnail_kind: crate::cloud::CloudThumbnailKind::Edited,
            folder_id: "b".repeat(64),
        };
        let source = LibrarySource::Cloud(asset);
        assert!(matches!(
            library.handle_touch_thumbnail_activation(&source, true),
            TouchThumbnailAction::SelectionChanged {
                back_navigation_active: true
            }
        ));
        assert!(library.selected_sources.contains(&source));
    }

    #[test]
    fn import_fab_is_square_bottom_right_and_uses_plus_icon() {
        let bounds = eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(10.0, 20.0),
            eframe::egui::vec2(300.0, 400.0),
        );
        let rect = library_import_fab_rect(bounds);
        assert_eq!(
            rect.size(),
            eframe::egui::vec2(LIBRARY_IMPORT_FAB_EDGE, LIBRARY_IMPORT_FAB_EDGE)
        );
        assert_eq!(rect.right_bottom(), bounds.right_bottom());
        assert_eq!(library_import_icon(), egui_phosphor::regular::PLUS);
    }

    #[test]
    fn cloud_cache_icons_distinguish_remote_and_downloaded_raws() {
        assert_eq!(cloud_cache_icon(false), egui_phosphor::regular::CLOUD);
        assert_eq!(cloud_cache_icon(true), egui_phosphor::regular::DOWNLOAD);
    }

    #[test]
    fn cloud_sync_badges_distinguish_queue_failure_and_conflict() {
        let (queued_icon, queued_color, _) =
            cloud_sync_badge(crate::cloud::CloudSyncState::Queued, true);
        assert_eq!(queued_icon, egui_phosphor::regular::ARROW_CLOCKWISE);
        assert_eq!(queued_color, Color32::from_rgb(245, 190, 55));

        let (failed_icon, failed_color, _) =
            cloud_sync_badge(crate::cloud::CloudSyncState::Failed, true);
        assert_eq!(failed_icon, egui_phosphor::regular::X);
        assert_eq!(failed_color, Color32::from_rgb(240, 78, 78));

        let (conflict_icon, conflict_color, _) =
            cloud_sync_badge(crate::cloud::CloudSyncState::Conflict, true);
        assert_eq!(conflict_icon, egui_phosphor::regular::INTERSECT);
        assert_eq!(conflict_color, Color32::from_rgb(240, 78, 78));
    }

    #[test]
    fn cloud_preview_provenance_uses_matching_in_thumbnail_badges() {
        use crate::cloud::CloudThumbnailKind::{Edited, Legacy, Placeholder, Raw};

        assert_eq!(cloud_preview_label(Edited), None);
        assert_eq!(cloud_preview_label(Raw), Some("UNEDITED PREVIEW"));
        assert_eq!(cloud_preview_label(Legacy), None);
        assert_eq!(
            cloud_preview_icon(Legacy),
            Some(egui_phosphor::regular::CLOCK_COUNTER_CLOCKWISE)
        );
        assert_eq!(cloud_preview_icon(Edited), None);
        assert_eq!(cloud_preview_label(Placeholder), Some("PREVIEW RENDERING"));
        assert!(cloud_preview_notice(Legacy).unwrap().contains("Legacy"));
    }

    #[test]
    fn justified_rows_rebalance_to_avoid_a_sparse_last_row() {
        let aspects = vec![1.5; 13];
        let rows = balanced_justified_row_ranges(&aspects, 1000.0, 140.0, 6.0);
        let row_sizes = rows
            .iter()
            .map(|(start, end)| end - start)
            .collect::<Vec<_>>();

        assert_eq!(row_sizes.iter().sum::<usize>(), aspects.len());
        assert!(row_sizes.len() >= 2);
        assert!(row_sizes.iter().all(|count| *count >= 4));
        assert!(row_sizes.iter().all(|count| *count <= 5));
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn sparse_galleries_never_grow_above_the_responsive_target() {
        for available_width in [320.0, 1024.0, 3440.0] {
            for target_height in [120.0, 140.0, 270.0] {
                for item_count in 1..=3 {
                    let entries = (0..item_count)
                        .map(|index| {
                            new_library_entry(LibraryFileInfo {
                                source: LibrarySource::File(PathBuf::from(format!(
                                    "sparse-{available_width}-{target_height}-{index}.dng"
                                ))),
                                display_path: format!("sparse-{index}.dng"),
                                name: format!("sparse-{index}.dng"),
                                bytes: 1,
                                dimensions_hint: Some([3, 2]),
                                cloud_downloaded: false,
                                cloud_sync_state: crate::cloud::CloudSyncState::Synced,
                                modified: None,
                            })
                        })
                        .collect::<Vec<_>>();

                    let (placements, _) =
                        justified_thumbnail_layout(&entries, available_width, target_height, 6.0);

                    assert_eq!(placements.len(), item_count);
                    assert!(placements
                        .iter()
                        .all(|rect| rect.height() <= target_height + 0.01));
                    assert!(placements
                        .iter()
                        .all(|rect| rect.width() <= target_height * 1.5 + 0.01));
                    assert!(placements
                        .iter()
                        .all(|rect| rect.right() <= available_width + 0.01));
                }
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn decoded_preview_does_not_change_reserved_gallery_geometry() {
        let info = LibraryFileInfo {
            source: LibrarySource::File(PathBuf::from("stable-layout.dng")),
            display_path: "stable-layout.dng".to_owned(),
            name: "stable-layout.dng".to_owned(),
            bytes: 1,
            dimensions_hint: Some([6000, 4000]),
            cloud_downloaded: false,
            cloud_sync_state: crate::cloud::CloudSyncState::Synced,
            modified: None,
        };
        let mut entry = new_library_entry(info);
        let (before, before_height) =
            justified_thumbnail_layout(std::slice::from_ref(&entry), 900.0, 140.0, 6.0);

        // Embedded previews can have a slightly different crop/aspect. Loading
        // those pixels must not invalidate the geometry already reserved from
        // the RAW header.
        entry.thumbnail_size = Some([1600, 1200]);
        let (after, after_height) =
            justified_thumbnail_layout(std::slice::from_ref(&entry), 900.0, 140.0, 6.0);

        assert_eq!(before, after);
        assert_eq!(before_height, after_height);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn opening_a_library_folder_records_it_before_async_scanning() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "auraw-library-open-folder-{}-{nonce}",
            std::process::id()
        ));
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);

        // The path deliberately does not exist: the asynchronous scanner may
        // fail later, but the user's chosen location must be visible immediately.
        library.open_folder(root.clone(), &context);

        assert_eq!(library.folder(), Some(root.as_path()));
        assert_eq!(library.root_folder(), Some(root.as_path()));
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_subfolder_navigation_keeps_the_chosen_root() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "auraw-library-navigation-{}-{nonce}",
            std::process::id()
        ));
        let nested = root.join("year").join("shoot");
        let outside = root.with_extension("outside");

        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);
        library.open_folder(root.clone(), &context);
        library.select_folder(nested.clone(), &context);

        assert_eq!(library.root_folder(), Some(root.as_path()));
        assert_eq!(library.folder(), Some(nested.as_path()));

        library.select_folder(outside.clone(), &context);
        assert_eq!(library.root_folder(), Some(root.as_path()));
        assert_eq!(library.folder(), Some(nested.as_path()));

        library.open_folder(root.clone(), &context);
        assert_eq!(library.root_folder(), Some(root.as_path()));
        assert_eq!(library.folder(), Some(root.as_path()));

        library.view = LibraryView::Cloud;
        assert!(library.select_folder(root.clone(), &context));
        assert_eq!(library.view, LibraryView::Local);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn restoring_a_library_reopens_and_reveals_its_selected_subfolder() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "auraw-library-restore-{}-{nonce}",
            std::process::id()
        ));
        let parent = root.join("year");
        let selected = parent.join("shoot");
        fs::create_dir_all(&selected).unwrap();
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);

        library.restore_folder(root.clone(), Some(selected.clone()), &context);

        assert_eq!(library.root_folder(), Some(root.as_path()));
        assert_eq!(library.folder(), Some(selected.as_path()));
        assert!(library.expanded_folders.contains(&root));
        assert!(library.expanded_folders.contains(&parent));
        assert!(library.expanded_folders.contains(&selected));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restoring_library_navigation_reopens_the_saved_cloud_folder_and_tab() {
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);
        library.cloud_config = crate::cloud::CloudConfig {
            enabled: true,
            server_url: "http://127.0.0.1:1".to_owned(),
            access_token: String::new(),
        };
        let folder_id = "a".repeat(64);

        library.restore_navigation(LibraryView::Cloud, folder_id.clone(), &context);

        assert_eq!(library.view(), LibraryView::Cloud);
        assert_eq!(library.cloud_folder_id(), folder_id);
    }

    #[test]
    fn restoring_invalid_cloud_navigation_falls_back_safely() {
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);

        library.restore_navigation(LibraryView::Cloud, "../outside".to_owned(), &context);

        assert_eq!(library.view(), LibraryView::Local);
        assert_eq!(
            library.cloud_folder_id(),
            crate::cloud::CLOUD_ROOT_FOLDER_ID
        );
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_folder_tree_contains_nested_directories_and_ignores_symlinks() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("auraw-library-tree-{}-{nonce}", std::process::id()));
        fs::create_dir_all(root.join("Zulu").join("Nested")).unwrap();
        fs::create_dir_all(root.join("alpha")).unwrap();
        fs::write(root.join("not-a-folder.dng"), b"raw").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, root.join("Zulu").join("cycle")).unwrap();

        let tree = scan_folder_tree(&root, || false).expect("folder tree");
        let child_names = tree
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(child_names, ["alpha", "Zulu"]);
        assert_eq!(tree.children[1].children[0].name, "Nested");
        #[cfg(unix)]
        assert_eq!(tree.children[1].children.len(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn evicted_thumbnail_restores_from_resident_pixels_without_reloading() {
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);
        let info = LibraryFileInfo {
            source: LibrarySource::File(PathBuf::from("resident-restore.dng")),
            display_path: "resident-restore.dng".to_owned(),
            name: "resident-restore.dng".to_owned(),
            bytes: 1,
            dimensions_hint: Some([6000, 4000]),
            cloud_downloaded: false,
            cloud_sync_state: crate::cloud::CloudSyncState::Synced,
            modified: None,
        };
        let mut entry = new_library_entry(info);
        entry.thumbnail_size = Some([512, 341]);
        entry.resident_thumbnail = Some(RawThumbnail {
            width: 384,
            height: 256,
            rgba: vec![127; 384 * 256 * 4],
        });
        library.entries.push(entry);

        library.touch_and_request_thumbnail(0, &context);

        assert!(library.entries[0].texture.is_some());
        assert!(library.entries[0].texture_is_resident);
        assert!(!library.entries[0].thumbnail_queued);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn develop_loading_thumbnail_uses_resident_pixels_without_queuing_decode() {
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);
        let path = PathBuf::from("develop-loading-resident.dng");
        let info = LibraryFileInfo {
            source: LibrarySource::File(path.clone()),
            display_path: "develop-loading-resident.dng".to_owned(),
            name: "develop-loading-resident.dng".to_owned(),
            bytes: 1,
            dimensions_hint: Some([6000, 4000]),
            cloud_downloaded: false,
            cloud_sync_state: crate::cloud::CloudSyncState::Synced,
            modified: None,
        };
        let mut entry = new_library_entry(info);
        entry.thumbnail_size = Some([512, 341]);
        entry.resident_thumbnail = Some(RawThumbnail {
            width: 384,
            height: 256,
            rgba: vec![127; 384 * 256 * 4],
        });
        library.entries.push(entry);
        library.rebuild_entry_indices();

        let (_, size) = library
            .desktop_loading_thumbnail_for_path(&path, &context)
            .expect("resident thumbnail");

        assert_eq!(size, [512, 341]);
        assert!(library.entries[0].texture_is_resident);
        assert!(!library.entries[0].thumbnail_queued);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn cloud_folder_entries_remain_available_to_the_develop_filmstrip() {
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);
        let folder_id = "a".repeat(64);
        let asset = crate::cloud::CloudAsset {
            id: "b".repeat(64),
            name: "folder-photo.NEF".to_owned(),
            bytes: 42,
            modified_seconds: 1,
            width: 6000,
            height: 4000,
            raw_etag: "c".repeat(64),
            sidecar_etag: Some("d".repeat(64)),
            thumbnail_etag: "e".repeat(64),
            thumbnail_kind: crate::cloud::CloudThumbnailKind::Edited,
            folder_id,
        };
        library.entries.push(new_library_entry(LibraryFileInfo {
            source: LibrarySource::Cloud(asset.clone()),
            display_path: "AuRaw Cloud/folder-photo.NEF".to_owned(),
            name: asset.name.clone(),
            bytes: asset.bytes,
            dimensions_hint: Some([asset.width, asset.height]),
            cloud_downloaded: false,
            cloud_sync_state: crate::cloud::CloudSyncState::Synced,
            modified: None,
        }));

        assert_eq!(library.filmstrip_len(), 1);
        let item = library.filmstrip_item(0).expect("cloud filmstrip item");
        assert!(matches!(
            item.source,
            super::DesktopFilmstripSource::Cloud(ref filmstrip_asset)
                if filmstrip_asset.id == asset.id
        ));
        assert!(item.path.is_none());
        assert_eq!(item.identity, format!("cloud:{}", asset.id));
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn resetting_adjustments_allows_an_unedited_thumbnail_to_replace_the_developed_one() {
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);
        let path = PathBuf::from("reset-preview.dng");
        let info = LibraryFileInfo {
            source: LibrarySource::File(path.clone()),
            display_path: "reset-preview.dng".to_owned(),
            name: "reset-preview.dng".to_owned(),
            bytes: 1,
            dimensions_hint: Some([6000, 4000]),
            cloud_downloaded: false,
            cloud_sync_state: crate::cloud::CloudSyncState::Synced,
            modified: None,
        };
        let mut entry = new_library_entry(info);
        entry.texture = Some(context.load_texture(
            "developed-before-reset",
            eframe::egui::ColorImage::from_rgba_unmultiplied([1, 1], &[1, 2, 3, 255]),
            eframe::egui::TextureOptions::LINEAR,
        ));
        entry.resident_thumbnail = Some(RawThumbnail {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        });
        entry.thumbnail_size = Some([1, 1]);
        entry.thumbnail_queued = true;
        entry.developed_thumbnail = true;
        library.entries.push(entry);
        library.rebuild_entry_indices();

        library.invalidate_adjustment_thumbnail_for_path(&path);

        let entry = &library.entries[0];
        assert!(entry.texture.is_none());
        assert!(entry.resident_thumbnail.is_none());
        assert!(entry.thumbnail_size.is_none());
        assert!(!entry.thumbnail_queued);
        assert!(!entry.developed_thumbnail);
        assert_eq!(entry.layout_size, Some([6000, 4000]));
    }

    #[test]
    fn resident_thumbnail_is_bounded_and_keeps_aspect_ratio() {
        let thumbnail = RawThumbnail {
            width: 768,
            height: 512,
            rgba: vec![255; 768 * 512 * 4],
        };
        let resident = make_resident_thumbnail(&thumbnail);
        assert_eq!([resident.width, resident.height], [384, 256]);
        assert_eq!(resident.rgba.len(), 384 * 256 * 4);
    }

    #[test]
    fn develop_pause_preserves_a_received_non_priority_thumbnail_request() {
        let generation = 1;
        let cancellation = Arc::new(AtomicU64::new(generation));
        let decoding_paused = Arc::new(AtomicBool::new(true));
        let (event_sender, event_receiver) = mpsc::sync_channel(2);
        // A rendezvous channel makes send return only after the worker has
        // received the request, which proves it is retained during the pause.
        let (request_sender, request_receiver) = mpsc::sync_channel(0);
        let (decode_started_sender, decode_started_receiver) = mpsc::sync_channel(1);
        let worker_pause = Arc::clone(&decoding_paused);
        let worker = std::thread::spawn(move || {
            run_thumbnail_workers(
                ThumbnailWorker {
                    files: Vec::new(),
                    warning_count: 0,
                    truncated: false,
                    generation,
                    cancellation,
                    decoding_paused: worker_pause,
                    decode_gate: Arc::new(RwLock::new(())),
                    event_sender,
                    request_receiver,
                    repaint: eframe::egui::Context::default(),
                },
                1,
                Arc::new(move |_| {
                    decode_started_sender.send(()).unwrap();
                    Ok(loaded_library_thumbnail(
                        RawThumbnail {
                            width: 1,
                            height: 1,
                            rgba: vec![0, 0, 0, 255],
                        },
                        false,
                    ))
                }),
            );
        });

        match event_receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(ScanEvent::Catalog {
                generation: event_generation,
                ..
            }) => assert_eq!(event_generation, generation),
            _ => panic!("thumbnail worker did not publish its catalog"),
        }
        let source = LibrarySource::File(PathBuf::from("paused.dng"));
        request_sender
            .send(ThumbnailRequest {
                generation,
                source: source.clone(),
                display_priority: false,
            })
            .unwrap();
        assert!(matches!(
            decode_started_receiver.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        decoding_paused.store(false, Ordering::Release);
        decode_started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("queued thumbnail should continue after resume");
        match event_receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(ScanEvent::Thumbnail {
                generation: event_generation,
                source: event_source,
                display_priority,
                result: Ok(loaded),
            }) => {
                assert_eq!(event_generation, generation);
                assert_eq!(event_source, source);
                assert!(!display_priority);
                assert_eq!((loaded.thumbnail.width, loaded.thumbnail.height), (1, 1));
            }
            _ => panic!("thumbnail worker did not preserve the paused request"),
        }
        drop(request_sender);
        worker.join().unwrap();
    }

    #[test]
    fn develop_pause_allows_display_priority_thumbnail_request() {
        let generation = 2;
        let cancellation = Arc::new(AtomicU64::new(generation));
        let decoding_paused = Arc::new(AtomicBool::new(true));
        let (event_sender, event_receiver) = mpsc::sync_channel(2);
        let (request_sender, request_receiver) = mpsc::sync_channel(0);
        let (decode_started_sender, decode_started_receiver) = mpsc::sync_channel(1);
        let worker_pause = Arc::clone(&decoding_paused);
        let worker = std::thread::spawn(move || {
            run_thumbnail_workers(
                ThumbnailWorker {
                    files: Vec::new(),
                    warning_count: 0,
                    truncated: false,
                    generation,
                    cancellation,
                    decoding_paused: worker_pause,
                    decode_gate: Arc::new(RwLock::new(())),
                    event_sender,
                    request_receiver,
                    repaint: eframe::egui::Context::default(),
                },
                1,
                Arc::new(move |_| {
                    decode_started_sender.send(()).unwrap();
                    Ok(loaded_library_thumbnail(
                        RawThumbnail {
                            width: 1,
                            height: 1,
                            rgba: vec![0, 0, 0, 255],
                        },
                        false,
                    ))
                }),
            );
        });

        match event_receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(ScanEvent::Catalog {
                generation: event_generation,
                ..
            }) => assert_eq!(event_generation, generation),
            _ => panic!("thumbnail worker did not publish its catalog"),
        }
        let source = LibrarySource::File(PathBuf::from("filmstrip.dng"));
        request_sender
            .send(ThumbnailRequest {
                generation,
                source: source.clone(),
                display_priority: true,
            })
            .unwrap();
        decode_started_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("display-priority thumbnail should run while Develop is paused");
        match event_receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(ScanEvent::Thumbnail {
                generation: event_generation,
                source: event_source,
                display_priority,
                result: Ok(loaded),
            }) => {
                assert_eq!(event_generation, generation);
                assert_eq!(event_source, source);
                assert!(display_priority);
                assert_eq!((loaded.thumbnail.width, loaded.thumbnail.height), (1, 1));
            }
            _ => panic!("thumbnail worker did not service the display-priority request"),
        }
        drop(request_sender);
        worker.join().unwrap();
    }

    #[test]
    fn thumbnail_workers_process_the_entire_catalog_without_view_requests() {
        let generation = 7;
        let files = ["one.dng", "two.dng", "three.dng"]
            .into_iter()
            .map(|name| LibraryFileInfo {
                source: LibrarySource::File(PathBuf::from(name)),
                display_path: name.to_owned(),
                name: name.to_owned(),
                bytes: 1,
                dimensions_hint: Some([3, 2]),
                cloud_downloaded: false,
                cloud_sync_state: crate::cloud::CloudSyncState::Synced,
                modified: None,
            })
            .collect::<Vec<_>>();
        let expected = files
            .iter()
            .map(|file| file.source.clone())
            .collect::<HashSet<_>>();
        let (event_sender, event_receiver) = mpsc::sync_channel(8);
        let (request_sender, request_receiver) = mpsc::sync_channel(1);
        drop(request_sender);
        let worker = std::thread::spawn(move || {
            run_thumbnail_workers(
                ThumbnailWorker {
                    files,
                    warning_count: 0,
                    truncated: false,
                    generation,
                    cancellation: Arc::new(AtomicU64::new(generation)),
                    decoding_paused: Arc::new(AtomicBool::new(false)),
                    decode_gate: Arc::new(RwLock::new(())),
                    event_sender,
                    request_receiver,
                    repaint: eframe::egui::Context::default(),
                },
                2,
                Arc::new(|_| {
                    Ok(loaded_library_thumbnail(
                        RawThumbnail {
                            width: 1,
                            height: 1,
                            rgba: vec![0, 0, 0, 255],
                        },
                        false,
                    ))
                }),
            );
        });

        assert!(matches!(
            event_receiver.recv_timeout(Duration::from_secs(2)),
            Ok(ScanEvent::Catalog { generation: 7, .. })
        ));
        let mut loaded = HashSet::new();
        for _ in 0..expected.len() {
            match event_receiver.recv_timeout(Duration::from_secs(2)) {
                Ok(ScanEvent::Thumbnail {
                    generation: 7,
                    source,
                    display_priority,
                    result: Ok(_),
                }) => {
                    assert!(!display_priority);
                    loaded.insert(source);
                }
                _ => panic!("thumbnail worker did not process the complete catalog"),
            }
        }
        assert_eq!(loaded, expected);
        worker.join().unwrap();
    }

    #[test]
    fn library_exposes_its_shared_decode_gate_and_resumes_in_library() {
        let context = eframe::egui::Context::default();
        let mut library = LibraryState::new(&context);
        let first = library.decode_gate();
        let second = library.decode_gate();
        assert!(Arc::ptr_eq(&first, &second));

        library.prepare_for_develop();
        assert!(library.decoding_paused.load(Ordering::Acquire));
        library.resume_thumbnail_decoding();
        assert!(!library.decoding_paused.load(Ordering::Acquire));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_remain_distinct_library_keys() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::path::PathBuf;

        let first = PathBuf::from(OsString::from_vec(vec![b'r', b'a', b'w', 0x80]));
        let second = PathBuf::from(OsString::from_vec(vec![b'r', b'a', b'w', 0x81]));
        assert_eq!(first.display().to_string(), second.display().to_string());

        let sources = HashSet::from([LibrarySource::File(first), LibrarySource::File(second)]);
        assert_eq!(sources.len(), 2);
    }

    #[test]
    fn duplicate_raw_copies_the_matching_sidecar_and_uses_unique_names() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-duplicate-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let raw = root.join("photo.CR3");
        fs::write(&raw, b"raw-bytes").unwrap();
        fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"sidecar-bytes").unwrap();
        install_test_developed_thumbnail(&raw);

        let first = duplicate_raw_and_sidecar(&raw).unwrap();
        let second = duplicate_raw_and_sidecar(&raw).unwrap();
        assert_eq!(first.file_name().unwrap(), "photo copy.CR3");
        assert_eq!(second.file_name().unwrap(), "photo copy 2.CR3");
        assert_eq!(fs::read(&first).unwrap(), b"raw-bytes");
        assert_eq!(
            fs::read(crate::sidecar::sidecar_path_for_raw(&first)).unwrap(),
            b"sidecar-bytes"
        );
        assert_test_developed_thumbnail(&first);
        assert_test_developed_thumbnail(&second);

        let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&raw);
        let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&first);
        let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&second);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_image_clipboard_copies_and_moves_raws_with_sidecars() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-image-clipboard-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        let raw = source.join("photo.CR3");
        fs::write(&raw, b"raw-bytes").unwrap();
        fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"sidecar-bytes").unwrap();
        install_test_developed_thumbnail(&raw);

        let copy = run_image_paste(
            &crate::cloud::CloudConfig::default(),
            None,
            false,
            ImageClipboard {
                mode: ImageClipboardMode::Copy,
                content: ImageClipboardContent::Local(vec![raw.clone()]),
            },
            ImagePasteDestination::LocalFolder(destination.clone()),
        );
        assert!(copy.result.is_ok());
        assert!(!copy.clear_clipboard);
        let copied = destination.join("photo.CR3");
        assert_eq!(fs::read(&copied).unwrap(), b"raw-bytes");
        assert_eq!(
            fs::read(crate::sidecar::sidecar_path_for_raw(&copied)).unwrap(),
            b"sidecar-bytes"
        );
        assert_test_developed_thumbnail(&copied);
        assert!(raw.is_file());

        let cut = run_image_paste(
            &crate::cloud::CloudConfig::default(),
            None,
            false,
            ImageClipboard {
                mode: ImageClipboardMode::Cut,
                content: ImageClipboardContent::Local(vec![raw.clone()]),
            },
            ImagePasteDestination::LocalFolder(destination.clone()),
        );
        assert!(cut.result.is_ok());
        assert!(cut.clear_clipboard);
        assert!(!raw.exists());
        assert!(!crate::sidecar::sidecar_path_for_raw(&raw).exists());
        let moved = destination.join("photo (1).CR3");
        assert_eq!(fs::read(&moved).unwrap(), b"raw-bytes");
        assert_eq!(
            fs::read(crate::sidecar::sidecar_path_for_raw(&moved)).unwrap(),
            b"sidecar-bytes"
        );
        assert_test_developed_thumbnail(&moved);

        let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&copied);
        let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&moved);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_local_cut_keeps_only_unmoved_raws_on_the_clipboard() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-partial-cut-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        let moved = source.join("moved.CR3");
        let missing = source.join("missing.CR3");
        fs::write(&moved, b"raw").unwrap();

        let completion = run_image_paste(
            &crate::cloud::CloudConfig::default(),
            None,
            false,
            ImageClipboard {
                mode: ImageClipboardMode::Cut,
                content: ImageClipboardContent::Local(vec![moved.clone(), missing.clone()]),
            },
            ImagePasteDestination::LocalFolder(destination.clone()),
        );

        assert!(completion.result.is_err());
        assert!(!completion.clear_clipboard);
        assert!(!moved.exists());
        assert_eq!(fs::read(destination.join("moved.CR3")).unwrap(), b"raw");
        let remaining = completion.remaining_clipboard.unwrap();
        assert_eq!(remaining.count(), 1);
        match remaining.content {
            ImageClipboardContent::Local(paths) => assert_eq!(paths, vec![missing]),
            ImageClipboardContent::Cloud(_) => panic!("expected a local clipboard"),
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_raw_rename_keeps_the_matching_sidecar() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-rename-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let raw = root.join("before.NEF");
        fs::write(&raw, b"raw").unwrap();
        fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"sidecar").unwrap();
        install_test_developed_thumbnail(&raw);

        let renamed = rename_raw_bundle(&raw, "after.NEF").unwrap();
        assert_eq!(renamed, root.join("after.NEF"));
        assert!(!raw.exists());
        assert!(!crate::sidecar::sidecar_path_for_raw(&raw).exists());
        assert_eq!(fs::read(&renamed).unwrap(), b"raw");
        assert_eq!(
            fs::read(crate::sidecar::sidecar_path_for_raw(&renamed)).unwrap(),
            b"sidecar"
        );
        assert_test_developed_thumbnail(&renamed);
        assert!(crate::sidecar::load_developed_thumbnail_cache(&raw, 512)
            .unwrap()
            .is_none());

        let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&renamed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_clipboard_uploads_local_raw_and_sidecar_to_cloud() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-local-cloud-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let raw = root.join("upload.DNG");
        let raw_bytes = b"clipboard-raw";
        let sidecar_bytes = b"clipboard-sidecar";
        fs::write(&raw, raw_bytes).unwrap();
        fs::write(crate::sidecar::sidecar_path_for_raw(&raw), sidecar_bytes).unwrap();
        install_test_developed_thumbnail(&raw);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let raw_etag = sha256_hex(raw_bytes);
        let sidecar_etag = sha256_hex(sidecar_bytes);
        let response = serde_json::to_vec(&serde_json::json!({
            "id": raw_etag,
            "name": "upload.DNG",
            "bytes": raw_bytes.len(),
            "modified_seconds": 1,
            "width": 32,
            "height": 24,
            "raw_etag": raw_etag,
            "sidecar_etag": sidecar_etag,
            "thumbnail_etag": "d".repeat(64),
            "folder_id": crate::cloud::CLOUD_ROOT_FOLDER_ID,
        }))
        .unwrap();
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            let request_text = String::from_utf8_lossy(&request);
            assert!(request_text.starts_with("POST /api/v1/assets HTTP/1.1\r\n"));
            assert!(request_text.contains("name=\"raw\""));
            assert!(request_text.contains("clipboard-raw"));
            assert!(request_text.contains("name=\"sidecar\""));
            assert!(request_text.contains("clipboard-sidecar"));
            assert!(request_text.contains("name=\"thumbnail\""));
            write_http_response(&mut stream, "application/json", &response);
        });

        let completion = run_image_paste(
            &crate::cloud::CloudConfig {
                enabled: true,
                server_url: format!("http://{address}"),
                access_token: String::new(),
            },
            None,
            true,
            ImageClipboard {
                mode: ImageClipboardMode::Copy,
                content: ImageClipboardContent::Local(vec![raw.clone()]),
            },
            ImagePasteDestination::CloudFolder(crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned()),
        );
        responder.join().unwrap();
        assert!(completion.result.is_ok());
        assert!(raw.is_file());
        assert!(crate::sidecar::sidecar_path_for_raw(&raw).is_file());
        let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&raw);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn image_clipboard_downloads_cloud_raw_and_sidecar_to_local_folder() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-cloud-local-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cache = root.join("cache");
        let destination = root.join("destination");
        fs::create_dir_all(&destination).unwrap();
        let raw = b"downloaded-cloud-raw";
        let sidecar = b"downloaded-cloud-sidecar";
        let thumbnail = test_developed_thumbnail_jpeg();
        let asset = crate::cloud::CloudAsset {
            id: sha256_hex(raw),
            name: "download.NEF".to_owned(),
            bytes: raw.len() as u64,
            modified_seconds: 1,
            width: 40,
            height: 30,
            raw_etag: sha256_hex(raw),
            sidecar_etag: Some(sha256_hex(sidecar)),
            thumbnail_etag: sha256_hex(&thumbnail),
            thumbnail_kind: crate::cloud::CloudThumbnailKind::Edited,
            folder_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
        };
        let catalog = serde_json::to_vec(&serde_json::json!({
            "items": [asset.clone()],
            "folders": [],
        }))
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected_asset_id = asset.id.clone();
        let responder = std::thread::spawn(move || {
            for (path, content_type, body) in [
                ("/api/v1/assets".to_owned(), "application/json", catalog),
                (
                    format!("/api/v1/assets/{expected_asset_id}/raw"),
                    "application/octet-stream",
                    raw.to_vec(),
                ),
                (
                    format!("/api/v1/assets/{expected_asset_id}/sidecar"),
                    "application/vnd.auraw.sidecar",
                    sidecar.to_vec(),
                ),
                (
                    format!(
                        "/api/v1/assets/{expected_asset_id}/thumbnail?v={}",
                        sha256_hex(&thumbnail)
                    ),
                    "image/jpeg",
                    thumbnail,
                ),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                assert!(String::from_utf8_lossy(&request)
                    .starts_with(&format!("GET {path} HTTP/1.1\r\n")));
                write_http_response(&mut stream, content_type, &body);
            }
        });

        let completion = run_image_paste(
            &crate::cloud::CloudConfig {
                enabled: true,
                server_url: format!("http://{address}"),
                access_token: String::new(),
            },
            Some(&cache),
            true,
            ImageClipboard {
                mode: ImageClipboardMode::Copy,
                content: ImageClipboardContent::Cloud(vec![asset]),
            },
            ImagePasteDestination::LocalFolder(destination.clone()),
        );
        responder.join().unwrap();
        assert!(completion.result.is_ok(), "{:?}", completion.result);
        let copied = destination.join("download.NEF");
        assert_eq!(fs::read(&copied).unwrap(), raw);
        assert_eq!(
            fs::read(crate::sidecar::sidecar_path_for_raw(&copied)).unwrap(),
            sidecar
        );
        assert_test_developed_thumbnail(&copied);
        let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&copied);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_names_are_single_safe_path_components() {
        assert!(validate_folder_name("Photos 2026").is_ok());
        for invalid in ["", " ", ".", "..", "../outside", "nested/folder", "/tmp"] {
            assert!(
                validate_folder_name(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
        #[cfg(windows)]
        assert!(validate_folder_name(r"nested\folder").is_err());
    }

    #[test]
    fn recursive_folder_copy_never_overwrites_and_rejects_symlinks() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-folder-copy-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let destination = root.join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("nested").join("photo.dng"), b"raw").unwrap();

        copy_directory_create_new(&source, &destination).unwrap();
        assert_eq!(
            fs::read(destination.join("nested").join("photo.dng")).unwrap(),
            b"raw"
        );
        assert!(copy_directory_create_new(&source, &destination).is_err());

        #[cfg(unix)]
        {
            let linked_source = root.join("linked-source");
            let linked_destination = root.join("linked-destination");
            fs::create_dir(&linked_source).unwrap();
            std::os::unix::fs::symlink(&source, linked_source.join("link")).unwrap();
            assert!(copy_directory_create_new(&linked_source, &linked_destination).is_err());
            assert!(!linked_destination.exists());
        }

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_operations_stay_inside_the_library_and_protect_the_root() {
        let base = std::env::temp_dir().join(format!(
            "auraw-library-folder-boundary-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("library");
        let outside = base.join("outside");
        let source = root.join("source");
        let child = source.join("child");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir(&outside).unwrap();

        assert!(run_folder_operation(LibraryFolderOperation::Create {
            root: root.clone(),
            parent: outside,
            name: "escape".to_owned(),
        })
        .is_err());
        assert!(run_folder_operation(LibraryFolderOperation::Delete {
            root: root.clone(),
            target: root.clone(),
        })
        .is_err());
        assert!(run_folder_operation(LibraryFolderOperation::Move {
            root: root.clone(),
            source,
            destination_parent: child,
            new_name: None,
        })
        .is_err());
        assert!(root.is_dir());

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn dropped_folders_are_copied_recursively_with_unique_names() {
        let base = std::env::temp_dir().join(format!(
            "auraw-library-folder-import-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = base.join("source").join("shoot");
        let library = base.join("library");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir(&library).unwrap();
        fs::write(source.join("photo.CR3"), b"raw").unwrap();

        let first = import_folder_into_library(&source, &library).unwrap();
        let second = import_folder_into_library(&source, &library).unwrap();
        assert_eq!(first.file_name().unwrap(), "shoot");
        assert_eq!(second.file_name().unwrap(), "shoot copy");
        assert_eq!(fs::read(first.join("photo.CR3")).unwrap(), b"raw");
        assert_eq!(fs::read(second.join("photo.CR3")).unwrap(), b"raw");

        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn dropped_raw_import_preserves_the_name_and_never_overwrites() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-import-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source_folder = root.join("source");
        let library_folder = root.join("library");
        fs::create_dir_all(&source_folder).unwrap();
        fs::create_dir_all(&library_folder).unwrap();
        let source = source_folder.join("photo.CR3");
        fs::write(&source, b"new-raw").unwrap();

        let first = import_raw_into_folder(&source, &library_folder).unwrap();
        let first_path = match first {
            RawImportOutcome::Imported(path) => path,
            RawImportOutcome::AlreadyPresent => panic!("external source was not imported"),
        };
        assert_eq!(first_path.file_name().unwrap(), "photo.CR3");
        assert_eq!(fs::read(&first_path).unwrap(), b"new-raw");

        fs::write(&source, b"newer-raw").unwrap();
        let second = import_raw_into_folder(&source, &library_folder).unwrap();
        let second_path = match second {
            RawImportOutcome::Imported(path) => path,
            RawImportOutcome::AlreadyPresent => panic!("changed external source was not imported"),
        };
        assert_eq!(second_path.file_name().unwrap(), "photo (1).CR3");
        assert_eq!(fs::read(&first_path).unwrap(), b"new-raw");
        assert_eq!(fs::read(&second_path).unwrap(), b"newer-raw");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dropping_a_raw_already_in_the_library_is_a_noop() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-import-noop-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let raw = root.join("photo.DNG");
        fs::write(&raw, b"raw").unwrap();

        assert!(matches!(
            import_raw_into_folder(&raw, &root).unwrap(),
            RawImportOutcome::AlreadyPresent
        ));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_scan_only_includes_direct_raw_children() {
        let root = std::env::temp_dir().join(format!(
            "auraw-library-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("one.DNG"), b"raw").unwrap();
        fs::write(nested.join("two.nef"), b"raw").unwrap();
        fs::write(root.join("ignore.jpg"), b"jpeg").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&root, nested.join("cycle")).unwrap();

        let (files, warnings, truncated) = scan_folder(&root, || false).unwrap().unwrap();
        let names = files
            .iter()
            .map(|file| file.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(warnings, 0);
        assert!(!truncated);
        assert!(names.contains(&"one.DNG"));
        assert!(!names.contains(&"two.nef"));
        assert_eq!(files.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn folder_scan_retains_newest_files_after_reaching_limit() {
        use std::time::Duration;

        let root = std::env::temp_dir().join(format!(
            "auraw-library-limit-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (name, age) in [
            ("oldest.dng", 1),
            ("newest.dng", 5),
            ("middle.dng", 3),
            ("older.dng", 2),
            ("newer.dng", 4),
        ] {
            let path = root.join(name);
            fs::write(&path, b"raw").unwrap();
            let file = fs::OpenOptions::new().write(true).open(path).unwrap();
            file.set_times(fs::FileTimes::new().set_modified(epoch + Duration::from_secs(age)))
                .unwrap();
        }

        let (files, warnings, truncated) =
            scan_folder_with_limit(&root, 3, || false).unwrap().unwrap();
        let names = files
            .iter()
            .map(|file| file.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["newest.dng", "newer.dng", "middle.dng"]);
        assert_eq!(warnings, 0);
        assert!(truncated);
        fs::remove_dir_all(root).unwrap();
    }
}
