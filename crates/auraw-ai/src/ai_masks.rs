#[cfg(not(target_os = "android"))]
use crate::execution_provider::try_lock_interactive_ai_model;
use crate::execution_provider::{
    create_session_with_fallback, lock_interactive_ai_model, CpuFallbackProfile, FallbackSession,
    SessionOptions,
};
use crate::model_artifact::{
    ensure_artifact, verify_artifact, ArtifactSize, DownloadOptions, ModelArtifact,
};
use crate::pipeline::{MaskImage};
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
// probabilities are normally sparse; keeping them sparse avoids evaluating all
// 150 classes for every query and pixel while preserving semantic argmaxes.
const VITMATTE_MAX_EDGE_ANDROID: u32 = 1024;
const VITMATTE_SIZE_DIVISOR: u32 = 32;

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

#[cfg(not(target_os = "android"))]
static SESSION: OnceLock<Mutex<Option<(BiRefNetQuality, FallbackSession)>>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
static VITMATTE_SESSION: OnceLock<Mutex<Option<FallbackSession>>> = OnceLock::new();
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

)
        .collect()
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
    ObjectMaskResult, SamTensorData, SAM21_DECODER_MODEL_URL, SAM21_DECODER_SHA256_HEX,
    SAM21_ENCODER_MODEL_URL, SAM21_ENCODER_SHA256_HEX, SAM21_MODEL_BYTES_ESTIMATE,
};
