use crate::execution_provider::{CpuFallbackProfile, FallbackSession, SessionOptions};
use crate::model_artifact::{ArtifactSize, DownloadOptions, ModelArtifact};
use crate::model_install::ModelInstallSpec;
use crate::model_runtime::{with_model_session, AiModel, AiRuntimeContext, ModelRetention};
use crate::pipeline::{MaskImage};
use crate::ModelDownloadProgress;
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
        mpsc, Arc, Mutex, OnceLock,
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
    pub input_width: u32,
    pub input_height: u32,
    pub explanation: &'static str,
}

impl BiRefNetModelSpec {
    fn install(self) -> ModelInstallSpec {
        ModelInstallSpec {
            artifact: ModelArtifact {
                name: self.checkpoint,
                url: Some(self.url),
                sha256: self.sha256_hex,
                size: ArtifactSize::Exact(self.bytes),
                progress_total: self.bytes,
            },
            download: BIREFNET_DOWNLOAD,
            progress_label: self.download_label,
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

    pub const fn requires_cpu_to_protect_interactive_gpu(self) -> bool {
        matches!(self, Self::Medium | Self::High)
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
                explanation: "Lite-2K at its native 2560 x 1440 tensor. More boundary detail with a 331 MB download; runs on CPU to keep the interactive GPU stable.",
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
                explanation: "The dedicated BiRefNet HR checkpoint at 2048 x 2048. Best fine-detail quality; a 1.10 GB download with the highest memory use. It runs on CPU to keep the interactive GPU stable.",
            },
        }
    }
}
pub const VITMATTE_MODEL_BYTES: u64 = 103_885_865;
pub const VITMATTE_MODEL_URL: &str = "https://huggingface.co/Xenova/vitmatte-small-composition-1k/resolve/5e04250c42d7a03dc125b13adb415a47584ec60b/onnx/model.onnx";
pub const VITMATTE_MODEL_SHA256_HEX: &str =
    "bf28d2e0be2c073286e88d60ad649d7123da2749a2d99133fd1098d5887e0225";

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
const VITMATTE_INSTALL: ModelInstallSpec = ModelInstallSpec {
    artifact: VITMATTE_ARTIFACT,
    download: VITMATTE_DOWNLOAD,
    progress_label: "ViTMatte edge-refinement model",
};

const BIREFNET_DOWNLOAD: DownloadOptions = DownloadOptions {
    connect_timeout: Duration::from_secs(30),
    response_timeout: Duration::from_secs(30),
    body_timeout: Duration::from_secs(10 * 60),
    attempts: 1,
    resume: false,
};
const VITMATTE_MAX_EDGE_ANDROID: u32 = 1024;
const VITMATTE_SIZE_DIVISOR: u32 = 32;

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

#[cfg(not(target_os = "android"))]
static DESKTOP_RUNTIME_IDENTITY: OnceLock<(PathBuf, String)> = OnceLock::new();
static RUNTIME_INITIALIZED: OnceLock<()> = OnceLock::new();
static RUNTIME_INIT_LOCK: Mutex<()> = Mutex::new(());
#[cfg(not(target_os = "android"))]
type RuntimeProbeResult = (PathBuf, String);
#[cfg(not(target_os = "android"))]
static RUNTIME_PROBE_CACHE: OnceLock<Mutex<Option<RuntimeProbeResult>>> = OnceLock::new();

fn subject_model_id(quality: BiRefNetQuality) -> AiModel {
    match quality {
        BiRefNetQuality::Low => AiModel::BiRefNetLow,
        BiRefNetQuality::Medium => AiModel::BiRefNetMedium,
        BiRefNetQuality::High => AiModel::BiRefNetHigh,
    }
}

fn mask_model_retention(cache_supported: bool) -> ModelRetention {
    #[cfg(target_os = "android")]
    {
        let _ = cache_supported;
        ModelRetention::OneShot
    }
    #[cfg(not(target_os = "android"))]
    {
        if cache_supported {
            ModelRetention::Interactive(AiRuntimeContext::Masks)
        } else {
            ModelRetention::OneShot
        }
    }
}

pub fn birefnet_model_is_verified(quality: BiRefNetQuality, path: &Path) -> bool {
    quality.model().install().is_installed(path)
}

pub fn object_models_are_verified(encoder: &Path, decoder: &Path, vitmatte: &Path) -> bool {
    object::SAM21_ENCODER_INSTALL.is_installed(encoder)
        && object::SAM21_DECODER_INSTALL.is_installed(decoder)
        && VITMATTE_INSTALL.is_installed(vitmatte)
}

pub struct SubjectMaskWorkerRequest {
    pub quality: BiRefNetQuality,
    pub model_path: PathBuf,
    pub allow_download: bool,
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
        allow_download,
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
                    ensure_model(
                        quality,
                        &model_path,
                        allow_download,
                        &worker_sender,
                        &cancellation,
                    )?;
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
    allow_download: bool,
    events: &mpsc::Sender<SubjectMaskEvent>,
    cancellation: &AtomicBool,
) -> Result<()> {
    quality.model().install().ensure_installed(
        path,
        allow_download,
        |progress| {
            let _ = events.send(SubjectMaskEvent::DownloadProgress(progress));
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

fn cache_object_ai_sessions() -> bool {
    #[cfg(target_os = "android")]
    {
        false
    }
    #[cfg(all(not(target_os = "android"), target_os = "linux"))]
    {
        !running_from_appimage()
    }
    #[cfg(all(not(target_os = "android"), not(target_os = "linux")))]
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

    let subject_session_options = if quality.requires_cpu_to_protect_interactive_gpu() {
        SessionOptions::new("BiRefNet").cpu_only()
    } else {
        SessionOptions::new("BiRefNet")
    };
    let (output_width, output_height, logits) = with_model_session(
        subject_model_id(quality),
        model_path,
        subject_session_options,
        mask_model_retention(true),
        |session| run_subject_session(session, input, model.input_width, model.input_height),
    )?;

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
= request;
    anyhow::ensure!(
    );
    let expected_bytes = pixels
        .checked_mul(4)
        .and_then(|value| usize::try_from(value).ok())
    anyhow::ensure!(
        rgba.len() == expected_bytes,
        rgba.len()
    );
    initialize_runtime(runtime_path, runtime_sha256)?;
    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, rgba)
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

    let (output_width, output_height, probabilities) = with_model_session(
        model_path,
        mask_model_retention(true),
    )?;

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
    anyhow::ensure!(
        mask.len() == width as usize * height as usize,
    );
        width,
        height,
        mask,
    })
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

fn ensure_vitmatte_model<F>(
    path: &Path,
    allow_download: bool,
    cancellation: &AtomicBool,
    progress: F,
) -> Result<()>
where
    F: FnMut(ModelDownloadProgress),
{
    VITMATTE_INSTALL.ensure_installed(path, allow_download, progress, || {
        ensure_ai_not_cancelled(cancellation)
    })
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

    let (output_width, output_height, alpha) = with_model_session(
        AiModel::ViTMatte,
        model_path,
        SessionOptions::new("ViTMatte"),
        mask_model_retention(cache_object_ai_sessions()),
        |session| run_vitmatte_session(session, input),
    )?;

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
    fn high_resolution_subject_models_preserve_interactive_gpu_headroom() {
        assert!(!BiRefNetQuality::Low.requires_cpu_to_protect_interactive_gpu());
        assert!(BiRefNetQuality::Medium.requires_cpu_to_protect_interactive_gpu());
        assert!(BiRefNetQuality::High.requires_cpu_to_protect_interactive_gpu());
    }

    #[test]
    fn sigmoid_probabilities_keep_model_calibration() {
        assert!((sigmoid_probability(0.0) - 0.5).abs() < f32::EPSILON);
        assert!((sigmoid_probability(2.0) - 0.880797).abs() < 1e-6);
        assert!((sigmoid_probability(-2.0) - 0.119203).abs() < 1e-6);
    }
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
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
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
            model_path: &model,
            vitmatte_path: &vitmatte,
            runtime_path: Some(&runtime),
            runtime_sha256: Some(&sha256),
            dimensions: [32, 24],
            rgba: vec![127; 32 * 24 * 4],
        })
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

mod object;

pub use object::{
    spawn_object_mask, ObjectCropRect, ObjectInferenceCache, ObjectMaskEvent, ObjectMaskRequest,
    ObjectMaskResult, ObjectMaskWorkerRequest, SamTensorData, SAM21_DECODER_MODEL_URL,
    SAM21_DECODER_SHA256_HEX, SAM21_ENCODER_MODEL_URL, SAM21_ENCODER_SHA256_HEX,
    SAM21_MODEL_BYTES_ESTIMATE,
};
