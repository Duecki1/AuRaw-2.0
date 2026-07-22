use crate::app::{AppTab, AurawApp};
#[cfg(not(target_os = "android"))]
use crate::pipeline::is_supported_raw_path;
#[cfg(not(target_os = "android"))]
use crate::pipeline::load_raw_thumbnail;
use crate::pipeline::RawThumbnail;
use eframe::egui::{self, Align2, Color32, FontId, Sense, Stroke, StrokeKind, Ui};
#[cfg(not(target_os = "android"))]
use std::cmp::Ordering as CmpOrdering;
#[cfg(not(target_os = "android"))]
use std::collections::BinaryHeap;
use std::collections::{HashMap, HashSet};
#[cfg(not(target_os = "android"))]
use std::ffi::OsString;
#[cfg(not(target_os = "android"))]
use std::fs::{self, OpenOptions};
#[cfg(not(target_os = "android"))]
use std::io;
use std::path::Path;
#[cfg(not(target_os = "android"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::Duration;
#[cfg(not(target_os = "android"))]
use std::time::SystemTime;

const THUMBNAIL_EDGE: u32 = 512;
const MAX_LIBRARY_FILES: usize = 20_000;
const MAX_EVENTS_PER_FRAME: usize = 12;
const MAX_PENDING_THUMBNAIL_RESULTS: usize = 32;
const MAX_PENDING_THUMBNAILS: usize = 64;
const DESKTOP_TEXTURE_CACHE_LIMIT: usize = 128;
const ANDROID_TEXTURE_CACHE_LIMIT: usize = 48;
#[cfg(target_os = "android")]
const ANDROID_DEVELOP_TEXTURE_CACHE_LIMIT: usize = 10;
const THUMBNAIL_PAUSE_POLL_INTERVAL: Duration = Duration::from_millis(20);
const THUMBNAIL_QUEUE_POLL_INTERVAL: Duration = Duration::from_millis(8);
pub(crate) const MAX_DESKTOP_THUMBNAIL_WORKERS: usize = 16;
pub(crate) const MAX_ANDROID_THUMBNAIL_WORKERS: usize = 4;

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

#[derive(Clone, Debug)]
struct LibraryFileInfo {
    source: LibrarySource,
    display_path: String,
    name: String,
    parent: String,
    bytes: u64,
    #[cfg(not(target_os = "android"))]
    modified: Option<SystemTime>,
}

pub(crate) struct LibraryEntry {
    info: LibraryFileInfo,
    texture: Option<egui::TextureHandle>,
    thumbnail_size: Option<[u32; 2]>,
    thumbnail_error: Option<String>,
    thumbnail_queued: bool,
    developed_thumbnail: bool,
    last_used: u64,
}

struct LoadedLibraryThumbnail {
    thumbnail: RawThumbnail,
    developed: bool,
}

#[derive(Clone)]
struct ThumbnailRequest {
    generation: u64,
    source: LibrarySource,
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
    selected_sources: HashSet<LibrarySource>,
    selection_mode: bool,
    #[cfg(not(target_os = "android"))]
    file_action_receiver: Option<mpsc::Receiver<Result<Vec<PathBuf>, String>>>,
}

impl LibraryState {
    #[cfg(all(not(target_os = "android"), test))]
    pub(crate) fn new(context: &egui::Context) -> Self {
        Self::new_with_workers(context, default_thumbnail_worker_count())
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn new_with_workers(context: &egui::Context, workers: usize) -> Self {
        let _ = context;
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
            thumbnail_workers: workers.clamp(1, maximum_thumbnail_worker_count()),
            selected_sources: HashSet::new(),
            selection_mode: false,
            file_action_receiver: None,
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn new_android(
        android_app: android_activity::AndroidApp,
        context: &egui::Context,
    ) -> Self {
        Self::new_android_with_workers(android_app, context, default_thumbnail_worker_count())
    }

    #[cfg(target_os = "android")]
    pub(crate) fn new_android_with_workers(
        android_app: android_activity::AndroidApp,
        context: &egui::Context,
        workers: usize,
    ) -> Self {
        let location = crate::android::library_location(&android_app).unwrap_or_else(|error| {
            log::warn!("{error}");
            "Download/AuRaw".to_owned()
        });
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
            thumbnail_workers: workers.clamp(1, maximum_thumbnail_worker_count()),
            selected_sources: HashSet::new(),
            selection_mode: false,
        };
        state.refresh(context);
        state
    }

    pub(crate) fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }

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

    #[cfg(not(target_os = "android"))]
    pub(crate) fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    pub(crate) fn thumbnail_worker_count(&self) -> usize {
        self.thumbnail_workers
    }

    pub(crate) fn set_thumbnail_worker_count(&mut self, workers: usize, context: &egui::Context) {
        let workers = workers.clamp(1, maximum_thumbnail_worker_count());
        if self.thumbnail_workers == workers {
            return;
        }
        self.thumbnail_workers = workers;
        if self.location.is_some() {
            self.refresh(context);
        }
    }

    /// Stops the thumbnail worker from beginning another decode while a full
    /// RAW is opened. Requests already queued by the virtualized grid remain
    /// queued and are continued when the Library tab is shown again.
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

    fn resume_thumbnail_decoding(&self) {
        self.decoding_paused.store(false, Ordering::Release);
    }

    #[cfg(not(target_os = "android"))]
    fn file_action_in_progress(&self) -> bool {
        self.file_action_receiver.is_some()
    }

    #[cfg(not(target_os = "android"))]
    fn duplicate_raws_with_sidecars(&mut self, raw_paths: Vec<PathBuf>, context: &egui::Context) {
        if self.file_action_receiver.is_some() {
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
        // flash every card back to a placeholder or decode cached PNGs again.
        for entry in &mut self.entries {
            entry.thumbnail_queued = false;
            entry.thumbnail_error = None;
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
            self.status = "Refreshing Download/AuRaw…".to_owned();
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
                            let parent = Path::new(&document.display_path)
                                .parent()
                                .map(|parent| parent.display().to_string())
                                .unwrap_or_default();
                            LibraryFileInfo {
                                source: LibrarySource::Android {
                                    uri: document.uri,
                                    display_name: document.display_name.clone(),
                                    bytes: document.bytes,
                                    modified_seconds: document.modified_seconds,
                                },
                                display_path: document.display_path,
                                name: document.display_name,
                                parent,
                                bytes: document.bytes,
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

    fn poll(&mut self, context: &egui::Context) {
        #[cfg(not(target_os = "android"))]
        {
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
                    self.entry_indices = self
                        .entries
                        .iter()
                        .enumerate()
                        .map(|(index, entry)| (entry.info.source.clone(), index))
                        .collect();
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
                            let thumbnail = loaded.thumbnail;
                            let image = egui::ColorImage::from_rgba_unmultiplied(
                                [thumbnail.width as usize, thumbnail.height as usize],
                                &thumbnail.rgba,
                            );
                            self.entries[index].texture = Some(context.load_texture(
                                format!("library-thumbnail-{generation}-{index}"),
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                            self.entries[index].thumbnail_size =
                                Some([thumbnail.width, thumbnail.height]);
                            self.entries[index].thumbnail_error = None;
                            self.entries[index].developed_thumbnail = loaded.developed;
                        }
                        Err(error) => {
                            if !self.entries[index].developed_thumbnail {
                                self.entries[index].thumbnail_error = Some(error);
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

    fn touch_and_request_thumbnail(&mut self, index: usize) {
        let Some(entry) = self.entries.get_mut(index) else {
            return;
        };
        self.usage_clock = self.usage_clock.wrapping_add(1).max(1);
        entry.last_used = self.usage_clock;
        if entry.texture.is_some() || entry.thumbnail_error.is_some() || entry.thumbnail_queued {
            return;
        }
        let request = ThumbnailRequest {
            generation: self.generation.load(Ordering::Acquire),
            source: entry.info.source.clone(),
        };
        if self
            .request_sender
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
        self.entries[index].thumbnail_size = Some([thumbnail.width, thumbnail.height]);
        self.entries[index].thumbnail_error = None;
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
    }

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
            .filter(|(index, entry)| {
                entry.texture.is_some() && !protected_indices.contains(index)
            })
            .map(|(index, entry)| (entry.last_used, index))
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        for (_, index) in candidates
            .into_iter()
            .take(texture_count - effective_limit)
        {
            self.entries[index].texture = None;
            // Keep decoded dimensions after GPU texture eviction so restored thumbnails
            // can still be center-cropped correctly in the fixed-height grid.
        }
    }
}

fn new_library_entry(info: LibraryFileInfo) -> LibraryEntry {
    LibraryEntry {
        info,
        texture: None,
        thumbnail_size: None,
        thumbnail_error: None,
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

type ThumbnailLoader =
    Arc<dyn Fn(&LibrarySource) -> Result<LoadedLibraryThumbnail, String> + Send + Sync + 'static>;

#[cfg(not(target_os = "android"))]
fn load_desktop_library_thumbnail(
    source: &LibrarySource,
) -> Result<LoadedLibraryThumbnail, String> {
    let LibrarySource::File(path) = source;
    match crate::sidecar::load_developed_thumbnail_cache(path, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => {
            return Ok(LoadedLibraryThumbnail {
                thumbnail,
                developed: true,
            })
        }
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use developed thumbnail cache for {}: {error}",
            path.display()
        ),
    }
    match crate::thumbnail_cache::load_desktop_raw_thumbnail(path, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => {
            return Ok(LoadedLibraryThumbnail {
                thumbnail,
                developed: false,
            })
        }
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use RAW thumbnail cache for {}: {error}",
            path.display()
        ),
    }

    let thumbnail =
        load_raw_thumbnail(path, THUMBNAIL_EDGE).map_err(|error| format!("{error:#}"))?;
    if let Err(error) = crate::thumbnail_cache::save_desktop_raw_thumbnail(path, &thumbnail) {
        log::warn!(
            "could not persist RAW thumbnail cache for {}: {error}",
            path.display()
        );
    }
    Ok(LoadedLibraryThumbnail {
        thumbnail,
        developed: false,
    })
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
        Ok(Some(thumbnail)) => {
            return Ok(LoadedLibraryThumbnail {
                thumbnail,
                developed: true,
            })
        }
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use Android developed-thumbnail cache for {display_name}: {error}"
        ),
    }
    crate::android::load_library_thumbnail(
        app,
        uri,
        display_name,
        *bytes,
        *modified_seconds,
        THUMBNAIL_EDGE,
    )
    .map(|thumbnail| LoadedLibraryThumbnail {
        thumbnail,
        developed: false,
    })
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
    repaint: egui::Context,
    load: ThumbnailLoader,
) {
    while cancellation.load(Ordering::Acquire) == generation {
        let received = request_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_recv();
        let request = match received {
            Ok(request) => request,
            Err(mpsc::TryRecvError::Empty) => {
                std::thread::sleep(THUMBNAIL_QUEUE_POLL_INTERVAL);
                continue;
            }
            Err(mpsc::TryRecvError::Disconnected) => break,
        };
        if request.generation != generation {
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
        if event_sender
            .send(ScanEvent::Thumbnail {
                generation,
                source: request.source,
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
    Duplicate(Vec<PathBuf>),
    ResetAdjustments(Vec<PathBuf>),
    Delete(Vec<PathBuf>),
}

#[cfg(target_os = "android")]
enum LibraryCardAction {
    ResetAdjustments(Vec<(String, String)>),
    Delete(Vec<(String, String)>),
}

#[cfg(target_os = "android")]
fn android_selection_menu(
    ui: &mut Ui,
    selected: &[(LibrarySource, String)],
    open_source: &mut Option<(LibrarySource, String)>,
    library_action: &mut Option<LibraryCardAction>,
) {
    if selected.len() == 1 && ui.button("Open").clicked() {
        *open_source = Some(selected[0].clone());
        ui.close();
    }

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

    let reset_label = if selected.len() == 1 {
        "Reset all adjustments"
    } else {
        "Reset adjustments for selected"
    };
    if ui.button(reset_label).clicked() {
        *library_action = Some(LibraryCardAction::ResetAdjustments(targets()));
        ui.close();
    }
    ui.separator();
    let delete_label = if selected.len() == 1 {
        "Delete"
    } else {
        "Delete selected"
    };
    if ui.button(delete_label).clicked() {
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
                        android_selection_menu(
                            ui,
                            &selected_android_items,
                            &mut open_source,
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
                    .button(if desktop_selection_mode { "Cancel" } else { "Select" })
                    .clicked()
                {
                    if desktop_selection_mode {
                        app.library.clear_selection();
                    } else {
                        app.library.begin_selection();
                    }
                }

                if ui
                    .add_enabled(
                        app.library.location.is_some() && !app.library.scanning,
                        egui::Button::new("Refresh"),
                    )
                    .clicked()
                {
                    refresh = true;
                }
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
                    ui.label("AuRaw shows RAW files directly inside the selected folder and builds previews as they become visible.");
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
            let scroll = egui::ScrollArea::vertical()
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
                        app.library.touch_and_request_thumbnail(index);
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
                            if response.secondary_clicked() {
                                app.library.begin_selection();
                                app.library.selected_sources.insert(source.clone());
                                crate::android::set_back_navigation_active(true);
                            } else if response.clicked() {
                                if !app.library.selection_mode() {
                                    open_source = Some((source.clone(), name.clone()));
                                } else if !app.library.selected_sources.remove(&source) {
                                    app.library.selected_sources.insert(source.clone());
                                }

                                if app.library.selection_mode() && app.library.selected_sources.is_empty() {
                                    app.library.clear_selection();
                                    crate::android::set_back_navigation_active(false);
                                }
                            }
                        }

                        #[cfg(not(target_os = "android"))]
                        {
                            let LibrarySource::File(path) = &source;
                            let path = path.clone();

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
                                        app.library.selected_sources.contains(&candidate.info.source)
                                    })
                                    .filter_map(|candidate| match &candidate.info.source {
                                        LibrarySource::File(path) => Some(path.clone()),
                                    })
                                    .collect::<Vec<_>>()
                            } else {
                                vec![path]
                            };
                            let selected_count = context_paths.len();
                            let action_enabled = !app.library.file_action_in_progress()
                                && !context_paths.is_empty();
                            response.context_menu(|ui| {
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
                                    .add_enabled(action_enabled, egui::Button::new(reset_label))
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
            app.library
                .evict_old_textures(&protected_thumbnail_indices);
        }

        #[cfg(not(target_os = "android"))]
        if let Some(action) = library_action {
            match action {
                LibraryCardAction::Duplicate(paths) => {
                    app.library.clear_selection();
                    app.library.duplicate_raws_with_sidecars(paths, ui.ctx());
                }
                LibraryCardAction::ResetAdjustments(paths) => {
                    let current_to_reopen = app.current_path.as_ref().and_then(|current| {
                        paths.iter().find(|path| *path == current).cloned()
                    });
                    if let Some(path) = current_to_reopen.as_deref() {
                        app.detach_current_file_for_library_action(path);
                    }

                    let total = paths.len();
                    let mut failures = Vec::new();
                    let mut removed_count = 0usize;
                    for path in &paths {
                        match crate::sidecar::remove_desktop_edits(path) {
                            Ok(removed) => {
                                if removed {
                                    removed_count += 1;
                                }
                            },
                            Err(error) => failures.push(format!("{}: {error}", path.display())),
                        }
                    }
                    app.library.clear_selection();
                    app.library.refresh(ui.ctx());
                    app.library.status = if failures.is_empty() {
                        format!(
                            "Reset adjustments for {total} selected {} ({removed_count} changed)",
                            if total == 1 { "image" } else { "images" }
                        )
                    } else {
                        format!(
                            "Reset adjustments for {} of {total} selected images. {}",
                            total.saturating_sub(failures.len()),
                            failures.join(" · ")
                        )
                    };
                    if let Some(path) = current_to_reopen {
                        app.open_path(path, frame);
                    }
                }
                LibraryCardAction::Delete(paths) => {
                    let current_target = app.current_path.as_ref().and_then(|current| {
                        paths.iter().find(|path| *path == current).cloned()
                    });
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
                        format!("Deleted {deleted} of {total} selected images. {}", details.join(" · "))
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
            let (verb, targets, operation): (
                &str,
                Vec<(String, String)>,
                fn(&mut AurawApp, &str, &str) -> Result<(), String>,
            ) = match action {
                LibraryCardAction::ResetAdjustments(targets) => (
                    "Reset adjustments for",
                    targets,
                    AurawApp::reset_android_library_adjustments,
                ),
                LibraryCardAction::Delete(targets) => (
                    "Deleted",
                    targets,
                    AurawApp::delete_android_library_item,
                ),
            };

            let total = targets.len();
            let mut failures = Vec::new();
            for (uri, display_name) in targets {
                if let Err(error) = operation(app, &uri, &display_name) {
                    failures.push(format!("{display_name}: {error}"));
                }
            }
            app.library.clear_selection();
            crate::android::set_back_navigation_active(false);
            app.library.refresh(ui.ctx());
            app.library.status = if failures.is_empty() {
                format!("{verb} {total} selected {}", if total == 1 { "image" } else { "images" })
            } else {
                format!(
                    "Completed {} of {total} selected actions. {}",
                    total.saturating_sub(failures.len()),
                    failures.join(" · ")
                )
            };
        }

        #[cfg(target_os = "android")]
        if !app.library.has_selection() {
            let bounds = ui.max_rect().shrink(16.0);
            let size = egui::vec2(56.0, 56.0);
            let rect = egui::Rect::from_min_size(bounds.right_bottom() - size, size);
            let response = ui.put(
                rect,
                egui::Button::new(egui::RichText::new("+").size(28.0))
                    .min_size(size)
                    .corner_radius(28)
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
            if (row_weight - desired_weight).abs()
                <= (with_next - desired_weight).abs()
            {
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
                .thumbnail_size
                .and_then(|[width, source_height]| {
                    (width > 0 && source_height > 0)
                        .then_some(width as f32 / source_height as f32)
                })
                .filter(|aspect| aspect.is_finite() && *aspect > 0.0)
                .unwrap_or(1.0)
        })
        .collect();

    let row_ranges = balanced_justified_row_ranges(
        &aspects,
        available_width,
        target_height,
        gap,
    );
    let mut placements = Vec::with_capacity(entries.len());
    let mut y = 0.0;

    for (row_start, row_end) in row_ranges {
        let row_aspects = &aspects[row_start..row_end];
        let item_count = row_aspects.len();
        let aspect_sum = row_aspects.iter().sum::<f32>();
        let gaps_width = gap * (item_count.saturating_sub(1) as f32);
        let row_height = ((available_width - gaps_width).max(1.0)
            / aspect_sum.max(f32::EPSILON))
            .max(1.0);
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
        let uv = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0));
        ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
    } else {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            if entry.thumbnail_error.is_some() {
                "Preview unavailable"
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
            parent: String::new(),
            bytes: file_metadata.as_ref().map_or(0, std::fs::Metadata::len),
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
    let files = files.into_iter().map(|ranked| ranked.info).collect();
    Ok(Some((files, warning_count, truncated)))
}

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
    #[cfg(unix)]
    use super::LibrarySource;
    use super::{
        balanced_justified_row_ranges, duplicate_raw_and_sidecar, elide_middle, format_file_size,
        run_thumbnail_workers, scan_folder, scan_folder_with_limit, LibraryState,
        LoadedLibraryThumbnail, ScanEvent,
        ThumbnailRequest, ThumbnailWorker,
    };
    use crate::pipeline::RawThumbnail;
    #[cfg(unix)]
    use std::collections::HashSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{mpsc, Arc, RwLock};
    use std::time::{Duration, SystemTime};

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
                    Ok(LoadedLibraryThumbnail {
                        thumbnail: RawThumbnail {
                            width: 1,
                            height: 1,
                            rgba: vec![0, 0, 0, 255],
                        },
                        developed: false,
                    })
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
                result: Ok(loaded),
            }) => {
                assert_eq!(event_generation, generation);
                assert_eq!(event_source, source);
                assert_eq!((loaded.thumbnail.width, loaded.thumbnail.height), (1, 1));
            }
            _ => panic!("thumbnail worker did not preserve the paused request"),
        }
        drop(request_sender);
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
