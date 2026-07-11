use android_activity::AndroidApp;
use jni::{
    errors::LogContextErrorAndDefault,
    objects::{JClass, JObject, JString},
    refs::Global,
    EnvUnowned, JavaVM,
};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

#[derive(Debug)]
pub struct PickedDocument {
    pub path: PathBuf,
    pub display_name: String,
}

#[derive(Debug)]
pub enum PickerResult {
    Picked(PickedDocument),
    Cancelled,
    Failed(String),
}

static RESULTS: OnceLock<Mutex<VecDeque<PickerResult>>> = OnceLock::new();
static EGUI_CONTEXT: Mutex<Option<eframe::egui::Context>> = Mutex::new(None);

fn results() -> &'static Mutex<VecDeque<PickerResult>> {
    RESULTS.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub fn install_context(context: &eframe::egui::Context) {
    if let Ok(mut installed) = EGUI_CONTEXT.lock() {
        *installed = Some(context.clone());
    }
}

pub fn take_picker_result() -> Option<PickerResult> {
    results().lock().ok()?.pop_front()
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

#[unsafe(no_mangle)]
pub extern "system" fn Java_de_duecki_auraw_AuRawActivity_nativeOnFilePicked<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
    display_name: JString<'local>,
    error: JString<'local>,
) {
    unowned_env
        .with_env(|_env| -> jni::errors::Result<()> {
            let path = path.to_string();
            let display_name = display_name.to_string();
            let error = error.to_string();

            let result = if !error.is_empty() {
                PickerResult::Failed(error)
            } else if path.is_empty() {
                PickerResult::Cancelled
            } else {
                PickerResult::Picked(PickedDocument {
                    path: PathBuf::from(path),
                    display_name,
                })
            };

            if let Ok(mut queue) = results().lock() {
                queue.push_back(result);
            }
            if let Ok(installed) = EGUI_CONTEXT.lock() {
                if let Some(context) = installed.as_ref() {
                    context.request_repaint();
                }
            }
            Ok(())
        })
        .resolve_with::<LogContextErrorAndDefault, _>(|| {
            "AuRawActivity.nativeOnFilePicked".to_owned()
        });
}
