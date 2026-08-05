use anyhow::{Context, Result};
use image::{imageops::FilterType, ImageBuffer, Luma, Rgba};
use ort::{session::Session, value::Tensor};
use rayon::prelude::*;
use ring::digest::{Context as Sha256Context, SHA256};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn ensure_ai_not_cancelled(cancellation: &AtomicBool) -> Result<()> {
    anyhow::ensure!(
        !cancellation.load(Ordering::Acquire),
        "background task cancelled"
    );
    Ok(())
}

pub const BIREFNET_LOW_MODEL_BYTES: u64 = 224_005_088;
pub const BIREFNET_LOW_MODEL_URL: &str = "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx";
pub const BIREFNET_LOW_MODEL_SHA256_HEX: &str =
    "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333";
pub const BIREFNET_MEDIUM_MODEL_BYTES: u64 = 331_082_421;
pub const BIREFNET_MEDIUM_MODEL_URL: &str = "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet_lite-general-2K-epoch_232.onnx";
pub const BIREFNET_MEDIUM_MODEL_SHA256_HEX: &str =
    "6003d2f758bdb4e4802a09e39167529bc2eef9288d5b8fa537331467cbc4759d";
pub const BIREFNET_HIGH_MODEL_BYTES: u64 = 1_098_928_953;
pub const BIREFNET_HIGH_MODEL_URL: &str = "https://github.com/ZhengPeng7/BiRefNet/releases/download/v1/BiRefNet_HR-general-epoch_130.onnx";
pub const BIREFNET_HIGH_MODEL_SHA256_HEX: &str =
    "db0217e99b25e0c4f6f4dca2892ff1f7ea7aba38fb6ad84f93122a4024be536a";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BiRefNetQuality {
    Low,
    #[default]
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BiRefNetModelSpec {
    pub(crate) checkpoint: &'static str,
    pub(crate) download_label: &'static str,
    pub(crate) url: &'static str,
    pub(crate) bytes: u64,
    pub(crate) sha256_hex: &'static str,
    pub(crate) cache_filename: &'static str,
    /// ONNX tensors are NCHW. Lite-2K's pinned graph declares H=2560,
    /// W=1440, matching the official checkpoint despite its conventional
    /// "2560 x 1440" resolution label.
    pub(crate) input_width: u32,
    pub(crate) input_height: u32,
    pub(crate) explanation: &'static str,
}

impl BiRefNetQuality {
    pub(crate) const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    pub(crate) const fn model(self) -> BiRefNetModelSpec {
        match self {
            Self::Low => BiRefNetModelSpec {
                checkpoint: "BiRefNet General-Lite",
                download_label: "BiRefNet General-Lite (Low)",
                url: BIREFNET_LOW_MODEL_URL,
                bytes: BIREFNET_LOW_MODEL_BYTES,
                sha256_hex: BIREFNET_LOW_MODEL_SHA256_HEX,
                cache_filename: "birefnet-general-lite.onnx",
                input_width: 1024,
                input_height: 1024,
                explanation: "General-Lite at 1024 x 1024. Fastest and lowest-memory; a 224 MB download.",
            },
            Self::Medium => BiRefNetModelSpec {
                checkpoint: "BiRefNet Lite-2K",
                download_label: "BiRefNet Lite-2K (Medium)",
                url: BIREFNET_MEDIUM_MODEL_URL,
                bytes: BIREFNET_MEDIUM_MODEL_BYTES,
                sha256_hex: BIREFNET_MEDIUM_MODEL_SHA256_HEX,
                cache_filename: "birefnet-lite-2k.onnx",
                input_width: 1440,
                input_height: 2560,
                explanation: "Lite-2K at its native 2560 x 1440 tensor. More boundary detail with a 331 MB download.",
            },
            Self::High => BiRefNetModelSpec {
                checkpoint: "BiRefNet HR",
                download_label: "BiRefNet HR (High)",
                url: BIREFNET_HIGH_MODEL_URL,
                bytes: BIREFNET_HIGH_MODEL_BYTES,
                sha256_hex: BIREFNET_HIGH_MODEL_SHA256_HEX,
                cache_filename: "birefnet-hr.onnx",
                input_width: 2048,
                input_height: 2048,
                explanation: "The dedicated BiRefNet HR checkpoint at 2048 x 2048. Best fine-detail quality; a 1.10 GB download with the highest memory use.",
            },
        }
    }
}
pub const VITMATTE_MODEL_BYTES: u64 = 103_885_865;
pub const VITMATTE_MODEL_URL: &str = "https://huggingface.co/Xenova/vitmatte-small-composition-1k/resolve/5e04250c42d7a03dc125b13adb415a47584ec60b/onnx/model.onnx";
pub const VITMATTE_MODEL_SHA256_HEX: &str =
    "bf28d2e0be2c073286e88d60ad649d7123da2749a2d99133fd1098d5887e0225";
// probabilities are normally sparse; keeping them sparse avoids evaluating all
// 150 classes for every query and pixel while preserving semantic argmaxes.
const VITMATTE_MAX_EDGE_ANDROID: u32 = 1024;
const VITMATTE_SIZE_DIVISOR: u32 = 32;

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

#[cfg(not(target_os = "android"))]
static SESSION: OnceLock<Mutex<Option<(BiRefNetQuality, Session)>>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
static VITMATTE_SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
static RUNTIME_INITIALIZED: OnceLock<()> = OnceLock::new();
static RUNTIME_INIT_LOCK: Mutex<()> = Mutex::new(());
#[cfg(not(target_os = "android"))]
type RuntimeProbeResult = (PathBuf, String);
#[cfg(not(target_os = "android"))]
static RUNTIME_PROBE_CACHE: OnceLock<Mutex<Option<RuntimeProbeResult>>> = OnceLock::new();

", path.display()))?;
    anyhow::ensure!(
        metadata.len()
    );
    let actual = sha256_file_hex(path)?;
    anyhow::ensure!(
    );
    Ok(())
}

#[cfg(test)]

pub struct SubjectMaskWorkerRequest {
    pub quality: BiRefNetQuality,
    pub model_path: PathBuf,
    pub runtime_path: Option<PathBuf>,
    pub runtime_sha256: Option<String>,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn spawn_subject_mask(
    request: SubjectMaskWorkerRequest,
    cancellation: Arc<AtomicBool>,
) -> mpsc::Receiver<SubjectMaskEvent> {
    let SubjectMaskWorkerRequest {
        quality,
        model_path,
        runtime_path,
        runtime_sha256,
        width,
        height,
        rgba,
    } = request;
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let spawn = std::thread::Builder::new()
        .name("auraw-onnx-subject".to_owned())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (|| {
                    ensure_ai_not_cancelled(&cancellation)?;
                    ensure_model(quality, &model_path, &worker_sender, &cancellation)?;
                    ensure_ai_not_cancelled(&cancellation)?;
                    let _ = worker_sender.send(SubjectMaskEvent::Inferencing);
                    infer_subject(
                        &model_path,
                        runtime_path.as_deref(),
                        runtime_sha256.as_deref(),
                        quality,
                        width,
                        height,
                        rgba,
                    )
                })()
            }))
            .unwrap_or_else(|panic| {
                let message = panic
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown ONNX Runtime failure");
                Err(anyhow::anyhow!(
                    "ONNX Runtime terminated inference: {message}"
                ))
            });
            let _ = worker_sender.send(SubjectMaskEvent::Finished(
                result.map_err(|error| format!("{error:#}")),
            ));
        });
    if let Err(error) = spawn {
        let _ = sender.send(SubjectMaskEvent::Finished(Err(format!(
            "could not start BiRefNet worker: {error}"
        ))));
    }
    receiver
}

fn ensure_model(
    quality: BiRefNetQuality,
    path: &Path,
    events: &mpsc::Sender<SubjectMaskEvent>,
    cancellation: &AtomicBool,
) -> Result<()> {
    match verify_model(quality, path) {
        Ok(()) => return Ok(()),
        Err(error) if path.exists() => {
            log::warn!(
                "discarding untrusted BiRefNet cache {}: {error:#}",
                path.display()
            );
            fs::remove_file(path)
                .with_context(|| format!("remove invalid model cache {}", path.display()))?;
        }
        Err(_) => {}
    }
    download_model(quality, path, events, cancellation)?;
    verify_model(quality, path).context("verify published BiRefNet model")
}

fn verify_model(quality: BiRefNetQuality, path: &Path) -> Result<()> {
    let model = quality.model();
    let metadata = fs::metadata(path)
        .with_context(|| format!("read BiRefNet model metadata {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "BiRefNet cache is not a regular file");
    anyhow::ensure!(
        metadata.len() == model.bytes,
        "{} size mismatch: found {}, expected {}",
        model.checkpoint,
        metadata.len(),
        model.bytes
    );
    let digest = sha256_file_hex(path)?;
    anyhow::ensure!(
        digest == model.sha256_hex,
        "{} SHA-256 mismatch (expected {})",
        model.checkpoint,
        model.sha256_hex
    );
    Ok(())
}

fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    sha256_reader(&mut file).with_context(|| format!("hash {}", path.display()))
}

fn sha256_reader(reader: &mut impl Read) -> Result<[u8; 32]> {
    let mut hasher = Sha256Context::new(&SHA256);
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finish();
    digest
        .as_ref()
        .try_into()
        .map_err(|_| anyhow::anyhow!("SHA-256 implementation returned the wrong digest length"))
}

pub(crate) fn sha256_file_hex(path: &Path) -> Result<String> {
    Ok(sha256_file(path)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn download_model(
    quality: BiRefNetQuality,
    path: &Path,
    events: &mpsc::Sender<SubjectMaskEvent>,
    cancellation: &AtomicBool,
) -> Result<()> {
    let model = quality.model();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create model cache {}", parent.display()))?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("onnx.{}.{}.part", std::process::id(), nonce));
    let result = (|| -> Result<()> {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(10 * 60)))
            .build();
        let agent: ureq::Agent = config.into();
        let mut response = agent
            .get(model.url)
            .call()
            .context("download BiRefNet ONNX model")?;
        if let Some(length) = response.body().content_length() {
            anyhow::ensure!(
                length == model.bytes,
                "{} server declared {length} bytes, expected {}",
                model.checkpoint,
                model.bytes
            );
        }
        let mut reader = response.body_mut().as_reader();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        let mut downloaded = 0u64;
        let mut hasher = Sha256Context::new(&SHA256);
        let mut buffer = [0u8; 256 * 1024];
        loop {
            ensure_ai_not_cancelled(cancellation)?;
            let read = reader.read(&mut buffer).context("read BiRefNet download")?;
            if read == 0 {
                break;
            }
            downloaded = downloaded
                .checked_add(read as u64)
                .context("BiRefNet download byte count overflow")?;
            anyhow::ensure!(
                downloaded <= model.bytes,
                "{} download exceeded its pinned {}-byte size",
                model.checkpoint,
                model.bytes
            );
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .context("write BiRefNet ONNX model")?;
            let _ = events.send(SubjectMaskEvent::DownloadProgress {
                label: model.download_label,
                downloaded,
                total: model.bytes,
            });
        }
        file.sync_all().context("flush BiRefNet ONNX model")?;
        anyhow::ensure!(
            downloaded == model.bytes,
            "{} size mismatch: received {downloaded}, expected {}",
            model.checkpoint,
            model.bytes
        );
        let digest = hasher
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        anyhow::ensure!(
            digest == model.sha256_hex,
            "{} SHA-256 mismatch (expected {})",
            model.checkpoint,
            model.sha256_hex
        );
        ensure_ai_not_cancelled(cancellation)?;
        fs::rename(&temporary, path)
            .with_context(|| format!("publish BiRefNet model to {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(target_os = "android"))]
pub(crate) fn probe_runtime_subprocess(runtime_path: &Path, expected_sha256: &str) -> Result<()> {
    let runtime_path = fs::canonicalize(runtime_path)
        .with_context(|| format!("resolve selected ONNX Runtime {}", runtime_path.display()))?;
    let cache = RUNTIME_PROBE_CACHE.get_or_init(|| Mutex::new(None));
    {
        let cached = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("ONNX Runtime probe cache lock was poisoned"))?;
        if let Some((cached_path, cached_sha256)) = cached.as_ref() {
            if cached_path == &runtime_path && cached_sha256 == expected_sha256 {
                return Ok(());
            }
        }
    }

    let executable =
        std::env::current_exe().context("locate AuRaw executable for ONNX Runtime probe")?;
    let status = std::process::Command::new(&executable)
        .arg("--auraw-onnx-runtime-probe")
        .arg(&runtime_path)
        .arg(expected_sha256)
        .status()
        .with_context(|| {
            format!(
                "start isolated ONNX Runtime probe with {}",
                executable.display()
            )
        })?;
    if !status.success() {
        return Err(anyhow::anyhow!(
            "the selected ONNX Runtime failed AuRaw's isolated compatibility probe ({status}). Use a matching native {} ONNX Runtime DLL; on Windows the standard CPU package is the safest choice",
            std::env::consts::ARCH
        ));
    }
    let mut cached = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("ONNX Runtime probe cache lock was poisoned"))?;
    *cached = Some((runtime_path, expected_sha256.to_owned()));
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub(crate) fn run_runtime_probe_process(runtime_path: &Path, expected_sha256: &str) -> Result<()> {
    initialize_runtime(Some(runtime_path), Some(expected_sha256))
}

#[cfg(not(target_os = "android"))]
pub(crate) fn initialize_runtime(
    runtime_path: Option<&Path>,
    expected_sha256: Option<&str>,
) -> Result<()> {
    let selected = runtime_path
        .context("no ONNX Runtime library is selected; choose one in Settings and try again")?;
    let expected_sha256 = expected_sha256
        .context("the selected ONNX Runtime has no pinned SHA-256; select it again in Settings")?;
    anyhow::ensure!(
        expected_sha256.len() == 64
            && expected_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "the selected ONNX Runtime SHA-256 pin is invalid"
    );
    let runtime_path = fs::canonicalize(selected)
        .with_context(|| format!("resolve selected ONNX Runtime {}", selected.display()))?;
    let metadata = fs::metadata(&runtime_path)
        .with_context(|| format!("inspect selected ONNX Runtime {}", runtime_path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "selected ONNX Runtime is not a regular file"
    );
    anyhow::ensure!(
        (1_000_000..=1_000_000_000).contains(&metadata.len()),
        "selected ONNX Runtime has an implausible size of {} bytes",
        metadata.len()
    );
    let (runtime_load_path, _verified_runtime_handle, actual_sha256) =
        verified_runtime_load_path(&runtime_path)
            .context("verify selected ONNX Runtime before loading")?;
    anyhow::ensure!(
        actual_sha256 == expected_sha256,
        "selected ONNX Runtime changed after approval: expected SHA-256 {expected_sha256}, found {actual_sha256}; select it again only if you trust the replacement"
    );
    if let Some((loaded_path, loaded_sha256)) = DESKTOP_RUNTIME_IDENTITY.get() {
        anyhow::ensure!(
            loaded_path == &runtime_path && loaded_sha256 == &actual_sha256,
            "a different ONNX Runtime is already active in this process; restart AuRaw before changing the pinned runtime"
        );
    }
    let _init_guard = RUNTIME_INIT_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("ONNX Runtime initialization lock was poisoned"))?;
    if RUNTIME_INITIALIZED.get().is_none() {
        let builder = ort::init_from(&runtime_load_path).map_err(|error| {
            anyhow::anyhow!(
                "could not load ONNX Runtime from {}: {error}",
                runtime_path.display()
            )
        })?;
        anyhow::ensure!(
            builder.with_name("AuRaw").commit(),
            "ONNX Runtime was already initialized before the selected pinned library could be committed"
        );
        DESKTOP_RUNTIME_IDENTITY
            .set((runtime_path.clone(), actual_sha256.clone()))
            .map_err(|_| anyhow::anyhow!("ONNX Runtime identity was initialized concurrently"))?;
        RUNTIME_INITIALIZED
            .set(())
            .map_err(|_| anyhow::anyhow!("ONNX Runtime state was initialized concurrently"))?;
    }
    let (loaded_path, loaded_sha256) = DESKTOP_RUNTIME_IDENTITY
        .get()
        .context("ONNX Runtime initialized without a pinned desktop identity")?;
    anyhow::ensure!(
        loaded_path == &runtime_path && loaded_sha256 == &actual_sha256,
        "a different ONNX Runtime is already active in this process; restart AuRaw before changing the pinned runtime"
    );
    Ok(())
}

/// Returns the verified runtime path and its SHA-256.
///
/// Do not load ONNX Runtime through `/proc/self/fd/<n>` on Linux. ONNX Runtime
/// discovers dynamically-loaded execution-provider libraries relative to the
/// path of `libonnxruntime.so`. A memfd path therefore makes it look for
/// siblings such as `libonnxruntime_providers_shared.so` under `/proc/self/fd/`,
/// which can never work. Load the canonical on-disk library instead so provider
/// discovery stays anchored to the directory the user selected.
#[cfg(target_os = "linux")]
fn verified_runtime_load_path(path: &Path) -> Result<(PathBuf, Option<File>, String)> {
    let actual_sha256 = sha256_file_hex(path).context("verify selected ONNX Runtime SHA-256")?;
    Ok((path.to_path_buf(), None, actual_sha256))
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn verified_runtime_load_path(path: &Path) -> Result<(PathBuf, Option<File>, String)> {
    let digest = sha256_file_hex(path).context("verify selected ONNX Runtime SHA-256")?;
    Ok((path.to_path_buf(), None, digest))
}

#[cfg(target_os = "android")]
pub(crate) fn initialize_runtime(
    _runtime_path: Option<&Path>,
    _expected_sha256: Option<&str>,
) -> Result<()> {
    if RUNTIME_INITIALIZED.get().is_some() {
        return Ok(());
    }
    let _init_guard = RUNTIME_INIT_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("ONNX Runtime initialization lock was poisoned"))?;
    if RUNTIME_INITIALIZED.get().is_none() {
        anyhow::ensure!(
            ort::init().with_name("AuRaw").commit(),
            "ONNX Runtime was already initialized before AuRaw could configure it"
        );
        RUNTIME_INITIALIZED
            .set(())
            .map_err(|_| anyhow::anyhow!("ONNX Runtime state was initialized concurrently"))?;
    }
    Ok(())
}

#[cfg(target_os = "android")]
pub(crate) fn create_session(model_path: &Path) -> Result<Session> {
    let xnnpack_result = (|| -> Result<Session> {
        let mut builder = Session::builder()
            .context("create XNNPACK ONNX Runtime session")?
            .with_memory_pattern(false)
            .map_err(|error| anyhow::anyhow!("disable Android memory pattern: {error}"))?
            .with_execution_providers([
                ort::ep::XNNPACK::default().build(),
                ort::ep::CPU::default().with_arena_allocator(false).build(),
            ])
            .map_err(|error| anyhow::anyhow!("configure Android XNNPACK: {error}"))?;
        builder
            .commit_from_file(model_path)
            .context("compile ONNX model for Android XNNPACK")
    })();
    let xnnpack_error = match xnnpack_result {
        Ok(session) => return Ok(session),
        Err(error) => {
            log::warn!("XNNPACK could not compile the ONNX model; trying CPU: {error:#}");
            format!("{error:#}")
        }
    };

    let mut builder = Session::builder()
        .context("create CPU ONNX Runtime session")?
        .with_memory_pattern(false)
        .map_err(|error| anyhow::anyhow!("disable Android memory pattern: {error}"))?
        .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
        .map_err(|error| anyhow::anyhow!("configure Android CPU fallback: {error}"))?;
    builder.commit_from_file(model_path).with_context(|| {
        format!("load ONNX model with Android CPU fallback (XNNPACK failed: {xnnpack_error})")
    })
}

#[cfg(not(target_os = "android"))]
pub(crate) fn create_cpu_session(model_path: &Path) -> Result<Session> {
    let mut builder = Session::builder()
        .context("create CPU ONNX Runtime session")?
        .with_memory_pattern(false)
        .map_err(|error| anyhow::anyhow!("disable desktop ONNX memory pattern: {error}"))?
        .with_execution_providers([ort::ep::CPU::default().build()])
        .map_err(|error| anyhow::anyhow!("configure ONNX CPU execution provider: {error}"))?;
    builder
        .commit_from_file(model_path)
        .with_context(|| format!("load ONNX model on CPU from {}", model_path.display()))
}

/// SAM 2.1's Hiera encoder is unusually sensitive to CPU graph/layout fusions in
/// some Windows ONNX Runtime builds. A runtime can load successfully and run
/// simpler models while still producing NaN/Inf feature maps for this encoder.
/// Keep this one session deliberately conservative so object selection remains
/// deterministic across user-selected Windows runtimes.
#[cfg(windows)]
fn create_windows_sam_encoder_session(model_path: &Path) -> Result<Session> {
    use ort::session::builder::GraphOptimizationLevel;

    let mut builder = Session::builder()
        .context("create conservative Windows SAM encoder session")?
        .with_memory_pattern(false)
        .map_err(|error| anyhow::anyhow!("disable Windows SAM memory pattern: {error}"))?
        .with_parallel_execution(false)
        .map_err(|error| anyhow::anyhow!("force sequential Windows SAM execution: {error}"))?
        .with_intra_threads(1)
        .map_err(|error| {
            anyhow::anyhow!("limit Windows SAM encoder to one inference thread: {error}")
        })?
        .with_optimization_level(GraphOptimizationLevel::Disable)
        .map_err(|error| anyhow::anyhow!("disable Windows SAM graph optimizations: {error}"))?
        .with_execution_providers([ort::ep::CPU::default().with_arena_allocator(false).build()])
        .map_err(|error| {
            anyhow::anyhow!("configure conservative Windows SAM CPU provider: {error}")
        })?;
    builder.commit_from_file(model_path).with_context(|| {
        format!(
            "load SAM 2.1 encoder with conservative Windows CPU settings from {}",
            model_path.display()
        )
    })
}

#[cfg(target_os = "linux")]
fn running_from_appimage() -> bool {
    std::env::var_os("APPIMAGE").is_some() || std::env::var_os("APPDIR").is_some()
}

#[cfg(not(target_os = "android"))]
fn cache_object_ai_sessions() -> bool {
    #[cfg(target_os = "linux")]
    {
        // AppImages frequently use a user-selected external ONNX Runtime. Keep
        // the object-mask pipeline conservative there: do not retain the SAM
        // encoder/decoder/ViTMatte sessions simultaneously between runs. This
        // reduces peak resident memory and avoids stale provider state.
        !running_from_appimage()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

#[cfg(target_os = "linux")]
fn create_accelerated_session(model_path: &Path) -> Result<Option<Session>> {
    if running_from_appimage() {
        log::info!(
            "Running from AppImage; using the CPU ONNX provider for stable AI-mask inference"
        );
        return Ok(None);
    }
    // Only register provider libraries that actually ship beside the selected
    // libonnxruntime. The ort crate/ONNX Runtime will fall back unsupported graph
    // nodes to CPU automatically inside a successfully-created session.
    let runtime_dir = DESKTOP_RUNTIME_IDENTITY
        .get()
        .and_then(|(runtime_path, _)| runtime_path.parent());
    let has_provider = |filename: &str| {
        runtime_dir
            .map(|directory| directory.join(filename).is_file())
            .unwrap_or(false)
    };
    let mut providers = Vec::new();
    if has_provider("libonnxruntime_providers_cuda.so") {
        providers.push(ort::ep::CUDA::default().build());
    }
    if has_provider("libonnxruntime_providers_openvino.so") {
        providers.push(ort::ep::OpenVINO::default().build());
    }
    if has_provider("libonnxruntime_providers_rocm.so") {
        providers.push(ort::ep::ROCm::default().build());
    }
    // Do not auto-register TensorRT simply because its provider .so exists. It
    // also requires an external matching TensorRT installation; CUDA is the
    // safer general-purpose NVIDIA accelerator and CPU remains the final fallback.
    if providers.is_empty() {
        return Ok(None);
    }

    let mut builder = Session::builder()
        .context("create accelerated ONNX Runtime session")?
        .with_execution_providers(providers)
        .map_err(|error| anyhow::anyhow!("configure Linux ONNX execution providers: {error}"))?;
    builder
        .commit_from_file(model_path)
        .map(Some)
        .with_context(|| {
            format!(
                "load ONNX model with Linux acceleration from {}",
                model_path.display()
            )
        })
}

#[cfg(target_os = "windows")]
fn create_accelerated_session(_model_path: &Path) -> Result<Option<Session>> {
    // A user-selected `onnxruntime.dll` may come from the CPU, CUDA, TensorRT,
    // or DirectML distribution. Calling provider factory APIs that are absent
    // from that exact build can cross a native ABI boundary before Rust gets a
    // recoverable error. The CPU provider is guaranteed by the core runtime,
    // so Windows uses it unless AuRaw later grows an explicit, validated
    // provider-package selection UI.
    Ok(None)
}

#[cfg(target_os = "macos")]
fn create_accelerated_session(model_path: &Path) -> Result<Option<Session>> {
    let mut builder = Session::builder()
        .context("create accelerated ONNX Runtime session")?
        .with_execution_providers([ort::ep::CoreML::default().build()])
        .map_err(|error| anyhow::anyhow!("configure macOS ONNX execution provider: {error}"))?;
    builder
        .commit_from_file(model_path)
        .map(Some)
        .with_context(|| format!("load ONNX model with CoreML from {}", model_path.display()))
}

#[cfg(all(
    not(target_os = "android"),
    not(any(target_os = "linux", target_os = "windows", target_os = "macos"))
))]
fn create_accelerated_session(_model_path: &Path) -> Result<Option<Session>> {
    Ok(None)
}

#[cfg(not(target_os = "android"))]
pub(crate) fn create_session(model_path: &Path) -> Result<Session> {
    match create_accelerated_session(model_path) {
        Ok(Some(session)) => Ok(session),
        Ok(None) => {
            log::info!(
                "No usable accelerated ONNX execution provider was found; using CPU for {}",
                model_path.display()
            );
            create_cpu_session(model_path)
        }
        Err(accelerated_error) => {
            log::warn!(
                "Accelerated ONNX session failed; retrying on CPU for {}: {accelerated_error:#}",
                model_path.display()
            );
            create_cpu_session(model_path).with_context(|| {
                format!(
                    "CPU fallback also failed after accelerated ONNX session error: {accelerated_error:#}"
                )
            })
        }
    }
}

fn infer_subject(
    model_path: &Path,
    runtime_path: Option<&Path>,
    runtime_sha256: Option<&str>,
    quality: BiRefNetQuality,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<SubjectMaskResult> {
    const MAX_SUBJECT_MASK_PIXELS: u64 = 17_000_000;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("subject-mask input dimensions overflow")?;
    anyhow::ensure!(
        pixels > 0 && pixels <= MAX_SUBJECT_MASK_PIXELS,
        "subject-mask input {width}x{height} exceeds the {MAX_SUBJECT_MASK_PIXELS}-pixel limit"
    );
    let expected_bytes = pixels
        .checked_mul(4)
        .and_then(|value| usize::try_from(value).ok())
        .context("subject-mask input byte count overflow")?;
    anyhow::ensure!(
        rgba.len() == expected_bytes,
        "subject-mask RGBA buffer has {} bytes, expected {expected_bytes}",
        rgba.len()
    );
    initialize_runtime(runtime_path, runtime_sha256)?;
    let model = quality.model();
    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, rgba)
        .context("invalid preview image for BiRefNet")?;
    // BiRefNet's official inference preprocessing resizes directly to the
    // checkpoint's native tensor shape. Letterboxing changes the image scale
    // and introduces out-of-distribution black borders; it is particularly
    // destructive for the portrait-shaped Lite-2K graph.
    let resized = image::imageops::resize(
        &image,
        model.input_width,
        model.input_height,
        FilterType::Lanczos3,
    );
    let input = normalized_birefnet_input(&resized, model.input_width, model.input_height)?;
    let input = Tensor::from_array((
        [
            1usize,
            3,
            model.input_height as usize,
            model.input_width as usize,
        ],
        input,
    ))
    .context("create BiRefNet input tensor")?;

    #[cfg(target_os = "android")]
    let (output_width, output_height, logits) = {
        // Mobile memory is more important than avoiding session startup. Drop
        // all model weights and allocator state immediately after inference.
        let mut session = create_session(model_path)?;
        run_subject_session(&mut session, input, model.input_width, model.input_height)?
    };

    #[cfg(not(target_os = "android"))]
    let (output_width, output_height, logits) = {
        let sessions = SESSION.get_or_init(|| Mutex::new(None));
        let mut session = sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("BiRefNet session lock was poisoned"))?;
        if session
            .as_ref()
            .is_none_or(|(cached_quality, _)| *cached_quality != quality)
        {
            *session = Some((quality, create_session(model_path)?));
        }
        let (_, session) = session.as_mut().ok_or_else(|| {
            anyhow::anyhow!("BiRefNet session initialization produced no session")
        })?;
        run_subject_session(session, input, model.input_width, model.input_height)?
    };

    // BiRefNet is itself a high-resolution dichotomous segmentation model and
    // its sigmoid output contains calibrated fractional coverage. Preserve it
    // directly: forcing an independently trained composition matting model to
    // replace every uncertain pixel caused semantic edge drift around hair,
    // straw, fur, and similarly complex subject boundaries.
    let mask = restore_birefnet_output(&logits, output_width, output_height, width, height)?;
    Ok(SubjectMaskResult {
        width,
        height,
        mask,
    })
}

fn run_subject_session(
    session: &mut Session,
    input: Tensor<f32>,
    input_width: u32,
    input_height: u32,
) -> Result<(u32, u32, Vec<f32>)> {
    let outputs = session
        .run(ort::inputs![input])
        .context("run BiRefNet ONNX inference")?;
    let output = outputs
        .values()
        .next()
        .context("BiRefNet returned no output tensors")?;
    let (shape, logits) = output
        .try_extract_tensor::<f32>()
        .context("read BiRefNet output tensor")?;
    let (output_width, output_height, output_elements) =
        validate_birefnet_output_shape(shape, logits.len(), input_width, input_height)?;
    anyhow::ensure!(
        logits.iter().all(|value| value.is_finite()),
        "BiRefNet output contains non-finite logits"
    );
    let mut owned_logits = Vec::new();
    owned_logits
        .try_reserve_exact(output_elements)
        .context("reserve BiRefNet output logits")?;
    owned_logits.extend_from_slice(logits);
    Ok((output_width, output_height, owned_logits))
}

fn validate_birefnet_output_shape(
    shape: &[i64],
    logits_len: usize,
    input_width: u32,
    input_height: u32,
) -> Result<(u32, u32, usize)> {
    anyhow::ensure!(
        shape.len() == 4 && shape[0] == 1 && shape[1] == 1,
        "unexpected BiRefNet output shape {shape:?}; expected [1, 1, H, W]"
    );
    let output_height =
        usize::try_from(shape[2]).context("BiRefNet output height is negative or too large")?;
    let output_width =
        usize::try_from(shape[3]).context("BiRefNet output width is negative or too large")?;
    anyhow::ensure!(
        output_width > 0 && output_height > 0,
        "BiRefNet output dimensions must be positive: {shape:?}"
    );
    let output_elements = output_width
        .checked_mul(output_height)
        .context("BiRefNet output dimensions overflow")?;
    anyhow::ensure!(
        output_elements <= input_width as usize * input_height as usize * 4,
        "BiRefNet output is implausibly large: {shape:?}"
    );
    anyhow::ensure!(
        logits_len == output_elements,
        "BiRefNet output shape {shape:?} describes {output_elements} values, but the tensor contains {logits_len}"
    );
    Ok((
        u32::try_from(output_width).context("BiRefNet output width exceeds u32")?,
        u32::try_from(output_height).context("BiRefNet output height exceeds u32")?,
        output_elements,
    ))
}
    );
    let queries =
    anyhow::ensure!(
    );
    let expected = queries
    anyhow::ensure!(
        logits_len == expected,
    );
    Ok(queries)
}

fn normalized_birefnet_input(
    resized: &ImageBuffer<Rgba<u8>, Vec<u8>>,
    input_width: u32,
    input_height: u32,
) -> Result<Vec<f32>> {
    anyhow::ensure!(
        resized.dimensions() == (input_width, input_height),
        "BiRefNet resized input does not match the model tensor dimensions"
    );
    let plane = usize::try_from(input_width)
        .ok()
        .and_then(|width| {
            usize::try_from(input_height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .context("BiRefNet input dimensions overflow")?;
    let values = plane
        .checked_mul(3)
        .context("BiRefNet input size overflow")?;
    let mut input = Vec::new();
    input
        .try_reserve_exact(values)
        .context("reserve BiRefNet input tensor")?;
    input.resize(values, 0.0);
    for y in 0..input_height {
        for x in 0..input_width {
            let pixel = resized.get_pixel(x, y);
            let destination = (y * input_width + x) as usize;
            for channel in 0..3 {
                let value = pixel[channel] as f32 / 255.0;
                input[channel * plane + destination] =
                    (value - IMAGENET_MEAN[channel]) / IMAGENET_STD[channel];
            }
        }
    }
    Ok(input)
}

fn restore_birefnet_output(
    logits: &[f32],
    output_width: u32,
    output_height: u32,
    target_width: u32,
    target_height: u32,
) -> Result<Vec<u8>> {
    let output_elements = usize::try_from(output_width)
        .ok()
        .and_then(|width| {
            usize::try_from(output_height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .context("BiRefNet output dimensions overflow")?;
    anyhow::ensure!(
        logits.len() == output_elements,
        "BiRefNet logits do not match the declared output dimensions"
    );
    let mut probabilities = Vec::new();
    probabilities
        .try_reserve_exact(output_elements)
        .context("reserve BiRefNet probability map")?;
    for &logit in logits {
        anyhow::ensure!(
            logit.is_finite(),
            "BiRefNet output contains a non-finite logit"
        );
        probabilities.push(sigmoid_probability(logit));
    }
    let output =
        ImageBuffer::<Luma<f32>, Vec<f32>>::from_raw(output_width, output_height, probabilities)
            .context("invalid BiRefNet output buffer")?;
    let resized =
        image::imageops::resize(&output, target_width, target_height, FilterType::Lanczos3);
    Ok(resized
        .into_raw()
        .into_iter()
        .map(|probability| (probability.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect())
}

fn ensure_vitmatte_model<F>(path: &Path, cancellation: &AtomicBool, mut progress: F) -> Result<()>
where
    F: FnMut(u64, u64),
{
    match verify_vitmatte_model(path) {
        Ok(()) => return Ok(()),
        Err(error) if path.exists() => {
            log::warn!(
                "discarding invalid ViTMatte cache {}: {error:#}",
                path.display()
            );
            fs::remove_file(path)
                .with_context(|| format!("remove invalid ViTMatte model {}", path.display()))?;
        }
        Err(_) => {}
    }
    download_vitmatte_model(path, &mut progress, cancellation)?;
    verify_vitmatte_model(path).context("verify published ViTMatte ONNX model")
}

fn verify_vitmatte_model(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("read ViTMatte model metadata {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "ViTMatte cache is not a regular file");
    anyhow::ensure!(
        metadata.len() == VITMATTE_MODEL_BYTES,
        "ViTMatte model size mismatch: found {}, expected {VITMATTE_MODEL_BYTES}",
        metadata.len()
    );
    let actual = sha256_file_hex(path)?;
    anyhow::ensure!(
        actual == VITMATTE_MODEL_SHA256_HEX,
        "ViTMatte model SHA-256 mismatch (expected {VITMATTE_MODEL_SHA256_HEX})"
    );
    Ok(())
}

fn download_vitmatte_model<F>(
    path: &Path,
    progress: &mut F,
    cancellation: &AtomicBool,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create model cache {}", parent.display()))?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("onnx.{}.{}.part", std::process::id(), nonce));
    const MAX_ATTEMPTS: usize = 5;

    let result = (|| -> Result<()> {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_connect(Some(Duration::from_secs(45)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
            .build();
        let agent: ureq::Agent = config.into();
        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 0..MAX_ATTEMPTS {
            ensure_ai_not_cancelled(cancellation)?;
            let mut downloaded = fs::metadata(&temporary)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if downloaded > VITMATTE_MODEL_BYTES {
                fs::remove_file(&temporary).context("remove oversized partial ViTMatte model")?;
                downloaded = 0;
            }
            if downloaded == VITMATTE_MODEL_BYTES
                && sha256_file_hex(&temporary).ok().as_deref() == Some(VITMATTE_MODEL_SHA256_HEX)
            {
                ensure_ai_not_cancelled(cancellation)?;
                fs::rename(&temporary, path).with_context(|| {
                    format!("publish resumed ViTMatte model to {}", path.display())
                })?;
                return Ok(());
            }
            if downloaded > 0 {
                progress(downloaded, VITMATTE_MODEL_BYTES);
            }

            let response_result = if downloaded > 0 {
                let range = format!("bytes={downloaded}-");
                agent
                    .get(VITMATTE_MODEL_URL)
                    .header("Range", range.as_str())
                    .call()
            } else {
                agent.get(VITMATTE_MODEL_URL).call()
            };
            let mut response = match response_result {
                Ok(response) => response,
                Err(error) => {
                    last_error = Some(anyhow::Error::new(error).context(format!(
                        "download ViTMatte ONNX model (attempt {}/{MAX_ATTEMPTS})",
                        attempt + 1
                    )));
                    if attempt + 1 < MAX_ATTEMPTS {
                        std::thread::sleep(Duration::from_secs(1u64 << attempt.min(3)));
                        continue;
                    }
                    break;
                }
            };

            let resuming = downloaded > 0 && response.status().as_u16() == 206;
            if downloaded > 0 && !resuming {
                downloaded = 0;
            }
            if let Some(length) = response.body().content_length() {
                let declared_total = if resuming {
                    downloaded
                        .checked_add(length)
                        .context("ViTMatte response length overflow")?
                } else {
                    length
                };
                anyhow::ensure!(
                    declared_total == VITMATTE_MODEL_BYTES,
                    "ViTMatte server declared {declared_total} total bytes, expected {VITMATTE_MODEL_BYTES}"
                );
            }

            let mut options = OpenOptions::new();
            options.write(true).create(true);
            if resuming {
                options.append(true);
            } else {
                options.truncate(true);
            }
            let mut file = options
                .open(&temporary)
                .with_context(|| format!("open partial ViTMatte model {}", temporary.display()))?;
            let mut reader = response.body_mut().as_reader();
            let mut buffer = [0u8; 256 * 1024];
            let mut transfer_error: Option<anyhow::Error> = None;

            loop {
                ensure_ai_not_cancelled(cancellation)?;
                let read = match reader.read(&mut buffer) {
                    Ok(read) => read,
                    Err(error) => {
                        let _ = file.sync_data();
                        transfer_error = Some(anyhow::Error::new(error).context(format!(
                            "read ViTMatte download (attempt {}/{MAX_ATTEMPTS})",
                            attempt + 1
                        )));
                        break;
                    }
                };
                if read == 0 {
                    break;
                }
                downloaded = downloaded
                    .checked_add(read as u64)
                    .context("ViTMatte download byte count overflow")?;
                anyhow::ensure!(
                    downloaded <= VITMATTE_MODEL_BYTES,
                    "ViTMatte download exceeded its pinned {VITMATTE_MODEL_BYTES}-byte size"
                );
                file.write_all(&buffer[..read])
                    .context("write ViTMatte ONNX model")?;
                progress(downloaded, VITMATTE_MODEL_BYTES);
            }

            if let Some(error) = transfer_error {
                last_error = Some(error);
                if attempt + 1 < MAX_ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(1u64 << attempt.min(3)));
                    continue;
                }
                break;
            }

            file.sync_all().context("flush ViTMatte ONNX model")?;
            if downloaded < VITMATTE_MODEL_BYTES {
                last_error = Some(anyhow::anyhow!(
                    "ViTMatte download ended early at {downloaded} / {VITMATTE_MODEL_BYTES} bytes"
                ));
                if attempt + 1 < MAX_ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(1u64 << attempt.min(3)));
                    continue;
                }
                break;
            }

            let actual = sha256_file_hex(&temporary).context("hash ViTMatte ONNX model")?;
            if actual == VITMATTE_MODEL_SHA256_HEX {
                ensure_ai_not_cancelled(cancellation)?;
                fs::rename(&temporary, path)
                    .with_context(|| format!("publish ViTMatte model to {}", path.display()))?;
                return Ok(());
            }

            // The full byte count with a wrong digest is not a resumable
            // prefix. Discard it and retry cleanly; the pinned SHA-256 remains
            // the final trust boundary.
            fs::remove_file(&temporary).context("remove corrupt ViTMatte partial")?;
            last_error = Some(anyhow::anyhow!(
                "ViTMatte model SHA-256 mismatch (expected {VITMATTE_MODEL_SHA256_HEX})"
            ));
            if attempt + 1 < MAX_ATTEMPTS {
                std::thread::sleep(Duration::from_secs(1u64 << attempt.min(3)));
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("download ViTMatte ONNX model failed")))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Clone, Copy, Debug)]
struct MatteCrop {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn matte_crop_for_mask(mask: &[u8], width: u32, height: u32) -> Option<MatteCrop> {
    let width_usize = width as usize;
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut found = false;
    for (index, &value) in mask.iter().enumerate() {
        if value <= 4 {
            continue;
        }
        let x = (index % width_usize) as u32;
        let y = (index / width_usize) as u32;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
        found = true;
    }
    if !found {
        return None;
    }
    let subject_width = max_x - min_x + 1;
    let subject_height = max_y - min_y + 1;
    let margin = ((subject_width.max(subject_height) as f32 * 0.08).round() as u32)
        .clamp(24, width.max(height).max(24));
    let x0 = min_x.saturating_sub(margin);
    let y0 = min_y.saturating_sub(margin);
    let x1 = max_x.saturating_add(margin).min(width - 1);
    let y1 = max_y.saturating_add(margin).min(height - 1);
    Some(MatteCrop {
        x: x0,
        y: y0,
        width: x1 - x0 + 1,
        height: y1 - y0 + 1,
    })
}

fn build_vitmatte_trimap(mask: &[u8], width: usize, height: usize) -> Vec<u8> {
    let mut unknown = vec![false; mask.len()];
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let value = mask[index];
            let foreground = value >= 128;
            // Do not classify every soft SAM probability as an unknown matte
            // pixel. That made textured object interiors (glass, fabric, fur)
            // entirely "unknown" and let ViTMatte punch speckled alpha holes
            // through otherwise solid selections. Only the actual binary edge
            // and a narrow ambiguous band around 0.5 become unknown.
            let mut boundary = (96..=160).contains(&value);
            if !boundary {
                let min_y = y.saturating_sub(1);
                let max_y = (y + 1).min(height - 1);
                let min_x = x.saturating_sub(1);
                let max_x = (x + 1).min(width - 1);
                'neighbors: for ny in min_y..=max_y {
                    for nx in min_x..=max_x {
                        if (mask[ny * width + nx] >= 128) != foreground {
                            boundary = true;
                            break 'neighbors;
                        }
                    }
                }
            }
            unknown[index] = boundary;
        }
    }

    let radius = ((width.min(height) as f32 / 64.0).round() as usize).clamp(6, 24);
    for _ in 0..radius {
        let source = unknown.clone();
        unknown
            .par_chunks_mut(width)
            .enumerate()
            .for_each(|(y, row)| {
                for (x, value) in row.iter_mut().enumerate() {
                    if *value {
                        continue;
                    }
                    let min_y = y.saturating_sub(1);
                    let max_y = (y + 1).min(height - 1);
                    let min_x = x.saturating_sub(1);
                    let max_x = (x + 1).min(width - 1);
                    *value =
                        (min_y..=max_y).any(|ny| (min_x..=max_x).any(|nx| source[ny * width + nx]));
                }
            });
    }

    mask.iter()
        .zip(unknown)
        .map(|(&value, unknown)| {
            if unknown {
                128
            } else if value >= 128 {
                255
            } else {
                0
            }
        })
        .collect()
}

fn padded_to_divisor(value: u32, divisor: u32) -> u32 {
    value.div_ceil(divisor) * divisor
}

fn refine_mask_with_vitmatte(
    model_path: &Path,
    rgba: &[u8],
    width: u32,
    height: u32,
    coarse_mask: &[u8],
    strength: f32,
) -> Result<Vec<u8>> {
    let pixels = usize::try_from(u64::from(width) * u64::from(height))
        .context("ViTMatte source dimensions overflow")?;
    anyhow::ensure!(
        coarse_mask.len() == pixels,
        "ViTMatte coarse-mask size mismatch"
    );
    anyhow::ensure!(
        rgba.len() == pixels.saturating_mul(4),
        "ViTMatte RGB size mismatch"
    );
    let Some(crop) = matte_crop_for_mask(coarse_mask, width, height) else {
        return Ok(coarse_mask.to_vec());
    };

    let source = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, rgba.to_vec())
        .context("invalid ViTMatte source image")?;
    let crop_image =
        image::imageops::crop_imm(&source, crop.x, crop.y, crop.width, crop.height).to_image();
    let full_mask_image = ImageBuffer::<Luma<u8>, _>::from_raw(width, height, coarse_mask.to_vec())
        .context("invalid ViTMatte coarse mask")?;
    let crop_mask =
        image::imageops::crop_imm(&full_mask_image, crop.x, crop.y, crop.width, crop.height)
            .to_image();

    let max_edge = if cfg!(target_os = "android") {
        VITMATTE_MAX_EDGE_ANDROID
    } else {
        VITMATTE_MAX_EDGE_DESKTOP
    };
    let scale = (max_edge as f64 / crop.width.max(crop.height) as f64).min(1.0);
    let model_width = ((crop.width as f64 * scale).round() as u32).max(1);
    let model_height = ((crop.height as f64 * scale).round() as u32).max(1);
    let resized_image = if model_width == crop.width && model_height == crop.height {
        crop_image
    } else {
        image::imageops::resize(&crop_image, model_width, model_height, FilterType::Lanczos3)
    };
    let resized_mask = if model_width == crop.width && model_height == crop.height {
        crop_mask
    } else {
        image::imageops::resize(&crop_mask, model_width, model_height, FilterType::Lanczos3)
    };
    let trimap = build_vitmatte_trimap(
        resized_mask.as_raw(),
        model_width as usize,
        model_height as usize,
    );
    if !trimap.contains(&128) {
        return Ok(coarse_mask.to_vec());
    }

    let padded_width = padded_to_divisor(model_width, VITMATTE_SIZE_DIVISOR);
    let padded_height = padded_to_divisor(model_height, VITMATTE_SIZE_DIVISOR);
    let plane = usize::try_from(u64::from(padded_width) * u64::from(padded_height))
        .context("ViTMatte padded dimensions overflow")?;
    let mut input = vec![0.0f32; plane * 4];
    // This ONNX conversion was published with mean/std = 0.5 and trimap
    // rescaling by 1/255, so RGB maps to [-1, 1] and trimap to {0,.5,1}.
    for y in 0..model_height as usize {
        for x in 0..model_width as usize {
            let source_index = y * model_width as usize + x;
            let pixel = resized_image.as_raw();
            let rgba_index = source_index * 4;
            let destination = y * padded_width as usize + x;
            input[destination] = pixel[rgba_index] as f32 / 127.5 - 1.0;
            input[plane + destination] = pixel[rgba_index + 1] as f32 / 127.5 - 1.0;
            input[plane * 2 + destination] = pixel[rgba_index + 2] as f32 / 127.5 - 1.0;
            input[plane * 3 + destination] = trimap[source_index] as f32 / 255.0;
        }
    }
    let input = Tensor::from_array((
        [1usize, 4, padded_height as usize, padded_width as usize],
        input,
    ))
    .context("create ViTMatte input tensor")?;

    #[cfg(target_os = "android")]
    let (output_width, output_height, alpha) = {
        let mut session = create_session(model_path)?;
        run_vitmatte_session(&mut session, input)?
    };
    #[cfg(not(target_os = "android"))]
    let (output_width, output_height, alpha) = {
        if cache_object_ai_sessions() {
            let sessions = VITMATTE_SESSION.get_or_init(|| Mutex::new(None));
            let mut session = sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("ViTMatte session lock was poisoned"))?;
            if session.is_none() {
                *session = Some(create_session(model_path)?);
            }
            run_vitmatte_session(
                session
                    .as_mut()
                    .context("ViTMatte session initialization produced no session")?,
                input,
            )?
        } else {
            let mut session = create_session(model_path)?;
            run_vitmatte_session(&mut session, input)?
        }
    };

    let alpha_image =
        ImageBuffer::<Luma<f32>, Vec<f32>>::from_raw(output_width, output_height, alpha)
            .context("invalid ViTMatte alpha buffer")?;
    let alpha_crop = image::imageops::crop_imm(
        &alpha_image,
        0,
        0,
        model_width.min(output_width),
        model_height.min(output_height),
    )
    .to_image();
    let alpha_full =
        image::imageops::resize(&alpha_crop, crop.width, crop.height, FilterType::Lanczos3);
    let trimap_image =
        ImageBuffer::<Luma<u8>, Vec<u8>>::from_raw(model_width, model_height, trimap)
            .context("invalid ViTMatte trimap buffer")?;
    let trimap_full =
        image::imageops::resize(&trimap_image, crop.width, crop.height, FilterType::Nearest);

    let mut output = coarse_mask.to_vec();
    let strength = strength.clamp(0.0, 1.0);
    for y in 0..crop.height as usize {
        for x in 0..crop.width as usize {
            let crop_index = y * crop.width as usize + x;
            if trimap_full.as_raw()[crop_index] != 128 {
                continue;
            }
            let target = (crop.y as usize + y) * width as usize + crop.x as usize + x;
            let predicted = (alpha_full.as_raw()[crop_index].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            let coarse = coarse_mask[target] as f32;
            let refined = coarse * (1.0 - strength) + predicted as f32 * strength;
            output[target] = refined.clamp(0.0, 255.0).round() as u8;
        }
    }
    Ok(output)
}

fn run_vitmatte_session(session: &mut Session, input: Tensor<f32>) -> Result<(u32, u32, Vec<f32>)> {
    let outputs = session
        .run(ort::inputs![input])
        .context("run ViTMatte ONNX inference")?;
    let output = outputs
        .values()
        .next()
        .context("ViTMatte returned no output tensors")?;
    let (shape, alphas) = output
        .try_extract_tensor::<f32>()
        .context("read ViTMatte output tensor")?;
    anyhow::ensure!(
        shape.len() == 4 && shape[0] == 1 && shape[1] == 1,
        "unexpected ViTMatte output shape {shape:?}; expected [1, 1, H, W]"
    );
    let height = usize::try_from(shape[2]).context("ViTMatte output height is invalid")?;
    let width = usize::try_from(shape[3]).context("ViTMatte output width is invalid")?;
    let elements = width
        .checked_mul(height)
        .context("ViTMatte output dimensions overflow")?;
    anyhow::ensure!(
        alphas.len() == elements,
        "ViTMatte output tensor length mismatch"
    );
    anyhow::ensure!(
        alphas.iter().all(|value| value.is_finite()),
        "ViTMatte output contains non-finite alpha values"
    );
    Ok((
        u32::try_from(width).context("ViTMatte output width exceeds u32")?,
        u32::try_from(height).context("ViTMatte output height exceeds u32")?,
        alphas.to_vec(),
    ))
}

fn sigmoid_probability(logit: f32) -> f32 {
    if logit >= 0.0 {
        1.0 / (1.0 + (-logit).exp())
    } else {
        let exponential = logit.exp();
        exponential / (1.0 + exponential)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalized_birefnet_input, restore_birefnet_output, sigmoid_probability,
        validate_birefnet_output_shape, BiRefNetQuality, IMAGENET_MEAN, IMAGENET_STD,
    };
    use image::{ImageBuffer, Rgba};

    #[test]
    fn birefnet_preprocessing_normalizes_the_complete_native_tensor() {
        let image = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(
            2,
            1,
            vec![255, 128, 0, 255, 0, 64, 255, 255],
        )
        .unwrap();
        let input = normalized_birefnet_input(&image, 2, 1).unwrap();

        assert_eq!(input.len(), 6);
        let expected = [
            (1.0 - IMAGENET_MEAN[0]) / IMAGENET_STD[0],
            (0.0 - IMAGENET_MEAN[0]) / IMAGENET_STD[0],
            (128.0 / 255.0 - IMAGENET_MEAN[1]) / IMAGENET_STD[1],
            (64.0 / 255.0 - IMAGENET_MEAN[1]) / IMAGENET_STD[1],
            (0.0 - IMAGENET_MEAN[2]) / IMAGENET_STD[2],
            (1.0 - IMAGENET_MEAN[2]) / IMAGENET_STD[2],
        ];
        for (actual, expected) in input.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn birefnet_output_keeps_soft_probabilities_when_restoring_size() {
        let mask = restore_birefnet_output(&[-2.0, 0.0, 2.0], 3, 1, 3, 1).unwrap();
        assert_eq!(mask, vec![30, 128, 225]);
    }

    #[test]
    fn birefnet_output_requires_single_batch_and_channel() {
        assert!(
            validate_birefnet_output_shape(&[1, 1, 1024, 1024], 1024 * 1024, 1024, 1024).is_ok()
        );
        assert!(
            validate_birefnet_output_shape(&[1, 2, 1024, 1024], 2 * 1024 * 1024, 1024, 1024)
                .is_err()
        );
        assert!(validate_birefnet_output_shape(&[1, 1, -1, 1024], 0, 1024, 1024).is_err());
        assert!(validate_birefnet_output_shape(&[1, 1, 2, 2], 5, 1024, 1024).is_err());
    }

    #[test]
    fn birefnet_quality_tiers_select_distinct_models_and_native_inputs() {
        let low = BiRefNetQuality::Low.model();
        let medium = BiRefNetQuality::Medium.model();
        let high = BiRefNetQuality::High.model();
        assert_eq!((low.input_width, low.input_height), (1024, 1024));
        assert_eq!((medium.input_width, medium.input_height), (1440, 2560));
        assert_eq!((high.input_width, high.input_height), (2048, 2048));
        assert_ne!(low.url, medium.url);
        assert_ne!(medium.url, high.url);
        assert_ne!(low.sha256_hex, medium.sha256_hex);
        assert_ne!(medium.sha256_hex, high.sha256_hex);
        assert_ne!(low.cache_filename, medium.cache_filename);
        assert_ne!(medium.cache_filename, high.cache_filename);
    }

    #[test]
    fn sigmoid_probabilities_keep_model_calibration() {
        assert!((sigmoid_probability(0.0) - 0.5).abs() < f32::EPSILON);
        assert!((sigmoid_probability(2.0) - 0.880797).abs() < 1e-6);
        assert!((sigmoid_probability(-2.0) - 0.119203).abs() < 1e-6);
    }
}

// -----------------------------------------------------------------------------
// Promptable object masks (SAM 2.1 Hiera Tiny)
// -----------------------------------------------------------------------------

pub const SAM21_ENCODER_MODEL_URL: &str = "https://huggingface.co/akiyamanx/sam2.1-hiera-tiny-onnx/resolve/main/sam2.1_hiera_tiny.encoder.onnx";
pub const SAM21_DECODER_MODEL_URL: &str = "https://huggingface.co/akiyamanx/sam2.1-hiera-tiny-onnx/resolve/main/sam2.1_hiera_tiny.decoder.onnx";
pub const SAM21_ENCODER_SHA256_HEX: &str =
    "667384d1e686de6828b841ac8a24db0fafa2b3452494225f82eeedac56141230";
pub const SAM21_DECODER_SHA256_HEX: &str =
    "c40f5aa7d37b681cd500481a85d44839fd81c93dce1e86271a2c866470d22105";
/// Display/progress estimate only. Integrity is enforced by SHA-256, because
/// Hugging Face's Xet response does not publish a stable Content-Length.
pub const SAM21_MODEL_BYTES_ESTIMATE: u64 = 125_500_000;
const SAM21_MODEL_SIZE: u32 = 1024;
const SAM21_MASK_INPUT_SIZE: u32 = 256;
const SAM21_MAX_PROMPTS: usize = 32;
const SAM21_ENCODER_MAX_BYTES: u64 = 160_000_000;
const SAM21_DECODER_MAX_BYTES: u64 = 32_000_000;
const MAX_OBJECT_MASK_PIXELS: u64 = 17_000_000;

#[cfg(not(target_os = "android"))]
static SAM_ENCODER_SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
static SAM_DECODER_SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectCropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug)]
pub struct SamTensorData {
    pub shape: Vec<usize>,
    pub values: std::sync::Arc<[f32]>,
}

#[derive(Clone, Debug)]
pub struct ObjectInferenceCache {
    pub source_width: u32,
    pub source_height: u32,
    pub crop: ObjectCropRect,
    pub high_res_feats_0: SamTensorData,
    pub high_res_feats_1: SamTensorData,
    pub image_embedding: SamTensorData,
    pub low_res_logits: std::sync::Arc<[f32]>,
    /// Prompt state that produced `low_res_logits`. Encoder features may be
    /// reused for any prompts in the same crop, while prior logits are only
    /// valid when the new prompt list extends this one.
    pub prompt_strokes: Vec<crate::pipeline::ObjectStroke>,
    pub prompt_brush_size: f32,
}

#[derive(Clone, Debug)]
pub struct ObjectMaskRequest {
    pub source_width: u32,
    pub source_height: u32,
    pub source_rgba: Vec<u8>,
    pub strokes: Vec<crate::pipeline::ObjectStroke>,
    pub brush_size: f32,
    pub edge_refine: f32,
    pub cache: Option<ObjectInferenceCache>,
}

#[derive(Debug)]
pub struct ObjectMaskResult {
    pub width: u32,
    pub height: u32,
    pub mask: Vec<u8>,
    pub cache: ObjectInferenceCache,
}

#[derive(Debug)]
pub enum ObjectMaskEvent {
    DownloadProgress {
        label: &'static str,
        downloaded: u64,
        total: u64,
    },
    Inferencing {
        decoder_only: bool,
    },
    Finished(Result<ObjectMaskResult, String>),
}

pub fn spawn_object_mask(
    encoder_path: PathBuf,
    decoder_path: PathBuf,
    vitmatte_path: PathBuf,
    runtime_path: Option<PathBuf>,
    runtime_sha256: Option<String>,
    request: ObjectMaskRequest,
    cancellation: Arc<AtomicBool>,
) -> mpsc::Receiver<ObjectMaskEvent> {
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let spawn = std::thread::Builder::new()
        .name("auraw-onnx-object".to_owned())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (|| {
                    ensure_sam_model(
                        &encoder_path,
                        "SAM 2.1 encoder",
                        SAM21_ENCODER_MODEL_URL,
                        SAM21_ENCODER_SHA256_HEX,
                        &worker_sender,
                        &cancellation,
                    )?;
                    ensure_sam_model(
                        &decoder_path,
                        "SAM 2.1 decoder",
                        SAM21_DECODER_MODEL_URL,
                        SAM21_DECODER_SHA256_HEX,
                        &worker_sender,
                        &cancellation,
                    )?;
                    ensure_vitmatte_model(&vitmatte_path, &cancellation, |downloaded, total| {
                        let _ = worker_sender.send(ObjectMaskEvent::DownloadProgress {
                            label: "ViTMatte edge-refinement model",
                            downloaded,
                            total,
                        });
                    })?;
                    ensure_ai_not_cancelled(&cancellation)?;
                    let decoder_only = request.cache.is_some();
                    let _ = worker_sender.send(ObjectMaskEvent::Inferencing { decoder_only });
                    infer_object_mask(
                        &encoder_path,
                        &decoder_path,
                        &vitmatte_path,
                        runtime_path.as_deref(),
                        runtime_sha256.as_deref(),
                        request,
                    )
                })()
            }))
            .unwrap_or_else(|panic| {
                let message = panic
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown ONNX Runtime failure");
                Err(anyhow::anyhow!(
                    "ONNX Runtime terminated object-mask inference: {message}"
                ))
            });
            let _ = worker_sender.send(ObjectMaskEvent::Finished(
                result.map_err(|error| format!("{error:#}")),
            ));
        });
    if let Err(error) = spawn {
        let _ = sender.send(ObjectMaskEvent::Finished(Err(format!(
            "could not start SAM 2.1 worker: {error}"
        ))));
    }
    receiver
}

fn ensure_sam_model(
    path: &Path,
    label: &'static str,
    url: &str,
    expected_sha256: &str,
    events: &mpsc::Sender<ObjectMaskEvent>,
    cancellation: &AtomicBool,
) -> Result<()> {
    let max_bytes = sam_model_max_bytes(label);
    if verify_sha256_hex(path, expected_sha256, max_bytes).is_ok() {
        return Ok(());
    }
    if path.exists() {
        log::warn!("discarding invalid {label} cache {}", path.display());
        fs::remove_file(path)
            .with_context(|| format!("remove invalid model {}", path.display()))?;
    }
    download_sam_model(path, label, url, expected_sha256, events, cancellation)?;
    verify_sha256_hex(path, expected_sha256, max_bytes).with_context(|| format!("verify {label}"))
}

fn sam_model_max_bytes(label: &str) -> u64 {
    if label.contains("encoder") {
        SAM21_ENCODER_MAX_BYTES
    } else {
        SAM21_DECODER_MAX_BYTES
    }
}

fn verify_sha256_hex(path: &Path, expected: &str, max_bytes: u64) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("read {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "model cache is not a regular file");
    anyhow::ensure!(
        metadata.len() > 0 && metadata.len() <= max_bytes,
        "model cache size {} is outside the allowed range 1..={max_bytes}",
        metadata.len()
    );
    let actual = sha256_file_hex(path)?;
    anyhow::ensure!(actual == expected, "model SHA-256 mismatch");
    Ok(())
}

fn download_sam_model(
    path: &Path,
    label: &'static str,
    url: &str,
    expected_sha256: &str,
    events: &mpsc::Sender<ObjectMaskEvent>,
    cancellation: &AtomicBool,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create model cache {}", parent.display()))?;
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("onnx.{}.{}.part", std::process::id(), nonce));
    let max_bytes = sam_model_max_bytes(label);
    let fallback_total = if label.contains("encoder") {
        109_000_000
    } else {
        16_500_000
    };
    const MAX_ATTEMPTS: usize = 5;

    let result = (|| -> Result<()> {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_connect(Some(Duration::from_secs(45)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            // Large Hugging Face/Xet model transfers can briefly stall or be
            // throttled. Keep a generous end-to-end body allowance; transient
            // disconnects are handled below by resuming the .part file.
            .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
            .build();
        let agent: ureq::Agent = config.into();
        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 0..MAX_ATTEMPTS {
            ensure_ai_not_cancelled(cancellation)?;
            let mut downloaded = fs::metadata(&temporary)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if downloaded > max_bytes {
                fs::remove_file(&temporary)
                    .with_context(|| format!("remove oversized partial {label}"))?;
                downloaded = 0;
            }
            if downloaded > 0
                && sha256_file_hex(&temporary).ok().as_deref() == Some(expected_sha256)
            {
                ensure_ai_not_cancelled(cancellation)?;
                fs::rename(&temporary, path)
                    .with_context(|| format!("publish resumed {label} to {}", path.display()))?;
                return Ok(());
            }

            if downloaded > 0 {
                let _ = events.send(ObjectMaskEvent::DownloadProgress {
                    label,
                    downloaded,
                    total: fallback_total.max(downloaded),
                });
            }

            let response_result = if downloaded > 0 {
                let range = format!("bytes={downloaded}-");
                agent.get(url).header("Range", range.as_str()).call()
            } else {
                agent.get(url).call()
            };

            let mut response = match response_result {
                Ok(response) => response,
                Err(error) => {
                    last_error = Some(anyhow::Error::new(error).context(format!(
                        "download {label} (attempt {}/{MAX_ATTEMPTS})",
                        attempt + 1
                    )));
                    if attempt + 1 < MAX_ATTEMPTS {
                        std::thread::sleep(Duration::from_secs(1u64 << attempt.min(3)));
                        continue;
                    }
                    break;
                }
            };

            // Hugging Face normally honors Range with 206. If a proxy/CDN
            // ignores it and sends 200, restart this attempt from byte zero
            // rather than appending a second full model to the partial file.
            let resuming = downloaded > 0 && response.status().as_u16() == 206;
            if downloaded > 0 && !resuming {
                downloaded = 0;
            }

            let declared_remaining = response.body().content_length();
            let total = match declared_remaining {
                Some(length) if resuming => downloaded
                    .checked_add(length)
                    .context("model response length overflow")?,
                Some(length) => length,
                None => fallback_total.max(downloaded),
            };
            anyhow::ensure!(
                total <= max_bytes,
                "{label} response declares {total} bytes, above the {max_bytes}-byte limit"
            );

            let mut options = OpenOptions::new();
            options.write(true).create(true);
            if resuming {
                options.append(true);
            } else {
                options.truncate(true);
            }
            let mut file = options.open(&temporary).with_context(|| {
                format!("open partial {label} download {}", temporary.display())
            })?;
            let mut reader = response.body_mut().as_reader();
            let mut buffer = [0u8; 256 * 1024];
            let mut transfer_error: Option<anyhow::Error> = None;

            loop {
                ensure_ai_not_cancelled(cancellation)?;
                let read = match reader.read(&mut buffer) {
                    Ok(read) => read,
                    Err(error) => {
                        // Persist everything already received before retrying so
                        // the next request can continue with an HTTP Range.
                        let _ = file.sync_data();
                        transfer_error = Some(anyhow::Error::new(error).context(format!(
                            "read {label} (attempt {}/{MAX_ATTEMPTS})",
                            attempt + 1
                        )));
                        break;
                    }
                };
                if read == 0 {
                    break;
                }
                downloaded = downloaded
                    .checked_add(read as u64)
                    .context("model download byte count overflow")?;
                anyhow::ensure!(
                    downloaded <= max_bytes,
                    "{label} download exceeded the {max_bytes}-byte limit"
                );
                file.write_all(&buffer[..read])
                    .with_context(|| format!("write {label}"))?;
                let _ = events.send(ObjectMaskEvent::DownloadProgress {
                    label,
                    downloaded,
                    total: total.max(downloaded),
                });
            }

            if let Some(error) = transfer_error {
                last_error = Some(error);
                if attempt + 1 < MAX_ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(1u64 << attempt.min(3)));
                    continue;
                }
                break;
            }

            file.sync_all().with_context(|| format!("flush {label}"))?;
            let actual =
                sha256_file_hex(&temporary).with_context(|| format!("hash downloaded {label}"))?;
            if actual == expected_sha256 {
                ensure_ai_not_cancelled(cancellation)?;
                fs::rename(&temporary, path)
                    .with_context(|| format!("publish {label} to {}", path.display()))?;
                return Ok(());
            }

            // A close-delimited CDN response can end early without a useful
            // Content-Length. Treat a hash mismatch as resumable first; the
            // byte cap and final SHA-256 pin still prevent accepting bad data.
            last_error = Some(anyhow::anyhow!(
                "{label} SHA-256 mismatch after receiving {downloaded} bytes"
            ));
            if attempt + 1 < MAX_ATTEMPTS {
                std::thread::sleep(Duration::from_secs(1u64 << attempt.min(3)));
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("download {label} failed")))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn infer_object_mask(
    encoder_path: &Path,
    decoder_path: &Path,
    vitmatte_path: &Path,
    runtime_path: Option<&Path>,
    runtime_sha256: Option<&str>,
    request: ObjectMaskRequest,
) -> Result<ObjectMaskResult> {
    anyhow::ensure!(
        request.source_width > 0 && request.source_height > 0,
        "object-mask source is empty"
    );
    let pixels = u64::from(request.source_width)
        .checked_mul(u64::from(request.source_height))
        .context("object-mask input dimensions overflow")?;
    anyhow::ensure!(
        pixels <= MAX_OBJECT_MASK_PIXELS,
        "object-mask input {}x{} exceeds the {MAX_OBJECT_MASK_PIXELS}-pixel limit",
        request.source_width,
        request.source_height
    );
    let expected = pixels
        .checked_mul(4)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .context("object-mask input byte count overflow")?;
    anyhow::ensure!(
        request.source_rgba.len() == expected,
        "object-mask RGBA buffer has {}, expected {expected}",
        request.source_rgba.len()
    );
    anyhow::ensure!(
        request
            .strokes
            .iter()
            .any(|stroke| stroke.positive && !stroke.points.is_empty()),
        "paint inside an object before running selection"
    );
    initialize_runtime(runtime_path, runtime_sha256)?;

    let source = ImageBuffer::<Rgba<u8>, _>::from_raw(
        request.source_width,
        request.source_height,
        request.source_rgba,
    )
    .context("invalid canonical image for object selection")?;

    let prompt_set = sampled_object_prompts(
        &request.strokes,
        request.brush_size,
        request.source_width,
        request.source_height,
        SAM21_MAX_PROMPTS,
    );
    let prompts = &prompt_set.prompts;
    let supplied_cache = request.cache;
    let mut last_result = None;

    for expansion in 0..3 {
        let crop = object_crop_for_prompts(
            request.source_width,
            request.source_height,
            prompt_set.focus,
            expansion,
        );
        let cached = supplied_cache
            .as_ref()
            .filter(|cache| {
                cache.source_width == request.source_width
                    && cache.source_height == request.source_height
                    && cache.crop == crop
            })
            .cloned();
        let crop_image =
            image::imageops::crop_imm(&source, crop.x, crop.y, crop.width, crop.height).to_image();
        let resized = image::imageops::resize(
            &crop_image,
            SAM21_MODEL_SIZE,
            SAM21_MODEL_SIZE,
            FilterType::Lanczos3,
        );

        let (features, previous_logits) = if let Some(cache) = cached {
            let can_reuse_logits = strokes_extend(&cache.prompt_strokes, &request.strokes)
                && (cache.prompt_brush_size - request.brush_size).abs() <= f32::EPSILON;
            (cache, can_reuse_logits)
        } else {
            (
                encode_sam_image(
                    encoder_path,
                    &resized,
                    request.source_width,
                    request.source_height,
                    crop,
                )?,
                false,
            )
        };
        let decoded = decode_sam_mask(decoder_path, &features, &prompt_set, previous_logits)?;
        let touches_border =
            mask_touches_crop_border(&decoded.probabilities, decoded.width, decoded.height);
        last_result = Some((crop, resized, features, decoded));
        if !touches_border
            || expansion == 2
            || crop_is_full(crop, request.source_width, request.source_height)
        {
            break;
        }
    }

    let (crop, resized_guidance, mut cache, decoded) =
        last_result.context("SAM 2.1 produced no object-mask candidate")?;
    let selected = keep_prompt_connected_component(
        decoded.probabilities,
        decoded.width,
        decoded.height,
        prompts,
        request.source_width,
        request.source_height,
        crop,
    );
    let refine_guidance = if decoded.width == resized_guidance.width()
        && decoded.height == resized_guidance.height()
    {
        resized_guidance
    } else {
        image::imageops::resize(
            &resized_guidance,
            decoded.width,
            decoded.height,
            FilterType::Lanczos3,
        )
    };
    let refined = edge_aware_refine(
        selected,
        decoded.width,
        decoded.height,
        refine_guidance.as_raw(),
        request.edge_refine,
    );
    let crop_mask = resize_probability_u8(
        &refined,
        decoded.width,
        decoded.height,
        crop.width,
        crop.height,
    );
    let mut full_mask = vec![0u8; request.source_width as usize * request.source_height as usize];
    for y in 0..crop.height as usize {
        let source_start = y * crop.width as usize;
        let target_start = (crop.y as usize + y) * request.source_width as usize + crop.x as usize;
        full_mask[target_start..target_start + crop.width as usize]
            .copy_from_slice(&crop_mask[source_start..source_start + crop.width as usize]);
    }
    full_mask = match refine_mask_with_vitmatte(
        vitmatte_path,
        source.as_raw(),
        request.source_width,
        request.source_height,
        &full_mask,
        (0.65 + request.edge_refine.clamp(0.0, 1.0) * 0.35).clamp(0.0, 1.0),
    ) {
        Ok(refined) => refined,
        Err(error) => {
            // ViTMatte is an edge refinement stage, not the source of the
            // object selection. A provider/model failure must not discard a
            // perfectly usable SAM mask and look like a cancelled operation.
            log::warn!(
                "ViTMatte object-edge refinement failed; using the cleaned SAM mask: {error:#}"
            );
            full_mask
        }
    };
    cache.low_res_logits = resize_f32(
        &decoded.selected_logits,
        decoded.width,
        decoded.height,
        SAM21_MASK_INPUT_SIZE,
        SAM21_MASK_INPUT_SIZE,
    )
    .into();
    cache.prompt_strokes = request.strokes.clone();
    cache.prompt_brush_size = request.brush_size;

    Ok(ObjectMaskResult {
        width: request.source_width,
        height: request.source_height,
        mask: full_mask,
        cache,
    })
}

fn strokes_extend(
    previous: &[crate::pipeline::ObjectStroke],
    current: &[crate::pipeline::ObjectStroke],
) -> bool {
    previous.len() <= current.len()
        && previous
            .iter()
            .zip(current.iter())
            .all(|(previous, current)| previous == current)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjectPromptKind {
    Foreground,
    Background,
    BoxTopLeft,
    BoxBottomRight,
}

impl ObjectPromptKind {
    const fn sam_label(self) -> f32 {
        match self {
            Self::Foreground => 1.0,
            Self::Background => 0.0,
            Self::BoxTopLeft => 2.0,
            Self::BoxBottomRight => 3.0,
        }
    }

    const fn is_foreground(self) -> bool {
        matches!(self, Self::Foreground)
    }

    const fn is_background(self) -> bool {
        matches!(self, Self::Background)
    }
}

#[derive(Clone, Copy, Debug)]
struct ObjectPrompt {
    point: [f32; 2],
    kind: ObjectPromptKind,
}

#[derive(Clone, Copy, Debug)]
struct ObjectPromptFocus {
    min: [f32; 2],
    max: [f32; 2],
}

impl ObjectPromptFocus {
    fn contains(self, point: [f32; 2]) -> bool {
        point[0] >= self.min[0]
            && point[0] <= self.max[0]
            && point[1] >= self.min[1]
            && point[1] <= self.max[1]
    }
}

#[derive(Clone, Debug)]
struct ObjectPromptSet {
    prompts: Vec<ObjectPrompt>,
    focus: ObjectPromptFocus,
}

fn sampled_object_prompts(
    strokes: &[crate::pipeline::ObjectStroke],
    brush_size: f32,
    source_width: u32,
    source_height: u32,
    limit: usize,
) -> ObjectPromptSet {
    let mut foreground_points = Vec::new();
    let mut explicit_background_points = Vec::new();
    for stroke in strokes {
        let target = if stroke.positive {
            &mut foreground_points
        } else {
            &mut explicit_background_points
        };
        target.extend(
            stroke
                .points
                .iter()
                .map(|point| [point[0].clamp(0.0, 1.0), point[1].clamp(0.0, 1.0)]),
        );
    }

    let focus = object_prompt_focus(&foreground_points, brush_size, source_width, source_height);
    let foreground_budget = limit.saturating_sub(10).clamp(1, 16);
    let background_budget = limit.saturating_sub(foreground_budget + 2).min(6);
    let mut prompts = evenly_sample(&foreground_points, foreground_budget)
        .into_iter()
        .map(|point| ObjectPrompt {
            point,
            kind: ObjectPromptKind::Foreground,
        })
        .collect::<Vec<_>>();
    prompts.extend(
        evenly_sample(&explicit_background_points, background_budget)
            .into_iter()
            .map(|point| ObjectPrompt {
                point,
                kind: ObjectPromptKind::Background,
            }),
    );

    // A box tells SAM that the painted region is the intended object part,
    // rather than merely one foreground sample somewhere on a larger person or
    // connected object. Labels 2 and 3 are SAM's top-left/bottom-right box
    // prompts.
    if prompts.len() + 2 <= limit {
        prompts.push(ObjectPrompt {
            point: focus.min,
            kind: ObjectPromptKind::BoxTopLeft,
        });
        prompts.push(ObjectPrompt {
            point: focus.max,
            kind: ObjectPromptKind::BoxBottomRight,
        });
    }

    // Automatic background guards just outside the brush-sized focus box make
    // a stroke through an arm prefer the arm instead of the nearby shoulder and
    // torso. They remain outside the painted area and are omitted at image
    // boundaries where clamping would move them back into the focus region.
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let image_min = source_width.min(source_height).max(1) as f32;
    let radius_x = brush_size.clamp(f32::EPSILON, 0.5) * image_min / width;
    let radius_y = brush_size.clamp(f32::EPSILON, 0.5) * image_min / height;
    let gap_x = (radius_x * 0.85).max(6.0 / width);
    let gap_y = (radius_y * 0.85).max(6.0 / height);
    let center = [
        (focus.min[0] + focus.max[0]) * 0.5,
        (focus.min[1] + focus.max[1]) * 0.5,
    ];
    let guards = [
        [focus.min[0] - gap_x, focus.min[1] - gap_y],
        [center[0], focus.min[1] - gap_y],
        [focus.max[0] + gap_x, focus.min[1] - gap_y],
        [focus.min[0] - gap_x, center[1]],
        [focus.max[0] + gap_x, center[1]],
        [focus.min[0] - gap_x, focus.max[1] + gap_y],
        [center[0], focus.max[1] + gap_y],
        [focus.max[0] + gap_x, focus.max[1] + gap_y],
    ];
    for guard in guards {
        if prompts.len() >= limit {
            break;
        }
        let point = [guard[0].clamp(0.0, 1.0), guard[1].clamp(0.0, 1.0)];
        if !focus.contains(point) {
            prompts.push(ObjectPrompt {
                point,
                kind: ObjectPromptKind::Background,
            });
        }
    }

    ObjectPromptSet { prompts, focus }
}

fn object_prompt_focus(
    foreground_points: &[[f32; 2]],
    brush_size: f32,
    source_width: u32,
    source_height: u32,
) -> ObjectPromptFocus {
    let mut min = [1.0f32, 1.0f32];
    let mut max = [0.0f32, 0.0f32];
    for point in foreground_points {
        min[0] = min[0].min(point[0]);
        min[1] = min[1].min(point[1]);
        max[0] = max[0].max(point[0]);
        max[1] = max[1].max(point[1]);
    }
    if foreground_points.is_empty() {
        min = [0.45, 0.45];
        max = [0.55, 0.55];
    }

    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let image_min = source_width.min(source_height).max(1) as f32;
    let radius = brush_size.clamp(f32::EPSILON, 0.5) * image_min;
    let padding_x = (radius * 1.35 + 8.0) / width;
    let padding_y = (radius * 1.35 + 8.0) / height;
    ObjectPromptFocus {
        min: [
            (min[0] - padding_x).clamp(0.0, 1.0),
            (min[1] - padding_y).clamp(0.0, 1.0),
        ],
        max: [
            (max[0] + padding_x).clamp(0.0, 1.0),
            (max[1] + padding_y).clamp(0.0, 1.0),
        ],
    }
}

fn evenly_sample<T: Copy>(values: &[T], count: usize) -> Vec<T> {
    if count == 0 || values.is_empty() {
        return Vec::new();
    }
    if values.len() <= count {
        return values.to_vec();
    }
    (0..count)
        .map(|index| values[index * (values.len() - 1) / (count - 1).max(1)])
        .collect()
}

fn object_crop_for_prompts(
    width: u32,
    height: u32,
    focus: ObjectPromptFocus,
    expansion: usize,
) -> ObjectCropRect {
    if expansion >= 2 {
        return ObjectCropRect {
            x: 0,
            y: 0,
            width,
            height,
        };
    }
    let center_x = ((focus.min[0] + focus.max[0]) * 0.5 * width as f32).clamp(0.0, width as f32);
    let center_y = ((focus.min[1] + focus.max[1]) * 0.5 * height as f32).clamp(0.0, height as f32);
    let bounds_w = (focus.max[0] - focus.min[0]).max(0.0) * width as f32;
    let bounds_h = (focus.max[1] - focus.min[1]).max(0.0) * height as f32;
    let minimum = width.min(height) as f32 * 0.16;
    let factor = if expansion == 0 { 1.5 } else { 2.3 };
    let mut edge = bounds_w.max(bounds_h).max(minimum).max(96.0) * factor;
    edge = edge.min(width.max(height) as f32);
    let crop_width = edge.round().clamp(1.0, width as f32) as u32;
    let crop_height = edge.round().clamp(1.0, height as f32) as u32;
    let x = (center_x - crop_width as f32 * 0.5)
        .round()
        .clamp(0.0, width.saturating_sub(crop_width) as f32) as u32;
    let y = (center_y - crop_height as f32 * 0.5)
        .round()
        .clamp(0.0, height.saturating_sub(crop_height) as f32) as u32;
    ObjectCropRect {
        x,
        y,
        width: crop_width,
        height: crop_height,
    }
}

fn crop_is_full(crop: ObjectCropRect, width: u32, height: u32) -> bool {
    crop.x == 0 && crop.y == 0 && crop.width == width && crop.height == height
}

fn encode_sam_image(
    encoder_path: &Path,
    resized: &ImageBuffer<Rgba<u8>, Vec<u8>>,
    source_width: u32,
    source_height: u32,
    crop: ObjectCropRect,
) -> Result<ObjectInferenceCache> {
    let plane = (SAM21_MODEL_SIZE * SAM21_MODEL_SIZE) as usize;
    let mut values = vec![0.0f32; plane * 3];
    for y in 0..SAM21_MODEL_SIZE {
        for x in 0..SAM21_MODEL_SIZE {
            let pixel = resized.get_pixel(x, y);
            let index = (y * SAM21_MODEL_SIZE + x) as usize;
            for channel in 0..3 {
                let normalized = pixel[channel] as f32 / 255.0;
                values[channel * plane + index] =
                    (normalized - IMAGENET_MEAN[channel]) / IMAGENET_STD[channel];
            }
        }
    }
    let input = Tensor::from_array((
        [
            1usize,
            3,
            SAM21_MODEL_SIZE as usize,
            SAM21_MODEL_SIZE as usize,
        ],
        values,
    ))
    .context("create SAM 2.1 encoder input")?;

    #[cfg(target_os = "android")]
    let tensors = {
        let mut session = create_session(encoder_path)?;
        run_sam_encoder(&mut session, input)?
    };
    #[cfg(not(target_os = "android"))]
    let tensors = {
        #[cfg(target_os = "windows")]
        {
            // Do not use the generic desktop session for the SAM image encoder
            // on Windows. Some otherwise-valid ONNX Runtime CPU DLLs produce
            // non-finite Hiera feature maps when the default graph/layout
            // optimizations are enabled. The conservative session is slower but
            // avoids the native-runtime numerical failure that makes Object Mask
            // unusable.
            let sessions = SAM_ENCODER_SESSION.get_or_init(|| Mutex::new(None));
            let mut guard = sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("SAM encoder session lock was poisoned"))?;
            if guard.is_none() {
                *guard = Some(create_windows_sam_encoder_session(encoder_path)?);
            }
            run_sam_encoder(
                guard
                    .as_mut()
                    .context("SAM encoder session is unavailable")?,
                input,
            )?
        }
        #[cfg(not(target_os = "windows"))]
        {
            if cache_object_ai_sessions() {
                let sessions = SAM_ENCODER_SESSION.get_or_init(|| Mutex::new(None));
                let mut guard = sessions
                    .lock()
                    .map_err(|_| anyhow::anyhow!("SAM encoder session lock was poisoned"))?;
                if guard.is_none() {
                    *guard = Some(create_session(encoder_path)?);
                }
                run_sam_encoder(
                    guard
                        .as_mut()
                        .context("SAM encoder session is unavailable")?,
                    input,
                )?
            } else {
                let mut session = create_session(encoder_path)?;
                run_sam_encoder(&mut session, input)?
            }
        }
    };

    Ok(ObjectInferenceCache {
        source_width,
        source_height,
        crop,
        high_res_feats_0: tensors.0,
        high_res_feats_1: tensors.1,
        image_embedding: tensors.2,
        low_res_logits: vec![0.0; (SAM21_MASK_INPUT_SIZE * SAM21_MASK_INPUT_SIZE) as usize].into(),
        prompt_strokes: Vec::new(),
        prompt_brush_size: 0.0,
    })
}

fn run_sam_encoder(
    session: &mut Session,
    input: Tensor<f32>,
) -> Result<(SamTensorData, SamTensorData, SamTensorData)> {
    let outputs = session
        .run(ort::inputs![input])
        .context("run SAM 2.1 image encoder")?;
    Ok((
        extract_sam_encoder_output(&outputs, 0, "high-resolution feature 0")?,
        extract_sam_encoder_output(&outputs, 1, "high-resolution feature 1")?,
        extract_sam_encoder_output(&outputs, 2, "image embedding")?,
    ))
}

fn extract_sam_encoder_output(
    outputs: &ort::session::SessionOutputs<'_>,
    index: usize,
    label: &str,
) -> Result<SamTensorData> {
    let value = outputs
        .values()
        .nth(index)
        .with_context(|| format!("SAM 2.1 returned no {label}"))?;
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .with_context(|| format!("read SAM 2.1 {label}"))?;

    let non_finite = data.iter().filter(|value| !value.is_finite()).count();
    #[cfg(target_os = "windows")]
    let values = if non_finite > 0 {
        // A very small number of isolated NaN/Inf values has been observed from
        // third-party Windows ORT CPU DLLs even with conservative session
        // settings. Replacing a handful with neutral zeros is safer than making
        // Object Mask unusable, but never accept broadly-corrupted feature maps.
        let repair_limit = 64usize.max(data.len() / 100_000);
        anyhow::ensure!(
            non_finite <= repair_limit,
            "SAM 2.1 {label} is numerically corrupted: {non_finite} of {} values are non-finite even with conservative Windows CPU inference. Select a current Microsoft x64 CPU onnxruntime.dll and restart AuRaw",
            data.len()
        );
        log::warn!(
            "SAM 2.1 {label} contained {non_finite} isolated non-finite values on Windows; replacing them with zero"
        );
        data.iter()
            .map(|value| if value.is_finite() { *value } else { 0.0 })
            .collect::<Vec<_>>()
    } else {
        data.to_vec()
    };
    #[cfg(not(target_os = "windows"))]
    let values = {
        anyhow::ensure!(
            non_finite == 0,
            "SAM 2.1 {label} contains non-finite values"
        );
        data.to_vec()
    };

    let shape = shape
        .iter()
        .map(|dimension| usize::try_from(*dimension).context("negative SAM tensor dimension"))
        .collect::<Result<Vec<_>>>()?;
    let expected = shape.iter().try_fold(1usize, |product, dimension| {
        product
            .checked_mul(*dimension)
            .context("SAM tensor shape overflow")
    })?;
    anyhow::ensure!(
        expected == values.len(),
        "SAM tensor shape does not match its data"
    );
    Ok(SamTensorData {
        shape,
        values: values.into(),
    })
}

fn extract_f32_output(
    outputs: &ort::session::SessionOutputs<'_>,
    index: usize,
    label: &str,
) -> Result<SamTensorData> {
    let value = outputs
        .values()
        .nth(index)
        .with_context(|| format!("SAM 2.1 returned no {label}"))?;
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .with_context(|| format!("read SAM 2.1 {label}"))?;
    anyhow::ensure!(
        data.iter().all(|value| value.is_finite()),
        "SAM 2.1 {label} contains non-finite values"
    );
    let shape = shape
        .iter()
        .map(|dimension| usize::try_from(*dimension).context("negative SAM tensor dimension"))
        .collect::<Result<Vec<_>>>()?;
    let expected = shape.iter().try_fold(1usize, |product, dimension| {
        product
            .checked_mul(*dimension)
            .context("SAM tensor shape overflow")
    })?;
    anyhow::ensure!(
        expected == data.len(),
        "SAM tensor shape does not match its data"
    );
    Ok(SamTensorData {
        shape,
        values: data.to_vec().into(),
    })
}

struct DecodedSamMask {
    width: u32,
    height: u32,
    probabilities: Vec<f32>,
    selected_logits: Vec<f32>,
}

fn decode_sam_mask(
    decoder_path: &Path,
    cache: &ObjectInferenceCache,
    prompt_set: &ObjectPromptSet,
    use_previous_mask: bool,
) -> Result<DecodedSamMask> {
    let prompts = &prompt_set.prompts;
    anyhow::ensure!(!prompts.is_empty(), "SAM 2.1 requires at least one prompt");
    anyhow::ensure!(
        prompts.len() <= SAM21_MAX_PROMPTS,
        "SAM 2.1 prompt count exceeds the fixed decoder budget"
    );
    // Keep prompt tensor shapes stable so GPU execution providers can reuse
    // compiled kernels. SAM uses label -1 for padding points.
    let mut coords = vec![0.0f32; SAM21_MAX_PROMPTS * 2];
    let mut labels = vec![-1.0f32; SAM21_MAX_PROMPTS];
    for (index, prompt) in prompts.iter().enumerate() {
        let source_x = prompt.point[0].clamp(0.0, 1.0) * cache.source_width as f32;
        let source_y = prompt.point[1].clamp(0.0, 1.0) * cache.source_height as f32;
        coords[index * 2] = ((source_x - cache.crop.x as f32) / cache.crop.width.max(1) as f32
            * SAM21_MODEL_SIZE as f32)
            .clamp(0.0, SAM21_MODEL_SIZE as f32 - 1.0);
        coords[index * 2 + 1] = ((source_y - cache.crop.y as f32)
            / cache.crop.height.max(1) as f32
            * SAM21_MODEL_SIZE as f32)
            .clamp(0.0, SAM21_MODEL_SIZE as f32 - 1.0);
        labels[index] = prompt.kind.sam_label();
    }

    let image_embedding = tensor_from_sam_data(&cache.image_embedding, "image embedding")?;
    let high_res_0 = tensor_from_sam_data(&cache.high_res_feats_0, "high-resolution feature 0")?;
    let high_res_1 = tensor_from_sam_data(&cache.high_res_feats_1, "high-resolution feature 1")?;
    let point_coords = Tensor::from_array(([1usize, SAM21_MAX_PROMPTS, 2usize], coords))
        .context("create SAM point coordinates")?;
    let point_labels = Tensor::from_array(([1usize, SAM21_MAX_PROMPTS], labels))
        .context("create SAM point labels")?;
    let mask_values = if use_previous_mask {
        cache.low_res_logits.to_vec()
    } else {
        vec![0.0; (SAM21_MASK_INPUT_SIZE * SAM21_MASK_INPUT_SIZE) as usize]
    };
    let mask_input = Tensor::from_array((
        [
            1usize,
            1,
            SAM21_MASK_INPUT_SIZE as usize,
            SAM21_MASK_INPUT_SIZE as usize,
        ],
        mask_values,
    ))
    .context("create SAM previous-mask input")?;
    let has_mask = Tensor::from_array(([1usize], vec![if use_previous_mask { 1.0 } else { 0.0 }]))
        .context("create SAM previous-mask flag")?;

    #[cfg(target_os = "android")]
    let (masks, scores) = {
        let mut session = create_session(decoder_path)?;
        run_sam_decoder(
            &mut session,
            image_embedding,
            high_res_0,
            high_res_1,
            point_coords,
            point_labels,
            mask_input,
            has_mask,
        )?
    };
    #[cfg(not(target_os = "android"))]
    let (masks, scores) = {
        if cache_object_ai_sessions() {
            let sessions = SAM_DECODER_SESSION.get_or_init(|| Mutex::new(None));
            let mut guard = sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("SAM decoder session lock was poisoned"))?;
            if guard.is_none() {
                *guard = Some(create_session(decoder_path)?);
            }
            run_sam_decoder(
                guard
                    .as_mut()
                    .context("SAM decoder session is unavailable")?,
                image_embedding,
                high_res_0,
                high_res_1,
                point_coords,
                point_labels,
                mask_input,
                has_mask,
            )?
        } else {
            let mut session = create_session(decoder_path)?;
            run_sam_decoder(
                &mut session,
                image_embedding,
                high_res_0,
                high_res_1,
                point_coords,
                point_labels,
                mask_input,
                has_mask,
            )?
        }
    };
    select_sam_candidate(masks, scores, prompt_set, cache)
}

fn tensor_from_sam_data(data: &SamTensorData, label: &str) -> Result<Tensor<f32>> {
    Tensor::from_array((data.shape.clone(), data.values.to_vec()))
        .with_context(|| format!("create SAM {label} input"))
}

#[allow(clippy::too_many_arguments)]
fn run_sam_decoder(
    session: &mut Session,
    image_embedding: Tensor<f32>,
    high_res_0: Tensor<f32>,
    high_res_1: Tensor<f32>,
    point_coords: Tensor<f32>,
    point_labels: Tensor<f32>,
    mask_input: Tensor<f32>,
    has_mask: Tensor<f32>,
) -> Result<(SamTensorData, SamTensorData)> {
    let outputs = session
        .run(ort::inputs![
            image_embedding,
            high_res_0,
            high_res_1,
            point_coords,
            point_labels,
            mask_input,
            has_mask
        ])
        .context("run SAM 2.1 mask decoder")?;
    Ok((
        extract_f32_output(&outputs, 0, "mask logits")?,
        extract_f32_output(&outputs, 1, "mask scores")?,
    ))
}

fn select_sam_candidate(
    masks: SamTensorData,
    scores: SamTensorData,
    prompt_set: &ObjectPromptSet,
    cache: &ObjectInferenceCache,
) -> Result<DecodedSamMask> {
    let prompts = &prompt_set.prompts;
    anyhow::ensure!(
        masks.shape.len() == 4 && masks.shape[0] == 1,
        "unexpected SAM mask shape {:?}",
        masks.shape
    );
    let candidates = masks.shape[1];
    let height = masks.shape[2];
    let width = masks.shape[3];
    anyhow::ensure!(
        candidates > 0 && width > 0 && height > 0,
        "empty SAM decoder output"
    );
    let plane = width
        .checked_mul(height)
        .context("SAM mask size overflow")?;
    anyhow::ensure!(
        masks.values.len() == candidates * plane,
        "SAM mask tensor length mismatch"
    );
    anyhow::ensure!(
        scores.values.len() >= candidates,
        "SAM score tensor is too short"
    );

    let mut best_index = 0usize;
    let mut best_score = f32::NEG_INFINITY;
    for candidate in 0..candidates {
        let logits = &masks.values[candidate * plane..(candidate + 1) * plane];
        let mut score = scores.values[candidate];
        for prompt in prompts {
            if !prompt.kind.is_foreground() && !prompt.kind.is_background() {
                continue;
            }
            let source_x = prompt.point[0].clamp(0.0, 1.0) * cache.source_width as f32;
            let source_y = prompt.point[1].clamp(0.0, 1.0) * cache.source_height as f32;
            let px = (((source_x - cache.crop.x as f32) / cache.crop.width.max(1) as f32)
                * width as f32)
                .round()
                .clamp(0.0, width.saturating_sub(1) as f32) as usize;
            let py = (((source_y - cache.crop.y as f32) / cache.crop.height.max(1) as f32)
                * height as f32)
                .round()
                .clamp(0.0, height.saturating_sub(1) as f32) as usize;
            let probability = sigmoid(logits[py * width + px]);
            score += if prompt.kind.is_foreground() {
                probability * 0.14
            } else {
                (1.0 - probability) * 0.16
            };
        }
        let (outside_focus, focus_fill, area_ratio) =
            candidate_focus_statistics(logits, width, height, prompt_set.focus, cache);
        score -= outside_focus * 0.95;
        score += focus_fill.min(0.75) * 0.22;
        score -= (area_ratio - 2.0).clamp(0.0, 5.0) * 0.10;
        let border = candidate_border_fraction(logits, width, height);
        score -= border * 0.20;
        if score > best_score {
            best_score = score;
            best_index = candidate;
        }
    }
    let selected_logits = masks.values[best_index * plane..(best_index + 1) * plane].to_vec();
    let probabilities = selected_logits
        .iter()
        .map(|value| sigmoid(*value))
        .collect();
    Ok(DecodedSamMask {
        width: u32::try_from(width).context("SAM output width exceeds u32")?,
        height: u32::try_from(height).context("SAM output height exceeds u32")?,
        probabilities,
        selected_logits,
    })
}

fn candidate_focus_statistics(
    logits: &[f32],
    width: usize,
    height: usize,
    focus: ObjectPromptFocus,
    cache: &ObjectInferenceCache,
) -> (f32, f32, f32) {
    let to_output_x = |normalized: f32| {
        ((normalized.clamp(0.0, 1.0) * cache.source_width as f32 - cache.crop.x as f32)
            / cache.crop.width.max(1) as f32
            * width as f32)
            .clamp(0.0, width as f32)
    };
    let to_output_y = |normalized: f32| {
        ((normalized.clamp(0.0, 1.0) * cache.source_height as f32 - cache.crop.y as f32)
            / cache.crop.height.max(1) as f32
            * height as f32)
            .clamp(0.0, height as f32)
    };
    let min_x = to_output_x(focus.min[0]);
    let max_x = to_output_x(focus.max[0]);
    let min_y = to_output_y(focus.min[1]);
    let max_y = to_output_y(focus.max[1]);
    let focus_area = ((max_x - min_x).max(1.0) * (max_y - min_y).max(1.0)).max(1.0);

    let mut active = 0usize;
    let mut inside = 0usize;
    for y in 0..height {
        for x in 0..width {
            if logits[y * width + x] <= 0.0 {
                continue;
            }
            active += 1;
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            if px >= min_x && px <= max_x && py >= min_y && py <= max_y {
                inside += 1;
            }
        }
    }
    if active == 0 {
        return (1.0, 0.0, 0.0);
    }
    let outside_fraction = (active - inside) as f32 / active as f32;
    let focus_fill = inside as f32 / focus_area;
    let area_ratio = active as f32 / focus_area;
    (outside_fraction, focus_fill, area_ratio)
}

fn sigmoid(value: f32) -> f32 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

fn candidate_border_fraction(logits: &[f32], width: usize, height: usize) -> f32 {
    if width == 0 || height == 0 {
        return 0.0;
    }
    let band = (width.min(height) / 64).clamp(2, 12);
    let mut active = 0usize;
    let mut total = 0usize;
    for y in 0..height {
        for x in 0..width {
            if x < band || y < band || x + band >= width || y + band >= height {
                total += 1;
                active += usize::from(logits[y * width + x] > 0.0);
            }
        }
    }
    active as f32 / total.max(1) as f32
}

fn mask_touches_crop_border(probabilities: &[f32], width: u32, height: u32) -> bool {
    let logits = probabilities
        .iter()
        .map(|value| if *value >= 0.5 { 1.0 } else { -1.0 })
        .collect::<Vec<_>>();
    candidate_border_fraction(&logits, width as usize, height as usize) > 0.025
}

fn keep_prompt_connected_component(
    probabilities: Vec<f32>,
    width: u32,
    height: u32,
    prompts: &[ObjectPrompt],
    source_width: u32,
    source_height: u32,
    crop: ObjectCropRect,
) -> Vec<f32> {
    let width_usize = width as usize;
    let height_usize = height as usize;
    let mut visited = vec![false; probabilities.len()];
    let mut keep = vec![false; probabilities.len()];
    let mut stack = Vec::new();

    for prompt in prompts.iter().filter(|prompt| prompt.kind.is_foreground()) {
        let sx = (prompt.point[0].clamp(0.0, 1.0) * source_width as f32 - crop.x as f32)
            / crop.width.max(1) as f32
            * width as f32;
        let sy = (prompt.point[1].clamp(0.0, 1.0) * source_height as f32 - crop.y as f32)
            / crop.height.max(1) as f32
            * height as f32;
        let mut x = sx.round().clamp(0.0, width.saturating_sub(1) as f32) as usize;
        let mut y = sy.round().clamp(0.0, height.saturating_sub(1) as f32) as usize;
        if probabilities[y * width_usize + x] < 0.5 {
            if let Some((nx, ny)) =
                nearest_foreground(&probabilities, width_usize, height_usize, x, y, 16)
            {
                x = nx;
                y = ny;
            } else {
                continue;
            }
        }
        let start = y * width_usize + x;
        if visited[start] {
            continue;
        }
        visited[start] = true;
        stack.push(start);
        while let Some(index) = stack.pop() {
            keep[index] = true;
            let px = index % width_usize;
            let py = index / width_usize;
            for (nx, ny) in neighbors4(px, py, width_usize, height_usize) {
                let next = ny * width_usize + nx;
                if !visited[next] && probabilities[next] >= 0.5 {
                    visited[next] = true;
                    stack.push(next);
                }
            }
        }
    }

    if !keep.iter().any(|value| *value) {
        // Fallback: retain the largest foreground component.
        let mut largest = Vec::new();
        visited.fill(false);
        for index in 0..probabilities.len() {
            if visited[index] || probabilities[index] < 0.5 {
                continue;
            }
            let mut component = Vec::new();
            visited[index] = true;
            stack.push(index);
            while let Some(current) = stack.pop() {
                component.push(current);
                let px = current % width_usize;
                let py = current / width_usize;
                for (nx, ny) in neighbors4(px, py, width_usize, height_usize) {
                    let next = ny * width_usize + nx;
                    if !visited[next] && probabilities[next] >= 0.5 {
                        visited[next] = true;
                        stack.push(next);
                    }
                }
            }
            if component.len() > largest.len() {
                largest = component;
            }
        }
        for index in largest {
            keep[index] = true;
        }
    }

    // SAM probabilities can contain small sub-threshold islands inside a
    // visually solid object. Fill enclosed holes in the selected silhouette,
    // then make the deep interior definitively opaque while keeping a narrow
    // soft band for edge-aware/ViTMatte refinement. This prevents texture from
    // appearing as pinholes without sacrificing hair or anti-aliased edges.
    fill_enclosed_component_holes(&mut keep, width_usize, height_usize);
    let background = keep.iter().map(|selected| !*selected).collect::<Vec<_>>();
    let near_background = dilate_component_band(&background, width_usize, height_usize, 3);
    let soft_band = dilate_component_band(&keep, width_usize, height_usize, 10);
    probabilities
        .into_iter()
        .enumerate()
        .map(|(index, probability)| {
            if keep[index] {
                if near_background[index] {
                    probability.max(0.82)
                } else {
                    1.0
                }
            } else if soft_band[index] {
                probability.min(0.49)
            } else {
                0.0
            }
        })
        .collect()
}

fn fill_enclosed_component_holes(selected: &mut [bool], width: usize, height: usize) {
    use std::collections::VecDeque;

    if width == 0 || height == 0 || selected.len() != width.saturating_mul(height) {
        return;
    }
    let mut exterior = vec![false; selected.len()];
    let mut queue = VecDeque::new();
    let seed = |x: usize, y: usize, exterior: &mut [bool], queue: &mut VecDeque<usize>| {
        let index = y * width + x;
        if !selected[index] && !exterior[index] {
            exterior[index] = true;
            queue.push_back(index);
        }
    };
    for x in 0..width {
        seed(x, 0, &mut exterior, &mut queue);
        if height > 1 {
            seed(x, height - 1, &mut exterior, &mut queue);
        }
    }
    for y in 0..height {
        seed(0, y, &mut exterior, &mut queue);
        if width > 1 {
            seed(width - 1, y, &mut exterior, &mut queue);
        }
    }
    while let Some(index) = queue.pop_front() {
        let x = index % width;
        let y = index / width;
        for (nx, ny) in neighbors4(x, y, width, height) {
            let next = ny * width + nx;
            if !selected[next] && !exterior[next] {
                exterior[next] = true;
                queue.push_back(next);
            }
        }
    }
    // Preserve genuine large holes (for example the opening inside a handle)
    // while removing tiny enclosed probability pinholes.
    let max_hole_area = (selected.len() / 512).clamp(32, 2048);
    let mut visited_holes = vec![false; selected.len()];
    for start in 0..selected.len() {
        if selected[start] || exterior[start] || visited_holes[start] {
            continue;
        }
        let mut component = Vec::new();
        visited_holes[start] = true;
        queue.push_back(start);
        while let Some(index) = queue.pop_front() {
            component.push(index);
            let x = index % width;
            let y = index / width;
            for (nx, ny) in neighbors4(x, y, width, height) {
                let next = ny * width + nx;
                if !selected[next] && !exterior[next] && !visited_holes[next] {
                    visited_holes[next] = true;
                    queue.push_back(next);
                }
            }
        }
        if component.len() <= max_hole_area {
            for index in component {
                selected[index] = true;
            }
        }
    }
}

fn dilate_component_band(selected: &[bool], width: usize, height: usize, radius: u16) -> Vec<bool> {
    use std::collections::VecDeque;

    let mut distance = vec![u16::MAX; selected.len()];
    let mut queue = VecDeque::new();
    for (index, is_selected) in selected.iter().copied().enumerate() {
        if is_selected {
            distance[index] = 0;
            queue.push_back(index);
        }
    }
    while let Some(index) = queue.pop_front() {
        let next_distance = distance[index].saturating_add(1);
        if next_distance > radius {
            continue;
        }
        let x = index % width;
        let y = index / width;
        for (nx, ny) in neighbors4(x, y, width, height) {
            let next = ny * width + nx;
            if next_distance < distance[next] {
                distance[next] = next_distance;
                queue.push_back(next);
            }
        }
    }
    distance.into_iter().map(|value| value <= radius).collect()
}

fn nearest_foreground(
    probabilities: &[f32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    radius: usize,
) -> Option<(usize, usize)> {
    let mut best = None;
    let mut best_distance = usize::MAX;
    let min_x = x.saturating_sub(radius);
    let max_x = (x + radius).min(width.saturating_sub(1));
    let min_y = y.saturating_sub(radius);
    let max_y = (y + radius).min(height.saturating_sub(1));
    for ny in min_y..=max_y {
        for nx in min_x..=max_x {
            if probabilities[ny * width + nx] >= 0.5 {
                let distance = nx.abs_diff(x).pow(2) + ny.abs_diff(y).pow(2);
                if distance < best_distance {
                    best_distance = distance;
                    best = Some((nx, ny));
                }
            }
        }
    }
    best
}

fn neighbors4(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> impl Iterator<Item = (usize, usize)> {
    let mut values = [(usize::MAX, usize::MAX); 4];
    let mut count = 0;
    if x > 0 {
        values[count] = (x - 1, y);
        count += 1;
    }
    if x + 1 < width {
        values[count] = (x + 1, y);
        count += 1;
    }
    if y > 0 {
        values[count] = (x, y - 1);
        count += 1;
    }
    if y + 1 < height {
        values[count] = (x, y + 1);
        count += 1;
    }
    values.into_iter().take(count)
}

fn edge_aware_refine(
    mut mask: Vec<f32>,
    width: u32,
    height: u32,
    rgba: &[u8],
    strength: f32,
) -> Vec<f32> {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.001 || rgba.len() != width as usize * height as usize * 4 {
        return mask;
    }
    let radius = (2.0 + strength * 4.0).round() as i32;
    let iterations = 2;
    let sigma_space = radius.max(1) as f32 * 0.75;
    let sigma_color = 0.04 + (1.0 - strength) * 0.10;
    let width_usize = width as usize;
    let height_usize = height as usize;

    for _ in 0..iterations {
        let source = mask.clone();
        mask.par_chunks_mut(width_usize)
            .enumerate()
            .for_each(|(y, row)| {
                for (x, output) in row.iter_mut().enumerate() {
                    let index = y * width_usize + x;
                    let value = source[index];
                    if !(0.02..=0.98).contains(&value) {
                        continue;
                    }
                    let base = index * 4;
                    let base_rgb = [
                        rgba[base] as f32 / 255.0,
                        rgba[base + 1] as f32 / 255.0,
                        rgba[base + 2] as f32 / 255.0,
                    ];
                    let mut weighted = 0.0;
                    let mut total = 0.0;
                    for dy in -radius..=radius {
                        let ny = (y as i32 + dy).clamp(0, height_usize as i32 - 1) as usize;
                        for dx in -radius..=radius {
                            let nx = (x as i32 + dx).clamp(0, width_usize as i32 - 1) as usize;
                            let neighbor = ny * width_usize + nx;
                            let rgb_index = neighbor * 4;
                            let dr = rgba[rgb_index] as f32 / 255.0 - base_rgb[0];
                            let dg = rgba[rgb_index + 1] as f32 / 255.0 - base_rgb[1];
                            let db = rgba[rgb_index + 2] as f32 / 255.0 - base_rgb[2];
                            let spatial = (dx * dx + dy * dy) as f32;
                            let color = dr * dr + dg * dg + db * db;
                            let weight = (-spatial / (2.0 * sigma_space * sigma_space)).exp()
                                * (-color / (2.0 * sigma_color * sigma_color)).exp();
                            weighted += source[neighbor] * weight;
                            total += weight;
                        }
                    }
                    if total > 0.0 {
                        let filtered = weighted / total;
                        *output = value + (filtered - value) * (0.45 + strength * 0.45);
                    }
                }
            });
    }
    mask
}

fn resize_probability_u8(
    values: &[f32],
    width: u32,
    height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<u8> {
    resize_f32(values, width, height, target_width, target_height)
        .into_iter()
        .map(|value| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect()
}

fn resize_f32(
    values: &[f32],
    width: u32,
    height: u32,
    target_width: u32,
    target_height: u32,
) -> Vec<f32> {
    if width == 0 || height == 0 || target_width == 0 || target_height == 0 {
        return Vec::new();
    }
    let mut output = vec![0.0; target_width as usize * target_height as usize];
    for y in 0..target_height {
        let source_y = ((y as f32 + 0.5) * height as f32 / target_height as f32 - 0.5)
            .clamp(0.0, height.saturating_sub(1) as f32);
        let y0 = source_y.floor() as usize;
        let y1 = (y0 + 1).min(height as usize - 1);
        let fy = source_y - y0 as f32;
        for x in 0..target_width {
            let source_x = ((x as f32 + 0.5) * width as f32 / target_width as f32 - 0.5)
                .clamp(0.0, width.saturating_sub(1) as f32);
            let x0 = source_x.floor() as usize;
            let x1 = (x0 + 1).min(width as usize - 1);
            let fx = source_x - x0 as f32;
            let top = values[y0 * width as usize + x0]
                + (values[y0 * width as usize + x1] - values[y0 * width as usize + x0]) * fx;
            let bottom = values[y1 * width as usize + x0]
                + (values[y1 * width as usize + x1] - values[y1 * width as usize + x0]) * fx;
            output[y as usize * target_width as usize + x as usize] = top + (bottom - top) * fy;
        }
    }
    output
}


    #[test]
        assert_eq!(
                .unwrap(),
            (336, 192)
        );
        assert!(
        );
    }

    #[test]
        assert_eq!((layout.resized_width, layout.resized_height), (1333, 750));
        assert_eq!((layout.padded_width, layout.padded_height), (1344, 768));
    }

    #[test]
    }

    #[test]
        let set_probability = |logits: &mut [f32], query, class, probability: f32| {
        };

        // Sky has the strongest individual semantic score. Two different
        // architecture classes have a larger combined score, which must not
        // let the much broader Architecture group steal the pixel.
        set_probability(&mut class_logits, 0, 2, 0.9);
        set_probability(&mut class_logits, 0, 12, 0.1);
        set_probability(&mut class_logits, 1, 0, 0.6);
        set_probability(&mut class_logits, 1, 20, 0.4);
        set_probability(&mut class_logits, 2, 1, 0.6);
        set_probability(&mut class_logits, 2, 126, 0.4);

        let query_classes = (0..queries)
            .map(|query| {
                let maximum = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let denominator = logits
                    .iter()
                    .map(|logit| (*logit - maximum).exp())
                    .sum::<f32>();
                logits[..ADE20K_CLASS_COUNT]
                    .iter()
                    .enumerate()
                    .filter_map(|(class, logit)| {
                        let probability = (*logit - maximum).exp() / denominator;
                            .then_some((class, probability))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mask_logits = vec![10.0; queries];

            &mask_logits,
            1,
            &query_classes::Sky.ade20k_class_ids(),
        );
            &mask_logits,
            1,
            &query_classes::Architecture.ade20k_class_ids(),
        );
        assert!(sky[0] > 0.5, "strongest sky class should select the pixel");
        assert!(
            architecture[0] < 0.5,
            "two weaker architecture classes must not win by category size"
        );
    }

    #[test]
-{}.onnx",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cancellation = AtomicBool::new(false);
        assert!(format!("{error:#}").contains("consent"));
        assert!(!missing.exists());
    }

    #[test]
        let vitmatte = PathBuf::from(std::env::var_os("AURAW_TEST_VITMATTE").unwrap());
        let runtime = PathBuf::from(std::env::var_os("AURAW_TEST_ORT").unwrap());
        let sha256 = sha256_file_hex(&runtime).unwrap();
            &model,
            &vitmatte,
            Some(&runtime),
            Some(&sha256),
            32,
            24,
            vec![127; 32 * 24 * 4]::Sky,
        )
        .unwrap();
        assert_eq!((result.width, result.height), (32, 24));
        assert_eq!(result.mask.len(), 32 * 24);
    }

    #[test]
    #[ignore = "manual integration probe requiring AURAW_TEST_VITMATTE and AURAW_TEST_ORT"]
        let runtime = PathBuf::from(std::env::var_os("AURAW_TEST_ORT").unwrap());
        let sha256 = sha256_file_hex(&runtime).unwrap();
        initialize_runtime(Some(&runtime), Some(&sha256)).unwrap();

        let width = 64;
        let height = 64;
        let mut rgba = vec![255u8; width * height * 4];
        let mut coarse = vec![0u8; width * height];
        for y in 0..height {
            for x in 0..width {
                let index = y * width + x;
                let value = if x < width / 2 { 48 } else { 208 };
                rgba[index * 4] = value;
                rgba[index * 4 + 1] = value;
                rgba[index * 4 + 2] = value;
                coarse[index] = if x < width / 2 { 255 } else { 0 };
            }
        }

        let refined =
            refine_mask_with_vitmatte(&vitmatte, &rgba, width as u32, height as u32, &coarse, 1.0)
                .unwrap();
        assert_eq!(refined.len(), coarse.len());
        assert!(refined.iter().any(|value| (1..=254).contains(value)));
    }
}

#[cfg(test)]
mod object_mask_tests {
    use super::*;
    use crate::pipeline::ObjectStroke;

    fn stroke(points: &[[f32; 2]], positive: bool) -> ObjectStroke {
        ObjectStroke {
            points: points.to_vec(),
            positive,
            brush_size: 0.0,
        }
    }

    #[test]
    fn sam_model_hashes_are_full_sha256_values() {
        for value in [SAM21_ENCODER_SHA256_HEX, SAM21_DECODER_SHA256_HEX] {
            assert_eq!(value.len(), 64);
            assert!(value.bytes().all(|byte| byte.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn prompt_sampling_adds_box_and_background_guards_within_limit() {
        let strokes = vec![
            stroke(&[[0.1, 0.1], [0.2, 0.2], [0.3, 0.3], [0.4, 0.4]], true),
            stroke(&[[0.8, 0.8], [0.7, 0.7], [0.6, 0.6]], false),
        ];
        let set = sampled_object_prompts(&strokes, 0.04, 1000, 600, SAM21_MAX_PROMPTS);
        assert!(set.prompts.len() <= SAM21_MAX_PROMPTS);
        assert!(set
            .prompts
            .iter()
            .any(|prompt| prompt.kind == ObjectPromptKind::Foreground));
        assert!(set
            .prompts
            .iter()
            .any(|prompt| prompt.kind == ObjectPromptKind::Background));
        assert!(set
            .prompts
            .iter()
            .any(|prompt| prompt.kind == ObjectPromptKind::BoxTopLeft));
        assert!(set
            .prompts
            .iter()
            .any(|prompt| prompt.kind == ObjectPromptKind::BoxBottomRight));
    }

    #[test]
    fn focus_and_guard_prompts_are_inside_the_adaptive_crop() {
        let strokes = vec![stroke(&[[0.50, 0.50], [0.62, 0.50]], true)];
        let set = sampled_object_prompts(&strokes, 0.035, 1000, 600, SAM21_MAX_PROMPTS);
        let crop = object_crop_for_prompts(1000, 600, set.focus, 0);
        for prompt in &set.prompts {
            let x = (prompt.point[0] * 1000.0) as u32;
            let y = (prompt.point[1] * 600.0) as u32;
            assert!(x >= crop.x && x <= crop.x + crop.width);
            assert!(y >= crop.y && y <= crop.y + crop.height);
        }
        assert_eq!(
            object_crop_for_prompts(1000, 600, set.focus, 2),
            ObjectCropRect {
                x: 0,
                y: 0,
                width: 1000,
                height: 600,
            }
        );
    }

    #[test]
    fn previous_logits_only_apply_to_prompt_extensions() {
        let original = vec![stroke(&[[0.4, 0.4], [0.5, 0.5]], true)];
        let mut extended = original.clone();
        extended.push(stroke(&[[0.7, 0.7]], false));
        assert!(strokes_extend(&original, &original));
        assert!(strokes_extend(&original, &extended));
        assert!(!strokes_extend(&extended, &original));
        assert!(!strokes_extend(&original, &[stroke(&[[0.1, 0.1]], true)]));
    }

    #[test]
    fn connected_component_cleanup_keeps_soft_edge_near_prompted_object() {
        let width = 50;
        let height = 5;
        let mut probabilities = vec![0.0; width * height];
        for y in 1..4 {
            for x in 1..4 {
                probabilities[y * width + x] = 0.9;
            }
            for x in 40..43 {
                probabilities[y * width + x] = 0.95;
            }
        }
        probabilities[2 * width + 4] = 0.35;
        let prompts = [ObjectPrompt {
            point: [2.0 / width as f32, 2.0 / height as f32],
            kind: ObjectPromptKind::Foreground,
        }];
        let cleaned = keep_prompt_connected_component(
            probabilities,
            width as u32,
            height as u32,
            &prompts,
            width as u32,
            height as u32,
            ObjectCropRect {
                x: 0,
                y: 0,
                width: width as u32,
                height: height as u32,
            },
        );
        assert!(cleaned[2 * width + 2] > 0.8);
        assert!(cleaned[2 * width + 4] > 0.3);
        assert_eq!(cleaned[2 * width + 41], 0.0);
    }

    #[test]
    fn probability_resize_preserves_endpoints() {
        let resized = resize_f32(&[0.0, 1.0], 2, 1, 5, 1);
        assert_eq!(resized.len(), 5);
        assert!(resized[0] <= 0.001);
        assert!(resized[4] >= 0.999);
        assert!(resized.windows(2).all(|pair| pair[0] <= pair[1]));
    }
}
