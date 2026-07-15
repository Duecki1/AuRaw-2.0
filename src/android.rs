use android_activity::AndroidApp;
use jni::{
    errors::LogContextErrorAndDefault,
    objects::{JClass, JObject, JString},
    refs::Global,
    EnvUnowned, JValue, JavaVM,
};
use std::{
    collections::VecDeque,
    fs::File,
    os::fd::FromRawFd,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

#[derive(Debug)]
pub struct PickedDocument {
    pub path: PathBuf,
    pub display_name: String,
    pub library_uri: String,
    pub delete_after_decode: bool,
}

#[derive(Clone, Debug)]
pub struct LibraryDocument {
    pub uri: String,
    pub display_name: String,
    pub display_path: String,
    pub bytes: u64,
}

#[derive(Debug)]
pub enum PickerResult {
    Picked(PickedDocument),
    Cancelled,
    Failed(String),
}

#[derive(Debug)]
pub enum ExportPublishResult {
    Published(String),
    Failed(String),
}

static RESULTS: OnceLock<Mutex<VecDeque<PickerResult>>> = OnceLock::new();
static EXPORT_RESULTS: OnceLock<Mutex<VecDeque<ExportPublishResult>>> = OnceLock::new();
static EGUI_CONTEXT: Mutex<Option<eframe::egui::Context>> = Mutex::new(None);

fn results() -> &'static Mutex<VecDeque<PickerResult>> {
    RESULTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn export_results() -> &'static Mutex<VecDeque<ExportPublishResult>> {
    EXPORT_RESULTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn request_repaint() {
    if let Ok(installed) = EGUI_CONTEXT.lock() {
        if let Some(context) = installed.as_ref() {
            context.request_repaint();
        }
    }
}

pub fn install_context(context: &eframe::egui::Context) {
    if let Ok(mut installed) = EGUI_CONTEXT.lock() {
        *installed = Some(context.clone());
    }
}

pub fn take_picker_result() -> Option<PickerResult> {
    results().lock().ok()?.pop_front()
}

pub fn take_export_publish_result() -> Option<ExportPublishResult> {
    export_results().lock().ok()?.pop_front()
}

pub fn open_raw_document(app: &AndroidApp) -> Result<(), String> {
    // SAFETY: Android owns the JavaVM for the process lifetime; `JavaVM` is a non-owning handle and does not destroy the VM on drop.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
        // SAFETY: `raw_activity` is the live NativeActivity object for this callback; converting it to a JNI global reference extends its lifetime safely.
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

pub fn library_location(app: &AndroidApp) -> Result<String, String> {
    with_activity(app, |env, activity| {
        let object = env
            .call_method(
                activity,
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
    let encoded = with_activity(app, |env, activity| {
        let object = env
            .call_method(
                activity,
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
            let _modified_seconds = fields
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
            })
        })
        .collect()
}

pub fn load_library_thumbnail(
    app: &AndroidApp,
    uri: &str,
    maximum_edge: u32,
) -> Result<crate::pipeline::RawThumbnail, String> {
    let uri_string = uri.to_owned();
    let fd = with_activity(app, |env, activity| {
        let uri = env.new_string(&uri_string)?;
        env.call_method(
            activity,
            jni::jni_str!("openRawLibraryFd"),
            jni::jni_sig!((JString) -> i32),
            &[JValue::Object(&uri)],
        )?
        .i()
    })
    .map_err(|error| format!("could not open Android RAW library item: {error:#}"))?;
    if fd < 0 {
        return Err("Android returned an invalid RAW file descriptor".to_owned());
    }

    // SAFETY: Java detached this descriptor from ParcelFileDescriptor and
    // transferred sole ownership to Rust. `File` closes it exactly once.
    let descriptor = unsafe { File::from_raw_fd(fd) };
    let path = PathBuf::from(format!("/proc/self/fd/{fd}"));
    let result = crate::pipeline::load_raw_thumbnail(&path, maximum_edge)
        .map_err(|error| format!("{error:#}"));
    drop(descriptor);
    result
}

pub fn open_library_document(
    app: &AndroidApp,
    uri: &str,
    display_name: &str,
) -> Result<(), String> {
    let uri = uri.to_owned();
    let display_name = display_name.to_owned();
    with_activity(app, |env, activity| {
        let uri = env.new_string(&uri)?;
        let display_name = env.new_string(&display_name)?;
        env.call_method(
            activity,
            jni::jni_str!("openRawLibraryDocument"),
            jni::jni_sig!((JString, JString) -> void),
            &[JValue::Object(&uri), JValue::Object(&display_name)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not open Android RAW library item: {error:#}"))
}

pub fn materialize_raw_sidecar(
    app: &AndroidApp,
    raw_uri: &str,
    display_name: &str,
) -> Result<Option<PathBuf>, String> {
    let raw_uri = raw_uri.to_owned();
    let display_name = display_name.to_owned();
    let path = with_activity(app, |env, activity| {
        let raw_uri = env.new_string(&raw_uri)?;
        let display_name = env.new_string(&display_name)?;
        let object = env
            .call_method(
                activity,
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
    let path = with_activity(app, |env, activity| {
        let object = env
            .call_method(
                activity,
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
    with_activity(app, |env, activity| {
        let cached_path = env.new_string(&cached_path)?;
        let raw_uri = env.new_string(&raw_uri)?;
        let display_name = env.new_string(&display_name)?;
        let object = env
            .call_method(
                activity,
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

fn with_activity<T>(
    app: &AndroidApp,
    operation: impl FnOnce(&mut jni::Env<'_>, &JObject) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    // SAFETY: Android owns the JavaVM for the process lifetime; `JavaVM` is a
    // non-owning handle and does not destroy the VM on drop.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| {
        let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
        // SAFETY: this is the live NativeActivity object. A global reference
        // keeps it valid for the duration of the attached call.
        let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
        operation(env, activity.as_ref())
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

pub fn publish_png(
    app: &AndroidApp,
    path: &std::path::Path,
    display_name: &str,
) -> Result<(), String> {
    let path = path
        .to_str()
        .ok_or_else(|| "Android export cache path is not valid UTF-8".to_owned())?;
    // SAFETY: Android owns the JavaVM for the process lifetime; `JavaVM` is a non-owning handle and does not destroy the VM on drop.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| -> jni::errors::Result<()> {
        let raw_activity = app.activity_as_ptr() as jni::sys::jobject;
        // SAFETY: `raw_activity` is the live NativeActivity object for this callback; converting it to a JNI global reference extends its lifetime safely.
        let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw_activity)? };
        let path = env.new_string(path)?;
        let display_name = env.new_string(display_name)?;
        env.call_method(
            activity,
            jni::jni_str!("publishPng"),
            jni::jni_sig!((JString, JString) -> void),
            &[JValue::Object(&path), JValue::Object(&display_name)],
        )?;
        Ok(())
    })
    .map_err(|error| format!("could not publish Android PNG: {error:#}"))
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
