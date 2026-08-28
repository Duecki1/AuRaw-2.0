use android_activity::AndroidApp;
use jni::{
    errors::LogContextErrorAndDefault,
    objects::{JClass, JObject, JString},
    refs::Global,
    EnvUnowned, JValue, JavaVM,
};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File},
    os::fd::FromRawFd,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct PickedDocument {
    pub path: PathBuf,
    pub display_name: String,
    pub library_uri: String,
    pub delete_after_decode: bool,
    pub raw_fd_guard: Option<File>,
}

#[derive(Clone, Debug)]
pub struct LibraryDocument {
    pub uri: String,
    pub display_name: String,
    pub display_path: String,
    pub bytes: u64,
    pub modified_seconds: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryFolder {
    pub path: String,
    pub name: String,
}

#[derive(Debug)]
pub enum PickerResult {
    Picked(PickedDocument),
    BatchImported {
        imported: usize,
        failed: usize,
        errors: String,
    },
    Cancelled,
    Failed(String),
}

#[derive(Debug)]
pub enum ExportPublishResult {
    Published(String),
    Failed(String),
}

#[derive(Debug)]
struct DirectExportTarget {
    descriptor: TransferredFileDescriptor,
    uri: String,
    location: String,
    temp_dir: PathBuf,
}

#[derive(Debug)]
struct TransferredFileDescriptor {
    _file: File,
    raw_fd: i32,
}

impl TransferredFileDescriptor {
    fn from_java(raw_fd: i32, invalid_descriptor_message: &str) -> Result<Self, String> {
        if raw_fd < 0 {
            return Err(invalid_descriptor_message.to_owned());
        }
        let file = unsafe { File::from_raw_fd(raw_fd) };
        Ok(Self {
            _file: file,
            raw_fd,
        })
    }

    fn proc_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.raw_fd))
    }
}

#[derive(Debug)]
struct PendingExportDescriptor {
    descriptor: TransferredFileDescriptor,
    uri: String,
    location: String,
}

impl PendingExportDescriptor {
    fn parse(encoded: &str) -> Result<Self, String> {
        let mut fields = encoded.splitn(3, '\t');
        let raw_fd = fields
            .next()
            .ok_or_else(|| "Android export descriptor is missing its fd".to_owned())?
            .parse::<i32>()
            .map_err(|error| format!("invalid Android export fd: {error}"))?;
        let descriptor = TransferredFileDescriptor::from_java(
            raw_fd,
            "Android export descriptor returned a negative fd",
        )?;
        let uri = fields
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Android export descriptor is missing its URI".to_owned())?
            .to_owned();
        let location = fields
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "Android export descriptor is missing its location".to_owned())?
            .to_owned();
        Ok(Self {
            descriptor,
            uri,
            location,
        })
    }
}

#[derive(Debug)]
pub enum CameraProfileFolderResult {
    ImportStarted {
        label: String,
    },
    Picked {
        path: PathBuf,
        label: String,
        profiles: usize,
    },
    Cancelled,
    Failed(String),
}

static RESULTS: OnceLock<Mutex<VecDeque<PickerResult>>> = OnceLock::new();
static CAMERA_PROFILE_FOLDER_RESULTS: OnceLock<Mutex<VecDeque<CameraProfileFolderResult>>> =
    OnceLock::new();
static EXPORT_RESULTS: OnceLock<Mutex<VecDeque<ExportPublishResult>>> = OnceLock::new();
static DIRECT_EXPORTS: OnceLock<Mutex<HashMap<PathBuf, DirectExportTarget>>> = OnceLock::new();
static EGUI_CONTEXT: Mutex<Option<egui::Context>> = Mutex::new(None);
static BACK_NAVIGATION_ACTIVE: AtomicBool = AtomicBool::new(false);
static BACK_REQUESTED: AtomicBool = AtomicBool::new(false);
static SYSTEM_INSET_LEFT_PX: AtomicI32 = AtomicI32::new(0);
static SYSTEM_INSET_TOP_PX: AtomicI32 = AtomicI32::new(0);
static SYSTEM_INSET_RIGHT_PX: AtomicI32 = AtomicI32::new(0);
static SYSTEM_INSET_BOTTOM_PX: AtomicI32 = AtomicI32::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
struct TaskNotificationPayload {
    title: String,
    phase: String,
    detail: String,
    progress_percent: i32,
    indeterminate: bool,
    queued_count: i32,
}

struct TaskNotificationState {
    activity_key: usize,
    payload: TaskNotificationPayload,
    posted_at: Instant,
}

static TASK_NOTIFICATION_STATE: Mutex<Option<TaskNotificationState>> = Mutex::new(None);
const TASK_NOTIFICATION_MIN_UPDATE_INTERVAL: Duration = Duration::from_millis(250);

fn results() -> &'static Mutex<VecDeque<PickerResult>> {
    RESULTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn camera_profile_folder_results() -> &'static Mutex<VecDeque<CameraProfileFolderResult>> {
    CAMERA_PROFILE_FOLDER_RESULTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn export_results() -> &'static Mutex<VecDeque<ExportPublishResult>> {
    EXPORT_RESULTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn direct_exports() -> &'static Mutex<HashMap<PathBuf, DirectExportTarget>> {
    DIRECT_EXPORTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn request_repaint() {
    if let Ok(installed) = EGUI_CONTEXT.lock() {
        if let Some(context) = installed.as_ref() {
            context.request_repaint();
        }
    }
}

pub fn install_context(context: &egui::Context) {
    if let Ok(mut installed) = EGUI_CONTEXT.lock() {
        *installed = Some(context.clone());
    }
    if let Ok(mut notification) = TASK_NOTIFICATION_STATE.lock() {
        *notification = None;
    }
}

pub fn uninstall_context() {
    if let Ok(mut installed) = EGUI_CONTEXT.lock() {
        *installed = None;
    }
    BACK_NAVIGATION_ACTIVE.store(false, Ordering::Release);
    BACK_REQUESTED.store(false, Ordering::Release);
}

pub fn set_back_navigation_active(active: bool) {
    BACK_NAVIGATION_ACTIVE.store(active, Ordering::Release);
}

pub fn take_back_request() -> bool {
    BACK_REQUESTED.swap(false, Ordering::AcqRel)
}

pub fn system_bar_insets_points(pixels_per_point: f32) -> [f32; 4] {
    let scale = pixels_per_point.max(0.5);
    let to_points = |value: i32| value.max(0) as f32 / scale;
    [
        to_points(SYSTEM_INSET_LEFT_PX.load(Ordering::Acquire)),
        to_points(SYSTEM_INSET_TOP_PX.load(Ordering::Acquire)),
        to_points(SYSTEM_INSET_RIGHT_PX.load(Ordering::Acquire)),
        to_points(SYSTEM_INSET_BOTTOM_PX.load(Ordering::Acquire)),
    ]
}

pub fn take_picker_result() -> Option<PickerResult> {
    results().lock().ok()?.pop_front()
}

pub fn take_camera_profile_folder_result() -> Option<CameraProfileFolderResult> {
    camera_profile_folder_results().lock().ok()?.pop_front()
}

pub fn take_export_publish_result() -> Option<ExportPublishResult> {
    export_results().lock().ok()?.pop_front()
}

pub fn set_light_system_bars(app: &AndroidApp, light: bool) -> Result<(), String> {
    with_activity(app, |env, activity| {
        env.call_method(
            activity,
            jni::jni_str!("setLightSystemBars"),
            jni::jni_sig!((i32) -> void),
            &[JValue::Int(i32::from(light))],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not update Android system-bar appearance: {error:#}"))
}

pub fn open_camera_profile_folder(app: &AndroidApp) -> Result<(), String> {
    with_activity(app, |env, activity| {
        env.call_method(
            activity,
            jni::jni_str!("openCameraProfileFolder"),
            jni::jni_sig!(() -> void),
            &[],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not open Android's camera-profile folder picker: {error:#}"))
}

pub fn remove_camera_profile_mirror(app: &AndroidApp, path: &Path) -> Result<(), String> {
    let path = path.to_string_lossy().into_owned();
    with_profile_importer(app, |env, profile_importer| {
        let path = env.new_string(&path)?;
        env.call_method(
            profile_importer,
            jni::jni_str!("removeCameraProfileMirror"),
            jni::jni_sig!((JString) -> void),
            &[JValue::Object(&path)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not schedule Android camera-profile cleanup: {error:#}"))
}

pub fn clear_camera_profile_folder_picker_location(app: &AndroidApp) -> Result<(), String> {
    with_profile_importer(app, |env, profile_importer| {
        env.call_method(
            profile_importer,
            jni::jni_str!("clearFolderPickerLocation"),
            jni::jni_sig!(() -> void),
            &[],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not clear Android's camera-profile picker location: {error:#}"))
}

pub fn open_raw_document(app: &AndroidApp) -> Result<(), String> {
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
        let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
        env.call_method(
            activity,
            jni::jni_str!("openRawDocument"),
            jni::jni_sig!(() -> void),
            &[],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not open Android's file picker: {error:#}"))
}

pub fn device_diagnostics(app: &AndroidApp) -> Result<String, String> {
    with_activity(app, |env, activity| {
        let object = env
            .call_method(
                activity,
                jni::jni_str!("deviceDiagnostics"),
                jni::jni_sig!(() -> JString),
                &[],
            )?
            .l()?;
        let string = env.cast_local::<JString>(object)?;
        Ok(string.to_string())
    })
    .map_err(|error| format!("could not read Android device diagnostics: {error:#}"))
}

pub fn copy_text_to_clipboard(app: &AndroidApp, label: &str, text: &str) -> Result<(), String> {
    let label = label.to_owned();
    let text = text.to_owned();
    with_activity(app, |env, activity| {
        let label = env.new_string(&label)?;
        let text = env.new_string(&text)?;
        env.call_method(
            activity,
            jni::jni_str!("copyTextToClipboard"),
            jni::jni_sig!((JString, JString) -> void),
            &[JValue::Object(&label), JValue::Object(&text)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not copy diagnostics to Android clipboard: {error:#}"))
}

pub fn performance_settings_path(app: &AndroidApp) -> Result<PathBuf, String> {
    let path = with_activity(app, |env, activity| {
        let object = env
            .call_method(
                activity,
                jni::jni_str!("performanceSettingsPath"),
                jni::jni_sig!(() -> JString),
                &[],
            )?
            .l()?;
        let path = env.cast_local::<JString>(object)?;
        Ok(path.to_string())
    })
    .map_err(|error| format!("could not locate Android performance settings: {error:#}"))?;
    if path.is_empty() {
        Err("Android returned no performance settings path".to_owned())
    } else {
        Ok(PathBuf::from(path))
    }
}

pub fn gpu_pipeline_cache_dir(app: &AndroidApp) -> Result<PathBuf, String> {
    let path = with_activity(app, |env, activity| {
        let object = env
            .call_method(
                activity,
                jni::jni_str!("gpuPipelineCacheDir"),
                jni::jni_sig!(() -> JString),
                &[],
            )?
            .l()?;
        let path = env.cast_local::<JString>(object)?;
        Ok(path.to_string())
    })
    .map_err(|error| format!("could not locate Android GPU pipeline cache: {error:#}"))?;
    if path.is_empty() {
        Err("Android returned no GPU pipeline cache directory".to_owned())
    } else {
        Ok(PathBuf::from(path))
    }
}

pub fn lensfun_database_dir(app: &AndroidApp) -> Result<PathBuf, String> {
    let path = with_activity(app, |env, activity| {
        let object = env
            .call_method(
                activity,
                jni::jni_str!("lensfunDatabaseDir"),
                jni::jni_sig!(() -> JString),
                &[],
            )?
            .l()?;
        let path = env.cast_local::<JString>(object)?;
        Ok(path.to_string())
    })
    .map_err(|error| format!("could not materialize bundled Lensfun database: {error:#}"))?;
    if path.is_empty() {
        Err("Android returned no Lensfun database directory".to_owned())
    } else {
        Ok(PathBuf::from(path))
    }
}

pub fn library_location(app: &AndroidApp) -> Result<String, String> {
    with_storage_manager(app, |env, storage_manager| {
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("rawLibraryLocation"),
                jni::jni_sig!(() -> JString),
                &[],
            )?
            .l()?;
        let string = env.cast_local::<JString>(object)?;
        Ok(string.to_string())
    })
    .map_err(|error| format!("could not locate Android RAW library: {error:#}"))
}

pub fn list_library_documents(app: &AndroidApp) -> Result<Vec<LibraryDocument>, String> {
    let encoded = with_storage_manager(app, |env, storage_manager| {
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("listRawLibrary"),
                jni::jni_sig!(() -> JString),
                &[],
            )?
            .l()?;
        let string = env.cast_local::<JString>(object)?;
        Ok(string.to_string())
    })
    .map_err(|error| format!("could not list Android RAW library: {error:#}"))?;
    encoded
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.split('\t');
            let uri = decode_uri_component(fields.next().unwrap_or_default())?;
            let display_name = decode_uri_component(fields.next().unwrap_or_default())?;
            let display_path = decode_uri_component(fields.next().unwrap_or_default())?;
            let bytes = fields
                .next()
                .ok_or_else(|| "Android library record has no byte size".to_owned())?
                .parse::<u64>()
                .map_err(|error| format!("invalid Android library byte size: {error}"))?;
            let modified_seconds = fields
                .next()
                .ok_or_else(|| "Android library record has no modification time".to_owned())?
                .parse::<u64>()
                .map_err(|error| format!("invalid Android library modification time: {error}"))?;
            if fields.next().is_some() || uri.is_empty() || display_name.is_empty() {
                return Err("malformed Android library record".to_owned());
            }
            Ok(LibraryDocument {
                uri,
                display_name,
                display_path,
                bytes,
                modified_seconds,
            })
        })
        .collect()
}

pub fn list_library_folders(app: &AndroidApp) -> Result<Vec<LibraryFolder>, String> {
    let encoded = with_storage_manager(app, |env, storage_manager| {
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("listRawLibraryFolders"),
                jni::jni_sig!(() -> JString),
                &[],
            )?
            .l()?;
        Ok(env.cast_local::<JString>(object)?.to_string())
    })
    .map_err(|error| format!("could not list Android RAW library folders: {error:#}"))?;
    encoded
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.split('\t');
            let path = decode_uri_component(fields.next().unwrap_or_default())?;
            let name = decode_uri_component(fields.next().unwrap_or_default())?;
            if fields.next().is_some() || path.is_empty() || name.is_empty() {
                return Err("malformed Android library folder record".to_owned());
            }
            Ok(LibraryFolder { path, name })
        })
        .collect()
}

pub fn select_library_folder(app: &AndroidApp, relative_path: &str) -> Result<(), String> {
    let relative_path = relative_path.to_owned();
    with_storage_manager(app, |env, storage_manager| {
        let relative_path = env.new_string(&relative_path)?;
        env.call_method(
            storage_manager,
            jni::jni_str!("selectRawLibraryFolder"),
            jni::jni_sig!((JString) -> void),
            &[JValue::Object(&relative_path)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not select Android RAW library folder: {error:#}"))
}

pub fn create_library_folder(
    app: &AndroidApp,
    parent_path: &str,
    name: &str,
) -> Result<String, String> {
    let parent_path = parent_path.to_owned();
    let name = name.to_owned();
    with_storage_manager(app, |env, storage_manager| {
        let parent_path = env.new_string(&parent_path)?;
        let name = env.new_string(&name)?;
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("createRawLibraryFolder"),
                jni::jni_sig!((JString, JString) -> JString),
                &[JValue::Object(&parent_path), JValue::Object(&name)],
            )?
            .l()?;
        Ok(env.cast_local::<JString>(object)?.to_string())
    })
    .map_err(|error| format!("could not create Android RAW library folder: {error:#}"))
}

pub fn load_library_thumbnail(
    app: &AndroidApp,
    uri: &str,
    display_name: &str,
    bytes: u64,
    modified_seconds: u64,
    maximum_edge: u32,
) -> Result<crate::pipeline::RawThumbnail, String> {
    let cache_path = raw_thumbnail_cache_path(app, uri, bytes, modified_seconds, maximum_edge)?;
    match crate::thumbnail_cache::load_jpeg(&cache_path, maximum_edge) {
        Ok(Some(thumbnail)) => return Ok(thumbnail),
        Ok(None) => {}
        Err(error) => log::warn!("discarding Android RAW thumbnail cache: {error}"),
    }

    let direct = load_library_thumbnail_from_fd(app, uri, maximum_edge);
    let thumbnail = match direct {
        Ok(thumbnail) => thumbnail,
        Err(direct_error) => {
            log::warn!(
                "direct Android RAW thumbnail extraction failed; retrying from private cache: {direct_error}"
            );
            let temporary = materialize_library_thumbnail(app, uri, display_name)?;
            let result = crate::pipeline::load_raw_thumbnail(&temporary, maximum_edge)
                .map_err(|error| format!("{error:#}"));
            if let Err(error) = fs::remove_file(&temporary) {
                log::warn!(
                    "could not remove Android thumbnail staging file {}: {error}",
                    temporary.display()
                );
            }
            result?
        }
    };
    if let Err(error) = crate::thumbnail_cache::save_jpeg(&cache_path, &thumbnail) {
        log::warn!("could not persist Android RAW thumbnail: {error}");
    }
    Ok(thumbnail)
}

pub fn copy_library_developed_thumbnail_cache(
    app: &AndroidApp,
    source_uri: &str,
    destination_uri: &str,
) -> Result<(), String> {
    let source_uri = source_uri.to_owned();
    let destination_uri = destination_uri.to_owned();
    with_storage_manager(app, |env, storage_manager| {
        let source_uri = env.new_string(&source_uri)?;
        let destination_uri = env.new_string(&destination_uri)?;
        env.call_method(
            storage_manager,
            jni::jni_str!("copyRawLibraryDevelopedThumbnail"),
            jni::jni_sig!((JString, JString) -> void),
            &[
                JValue::Object(&source_uri),
                JValue::Object(&destination_uri),
            ],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not preserve Android developed thumbnail: {error:#}"))
}

pub fn clear_thumbnail_cache(app: &AndroidApp) -> Result<(), String> {
    with_storage_manager(app, |env, storage_manager| {
        env.call_method(
            storage_manager,
            jni::jni_str!("clearThumbnailCache"),
            jni::jni_sig!(() -> void),
            &[],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not clear Android thumbnail cache: {error:#}"))
}

pub fn thumbnail_cache_size_bytes(app: &AndroidApp) -> Result<u64, String> {
    let bytes = with_storage_manager(app, |env, storage_manager| {
        env.call_method(
            storage_manager,
            jni::jni_str!("thumbnailCacheSizeBytes"),
            jni::jni_sig!(() -> i64),
            &[],
        )?
        .j()
    })
    .map_err(|error| format!("could not measure Android thumbnail cache: {error:#}"))?;
    u64::try_from(bytes).map_err(|_| "Android returned a negative thumbnail cache size".to_owned())
}

pub fn load_library_display_dimensions(app: &AndroidApp, uri: &str) -> Result<[u32; 2], String> {
    let descriptor = open_library_descriptor(app, uri)?;
    crate::pipeline::load_raw_display_dimensions(&descriptor.proc_path())
        .map_err(|error| format!("{error:#}"))
}

fn load_library_thumbnail_from_fd(
    app: &AndroidApp,
    uri: &str,
    maximum_edge: u32,
) -> Result<crate::pipeline::RawThumbnail, String> {
    let descriptor = open_library_descriptor(app, uri)?;
    crate::pipeline::load_raw_thumbnail(&descriptor.proc_path(), maximum_edge)
        .map_err(|error| format!("{error:#}"))
}

fn open_library_descriptor(
    app: &AndroidApp,
    uri: &str,
) -> Result<TransferredFileDescriptor, String> {
    let uri_string = uri.to_owned();
    let raw_fd = with_storage_manager(app, |env, storage_manager| {
        let uri = env.new_string(&uri_string)?;
        env.call_method(
            storage_manager,
            jni::jni_str!("openRawLibraryFd"),
            jni::jni_sig!((JString) -> i32),
            &[JValue::Object(&uri)],
        )?
        .i()
    })
    .map_err(|error| format!("could not open Android RAW library item: {error:#}"))?;
    TransferredFileDescriptor::from_java(raw_fd, "Android returned an invalid RAW file descriptor")
}

fn raw_thumbnail_cache_path(
    app: &AndroidApp,
    uri: &str,
    bytes: u64,
    modified_seconds: u64,
    maximum_edge: u32,
) -> Result<PathBuf, String> {
    let uri = uri.to_owned();
    let path = with_storage_manager(app, |env, storage_manager| {
        let uri = env.new_string(&uri)?;
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("rawThumbnailCachePath"),
                jni::jni_sig!((JString, i64, i64, i32) -> JString),
                &[
                    JValue::Object(&uri),
                    JValue::Long(bytes as i64),
                    JValue::Long(modified_seconds as i64),
                    JValue::Int(maximum_edge as i32),
                ],
            )?
            .l()?;
        let path = env.cast_local::<JString>(object)?;
        Ok(path.to_string())
    })
    .map_err(|error| format!("could not locate Android thumbnail cache: {error:#}"))?;
    if path.is_empty() {
        Err("Android returned no thumbnail cache path".to_owned())
    } else {
        Ok(PathBuf::from(path))
    }
}

fn developed_thumbnail_cache_path(app: &AndroidApp, uri: &str) -> Result<PathBuf, String> {
    let uri = uri.to_owned();
    let path = with_storage_manager(app, |env, storage_manager| {
        let uri = env.new_string(&uri)?;
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("developedThumbnailCachePath"),
                jni::jni_sig!((JString) -> JString),
                &[JValue::Object(&uri)],
            )?
            .l()?;
        let path = env.cast_local::<JString>(object)?;
        Ok(path.to_string())
    })
    .map_err(|error| format!("could not locate Android developed-thumbnail cache: {error:#}"))?;
    if path.is_empty() {
        Err("Android returned no developed-thumbnail cache path".to_owned())
    } else {
        Ok(PathBuf::from(path))
    }
}

fn developed_thumbnail_fingerprint_path(cache_path: &std::path::Path) -> PathBuf {
    let mut path = cache_path.as_os_str().to_owned();
    path.push(".fingerprint");
    PathBuf::from(path)
}

pub fn load_developed_thumbnail_cache(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
    maximum_edge: u32,
) -> Result<Option<crate::pipeline::RawThumbnail>, String> {
    let cache_path = developed_thumbnail_cache_path(app, raw_uri)?;
    let fingerprint_path = developed_thumbnail_fingerprint_path(&cache_path);
    if !cache_path.is_file() || !fingerprint_path.is_file() {
        return Ok(None);
    }
    let Some(sidecar_path) = materialize_raw_sidecar(app, raw_uri, display_name)? else {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(&fingerprint_path);
        return Ok(None);
    };
    let fingerprint =
        crate::thumbnail_cache::fingerprint_file(&sidecar_path, crate::sidecar::MAX_SIDECAR_BYTES);
    let _ = fs::remove_file(&sidecar_path);
    let fingerprint = fingerprint?;
    let cached = fs::read_to_string(&fingerprint_path).map_err(|error| {
        format!(
            "could not read Android developed-thumbnail fingerprint {}: {error}",
            fingerprint_path.display()
        )
    })?;
    if cached.trim()
        != format!(
            "{:016x}",
            fingerprint ^ crate::sidecar::DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT
        )
    {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(&fingerprint_path);
        return Ok(None);
    }
    crate::thumbnail_cache::load_jpeg(&cache_path, maximum_edge)
}

pub fn save_developed_thumbnail_cache(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
    thumbnail: &crate::pipeline::RawThumbnail,
) -> Result<(), String> {
    let Some(sidecar_path) = materialize_raw_sidecar(app, raw_uri, display_name)? else {
        return Err("edit sidecar disappeared before thumbnail capture".to_owned());
    };
    let fingerprint =
        crate::thumbnail_cache::fingerprint_file(&sidecar_path, crate::sidecar::MAX_SIDECAR_BYTES);
    let _ = fs::remove_file(&sidecar_path);
    let fingerprint = fingerprint?;
    let cache_path = developed_thumbnail_cache_path(app, raw_uri)?;
    let fingerprint_path = developed_thumbnail_fingerprint_path(&cache_path);
    crate::thumbnail_cache::save_jpeg(&cache_path, thumbnail)?;
    crate::thumbnail_cache::write_bytes_atomic(
        &fingerprint_path,
        format!(
            "{:016x}\n",
            fingerprint ^ crate::sidecar::DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT
        )
        .as_bytes(),
    )
    .map_err(|error| {
        format!(
            "could not write Android developed-thumbnail fingerprint {}: {error}",
            fingerprint_path.display()
        )
    })?;

    let Some(sidecar_path) = materialize_raw_sidecar(app, raw_uri, display_name)? else {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(&fingerprint_path);
        return Err("edit sidecar changed while its thumbnail was being cached".to_owned());
    };
    let latest =
        crate::thumbnail_cache::fingerprint_file(&sidecar_path, crate::sidecar::MAX_SIDECAR_BYTES);
    let _ = fs::remove_file(&sidecar_path);
    if latest? != fingerprint {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(&fingerprint_path);
        return Err("edit sidecar changed while its thumbnail was being cached".to_owned());
    }
    Ok(())
}

pub fn materialize_library_document(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
) -> Result<PathBuf, String> {
    let raw_uri = raw_uri.to_owned();
    let display_name = display_name.to_owned();
    let path = with_storage_manager(app, |env, storage_manager| {
        let raw_uri = env.new_string(&raw_uri)?;
        let display_name = env.new_string(&display_name)?;
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("materializeRawLibraryDocument"),
                jni::jni_sig!((JString, JString) -> JString),
                &[JValue::Object(&raw_uri), JValue::Object(&display_name)],
            )?
            .l()?;
        Ok(env.cast_local::<JString>(object)?.to_string())
    })
    .map_err(|error| format!("could not materialize Android RAW: {error:#}"))?;
    if path.is_empty() {
        Err("Android returned no RAW staging path".to_owned())
    } else {
        Ok(PathBuf::from(path))
    }
}

fn materialize_library_thumbnail(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
) -> Result<PathBuf, String> {
    let raw_uri = raw_uri.to_owned();
    let display_name = display_name.to_owned();
    let path = with_storage_manager(app, |env, storage_manager| {
        let raw_uri = env.new_string(&raw_uri)?;
        let display_name = env.new_string(&display_name)?;
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("materializeRawLibraryThumbnail"),
                jni::jni_sig!((JString, JString) -> JString),
                &[JValue::Object(&raw_uri), JValue::Object(&display_name)],
            )?
            .l()?;
        let path = env.cast_local::<JString>(object)?;
        Ok(path.to_string())
    })
    .map_err(|error| format!("could not materialize Android RAW thumbnail: {error:#}"))?;
    if path.is_empty() {
        Err("Android returned no RAW thumbnail staging path".to_owned())
    } else {
        Ok(PathBuf::from(path))
    }
}

pub fn open_library_document(
    app: &AndroidApp,
    uri: &str,
    display_name: &str,
) -> Result<(), String> {
    let uri = uri.to_owned();
    let display_name = display_name.to_owned();
    with_storage_manager(app, |env, storage_manager| {
        let uri = env.new_string(&uri)?;
        let display_name = env.new_string(&display_name)?;
        env.call_method(
            storage_manager,
            jni::jni_str!("openRawLibraryDocument"),
            jni::jni_sig!((JString, JString) -> void),
            &[JValue::Object(&uri), JValue::Object(&display_name)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not open Android RAW library item: {error:#}"))
}

#[derive(Debug)]
pub struct ImportedLibraryDocument {
    pub uri: String,
    pub display_name: String,
}

pub fn import_local_library_document(
    app: &AndroidApp,
    raw_path: &std::path::Path,
    display_name: &str,
) -> Result<ImportedLibraryDocument, String> {
    let raw_path = raw_path
        .to_str()
        .ok_or_else(|| "Local library staging path is not valid UTF-8".to_owned())?
        .to_owned();
    let display_name = display_name.to_owned();
    let identity = with_storage_manager(app, |env, storage_manager| {
        let raw_path = env.new_string(&raw_path)?;
        let display_name = env.new_string(&display_name)?;
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("importLocalRawLibraryDocument"),
                jni::jni_sig!((JString, JString) -> JString),
                &[JValue::Object(&raw_path), JValue::Object(&display_name)],
            )?
            .l()?;
        Ok(env.cast_local::<JString>(object)?.to_string())
    })
    .map_err(|error| format!("could not import local RAW into Android library: {error:#}"))?;
    let (uri, display_name) = identity.split_once('\n').ok_or_else(|| {
        "could not import local RAW into Android library: Android returned an invalid document identity"
            .to_owned()
    })?;
    if uri.is_empty() || display_name.is_empty() {
        return Err(
            "could not import local RAW into Android library: Android returned an empty document identity"
                .to_owned(),
        );
    }
    Ok(ImportedLibraryDocument {
        uri: uri.to_owned(),
        display_name: display_name.to_owned(),
    })
}

pub fn delete_imported_library_document(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
) -> Result<(), String> {
    let raw_uri = raw_uri.to_owned();
    let display_name = display_name.to_owned();
    with_storage_manager(app, |env, storage_manager| {
        let raw_uri = env.new_string(&raw_uri)?;
        let display_name = env.new_string(&display_name)?;
        env.call_method(
            storage_manager,
            jni::jni_str!("deleteImportedRawLibraryDocument"),
            jni::jni_sig!((JString, JString) -> void),
            &[JValue::Object(&raw_uri), JValue::Object(&display_name)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not roll back imported Android RAW: {error:#}"))
}

pub fn rename_library_document(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
    requested_name: &str,
) -> Result<String, String> {
    let raw_uri = raw_uri.to_owned();
    let display_name = display_name.to_owned();
    let requested_name = requested_name.to_owned();
    with_storage_manager(app, |env, storage_manager| {
        let raw_uri = env.new_string(&raw_uri)?;
        let display_name = env.new_string(&display_name)?;
        let requested_name = env.new_string(&requested_name)?;
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("renameRawLibraryDocument"),
                jni::jni_sig!((JString, JString, JString) -> JString),
                &[
                    JValue::Object(&raw_uri),
                    JValue::Object(&display_name),
                    JValue::Object(&requested_name),
                ],
            )?
            .l()?;
        let renamed_uri = env.cast_local::<JString>(object)?;
        Ok(renamed_uri.to_string())
    })
    .map_err(|error| format!("could not rename Android RAW library item: {error:#}"))
}

pub fn delete_library_document(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
) -> Result<(), String> {
    let raw_uri_owned = raw_uri.to_owned();
    let display_name_owned = display_name.to_owned();
    with_storage_manager(app, |env, storage_manager| {
        let raw_uri = env.new_string(&raw_uri_owned)?;
        let display_name = env.new_string(&display_name_owned)?;
        env.call_method(
            storage_manager,
            jni::jni_str!("deleteRawLibraryDocument"),
            jni::jni_sig!((JString, JString) -> void),
            &[JValue::Object(&raw_uri), JValue::Object(&display_name)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not delete Android RAW library item: {error:#}"))?;
    clear_developed_thumbnail_cache(app, raw_uri);
    Ok(())
}

pub fn remove_raw_sidecar(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
) -> Result<(), String> {
    let raw_uri_owned = raw_uri.to_owned();
    let display_name_owned = display_name.to_owned();
    with_storage_manager(app, |env, storage_manager| {
        let raw_uri = env.new_string(&raw_uri_owned)?;
        let display_name = env.new_string(&display_name_owned)?;
        env.call_method(
            storage_manager,
            jni::jni_str!("removeRawSidecar"),
            jni::jni_sig!((JString, JString) -> void),
            &[JValue::Object(&raw_uri), JValue::Object(&display_name)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not reset Android RAW adjustments: {error:#}"))?;

    clear_developed_thumbnail_cache(app, raw_uri);
    Ok(())
}

fn clear_developed_thumbnail_cache(app: &AndroidApp, raw_uri: &str) {
    if let Ok(cache_path) = developed_thumbnail_cache_path(app, raw_uri) {
        let fingerprint_path = developed_thumbnail_fingerprint_path(&cache_path);
        let _ = fs::remove_file(cache_path);
        let _ = fs::remove_file(fingerprint_path);
    }
}

pub fn materialize_raw_sidecar(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
) -> Result<Option<PathBuf>, String> {
    let raw_uri = raw_uri.to_owned();
    let display_name = display_name.to_owned();
    let path = with_storage_manager(app, |env, storage_manager| {
        let raw_uri = env.new_string(&raw_uri)?;
        let display_name = env.new_string(&display_name)?;
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("materializeRawSidecar"),
                jni::jni_sig!((JString, JString) -> JString),
                &[JValue::Object(&raw_uri), JValue::Object(&display_name)],
            )?
            .l()?;
        let path = env.cast_local::<JString>(object)?;
        Ok(path.to_string())
    })
    .map_err(|error| format!("could not read Android RAW sidecar: {error:#}"))?;
    Ok((!path.is_empty()).then(|| PathBuf::from(path)))
}

pub fn create_raw_sidecar_cache(app: &AndroidApp) -> Result<PathBuf, String> {
    let path = with_storage_manager(app, |env, storage_manager| {
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("createRawSidecarCache"),
                jni::jni_sig!(() -> JString),
                &[],
            )?
            .l()?;
        let path = env.cast_local::<JString>(object)?;
        Ok(path.to_string())
    })
    .map_err(|error| format!("could not create Android sidecar cache: {error:#}"))?;
    if path.is_empty() {
        Err("Android returned no sidecar cache path".to_owned())
    } else {
        Ok(PathBuf::from(path))
    }
}

pub fn publish_raw_sidecar(
    app: &AndroidApp,
    cached_path: &std::path::Path,
    raw_uri: &str,
    display_name: &str,
) -> Result<String, String> {
    let cached_path = cached_path
        .to_str()
        .ok_or_else(|| "Android sidecar cache path is not valid UTF-8".to_owned())?
        .to_owned();
    let raw_uri = raw_uri.to_owned();
    let display_name = display_name.to_owned();
    with_storage_manager(app, |env, storage_manager| {
        let cached_path = env.new_string(&cached_path)?;
        let raw_uri = env.new_string(&raw_uri)?;
        let display_name = env.new_string(&display_name)?;
        let object = env
            .call_method(
                storage_manager,
                jni::jni_str!("publishRawSidecar"),
                jni::jni_sig!((JString, JString, JString) -> JString),
                &[
                    JValue::Object(&cached_path),
                    JValue::Object(&raw_uri),
                    JValue::Object(&display_name),
                ],
            )?
            .l()?;
        let location = env.cast_local::<JString>(object)?;
        Ok(location.to_string())
    })
    .map_err(|error| format!("could not publish Android RAW sidecar: {error:#}"))
}

pub fn update_background_task_notification(
    app: &AndroidApp,
    title: &str,
    phase: &str,
    detail: Option<&str>,
    progress_percent: i32,
    indeterminate: bool,
    queued_count: usize,
) -> Result<(), String> {
    let activity_key = app.activity_as_ptr() as usize;
    let payload = TaskNotificationPayload {
        title: title.to_owned(),
        phase: phase.to_owned(),
        detail: detail.unwrap_or_default().to_owned(),
        progress_percent: progress_percent.clamp(0, 100),
        indeterminate,
        queued_count: i32::try_from(queued_count).unwrap_or(i32::MAX),
    };
    if let Ok(current) = TASK_NOTIFICATION_STATE.lock() {
        if let Some(current) = current
            .as_ref()
            .filter(|current| current.activity_key == activity_key)
        {
            if current.payload == payload {
                return Ok(());
            }
            let same_operation = current.payload.title == payload.title
                && current.payload.phase == payload.phase
                && current.payload.indeterminate == payload.indeterminate
                && current.payload.queued_count == payload.queued_count;
            if same_operation && current.posted_at.elapsed() < TASK_NOTIFICATION_MIN_UPDATE_INTERVAL
            {
                return Ok(());
            }
        }
    }

    with_activity(app, |env, activity| {
        let title = env.new_string(&payload.title)?;
        let phase = env.new_string(&payload.phase)?;
        let detail = env.new_string(&payload.detail)?;
        env.call_method(
            activity,
            jni::jni_str!("updateBackgroundTaskNotification"),
            jni::jni_sig!((JString, JString, JString, i32, i32, i32) -> void),
            &[
                JValue::Object(&title),
                JValue::Object(&phase),
                JValue::Object(&detail),
                JValue::Int(payload.progress_percent),
                JValue::Int(if payload.indeterminate { 1 } else { 0 }),
                JValue::Int(payload.queued_count),
            ],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not update Android task notification: {error:#}"))?;

    if let Ok(mut current) = TASK_NOTIFICATION_STATE.lock() {
        *current = Some(TaskNotificationState {
            activity_key,
            payload,
            posted_at: Instant::now(),
        });
    }
    Ok(())
}

pub fn clear_background_task_notification(app: &AndroidApp) -> Result<(), String> {
    let had_notification = TASK_NOTIFICATION_STATE
        .lock()
        .map(|current| current.is_some())
        .unwrap_or(true);
    if !had_notification {
        return Ok(());
    }
    with_activity(app, |env, activity| {
        env.call_method(
            activity,
            jni::jni_str!("clearBackgroundTaskNotification"),
            jni::jni_sig!(() -> void),
            &[],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not clear Android task notification: {error:#}"))?;
    if let Ok(mut current) = TASK_NOTIFICATION_STATE.lock() {
        *current = None;
    }
    Ok(())
}

fn with_activity<T>(
    app: &AndroidApp,
    operation: impl FnOnce(&mut jni::Env<'_>, &JObject) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| {
        let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
        let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
        operation(env, activity.as_ref())
    })
}

fn with_storage_manager<T>(
    app: &AndroidApp,
    operation: impl FnOnce(&mut jni::Env<'_>, &JObject) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    with_activity(app, |env, activity| {
        let storage_manager = env
            .get_field(
                activity,
                jni::jni_str!("storageManager"),
                jni::jni_sig!(de.duecki.auraw.StorageManager),
            )?
            .l()?;
        operation(env, &storage_manager)
    })
}

fn with_profile_importer<T>(
    app: &AndroidApp,
    operation: impl FnOnce(&mut jni::Env<'_>, &JObject) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    with_activity(app, |env, activity| {
        let profile_importer = env
            .get_field(
                activity,
                jni::jni_str!("profileImporter"),
                jni::jni_sig!(de.duecki.auraw.ProfileImporter),
            )?
            .l()?;
        operation(env, &profile_importer)
    })
}

fn with_export_publisher<T>(
    app: &AndroidApp,
    operation: impl FnOnce(&mut jni::Env<'_>, &JObject) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    with_activity(app, |env, activity| {
        let export_publisher = env
            .get_field(
                activity,
                jni::jni_str!("exportPublisher"),
                jni::jni_sig!(de.duecki.auraw.ExportPublisher),
            )?
            .l()?;
        operation(env, &export_publisher)
    })
}

fn decode_uri_component(encoded: &str) -> Result<String, String> {
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err("truncated percent escape in Android library record".to_owned());
        }
        let high = hex_digit(bytes[index + 1])?;
        let low = hex_digit(bytes[index + 2])?;
        decoded.push((high << 4) | low);
        index += 3;
    }
    String::from_utf8(decoded)
        .map_err(|error| format!("Android library record is not valid UTF-8: {error}"))
}

fn hex_digit(value: u8) -> Result<u8, String> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err("invalid percent escape in Android library record".to_owned()),
    }
}

pub fn prepare_direct_export(
    app: &AndroidApp,
    temp_dir: &Path,
    display_name: &str,
    mime_type: &str,
) -> Result<Option<PathBuf>, String> {
    let encoded = with_export_publisher(app, |env, export_publisher| {
        let display_name = env.new_string(display_name)?;
        let mime_type = env.new_string(mime_type)?;
        let object = env
            .call_method(
                export_publisher,
                jni::jni_str!("createPendingExport"),
                jni::jni_sig!((JString, JString) -> JString),
                &[JValue::Object(&display_name), JValue::Object(&mime_type)],
            )?
            .l()?;
        let string = env.cast_local::<JString>(object)?;
        Ok(string.to_string())
    })
    .map_err(|error| {
        format!("could not create Android MediaStore export destination: {error:#}")
    })?;
    if encoded.is_empty() {
        return Ok(None);
    }
    let PendingExportDescriptor {
        descriptor,
        uri,
        location,
    } = PendingExportDescriptor::parse(&encoded)?;
    let path = descriptor.proc_path();
    let target = DirectExportTarget {
        descriptor,
        uri,
        location,
        temp_dir: temp_dir.to_path_buf(),
    };
    direct_exports()
        .lock()
        .map_err(|_| "Android direct-export state is poisoned".to_owned())?
        .insert(path.clone(), target);
    Ok(Some(path))
}

pub fn is_direct_export_path(path: &Path) -> bool {
    direct_exports()
        .lock()
        .ok()
        .is_some_and(|targets| targets.contains_key(path))
}

pub fn direct_export_temp_dir(path: &Path) -> Option<PathBuf> {
    direct_exports()
        .lock()
        .ok()?
        .get(path)
        .map(|target| target.temp_dir.clone())
}

pub fn finalize_direct_export(app: &AndroidApp, path: &Path) -> Result<String, String> {
    let target = direct_exports()
        .lock()
        .map_err(|_| "Android direct-export state is poisoned".to_owned())?
        .remove(path)
        .ok_or_else(|| "Android direct-export destination is no longer available".to_owned())?;
    let DirectExportTarget {
        descriptor,
        uri,
        location,
        ..
    } = target;
    drop(descriptor);
    if let Err(error) = finish_pending_export(app, &uri, true) {
        let _ = finish_pending_export(app, &uri, false);
        return Err(error);
    }
    Ok(location)
}

pub fn cancel_direct_export(app: &AndroidApp, path: &Path) {
    let target = direct_exports()
        .lock()
        .ok()
        .and_then(|mut targets| targets.remove(path));
    if let Some(target) = target {
        let DirectExportTarget {
            descriptor, uri, ..
        } = target;
        drop(descriptor);
        if let Err(error) = finish_pending_export(app, &uri, false) {
            log::warn!("could not delete failed Android direct export: {error}");
        }
    }
}

pub fn cancel_all_direct_exports(app: &AndroidApp) {
    let targets = direct_exports()
        .lock()
        .map(|mut targets| {
            targets
                .drain()
                .map(|(_, target)| target)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for target in targets {
        let DirectExportTarget {
            descriptor, uri, ..
        } = target;
        drop(descriptor);
        if let Err(error) = finish_pending_export(app, &uri, false) {
            log::warn!("could not delete failed Android direct export: {error}");
        }
    }
}

fn finish_pending_export(app: &AndroidApp, uri: &str, success: bool) -> Result<(), String> {
    with_export_publisher(app, |env, export_publisher| {
        let uri = env.new_string(uri)?;
        env.call_method(
            export_publisher,
            jni::jni_str!("finishPendingExport"),
            jni::jni_sig!((JString, i32) -> void),
            &[
                JValue::Object(&uri),
                JValue::Int(if success { 1 } else { 0 }),
            ],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not finalize Android MediaStore export: {error:#}"))
}

pub fn publish_image(
    app: &AndroidApp,
    path: &std::path::Path,
    display_name: &str,
    mime_type: &str,
) -> Result<(), String> {
    let path = path
        .to_str()
        .ok_or_else(|| "Android export cache path is not valid UTF-8".to_owned())?;
    with_export_publisher(app, |env, export_publisher| {
        let path = env.new_string(path)?;
        let display_name = env.new_string(display_name)?;
        let mime_type = env.new_string(mime_type)?;
        env.call_method(
            export_publisher,
            jni::jni_str!("publishImage"),
            jni::jni_sig!((JString, JString, JString) -> void),
            &[
                JValue::Object(&path),
                JValue::Object(&display_name),
                JValue::Object(&mime_type),
            ],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not publish Android image: {error:#}"))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_de_duecki_auraw_AuRawActivity_nativeOnBackRequested<'local>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
) -> jni::sys::jboolean {
    if !BACK_NAVIGATION_ACTIVE.load(Ordering::Acquire) {
        return false;
    }
    BACK_REQUESTED.store(true, Ordering::Release);
    request_repaint();
    true
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_de_duecki_auraw_AuRawActivity_nativeOnSystemInsetsChanged<'local>(
    _unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    left: jni::sys::jint,
    top: jni::sys::jint,
    right: jni::sys::jint,
    bottom: jni::sys::jint,
) {
    SYSTEM_INSET_LEFT_PX.store(left.max(0), Ordering::Release);
    SYSTEM_INSET_TOP_PX.store(top.max(0), Ordering::Release);
    SYSTEM_INSET_RIGHT_PX.store(right.max(0), Ordering::Release);
    SYSTEM_INSET_BOTTOM_PX.store(bottom.max(0), Ordering::Release);
    request_repaint();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_de_duecki_auraw_AuRawActivity_nativeOnFilePicked<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
    display_name: JString<'local>,
    library_uri: JString<'local>,
    error: JString<'local>,
    temporary: jni::sys::jboolean,
) {
    unowned_env
        .with_env(|_env| -> jni::errors::Result<()> {
            let path = path.to_string();
            let display_name = display_name.to_string();
            let library_uri = library_uri.to_string();
            let error = error.to_string();

            let result = if !error.is_empty() {
                PickerResult::Failed(error)
            } else if path.is_empty() {
                PickerResult::Cancelled
            } else {
                PickerResult::Picked(PickedDocument {
                    path: PathBuf::from(path),
                    display_name,
                    library_uri,
                    delete_after_decode: temporary,
                    raw_fd_guard: None,
                })
            };

            if let Ok(mut queue) = results().lock() {
                queue.push_back(result);
            }
            request_repaint();
            Ok(())
        })
        .resolve_with::<LogContextErrorAndDefault, _>(|| {
            "AuRawActivity.nativeOnFilePicked".to_owned()
        });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_de_duecki_auraw_AuRawActivity_nativeOnFilePickedFd<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    fd: jni::sys::jint,
    display_name: JString<'local>,
    library_uri: JString<'local>,
    error: JString<'local>,
) {
    unowned_env
        .with_env(|_env| -> jni::errors::Result<()> {
            let display_name = display_name.to_string();
            let library_uri = library_uri.to_string();
            let error = error.to_string();

            let result = if !error.is_empty() {
                if fd >= 0 {
                    drop(unsafe { File::from_raw_fd(fd) });
                }
                PickerResult::Failed(error)
            } else if fd < 0 {
                PickerResult::Failed("Android returned an invalid RAW file descriptor".to_owned())
            } else {
                let guard = unsafe { File::from_raw_fd(fd) };
                PickerResult::Picked(PickedDocument {
                    path: PathBuf::from(format!("/proc/self/fd/{fd}")),
                    display_name,
                    library_uri,
                    delete_after_decode: false,
                    raw_fd_guard: Some(guard),
                })
            };

            if let Ok(mut queue) = results().lock() {
                queue.push_back(result);
            }
            request_repaint();
            Ok(())
        })
        .resolve_with::<LogContextErrorAndDefault, _>(|| {
            "AuRawActivity.nativeOnFilePickedFd".to_owned()
        });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_de_duecki_auraw_AuRawActivity_nativeOnCameraProfileFolderImportStarted<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    label: JString<'local>,
) {
    unowned_env
        .with_env(|_env| -> jni::errors::Result<()> {
            let label = label.to_string();
            if let Ok(mut queue) = camera_profile_folder_results().lock() {
                queue.push_back(CameraProfileFolderResult::ImportStarted { label });
            }
            request_repaint();
            Ok(())
        })
        .resolve_with::<LogContextErrorAndDefault, _>(|| {
            "AuRawActivity.nativeOnCameraProfileFolderImportStarted".to_owned()
        });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_de_duecki_auraw_AuRawActivity_nativeOnCameraProfileFolderPicked<
    'local,
>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
    label: JString<'local>,
    profile_count: jni::sys::jint,
    error: JString<'local>,
) {
    unowned_env
        .with_env(|_env| -> jni::errors::Result<()> {
            let path = path.to_string();
            let label = label.to_string();
            let error = error.to_string();
            let result = if !error.is_empty() {
                CameraProfileFolderResult::Failed(error)
            } else if path.is_empty() {
                CameraProfileFolderResult::Cancelled
            } else {
                CameraProfileFolderResult::Picked {
                    path: PathBuf::from(path),
                    label,
                    profiles: usize::try_from(profile_count.max(0)).unwrap_or(0),
                }
            };
            if let Ok(mut queue) = camera_profile_folder_results().lock() {
                queue.push_back(result);
            }
            request_repaint();
            Ok(())
        })
        .resolve_with::<LogContextErrorAndDefault, _>(|| {
            "AuRawActivity.nativeOnCameraProfileFolderPicked".to_owned()
        });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_de_duecki_auraw_AuRawActivity_nativeOnImportBatchFinished<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    imported_count: jni::sys::jint,
    failed_count: jni::sys::jint,
    errors: JString<'local>,
) {
    unowned_env
        .with_env(|_env| -> jni::errors::Result<()> {
            let result = PickerResult::BatchImported {
                imported: usize::try_from(imported_count.max(0)).unwrap_or(0),
                failed: usize::try_from(failed_count.max(0)).unwrap_or(0),
                errors: errors.to_string(),
            };
            if let Ok(mut queue) = results().lock() {
                queue.push_back(result);
            }
            request_repaint();
            Ok(())
        })
        .resolve_with::<LogContextErrorAndDefault, _>(|| {
            "AuRawActivity.nativeOnImportBatchFinished".to_owned()
        });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_de_duecki_auraw_AuRawActivity_nativeOnExportPublished<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    location: JString<'local>,
    error: JString<'local>,
) {
    unowned_env
        .with_env(|_env| -> jni::errors::Result<()> {
            let location = location.to_string();
            let error = error.to_string();
            let result = if error.is_empty() {
                ExportPublishResult::Published(location)
            } else {
                ExportPublishResult::Failed(error)
            };
            if let Ok(mut queue) = export_results().lock() {
                queue.push_back(result);
            }
            request_repaint();
            Ok(())
        })
        .resolve_with::<LogContextErrorAndDefault, _>(|| {
            "AuRawActivity.nativeOnExportPublished".to_owned()
        });
}

pub fn load_android(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
) -> Result<Option<crate::sidecar::LoadedSidecar>, crate::sidecar::SidecarError> {
    let Some(path) = materialize_raw_sidecar(app, raw_uri, display_name)
        .map_err(crate::sidecar::SidecarError::Platform)?
    else {
        return Ok(None);
    };
    let result =
        crate::sidecar::read_bounded(&path).and_then(|bytes| crate::sidecar::decode(&bytes));
    if let Err(error) = fs::remove_file(&path) {
        log::warn!(
            "could not remove Android sidecar cache {}: {error}",
            path.display()
        );
    }
    result.map(Some)
}

pub fn save_android(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
    edits: crate::sidecar::EditState,
) -> Result<String, crate::sidecar::SidecarError> {
    let bytes = crate::sidecar::encode(edits)?;
    let path = create_raw_sidecar_cache(app).map_err(crate::sidecar::SidecarError::Platform)?;
    let result = crate::sidecar::write_synced(&path, &bytes).and_then(|()| {
        publish_raw_sidecar(app, &path, raw_uri, display_name)
            .map_err(crate::sidecar::SidecarError::Platform)
    });
    if let Err(error) = fs::remove_file(&path) {
        log::warn!(
            "could not remove Android sidecar cache {}: {error}",
            path.display()
        );
    }
    result
}
