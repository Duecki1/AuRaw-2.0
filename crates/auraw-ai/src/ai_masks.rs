#[cfg(not(target_os = "android"))]
use crate::execution_provider::try_lock_interactive_ai_model;
use crate::execution_provider::{
    create_session_with_fallback, lock_interactive_ai_model, CpuFallbackProfile, FallbackSession,
    SessionOptions,
};
use crate::model_artifact::{
    ensure_artifact, verify_artifact, ArtifactSize, DownloadOptions, ModelArtifact,
};
use crate::pipeline::{LandscapeCategory, MaskImage};
use anyhow::{Context, Result};
use image::{imageops::FilterType, ImageBuffer, Luma, Rgba};
use ort::value::Tensor;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, MutexGuard, OnceLock,
    },
    time::Duration,
};

fn ensure_ai_not_cancelled(cancellation: &AtomicBool) -> Result<()> {
    anyhow::ensure!(
        !cancellation.load(Ordering::Acquire),
        "background task cancelled"
    );
    Ok(())
}

pub use crate::model_artifact::sha256_file_hex;

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
pub enum BiRefNetQuality {
    #[default]
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BiRefNetModelSpec {
    pub checkpoint: &'static str,
    pub download_label: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256_hex: &'static str,
    pub cache_filename: &'static str,
    /// ONNX tensors are NCHW. Lite-2K's pinned graph declares H=2560,
    /// W=1440, matching the official checkpoint despite its conventional
    /// "2560 x 1440" resolution label.
    pub input_width: u32,
    pub input_height: u32,
    pub explanation: &'static str,
}

impl BiRefNetModelSpec {
    fn artifact(self) -> ModelArtifact {
        ModelArtifact {
            name: self.checkpoint,
            url: Some(self.url),
            sha256: self.sha256_hex,
            size: ArtifactSize::Exact(self.bytes),
            progress_total: self.bytes,
        }
    }
}

impl BiRefNetQuality {
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    pub const fn model(self) -> BiRefNetModelSpec {
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
pub const LANDSCAPE_MODEL_BYTES: u64 = 141_790_090;
pub const LANDSCAPE_MODEL_URL: &str = "https://huggingface.co/onnx-community/maskformer-swin-base-ade/resolve/9366a4a18164800bcb3e01eb3ddb82160173c1c7/onnx/model_quantized.onnx";
pub const LANDSCAPE_MODEL_SHA256_HEX: &str =
    "9d46ef6268d4d37d3ec3733d961e2462ef2d8ff1c2a54e1122c4bbba561ad738";

const LANDSCAPE_ARTIFACT: ModelArtifact = ModelArtifact {
    name: "MaskFormer model",
    url: Some(LANDSCAPE_MODEL_URL),
    sha256: LANDSCAPE_MODEL_SHA256_HEX,
    size: ArtifactSize::Exact(LANDSCAPE_MODEL_BYTES),
    progress_total: LANDSCAPE_MODEL_BYTES,
};
const LANDSCAPE_DOWNLOAD: DownloadOptions = DownloadOptions {
    connect_timeout: Duration::from_secs(30),
    response_timeout: Duration::from_secs(60),
    body_timeout: Duration::from_secs(10 * 60),
    attempts: 1,
    resume: false,
};

const VITMATTE_ARTIFACT: ModelArtifact = ModelArtifact {
    name: "ViTMatte model",
    url: Some(VITMATTE_MODEL_URL),
    sha256: VITMATTE_MODEL_SHA256_HEX,
    size: ArtifactSize::Exact(VITMATTE_MODEL_BYTES),
    progress_total: VITMATTE_MODEL_BYTES,
};
const VITMATTE_DOWNLOAD: DownloadOptions = DownloadOptions {
    connect_timeout: Duration::from_secs(45),
    response_timeout: Duration::from_secs(60),
    body_timeout: Duration::from_secs(30 * 60),
    attempts: 5,
    resume: true,
};

const BIREFNET_DOWNLOAD: DownloadOptions = DownloadOptions {
    connect_timeout: Duration::from_secs(30),
    response_timeout: Duration::from_secs(30),
    body_timeout: Duration::from_secs(10 * 60),
    attempts: 1,
    resume: false,
};
const MASKFORMER_SHORTEST_EDGE: u32 = 800;
const MASKFORMER_LONGEST_EDGE: u32 = 1333;
const MASKFORMER_SIZE_DIVISOR: u32 = 32;
const MASKFORMER_CLASS_OUTPUT_COUNT: usize = 151;
const MASKFORMER_MAX_QUERIES: usize = 256;
const ADE20K_CLASS_COUNT: usize = 150;
// Discard only numerically insignificant query/class pairs. MaskFormer class
// probabilities are normally sparse; keeping them sparse avoids evaluating all
// 150 classes for every query and pixel while preserving semantic argmaxes.
const MASKFORMER_CLASS_PROBABILITY_EPSILON: f32 = 1e-5;
// Keep enough native guidance for individual hair/fur strands. The previous
// 1280/768 caps made ViTMatte's otherwise full-size alpha visibly stair-step
// after it was enlarged to modern high-resolution RAW dimensions.
const VITMATTE_MAX_EDGE_DESKTOP: u32 = 2048;
const VITMATTE_MAX_EDGE_ANDROID: u32 = 1024;
const VITMATTE_SIZE_DIVISOR: u32 = 32;

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

#[cfg(not(target_os = "android"))]
static SESSION: OnceLock<Mutex<Option<(BiRefNetQuality, FallbackSession)>>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
static VITMATTE_SESSION: OnceLock<Mutex<Option<FallbackSession>>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
static LANDSCAPE_SESSION: OnceLock<Mutex<Option<FallbackSession>>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
static DESKTOP_RUNTIME_IDENTITY: OnceLock<(PathBuf, String)> = OnceLock::new();
static RUNTIME_INITIALIZED: OnceLock<()> = OnceLock::new();
static RUNTIME_INIT_LOCK: Mutex<()> = Mutex::new(());
#[cfg(not(target_os = "android"))]
static AI_MASK_MODEL_CACHE_ENABLED: AtomicBool = AtomicBool::new(false);
#[cfg(not(target_os = "android"))]
type RuntimeProbeResult = (PathBuf, String);
#[cfg(not(target_os = "android"))]
static RUNTIME_PROBE_CACHE: OnceLock<Mutex<Option<RuntimeProbeResult>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AiMaskModel {
    Subject,
    Landscape,
    VitMatte,
    SamEncoder,
    SamDecoder,
}

#[cfg(not(target_os = "android"))]
fn clear_cached_session<T>(cache: &OnceLock<Mutex<Option<T>>>, label: &str) -> Result<()> {
    let Some(cache) = cache.get() else {
        return Ok(());
    };
    let mut session = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("{label} session lock was poisoned"))?;
    *session = None;
    Ok(())
}

pub(crate) fn unload_all_models_locked() -> Result<()> {
    #[cfg(not(target_os = "android"))]
    {
        clear_cached_session(&SESSION, "BiRefNet")?;
        clear_cached_session(&LANDSCAPE_SESSION, "MaskFormer")?;
        clear_cached_session(&VITMATTE_SESSION, "ViTMatte")?;
        clear_cached_session(&object::SAM_ENCODER_SESSION, "SAM encoder")?;
        clear_cached_session(&object::SAM_DECODER_SESSION, "SAM decoder")?;
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn unload_other_models_locked(active: AiMaskModel) -> Result<()> {
    if active != AiMaskModel::Subject {
        clear_cached_session(&SESSION, "BiRefNet")?;
    }
    if active != AiMaskModel::Landscape {
        clear_cached_session(&LANDSCAPE_SESSION, "MaskFormer")?;
    }
    if active != AiMaskModel::VitMatte {
        clear_cached_session(&VITMATTE_SESSION, "ViTMatte")?;
    }
    if active != AiMaskModel::SamEncoder {
        clear_cached_session(&object::SAM_ENCODER_SESSION, "SAM encoder")?;
    }
    if active != AiMaskModel::SamDecoder {
        clear_cached_session(&object::SAM_DECODER_SESSION, "SAM decoder")?;
    }
    Ok(())
}

fn prepare_model(active: AiMaskModel) -> Result<MutexGuard<'static, ()>> {
    let guard = lock_interactive_ai_model();
    #[cfg(not(target_os = "android"))]
    unload_other_models_locked(active)?;
    #[cfg(target_os = "android")]
    let _ = active;
    Ok(guard)
}

#[cfg(not(target_os = "android"))]
fn model_cache_enabled() -> bool {
    AI_MASK_MODEL_CACHE_ENABLED.load(Ordering::Acquire)
}

/// Keep at most the active mask model cached while the Masking tab is visible.
/// Disabling the cache never waits on an in-flight native inference; that
/// inference observes the flag before releasing the shared model gate and drops
/// its session then.
pub fn set_model_cache_enabled(enabled: bool) {
    #[cfg(not(target_os = "android"))]
    {
        AI_MASK_MODEL_CACHE_ENABLED.store(enabled, Ordering::Release);
        if !enabled {
            // Retry on every policy synchronization. This closes the narrow
            // race where inference checked the flag just before the UI left the
            // tab, while the UI's first non-blocking gate attempt still saw the
            // inference as active.
            if let Some(_guard) = try_lock_interactive_ai_model() {
                if let Err(error) = unload_all_models_locked() {
                    log::warn!("could not unload cached AI-mask models: {error:#}");
                }
            }
        }
    }
    #[cfg(target_os = "android")]
    let _ = enabled;
}

#[derive(Debug)]
pub enum SubjectMaskEvent {
    DownloadProgress {
        label: &'static str,
        downloaded: u64,
        total: u64,
    },
    Inferencing,
    Finished(Result<SubjectMaskResult, String>),
}

#[derive(Debug)]
pub struct SubjectMaskResult {
    pub width: u32,
    pub height: u32,
    pub mask: Vec<u8>,
}

impl SubjectMaskResult {
    /// Converts BiRefNet output into the raw shared subject-probability image.
    /// Subject refinement deliberately stays outside inference: `MaskStack`
    /// composites its persisted delta at atlas raster time, so regenerating or
    /// switching quality tiers can replace this raw probability map without
    /// destroying the user's refinement history.
    pub fn into_probability_mask(self) -> Option<MaskImage> {
        MaskImage::new(self.width, self.height, self.mask)
    }
}

#[derive(Debug)]
pub enum LandscapeMaskEvent {
    DownloadProgress {
        label: &'static str,
        downloaded: u64,
        total: u64,
    },
    Inferencing,
    Finished(Result<LandscapeMaskResult, String>),
}

#[derive(Debug)]
pub struct LandscapeMaskResult {
    pub width: u32,
    pub height: u32,
    pub mask: Vec<u8>,
}

pub struct LandscapeMaskWorkerRequest {
    pub model_path: PathBuf,
    pub vitmatte_path: PathBuf,
    pub allow_download: bool,
    pub runtime_path: Option<PathBuf>,
    pub runtime_sha256: Option<String>,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub category: LandscapeCategory,
}

struct LandscapeInferenceRequest<'a> {
    model_path: &'a Path,
    vitmatte_path: &'a Path,
    runtime_path: Option<&'a Path>,
    runtime_sha256: Option<&'a str>,
    dimensions: [u32; 2],
    rgba: Vec<u8>,
    category: LandscapeCategory,
}

pub fn spawn_landscape_mask(
    request: LandscapeMaskWorkerRequest,
    cancellation: Arc<AtomicBool>,
) -> mpsc::Receiver<LandscapeMaskEvent> {
    let LandscapeMaskWorkerRequest {
        model_path,
        vitmatte_path,
        allow_download,
        runtime_path,
        runtime_sha256,
        width,
        height,
        rgba,
        category,
    } = request;
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let spawn = std::thread::Builder::new()
        .name("auraw-onnx-landscape".to_owned())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (|| {
                    ensure_ai_not_cancelled(&cancellation)?;
                    ensure_landscape_model(
                        &model_path,
                        allow_download,
                        &cancellation,
                        |downloaded, total| {
                            let _ = worker_sender.send(LandscapeMaskEvent::DownloadProgress {
                                label: "MaskFormer landscape model",
                                downloaded,
                                total,
                            });
                        },
                    )?;
                    if allow_download {
                        ensure_vitmatte_model(
                            &vitmatte_path,
                            &cancellation,
                            |downloaded, total| {
                                let _ = worker_sender.send(
                                    LandscapeMaskEvent::DownloadProgress {
                                        label: "ViTMatte edge-refinement model",
                                        downloaded,
                                        total,
                                    },
                                );
                            },
                        )?;
                    } else {
                        verify_artifact(&vitmatte_path, VITMATTE_ARTIFACT).context(
                            "the pinned ViTMatte landscape refiner is unavailable or invalid; consent to its download again",
                        )?;
                    }
                    ensure_ai_not_cancelled(&cancellation)?;
                    let _ = worker_sender.send(LandscapeMaskEvent::Inferencing);
                    infer_landscape(LandscapeInferenceRequest {
                        model_path: &model_path,
                        vitmatte_path: &vitmatte_path,
                        runtime_path: runtime_path.as_deref(),
                        runtime_sha256: runtime_sha256.as_deref(),
                        dimensions: [width, height],
                        rgba,
                        category,
                    })
                })()
            }))
            .unwrap_or_else(|panic| {
                let message = panic
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown ONNX Runtime failure");
                Err(anyhow::anyhow!(
                    "ONNX Runtime terminated landscape inference: {message}"
                ))
            });
            let _ = worker_sender.send(LandscapeMaskEvent::Finished(
                result.map_err(|error| format!("{error:#}")),
            ));
        });
    if let Err(error) = spawn {
        let _ = sender.send(LandscapeMaskEvent::Finished(Err(format!(
            "could not start landscape-mask worker: {error}"
        ))));
    }
    receiver
}

fn ensure_landscape_model<F>(
    path: &Path,
    allow_download: bool,
    cancellation: &AtomicBool,
    mut progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    if !allow_download {
        return verify_artifact(path, LANDSCAPE_ARTIFACT).map_err(|error| {
            anyhow::anyhow!(
                "the pinned MaskFormer model is unavailable or invalid ({error:#}); consent to its download again"
            )
        });
    }
    ensure_artifact(
        path,
        LANDSCAPE_ARTIFACT,
        LANDSCAPE_DOWNLOAD,
        &mut progress,
        || ensure_ai_not_cancelled(cancellation),
    )
}

pub fn landscape_model_is_verified(path: &Path) -> bool {
    verify_artifact(path, LANDSCAPE_ARTIFACT).is_ok()
}

pub fn vitmatte_model_is_verified(path: &Path) -> bool {
    verify_artifact(path, VITMATTE_ARTIFACT).is_ok()
}

#[cfg(test)]
#[allow(dead_code)]
pub fn object_models_are_verified(encoder: &Path, decoder: &Path, vitmatte: &Path) -> bool {
    verify_artifact(encoder, object::SAM21_ENCODER_ARTIFACT).is_ok()
        && verify_artifact(decoder, object::SAM21_DECODER_ARTIFACT).is_ok()
        && verify_artifact(vitmatte, VITMATTE_ARTIFACT).is_ok()
}

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
    let model = quality.model();
    ensure_artifact(
        path,
        model.artifact(),
        BIREFNET_DOWNLOAD,
        |downloaded, total| {
            let _ = events.send(SubjectMaskEvent::DownloadProgress {
                label: model.download_label,
                downloaded,
                total,
            });
        },
        || ensure_ai_not_cancelled(cancellation),
    )
}

#[cfg(not(target_os = "android"))]
pub fn probe_runtime_subprocess(runtime_path: &Path, expected_sha256: &str) -> Result<()> {
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
pub fn run_runtime_probe_process(runtime_path: &Path, expected_sha256: &str) -> Result<()> {
    initialize_runtime(Some(runtime_path), Some(expected_sha256))
}

#[cfg(not(target_os = "android"))]
pub fn initialize_runtime(
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
pub fn initialize_runtime(
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

#[cfg(target_os = "linux")]
fn running_from_appimage() -> bool {
    std::env::var_os("APPIMAGE").is_some() || std::env::var_os("APPDIR").is_some()
}

#[cfg(not(target_os = "android"))]
fn cache_object_ai_sessions() -> bool {
    #[cfg(target_os = "linux")]
    {
        // AppImages frequently use a user-selected external ONNX Runtime. Do
        // not retain even the one active object-mask session between calls;
        // each temporary session still gets the full GPU -> CPU fallback.
        !running_from_appimage()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
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
        let _model_guard = prepare_model(AiMaskModel::Subject)?;
        // Mobile memory is more important than avoiding session startup. Drop
        // all model weights and allocator state immediately after inference.
        let mut session =
            create_session_with_fallback(model_path, SessionOptions::new("BiRefNet"))?;
        run_subject_session(&mut session, input, model.input_width, model.input_height)?
    };

    #[cfg(not(target_os = "android"))]
    let (output_width, output_height, logits) = {
        let _model_guard = prepare_model(AiMaskModel::Subject)?;
        let sessions = SESSION.get_or_init(|| Mutex::new(None));
        let mut guard = sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("BiRefNet session lock was poisoned"))?;
        if guard
            .as_ref()
            .is_none_or(|(cached_quality, _)| *cached_quality != quality)
        {
            // A quality change selects a different BiRefNet checkpoint. Drop
            // the old tier before constructing the new session so even two
            // variants of the same mask family never overlap in memory.
            *guard = None;
            let session =
                create_session_with_fallback(model_path, SessionOptions::new("BiRefNet"))?;
            *guard = Some((quality, session));
        }
        let result = {
            let (_, session) = guard.as_mut().ok_or_else(|| {
                anyhow::anyhow!("BiRefNet session initialization produced no session")
            })?;
            run_subject_session(session, input, model.input_width, model.input_height)
        };
        if !model_cache_enabled() {
            *guard = None;
        }
        result?
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
    session: &mut FallbackSession,
    input: Tensor<f32>,
    input_width: u32,
    input_height: u32,
) -> Result<(u32, u32, Vec<f32>)> {
    session.run_with_fallback("BiRefNet ONNX inference", |ort_session, _accelerated| {
        let outputs = ort_session
            .run(ort::inputs![&input])
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
    })
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MaskFormerInputLayout {
    resized_width: u32,
    resized_height: u32,
    padded_width: u32,
    padded_height: u32,
}

impl MaskFormerInputLayout {
    fn for_image(width: u32, height: u32) -> Result<Self> {
        anyhow::ensure!(width > 0 && height > 0, "landscape-mask input is empty");
        let shortest = width.min(height) as f64;
        let longest = width.max(height) as f64;
        let mut scale = MASKFORMER_SHORTEST_EDGE as f64 / shortest;
        if longest * scale > MASKFORMER_LONGEST_EDGE as f64 {
            scale = MASKFORMER_LONGEST_EDGE as f64 / longest;
        }
        let resized_width =
            ((width as f64 * scale).round() as u32).clamp(1, MASKFORMER_LONGEST_EDGE);
        let resized_height =
            ((height as f64 * scale).round() as u32).clamp(1, MASKFORMER_LONGEST_EDGE);
        let padded_width =
            resized_width.div_ceil(MASKFORMER_SIZE_DIVISOR) * MASKFORMER_SIZE_DIVISOR;
        let padded_height =
            resized_height.div_ceil(MASKFORMER_SIZE_DIVISOR) * MASKFORMER_SIZE_DIVISOR;
        Ok(Self {
            resized_width,
            resized_height,
            padded_width,
            padded_height,
        })
    }
}

fn infer_landscape(request: LandscapeInferenceRequest<'_>) -> Result<LandscapeMaskResult> {
    let LandscapeInferenceRequest {
        model_path,
        vitmatte_path,
        runtime_path,
        runtime_sha256,
        dimensions: [width, height],
        rgba,
        category,
    } = request;
    const MAX_LANDSCAPE_MASK_PIXELS: u64 = 17_000_000;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("landscape-mask input dimensions overflow")?;
    anyhow::ensure!(
        pixels > 0 && pixels <= MAX_LANDSCAPE_MASK_PIXELS,
        "landscape-mask input {width}x{height} exceeds the {MAX_LANDSCAPE_MASK_PIXELS}-pixel limit"
    );
    let expected_bytes = pixels
        .checked_mul(4)
        .and_then(|value| usize::try_from(value).ok())
        .context("landscape-mask input byte count overflow")?;
    anyhow::ensure!(
        rgba.len() == expected_bytes,
        "landscape-mask RGBA buffer has {} bytes, expected {expected_bytes}",
        rgba.len()
    );
    initialize_runtime(runtime_path, runtime_sha256)?;
    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, rgba)
        .context("invalid preview image for MaskFormer")?;
    let layout = MaskFormerInputLayout::for_image(width, height)?;
    let resized = image::imageops::resize(
        &image,
        layout.resized_width,
        layout.resized_height,
        FilterType::Triangle,
    );
    let plane = layout.padded_width as usize * layout.padded_height as usize;
    let mut values = vec![0.0f32; plane * 3];
    for (x, y, pixel) in resized.enumerate_pixels() {
        let index = y as usize * layout.padded_width as usize + x as usize;
        for channel in 0..3usize {
            values[channel * plane + index] =
                (pixel[channel] as f32 / 255.0 - IMAGENET_MEAN[channel]) / IMAGENET_STD[channel];
        }
    }
    let input = Tensor::from_array((
        [
            1usize,
            3,
            layout.padded_height as usize,
            layout.padded_width as usize,
        ],
        values,
    ))
    .context("create MaskFormer input tensor")?;

    #[cfg(target_os = "android")]
    let (output_width, output_height, probabilities) = {
        let _model_guard = prepare_model(AiMaskModel::Landscape)?;
        let mut session =
            create_session_with_fallback(model_path, SessionOptions::new("MaskFormer"))?;
        run_landscape_session(&mut session, input, category, layout)?
    };

    #[cfg(not(target_os = "android"))]
    let (output_width, output_height, probabilities) = {
        let _model_guard = prepare_model(AiMaskModel::Landscape)?;
        let sessions = LANDSCAPE_SESSION.get_or_init(|| Mutex::new(None));
        let mut guard = sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("MaskFormer session lock was poisoned"))?;
        if guard.is_none() {
            *guard = Some(create_session_with_fallback(
                model_path,
                SessionOptions::new("MaskFormer"),
            )?);
        }
        let result = {
            let session = guard.as_mut().ok_or_else(|| {
                anyhow::anyhow!("MaskFormer session initialization produced no session")
            })?;
            run_landscape_session(session, input, category, layout)
        };
        if !model_cache_enabled() {
            *guard = None;
        }
        result?
    };

    let coarse_mask =
        object::resize_probability_u8(&probabilities, output_width, output_height, width, height);
    let mask = refine_mask_with_vitmatte(
        vitmatte_path,
        image.as_raw(),
        width,
        height,
        &coarse_mask,
        1.0,
    )
    .context("refine landscape edges with ViTMatte")?;
    anyhow::ensure!(
        mask.len() == width as usize * height as usize,
        "MaskFormer output resize produced an invalid mask"
    );
    Ok(LandscapeMaskResult {
        width,
        height,
        mask,
    })
}

fn run_landscape_session(
    session: &mut FallbackSession,
    input: Tensor<f32>,
    category: LandscapeCategory,
    layout: MaskFormerInputLayout,
) -> Result<(u32, u32, Vec<f32>)> {
    session.run_with_fallback("MaskFormer ONNX inference", |ort_session, _accelerated| {
        let outputs = ort_session
            .run(ort::inputs![&input])
            .context("run MaskFormer ONNX inference")?;
        let class_output = outputs
            .get("class_queries_logits")
            .context("MaskFormer returned no class_queries_logits tensor")?;
        let mask_output = outputs
            .get("masks_queries_logits")
            .context("MaskFormer returned no masks_queries_logits tensor")?;
        let (class_shape, class_logits) = class_output
            .try_extract_tensor::<f32>()
            .context("read MaskFormer class-query output tensor")?;
        let (mask_shape, mask_logits) = mask_output
            .try_extract_tensor::<f32>()
            .context("read MaskFormer mask-query output tensor")?;
        let queries = validate_maskformer_class_output_shape(class_shape, class_logits.len())?;
        let (width, height) =
            validate_maskformer_mask_output_shape(mask_shape, mask_logits.len(), queries)?;
        anyhow::ensure!(
            class_logits.iter().all(|value| value.is_finite())
                && mask_logits.iter().all(|value| value.is_finite()),
            "MaskFormer output contains non-finite logits"
        );
        let plane = width as usize * height as usize;
        let class_ids = category.ade20k_class_ids();
        anyhow::ensure!(
            class_ids.iter().all(|class| *class < ADE20K_CLASS_COUNT),
            "landscape category contains an invalid ADE20K class"
        );

        let query_class_probabilities = (0..queries)
            .map(|query| {
                let logits = &class_logits[query * MASKFORMER_CLASS_OUTPUT_COUNT
                    ..(query + 1) * MASKFORMER_CLASS_OUTPUT_COUNT];
                let maximum = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                let denominator = logits
                    .iter()
                    .map(|logit| (*logit - maximum).exp())
                    .sum::<f32>();
                anyhow::ensure!(
                    denominator.is_finite() && denominator > 0.0,
                    "MaskFormer class-query softmax is invalid"
                );
                Ok(logits[..ADE20K_CLASS_COUNT]
                    .iter()
                    .enumerate()
                    .filter_map(|(class, logit)| {
                        let probability = (*logit - maximum).exp() / denominator;
                        (probability >= MASKFORMER_CLASS_PROBABILITY_EPSILON)
                            .then_some((class, probability))
                    })
                    .collect::<Vec<_>>())
            })
            .collect::<Result<Vec<Vec<(usize, f32)>>>>()?;

        let padded_probabilities = maskformer_semantic_category_mask(
            mask_logits,
            plane,
            &query_class_probabilities,
            class_ids,
        );
        crop_maskformer_probabilities(&padded_probabilities, width, height, layout)
    })
}

fn maskformer_semantic_category_mask(
    mask_logits: &[f32],
    plane: usize,
    query_class_probabilities: &[Vec<(usize, f32)>],
    selected_class_ids: &[usize],
) -> Vec<f32> {
    (0..plane)
        .into_par_iter()
        .map(|pixel| {
            let mut semantic_scores = [0.0f32; ADE20K_CLASS_COUNT];
            for (query, class_probabilities) in query_class_probabilities.iter().enumerate() {
                let mask_probability = sigmoid_probability(mask_logits[query * plane + pixel]);
                for &(class, class_probability) in class_probabilities {
                    semantic_scores[class] += class_probability * mask_probability;
                }
            }

            // Start with MaskFormer's reference semantic scores by combining
            // query-class and query-mask probabilities. Compare the strongest
            // selected class with the strongest competing class individually;
            // summing a broad AuRaw category first would unfairly favor groups
            // that contain more ADE20K labels.
            let (best_selected, best_other) = semantic_scores.iter().copied().enumerate().fold(
                (0.0f32, 0.0f32),
                |(selected, other), (class, score)| {
                    if !score.is_finite() || score <= 0.0 {
                        (selected, other)
                    } else if selected_class_ids.contains(&class) {
                        (selected.max(score), other)
                    } else {
                        (selected, other.max(score))
                    }
                },
            );
            let total = best_selected + best_other;
            if total <= f32::EPSILON {
                return 0.0;
            }

            // Keep a soft boundary for full-resolution upsampling and
            // ViTMatte, but require both relative class dominance and useful
            // absolute confidence. This avoids forcing an arbitrary landscape
            // label onto out-of-domain objects while retaining a calibrated
            // uncertain band around real semantic boundaries.
            let competition = best_selected / total;
            let confidence = best_selected.clamp(0.0, 1.0).sqrt();
            (competition * confidence).clamp(0.0, 1.0)
        })
        .collect()
}

fn validate_maskformer_class_output_shape(shape: &[i64], logits_len: usize) -> Result<usize> {
    anyhow::ensure!(
        shape.len() == 3
            && shape[0] == 1
            && shape[2] == MASKFORMER_CLASS_OUTPUT_COUNT as i64,
        "unexpected MaskFormer class output shape {shape:?}; expected [1, queries, {MASKFORMER_CLASS_OUTPUT_COUNT}]"
    );
    let queries =
        usize::try_from(shape[1]).context("MaskFormer query count is negative or too large")?;
    anyhow::ensure!(
        (1..=MASKFORMER_MAX_QUERIES).contains(&queries),
        "MaskFormer query count is invalid: {shape:?}"
    );
    let expected = queries
        .checked_mul(MASKFORMER_CLASS_OUTPUT_COUNT)
        .context("MaskFormer class output dimensions overflow")?;
    anyhow::ensure!(
        logits_len == expected,
        "MaskFormer class output has {logits_len} values, expected {expected}"
    );
    Ok(queries)
}

fn validate_maskformer_mask_output_shape(
    shape: &[i64],
    logits_len: usize,
    expected_queries: usize,
) -> Result<(u32, u32)> {
    anyhow::ensure!(
        shape.len() == 4 && shape[0] == 1 && shape[1] == expected_queries as i64,
        "unexpected MaskFormer mask output shape {shape:?}; expected [1, {expected_queries}, H, W]"
    );
    let height =
        usize::try_from(shape[2]).context("MaskFormer output height is negative or too large")?;
    let width =
        usize::try_from(shape[3]).context("MaskFormer output width is negative or too large")?;
    anyhow::ensure!(
        width > 0 && height > 0 && width <= 2048 && height <= 2048,
        "MaskFormer output dimensions are invalid: {shape:?}"
    );
    let expected = expected_queries
        .checked_mul(width)
        .and_then(|value| value.checked_mul(height))
        .context("MaskFormer mask output dimensions overflow")?;
    anyhow::ensure!(
        logits_len == expected,
        "MaskFormer mask output has {logits_len} values, expected {expected}"
    );
    Ok((
        u32::try_from(width).context("MaskFormer output width exceeds u32")?,
        u32::try_from(height).context("MaskFormer output height exceeds u32")?,
    ))
}

fn crop_maskformer_probabilities(
    probabilities: &[f32],
    width: u32,
    height: u32,
    layout: MaskFormerInputLayout,
) -> Result<(u32, u32, Vec<f32>)> {
    anyhow::ensure!(
        probabilities.len() == width as usize * height as usize,
        "MaskFormer probability dimensions do not match their buffer"
    );
    let valid_width = ((u64::from(width) * u64::from(layout.resized_width)
        + u64::from(layout.padded_width) / 2)
        / u64::from(layout.padded_width))
    .clamp(1, u64::from(width)) as u32;
    let valid_height = ((u64::from(height) * u64::from(layout.resized_height)
        + u64::from(layout.padded_height) / 2)
        / u64::from(layout.padded_height))
    .clamp(1, u64::from(height)) as u32;
    let mut cropped = Vec::new();
    cropped
        .try_reserve_exact(valid_width as usize * valid_height as usize)
        .context("reserve cropped MaskFormer probabilities")?;
    for y in 0..valid_height as usize {
        let row = y * width as usize;
        cropped.extend_from_slice(&probabilities[row..row + valid_width as usize]);
    }
    Ok((valid_width, valid_height, cropped))
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

fn ensure_vitmatte_model<F>(path: &Path, cancellation: &AtomicBool, progress: F) -> Result<()>
where
    F: FnMut(u64, u64),
{
    ensure_artifact(
        path,
        VITMATTE_ARTIFACT,
        VITMATTE_DOWNLOAD,
        progress,
        || ensure_ai_not_cancelled(cancellation),
    )
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
        let _model_guard = prepare_model(AiMaskModel::VitMatte)?;
        let mut session =
            create_session_with_fallback(model_path, SessionOptions::new("ViTMatte"))?;
        run_vitmatte_session(&mut session, input)?
    };
    #[cfg(not(target_os = "android"))]
    let (output_width, output_height, alpha) = {
        let _model_guard = prepare_model(AiMaskModel::VitMatte)?;
        if model_cache_enabled() && cache_object_ai_sessions() {
            let sessions = VITMATTE_SESSION.get_or_init(|| Mutex::new(None));
            let mut session = sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("ViTMatte session lock was poisoned"))?;
            if session.is_none() {
                *session = Some(create_session_with_fallback(
                    model_path,
                    SessionOptions::new("ViTMatte"),
                )?);
            }
            let result = run_vitmatte_session(
                session
                    .as_mut()
                    .context("ViTMatte session initialization produced no session")?,
                input,
            );
            if !model_cache_enabled() {
                *session = None;
            }
            result?
        } else {
            let mut session =
                create_session_with_fallback(model_path, SessionOptions::new("ViTMatte"))?;
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

fn run_vitmatte_session(
    session: &mut FallbackSession,
    input: Tensor<f32>,
) -> Result<(u32, u32, Vec<f32>)> {
    session.run_with_fallback("ViTMatte ONNX inference", |ort_session, _accelerated| {
        let outputs = ort_session
            .run(ort::inputs![&input])
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
    })
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
    fn birefnet_defaults_to_the_low_quality_model() {
        assert_eq!(BiRefNetQuality::default(), BiRefNetQuality::Low);
    }

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

#[cfg(test)]
mod landscape_mask_tests {
    use super::*;

    #[test]
    fn maskformer_shape_guards_require_matching_queries() {
        assert_eq!(
            validate_maskformer_class_output_shape(&[1, 100, 151], 100 * 151).unwrap(),
            100
        );
        assert_eq!(
            validate_maskformer_mask_output_shape(&[1, 100, 192, 336], 100 * 192 * 336, 100)
                .unwrap(),
            (336, 192)
        );
        assert!(validate_maskformer_class_output_shape(&[1, 100, 150], 100 * 150).is_err());
        assert!(
            validate_maskformer_mask_output_shape(&[1, 99, 192, 336], 99 * 192 * 336, 100).is_err()
        );
    }

    #[test]
    fn maskformer_input_preserves_aspect_ratio_and_pads_to_swin_divisor() {
        let layout = MaskFormerInputLayout::for_image(1600, 900).unwrap();
        assert_eq!((layout.resized_width, layout.resized_height), (1333, 750));
        assert_eq!((layout.padded_width, layout.padded_height), (1344, 768));
        assert_eq!(layout.padded_width % MASKFORMER_SIZE_DIVISOR, 0);
        assert_eq!(layout.padded_height % MASKFORMER_SIZE_DIVISOR, 0);
    }

    #[test]
    fn landscape_categories_map_to_valid_ade20k_classes() {
        for category in LandscapeCategory::ALL {
            assert!(!category.ade20k_class_ids().is_empty());
            assert!(category
                .ade20k_class_ids()
                .iter()
                .all(|class| *class < ADE20K_CLASS_COUNT));
        }
    }

    #[test]
    fn maskformer_resolves_a_class_before_grouping_landscape_categories() {
        let queries = 3;
        let mut class_logits = vec![-30.0f32; queries * MASKFORMER_CLASS_OUTPUT_COUNT];
        let set_probability = |logits: &mut [f32], query, class, probability: f32| {
            logits[query * MASKFORMER_CLASS_OUTPUT_COUNT + class] = probability.ln();
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
                let logits = &class_logits[query * MASKFORMER_CLASS_OUTPUT_COUNT
                    ..(query + 1) * MASKFORMER_CLASS_OUTPUT_COUNT];
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
                        (probability >= MASKFORMER_CLASS_PROBABILITY_EPSILON)
                            .then_some((class, probability))
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let mask_logits = vec![10.0; queries];

        let sky = maskformer_semantic_category_mask(
            &mask_logits,
            1,
            &query_classes,
            LandscapeCategory::Sky.ade20k_class_ids(),
        );
        let architecture = maskformer_semantic_category_mask(
            &mask_logits,
            1,
            &query_classes,
            LandscapeCategory::Architecture.ade20k_class_ids(),
        );
        assert!(sky[0] > 0.5, "strongest sky class should select the pixel");
        assert!(
            architecture[0] < 0.5,
            "two weaker architecture classes must not win by category size"
        );
    }

    #[test]
    fn landscape_worker_cannot_download_without_explicit_authorization() {
        let missing = std::env::temp_dir().join(format!(
            "auraw-missing-maskformer-{}-{}.onnx",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cancellation = AtomicBool::new(false);
        let error = ensure_landscape_model(&missing, false, &cancellation, |_, _| {}).unwrap_err();
        assert!(format!("{error:#}").contains("consent"));
        assert!(!missing.exists());
    }

    #[test]
    #[ignore = "manual integration probe requiring AURAW_TEST_MASKFORMER, AURAW_TEST_VITMATTE, and AURAW_TEST_ORT"]
    fn pinned_maskformer_runs_through_onnx_runtime() {
        let model = PathBuf::from(std::env::var_os("AURAW_TEST_MASKFORMER").unwrap());
        let vitmatte = PathBuf::from(std::env::var_os("AURAW_TEST_VITMATTE").unwrap());
        let runtime = PathBuf::from(std::env::var_os("AURAW_TEST_ORT").unwrap());
        let sha256 = sha256_file_hex(&runtime).unwrap();
        let result = infer_landscape(LandscapeInferenceRequest {
            model_path: &model,
            vitmatte_path: &vitmatte,
            runtime_path: Some(&runtime),
            runtime_sha256: Some(&sha256),
            dimensions: [32, 24],
            rgba: vec![127; 32 * 24 * 4],
            category: LandscapeCategory::Sky,
        })
        .unwrap();
        assert_eq!((result.width, result.height), (32, 24));
        assert_eq!(result.mask.len(), 32 * 24);
    }

    #[test]
    #[ignore = "manual integration probe requiring AURAW_TEST_VITMATTE and AURAW_TEST_ORT"]
    fn pinned_vitmatte_refines_a_landscape_boundary() {
        let vitmatte = PathBuf::from(std::env::var_os("AURAW_TEST_VITMATTE").unwrap());
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

mod object;

pub use object::{
    spawn_object_mask, ObjectCropRect, ObjectInferenceCache, ObjectMaskEvent, ObjectMaskRequest,
    ObjectMaskResult, SamTensorData, SAM21_DECODER_MODEL_URL, SAM21_DECODER_SHA256_HEX,
    SAM21_ENCODER_MODEL_URL, SAM21_ENCODER_SHA256_HEX, SAM21_MODEL_BYTES_ESTIMATE,
};
