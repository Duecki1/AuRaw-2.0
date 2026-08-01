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
#[cfg(not(target_os = "android"))]
use std::path::Path;
#[cfg(not(target_os = "android"))]
use std::path::PathBuf;
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
#[cfg(not(target_os = "android"))]
const DEVELOPED_THUMBNAIL_PROXY_EDGE: u32 = 1024;
pub(crate) const MAX_DESKTOP_THUMBNAIL_WORKERS: usize = 8;
pub(crate) const MAX_ANDROID_THUMBNAIL_WORKERS: usize = 2;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum LibrarySortOrder {
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

#[cfg(any(target_os = "android", test))]
const ANDROID_LIBRARY_IMPORT_FAB_EDGE: f32 = 56.0;

#[cfg(any(target_os = "android", test))]
fn android_library_import_fab_rect(bounds: egui::Rect) -> egui::Rect {
    let size = egui::vec2(
        ANDROID_LIBRARY_IMPORT_FAB_EDGE,
        ANDROID_LIBRARY_IMPORT_FAB_EDGE,
    );
    egui::Rect::from_min_size(bounds.right_bottom() - size, size)
}

#[cfg(any(target_os = "android", test))]
fn android_library_import_icon() -> &'static str {
    egui_phosphor::regular::PLUS
}

#[derive(Clone, Debug)]
struct LibraryFileInfo {
    source: LibrarySource,
    display_path: String,
    name: String,
    bytes: u64,
    dimensions_hint: Option<[u32; 2]>,
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
    Catalog {
        generation: u64,
        files: Vec<LibraryFileInfo>,
        warning_count: usize,
        truncated: bool,
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
    targets: Vec<(String, String)>,
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
struct LibraryAdjustmentPasteDialog {
    targets: Vec<(String, String)>,
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
    already_present: usize,
    ignored: usize,
    failures: Vec<String>,
}

#[cfg(not(target_os = "android"))]
enum RawImportOutcome {
    Imported(PathBuf),
    AlreadyPresent,
}

#[cfg(target_os = "android")]
#[derive(Clone)]
struct LibraryAiMaskRefreshPrompt {
    targets: Vec<(String, String)>,
}

pub(crate) struct LibraryState {
    location: Option<String>,
    #[cfg(not(target_os = "android"))]
    folder: Option<PathBuf>,
    #[cfg(target_os = "android")]
    android_app: android_activity::AndroidApp,
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
    selected_sources: HashSet<LibrarySource>,
    selection_mode: bool,
    #[cfg(not(target_os = "android"))]
    file_action_receiver: Option<mpsc::Receiver<Result<Vec<PathBuf>, String>>>,
    #[cfg(not(target_os = "android"))]
    raw_import_receiver: Option<mpsc::Receiver<RawImportResult>>,
    export_dialog: Option<LibraryExportDialog>,
    adjustment_paste_dialog: Option<LibraryAdjustmentPasteDialog>,
    ai_mask_refresh_prompt: Option<LibraryAiMaskRefreshPrompt>,
}

impl LibraryState {
    #[cfg(all(not(target_os = "android"), test))]
    pub(crate) fn new(context: &egui::Context) -> Self {
        Self::new_with_workers(context, default_thumbnail_worker_count())
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn new_with_workers(context: &egui::Context, workers: usize) -> Self {
        let _ = context;
        let thumbnail_workers = workers.clamp(1, maximum_thumbnail_worker_count());
        crate::thumbnail_cache::set_rendered_thumbnail_worker_limit(thumbnail_workers);
        Self {
            location: None,
            folder: None,
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
            sort_order: LibrarySortOrder::default(),
            selected_sources: HashSet::new(),
            selection_mode: false,
            file_action_receiver: None,
            raw_import_receiver: None,
            export_dialog: None,
            adjustment_paste_dialog: None,
            ai_mask_refresh_prompt: None,
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn new_android_with_workers(
        android_app: android_activity::AndroidApp,
        context: &egui::Context,
        workers: usize,
    ) -> Self {
        let location = crate::android::library_location(&android_app).unwrap_or_else(|error| {
            log::warn!("{error}");
            "Android/media/de.duecki.auraw/.library".to_owned()
        });
        let thumbnail_workers = workers.clamp(1, maximum_thumbnail_worker_count());
        crate::thumbnail_cache::set_rendered_thumbnail_worker_limit(thumbnail_workers);
        let mut state = Self {
            location: Some(location),
            android_app,
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
            sort_order: LibrarySortOrder::default(),
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

    pub(crate) fn thumbnail_worker_count(&self) -> usize {
        self.thumbnail_workers
    }

    fn set_sort_order(&mut self, sort_order: LibrarySortOrder) {
        if self.sort_order == sort_order {
            return;
        }
        self.sort_order = sort_order;
        self.sort_entries();
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

    /// Stops the thumbnail worker from beginning another decode while a full
    /// RAW is opened. Catalog-wide background work and visible-card requests
    /// remain queued and continue when the Library tab is shown again.
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
        self.file_action_receiver.is_some() || self.raw_import_receiver.is_some()
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
                let mut already_present = 0usize;
                let mut ignored = 0usize;
                let mut failures = Vec::new();
                let mut seen_sources = HashSet::new();

                for source in source_paths {
                    let source_key = fs::canonicalize(&source).unwrap_or_else(|_| source.clone());
                    if !seen_sources.insert(source_key) {
                        ignored += 1;
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
        let folder_changed = self.folder.as_ref() != Some(&folder);
        self.location = Some(folder.display().to_string());
        self.folder = Some(folder);
        if folder_changed {
            self.entries.clear();
            self.entry_indices.clear();
            self.catalog_ready = false;
        }
        self.refresh(context);
    }

    pub(crate) fn refresh(&mut self, context: &egui::Context) {
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

        #[cfg(not(target_os = "android"))]
        let worker = {
            let Some(folder) = self.folder.clone() else {
                self.event_receiver = None;
                self.request_sender = None;
                self.scanning = false;
                self.status = "Open a folder to build your RAW library.".to_owned();
                return;
            };
            self.status = format!("Scanning {}…", folder.display());
            std::thread::Builder::new()
                .name("auraw-library".to_owned())
                .spawn(move || {
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
                if !result.imported.is_empty() {
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

    fn poll(&mut self, context: &egui::Context) {
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
                    self.status = error;
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.file_action_receiver = None;
                    self.status = "The library file operation stopped unexpectedly.".to_owned();
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
                    self.status = catalog_status(self.entries.len(), warning_count, truncated);
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
                    self.status = error;
                    break;
                }
                _ => {}
            }
        }
        let _ = context;
    }

    fn touch_and_request_thumbnail(&mut self, index: usize, context: &egui::Context) {
        let generation = self.generation.load(Ordering::Acquire);
        self.usage_clock = self.usage_clock.wrapping_add(1).max(1);
        let usage_clock = self.usage_clock;
        let request_sender = self.request_sender.clone();

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

    fn evict_old_textures(&mut self, protected_indices: &HashSet<usize>) {
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
    let LibrarySource::Android {
        modified_seconds, ..
    } = &info.source;
    *modified_seconds
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
        return Ok(Some(thumbnail));
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
    let mut preview_raw = if full_raw.width.max(full_raw.height) > DEVELOPED_THUMBNAIL_PROXY_EDGE {
        build_proxy(
            &full_raw,
            ProxySpec {
                max_edge: DEVELOPED_THUMBNAIL_PROXY_EDGE,
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
fn load_desktop_library_thumbnail(
    source: &LibrarySource,
) -> Result<LoadedLibraryThumbnail, String> {
    let LibrarySource::File(path) = source;
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
    app: &android_activity::AndroidApp,
    source: &LibrarySource,
) -> Result<LoadedLibraryThumbnail, String> {
    let LibrarySource::Android {
        uri,
        display_name,
        bytes,
        modified_seconds,
    } = source;
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
            // Keep the request while Develop owns the exclusive writer lock.
            while decoding_paused.load(Ordering::Acquire) {
                if cancellation.load(Ordering::Acquire) != generation {
                    return;
                }
                std::thread::sleep(THUMBNAIL_PAUSE_POLL_INTERVAL);
            }

            // Thumbnail workers share read access with one another. A full RAW
            // decode takes the writer lock, so it remains exclusive and gets a
            // clean memory budget even when many thumbnail workers are enabled.
            let decode_guard = decode_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cancellation.load(Ordering::Acquire) != generation {
                return;
            }
            if decoding_paused.load(Ordering::Acquire) {
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

fn catalog_status(count: usize, warning_count: usize, truncated: bool) -> String {
    let warnings = if warning_count == 0 {
        String::new()
    } else {
        format!(" · {warning_count} unreadable items")
    };
    let truncated = if truncated {
        format!(" · newest {MAX_LIBRARY_FILES} shown")
    } else {
        String::new()
    };
    format!(
        "{count} RAW {}{truncated}{warnings}",
        if count == 1 { "file" } else { "files" }
    )
}

#[cfg(not(target_os = "android"))]
enum LibraryCardAction {
    Export(Vec<PathBuf>),
    CopyAdjustments(PathBuf),
    PasteAdjustments(Vec<PathBuf>),
    Duplicate(Vec<PathBuf>),
    ResetAdjustments(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
}

#[cfg(target_os = "android")]
enum LibraryCardAction {
    Export(Vec<(String, String)>),
    CopyAdjustments((String, String)),
    PasteAdjustments(Vec<(String, String)>),
    Duplicate(Vec<(String, String)>),
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
            .map(|(source, _)| {
                let LibrarySource::Android {
                    uri, display_name, ..
                } = source;
                (uri.clone(), display_name.clone())
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
        if let Some((source, _)) = selected.first() {
            let LibrarySource::Android {
                uri, display_name, ..
            } = source;
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
        "No RAW files were imported.".to_owned()
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

pub struct Library;

impl Library {
    pub fn show(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        app.library.resume_thumbnail_decoding();
        app.library.poll(ui.ctx());

        let mut refresh = false;
        #[cfg(target_os = "android")]
        let mut import_raw = false;
        let mut open_source = None;
        let mut library_action = None;

        #[cfg(target_os = "android")]
        let selected_android_items = app
            .library
            .entries
            .iter()
            .filter(|entry| app.library.selected_sources.contains(&entry.info.source))
            .map(|entry| (entry.info.source.clone(), entry.info.name.clone()))
            .collect::<Vec<_>>();

        ui.horizontal(|ui| {
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
                        let action_enabled = app.library_batch_export_progress().is_none()
                            && app.library_ai_mask_refresh_status().is_none();
                        android_selection_menu(
                            ui,
                            &selected_android_items,
                            action_enabled,
                            action_enabled && app.has_copied_adjustments(),
                            &mut library_action,
                        );
                    });
                });
                return;
            }

            #[cfg(not(target_os = "android"))]
            let desktop_selection_mode = app.library.selection_mode();
            #[cfg(not(target_os = "android"))]
            let desktop_selection_count = app.library.selected_sources.len();

            #[cfg(not(target_os = "android"))]
            if desktop_selection_mode {
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
                let count = app.library.entries.len();
                ui.strong(format!(
                    "{count} RAW {}",
                    if count == 1 { "file" } else { "files" }
                ));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                #[cfg(target_os = "android")]
                if ui.button("Settings").clicked() {
                    app.activate_tab(AppTab::Settings);
                }

                #[cfg(not(target_os = "android"))]
                if ui
                    .button(desktop_selection_toggle_label(desktop_selection_mode))
                    .clicked()
                {
                    if desktop_selection_mode {
                        app.library.clear_selection();
                    } else {
                        app.library.begin_selection();
                    }
                }

                if crate::ui::icons::phosphor_icon_button_enabled(
                    ui,
                    app.library.location.is_some() && !app.library.scanning,
                    egui_phosphor::regular::ARROW_CLOCKWISE,
                    egui::vec2(32.0, 26.0),
                    "Refresh library",
                )
                .clicked()
                {
                    refresh = true;
                }

                let mut selected_sort = app.library.sort_order;
                egui::ComboBox::from_id_salt("library-sort-order")
                    .selected_text(selected_sort.label())
                    .width(128.0)
                    .show_ui(ui, |ui| {
                        for sort_order in LibrarySortOrder::ALL {
                            ui.selectable_value(&mut selected_sort, sort_order, sort_order.label());
                        }
                    });
                app.library.set_sort_order(selected_sort);
            });
        });

        #[cfg(not(target_os = "android"))]
        if let Some(location) = app.library.location() {
            ui.label(
                egui::RichText::new(location)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }
        ui.separator();

        if app.library.location.is_none() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("Choose a photo folder");
                    ui.label("AuRaw shows RAW files directly inside the selected folder, builds every preview in the background, and prioritizes visible rows.");
                    ui.add_space(8.0);
                    #[cfg(not(target_os = "android"))]
                    if ui.button("Open Folder…").clicked() {
                        app.open_library_folder_dialog();
                    }
                });
            });
        } else if app.library.catalog_ready && app.library.entries.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("No RAW files here yet");
                    #[cfg(not(target_os = "android"))]
                    ui.label("Choose another folder or add RAW files to this folder.");
                    #[cfg(target_os = "android")]
                    ui.label("Tap + to import one or more RAW files.");
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
            );
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
                            let LibrarySource::File(path) = &source;
                            let path = path.clone();
                            let context_source_path = path.clone();

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
                                    .map(|candidate| match &candidate.info.source {
                                        LibrarySource::File(path) => path.clone(),
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                vec![path.clone()]
                            };
                            let selected_count = context_paths.len();
                            let action_enabled = !app.library.file_action_in_progress()
                                && app.library_batch_export_progress().is_none()
                                && app.library_ai_mask_refresh_status().is_none()
                                && !context_paths.is_empty();
                            let can_paste_adjustments =
                                action_enabled && app.has_copied_adjustments();
                            response.context_menu(|ui| {
                                let export_label = if selected_count > 1 {
                                    "Export selected…"
                                } else {
                                    "Export…"
                                };
                                if ui
                                    .add_enabled(action_enabled, egui::Button::new(export_label))
                                    .clicked()
                                {
                                    library_action =
                                        Some(LibraryCardAction::Export(context_paths.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                if ui
                                    .add_enabled(
                                        action_enabled,
                                        egui::Button::new("Copy adjustments"),
                                    )
                                    .clicked()
                                {
                                    library_action = Some(LibraryCardAction::CopyAdjustments(
                                        context_source_path.clone(),
                                    ));
                                    ui.close();
                                }
                                let paste_label = if selected_count > 1 {
                                    "Paste adjustments to selected"
                                } else {
                                    "Paste adjustments"
                                };
                                if ui
                                    .add_enabled(
                                        can_paste_adjustments,
                                        egui::Button::new(paste_label),
                                    )
                                    .on_disabled_hover_text("Copy adjustments from an image first")
                                    .clicked()
                                {
                                    library_action = Some(LibraryCardAction::PasteAdjustments(
                                        context_paths.clone(),
                                    ));
                                    ui.close();
                                }
                                ui.separator();
                                let duplicate_label = if selected_count > 1 {
                                    "Duplicate selected (RAW + sidecars)"
                                } else {
                                    "Duplicate (RAW + sidecar)"
                                };
                                if ui
                                    .add_enabled(action_enabled, egui::Button::new(duplicate_label))
                                    .clicked()
                                {
                                    library_action =
                                        Some(LibraryCardAction::Duplicate(context_paths.clone()));
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
                                    library_action = Some(LibraryCardAction::ResetAdjustments(
                                        context_paths.clone(),
                                    ));
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
                                    library_action =
                                        Some(LibraryCardAction::Delete(context_paths.clone()));
                                    ui.close();
                                }
                            });
                        }
                    }
                });
            app.library.evict_old_textures(&protected_thumbnail_indices);
        }

        #[cfg(not(target_os = "android"))]
        if let Some(action) = library_action {
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
                            app.library.adjustment_paste_dialog =
                                Some(LibraryAdjustmentPasteDialog {
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
                LibraryCardAction::Duplicate(paths) => {
                    app.library.clear_selection();
                    app.library.duplicate_raws_with_sidecars(paths, ui.ctx());
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

        #[cfg(target_os = "android")]
        if let Some(action) = library_action {
            match action {
                LibraryCardAction::Export(targets) => {
                    if !targets.is_empty() {
                        app.library.export_dialog = Some(LibraryExportDialog {
                            targets,
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
                                    targets,
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
        {
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
        }

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
                    match paste_action {
                        2 => apply_library_adjustment_paste(
                            app,
                            dialog.targets,
                            crate::sidecar::AdjustmentPasteMode::Merge,
                            ui.ctx(),
                            frame,
                        ),
                        3 => apply_library_adjustment_paste(
                            app,
                            dialog.targets,
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

        #[cfg(not(target_os = "android"))]
        {
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
                        app.start_library_exports(
                            jobs,
                            dialog.settings.clone(),
                            dialog.format,
                            frame,
                        );
                    }
                }
            } else if close_export_dialog {
                app.library.export_dialog = None;
            }

            show_library_batch_export_progress(ui, app);
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
        if !app.library.has_selection() {
            let bounds = ui.max_rect().shrink(16.0);
            let rect = android_library_import_fab_rect(bounds);
            let response = ui.put(
                rect,
                egui::Button::new(egui::RichText::new(android_library_import_icon()).size(26.0))
                    .min_size(rect.size())
                    .corner_radius(ANDROID_LIBRARY_IMPORT_FAB_EDGE * 0.5)
                    .fill(ui.visuals().selection.bg_fill),
            );
            if response.clicked() {
                import_raw = true;
            }
            response.on_hover_text("Import RAW");
        }

        if refresh {
            app.library.refresh(ui.ctx());
        }
        #[cfg(target_os = "android")]
        if import_raw {
            app.open_file_dialog(frame);
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
        let row_height =
            ((available_width - gaps_width).max(1.0) / aspect_sum.max(f32::EPSILON)).max(1.0);
        let mut x = 0.0;

        for (row_offset, aspect) in row_aspects.iter().copied().enumerate() {
            // Give the final item the exact remaining width to absorb floating-
            // point rounding and keep every row flush with the right edge.
            let width = if row_offset + 1 == item_count {
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

    let mut tooltip = entry.info.display_path.clone();
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
            })
    }
}

#[cfg(not(target_os = "android"))]
type FolderScan = (Vec<LibraryFileInfo>, usize, bool);

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
        let LibrarySource::File(path) = &info.source;
        info.dimensions_hint = load_raw_display_dimensions(path).ok();
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
        android_library_import_fab_rect, android_library_import_icon,
        balanced_justified_row_ranges, desktop_selection_toggle_label, duplicate_raw_and_sidecar,
        elide_middle, format_file_size, import_raw_into_folder, justified_thumbnail_layout,
        loaded_library_thumbnail, make_resident_thumbnail, new_library_entry,
        run_thumbnail_workers, scan_folder, scan_folder_with_limit, LibraryFileInfo, LibraryState,
        RawImportOutcome, ScanEvent, ThumbnailRequest, ThumbnailWorker, TouchThumbnailAction,
        ANDROID_LIBRARY_IMPORT_FAB_EDGE,
    };
    use crate::pipeline::RawThumbnail;
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, RwLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    fn desktop_selection_toggle_label_matches_the_next_action() {
        assert_eq!(desktop_selection_toggle_label(false), "Select");
        assert_eq!(desktop_selection_toggle_label(true), "Cancel");
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

    #[test]
    fn android_import_fab_is_square_bottom_right_and_uses_plus_icon() {
        let bounds = eframe::egui::Rect::from_min_size(
            eframe::egui::pos2(10.0, 20.0),
            eframe::egui::vec2(300.0, 400.0),
        );
        let rect = android_library_import_fab_rect(bounds);
        assert_eq!(
            rect.size(),
            eframe::egui::vec2(
                ANDROID_LIBRARY_IMPORT_FAB_EDGE,
                ANDROID_LIBRARY_IMPORT_FAB_EDGE
            )
        );
        assert_eq!(rect.right_bottom(), bounds.right_bottom());
        assert_eq!(android_library_import_icon(), egui_phosphor::regular::PLUS);
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
    fn decoded_preview_does_not_change_reserved_gallery_geometry() {
        let info = LibraryFileInfo {
            source: LibrarySource::File(PathBuf::from("stable-layout.dng")),
            display_path: "stable-layout.dng".to_owned(),
            name: "stable-layout.dng".to_owned(),
            bytes: 1,
            dimensions_hint: Some([6000, 4000]),
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
    fn develop_pause_preserves_a_received_thumbnail_request() {
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
                display_priority: true,
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
                assert!(display_priority);
                assert_eq!((loaded.thumbnail.width, loaded.thumbnail.height), (1, 1));
            }
            _ => panic!("thumbnail worker did not preserve the paused request"),
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

        let first = duplicate_raw_and_sidecar(&raw).unwrap();
        let second = duplicate_raw_and_sidecar(&raw).unwrap();
        assert_eq!(first.file_name().unwrap(), "photo copy.CR3");
        assert_eq!(second.file_name().unwrap(), "photo copy 2.CR3");
        assert_eq!(fs::read(&first).unwrap(), b"raw-bytes");
        assert_eq!(
            fs::read(crate::sidecar::sidecar_path_for_raw(&first)).unwrap(),
            b"sidecar-bytes"
        );

        fs::remove_dir_all(root).unwrap();
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
