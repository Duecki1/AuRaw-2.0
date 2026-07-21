use anyhow::{Context, Result};
use ort::{session::Session, value::Tensor};
use ring::digest::{Context as Sha256Context, SHA256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::pipeline::{rasterize_inpaint_dabs_binary, BrushDab, InpaintPatch};

pub const LAMA_MODEL_URL: &str =
    "https://huggingface.co/Carve/LaMa-ONNX/resolve/main/lama_fp32.onnx";
pub const LAMA_MODEL_BYTES: u64 = 208_044_816;
pub const LAMA_MODEL_SHA256_HEX: &str =
    "1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6";
const LAMA_MODEL_SHA256: [u8; 32] = [
    0x1f, 0xae, 0xf5, 0x30, 0x1d, 0x78, 0xdb, 0x7d, 0xda, 0x50, 0x2f, 0xe5, 0x99, 0x66, 0x95, 0x7e,
    0xc4, 0xb7, 0x9d, 0xd6, 0x4e, 0x16, 0xf0, 0x3e, 0xd9, 0x69, 0x13, 0xc7, 0xa4, 0xeb, 0x68, 0xd6,
];
pub const LAMA_EDGE: u32 = 512;
const MAX_INPAINT_PIXELS: u64 = 20_000_000;

#[cfg(not(target_os = "android"))]
static LAMA_SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
static LAMA_VERIFIED_MODEL: OnceLock<Mutex<Option<LamaModelIdentity>>> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
struct LamaModelIdentity {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

#[derive(Clone, Debug)]
pub struct PreparedInpaintSource {
    /// Neutral scene-linear Rec.2020 RGB already resized to LaMa's fixed
    /// 512x512 model input. The editor/render pipeline stays wide-gamut and
    /// high precision; only the small model-facing crop is transferred back to
    /// the CPU.
    pub rgb_rec2020: Vec<f32>,
    /// Full-resolution square patch that LaMa will reconstruct.
    pub width: u32,
    pub height: u32,
    pub origin_x: u32,
    pub origin_y: u32,
    pub full_width: u32,
    pub full_height: u32,
}

#[derive(Clone, Debug)]
pub struct InpaintRequest {
    pub source: PreparedInpaintSource,
    /// Brush dabs remain in full-image normalized coordinates. The worker
    /// localizes them only after the exact full-resolution source crop is known.
    pub dabs: Vec<BrushDab>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InpaintCaptureRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InpaintPatchRect {
    pub x: u32,
    pub y: u32,
    pub size: u32,
}

fn stroke_bounds(dabs: &[BrushDab], width: u32, height: u32) -> Option<(f32, f32, f32, f32)> {
    if dabs.is_empty() || width == 0 || height == 0 {
        return None;
    }
    let image_min = width.min(height) as f32;
    let mut min_x = width as f32;
    let mut min_y = height as f32;
    let mut max_x = 0.0f32;
    let mut max_y = 0.0f32;
    let mut found = false;
    for dab in dabs {
        if dab.opacity <= 0.0 {
            continue;
        }
        let radius = dab.size.clamp(0.0025, 0.5) * image_min + 2.0;
        let cx = dab.center[0] * width as f32;
        let cy = dab.center[1] * height as f32;
        min_x = min_x.min(cx - radius);
        min_y = min_y.min(cy - radius);
        max_x = max_x.max(cx + radius);
        max_y = max_y.max(cy + radius);
        found = true;
    }
    found.then_some((min_x, min_y, max_x, max_y))
}

/// Computes the actual square full-resolution area that will be fed to LaMa
/// after resizing to 512x512. This mirrors AuRaw 1's ~1.5x padded context crop.
pub fn inpaint_patch_rect(dabs: &[BrushDab], width: u32, height: u32) -> Option<InpaintPatchRect> {
    let (min_x, min_y, max_x, max_y) = stroke_bounds(dabs, width, height)?;
    let shorter = width.min(height);
    let bounds_width = (max_x.ceil() as i64 - min_x.floor() as i64).max(1) as u32;
    let bounds_height = (max_y.ceil() as i64 - min_y.floor() as i64).max(1) as u32;
    let base = bounds_width.max(bounds_height);
    let size = ((base as f32 * 1.5).ceil() as u32).max(64).min(shorter);
    let center_x = ((min_x + max_x) * 0.5).round() as i64;
    let center_y = ((min_y + max_y) * 0.5).round() as i64;
    let x = (center_x - i64::from(size) / 2).clamp(0, i64::from(width.saturating_sub(size))) as u32;
    let y =
        (center_y - i64::from(size) / 2).clamp(0, i64::from(height.saturating_sub(size))) as u32;
    Some(InpaintPatchRect { x, y, size })
}

/// Computes a full-resolution RAW region around the exact LaMa crop. The extra
/// halo is outside LaMa's own context crop and exists only to keep demosaic/GPU
/// crop boundaries away from the pixels that will actually be generated.
pub fn inpaint_capture_rect(
    dabs: &[BrushDab],
    width: u32,
    height: u32,
) -> Option<InpaintCaptureRect> {
    let patch = inpaint_patch_rect(dabs, width, height)?;
    let halo = 32u32;
    let x0 = patch.x.saturating_sub(halo);
    let y0 = patch.y.saturating_sub(halo);
    let x1 = patch
        .x
        .saturating_add(patch.size)
        .saturating_add(halo)
        .min(width);
    let y1 = patch
        .y
        .saturating_add(patch.size)
        .saturating_add(halo)
        .min(height);
    (x1 > x0 && y1 > y0).then_some(InpaintCaptureRect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

#[derive(Debug)]
pub enum InpaintEvent {
    DownloadProgress { downloaded: u64, total: u64 },
    Inferencing,
    Finished(Result<InpaintPatch, String>),
}

pub fn spawn_inpaint(
    model_path: PathBuf,
    runtime_path: Option<PathBuf>,
    runtime_sha256: Option<String>,
    request: InpaintRequest,
) -> mpsc::Receiver<InpaintEvent> {
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let spawn = std::thread::Builder::new()
        .name("auraw-onnx-lama-inpaint".to_owned())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (|| {
                    ensure_lama_model(&model_path, &worker_sender)?;
                    let _ = worker_sender.send(InpaintEvent::Inferencing);
                    infer_lama(
                        &model_path,
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
                    "ONNX Runtime terminated LaMa inpainting: {message}"
                ))
            });
            let _ = worker_sender.send(InpaintEvent::Finished(
                result.map_err(|error| format!("{error:#}")),
            ));
        });
    if let Err(error) = spawn {
        let _ = sender.send(InpaintEvent::Finished(Err(format!(
            "could not start LaMa inpainting worker: {error}"
        ))));
    }
    receiver
}

fn ensure_lama_model(path: &Path, events: &mpsc::Sender<InpaintEvent>) -> Result<()> {
    // Hashing the 208 MB model on every released brush stroke caused a visible
    // pause before inference began. Keep the strong SHA-256 verification, but
    // only repeat it when the path/size/mtime identity changes.
    if lama_model_identity(path)
        .ok()
        .is_some_and(|identity| cached_lama_model_identity().as_ref() == Some(&identity))
    {
        return Ok(());
    }
    if verify_lama_model(path).is_ok() {
        remember_lama_model_identity(path);
        return Ok(());
    }
    if path.exists() {
        log::warn!("discarding invalid LaMa cache {}", path.display());
        fs::remove_file(path)
            .with_context(|| format!("remove invalid LaMa model {}", path.display()))?;
    }
    download_lama_model(path, events)?;
    verify_lama_model(path).context("verify published LaMa model")?;
    remember_lama_model_identity(path);
    Ok(())
}

fn lama_model_identity(path: &Path) -> Result<LamaModelIdentity> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("read LaMa model metadata {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "LaMa cache is not a regular file");
    Ok(LamaModelIdentity {
        path: path.to_path_buf(),
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn cached_lama_model_identity() -> Option<LamaModelIdentity> {
    LAMA_VERIFIED_MODEL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|identity| (*identity).clone())
}

fn remember_lama_model_identity(path: &Path) {
    let Ok(identity) = lama_model_identity(path) else {
        return;
    };
    if let Ok(mut cached) = LAMA_VERIFIED_MODEL.get_or_init(|| Mutex::new(None)).lock() {
        *cached = Some(identity);
    }
}

fn verify_lama_model(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("read LaMa model metadata {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "LaMa cache is not a regular file");
    anyhow::ensure!(
        metadata.len() == LAMA_MODEL_BYTES,
        "LaMa model size mismatch: found {}, expected {LAMA_MODEL_BYTES}",
        metadata.len()
    );
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256Context::new(&SHA256);
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest: [u8; 32] = hasher
        .finish()
        .as_ref()
        .try_into()
        .map_err(|_| anyhow::anyhow!("SHA-256 implementation returned the wrong length"))?;
    anyhow::ensure!(
        digest == LAMA_MODEL_SHA256,
        "LaMa model SHA-256 mismatch (expected {LAMA_MODEL_SHA256_HEX})"
    );
    Ok(())
}

fn download_lama_model(path: &Path, events: &mpsc::Sender<InpaintEvent>) -> Result<()> {
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
            .timeout_recv_body(Some(Duration::from_secs(15 * 60)))
            .build();
        let agent: ureq::Agent = config.into();
        let mut response = agent
            .get(LAMA_MODEL_URL)
            .call()
            .context("download LaMa ONNX model")?;
        if let Some(length) = response.body().content_length() {
            anyhow::ensure!(
                length == LAMA_MODEL_BYTES,
                "LaMa server declared {length} bytes, expected {LAMA_MODEL_BYTES}"
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
            let read = reader.read(&mut buffer).context("read LaMa download")?;
            if read == 0 {
                break;
            }
            downloaded = downloaded
                .checked_add(read as u64)
                .context("LaMa download byte count overflow")?;
            anyhow::ensure!(
                downloaded <= LAMA_MODEL_BYTES,
                "LaMa download exceeded its pinned {LAMA_MODEL_BYTES}-byte size"
            );
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .context("write LaMa ONNX model")?;
            let _ = events.send(InpaintEvent::DownloadProgress {
                downloaded,
                total: LAMA_MODEL_BYTES,
            });
        }
        file.sync_all().context("flush LaMa ONNX model")?;
        anyhow::ensure!(
            downloaded == LAMA_MODEL_BYTES,
            "LaMa model size mismatch: received {downloaded}, expected {LAMA_MODEL_BYTES}"
        );
        let digest: [u8; 32] = hasher
            .finish()
            .as_ref()
            .try_into()
            .map_err(|_| anyhow::anyhow!("SHA-256 implementation returned the wrong length"))?;
        anyhow::ensure!(digest == LAMA_MODEL_SHA256, "LaMa model SHA-256 mismatch");
        fs::rename(&temporary, path)
            .with_context(|| format!("publish LaMa model to {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn infer_lama(
    model_path: &Path,
    runtime_path: Option<&Path>,
    runtime_sha256: Option<&str>,
    request: InpaintRequest,
) -> Result<InpaintPatch> {
    let prepared = request.source;
    let pixels = u64::from(prepared.width)
        .checked_mul(u64::from(prepared.height))
        .context("inpainting patch dimensions overflow")?;
    anyhow::ensure!(
        pixels > 0 && pixels <= MAX_INPAINT_PIXELS,
        "inpainting patch {}x{} exceeds the {MAX_INPAINT_PIXELS}-pixel limit",
        prepared.width,
        prepared.height
    );
    anyhow::ensure!(
        prepared.width == prepared.height,
        "inpainting patch must remain square"
    );
    let expected = usize::try_from(LAMA_EDGE)
        .ok()
        .and_then(|edge| edge.checked_mul(LAMA_EDGE as usize))
        .and_then(|pixels| pixels.checked_mul(3))
        .context("inpainting model input value count overflow")?;
    anyhow::ensure!(
        prepared.rgb_rec2020.len() == expected,
        "inpainting Rec.2020 buffer has {}, expected {expected}",
        prepared.rgb_rec2020.len()
    );
    anyhow::ensure!(
        prepared.rgb_rec2020.iter().all(|value| value.is_finite()),
        "inpainting Rec.2020 source contains non-finite values"
    );
    anyhow::ensure!(!request.dabs.is_empty(), "erase stroke is empty");
    anyhow::ensure!(
        prepared.full_width > 0
            && prepared.full_height > 0
            && prepared.origin_x + prepared.width <= prepared.full_width
            && prepared.origin_y + prepared.height <= prepared.full_height,
        "invalid full-resolution inpainting source coordinates"
    );

    crate::ai_masks::initialize_runtime(runtime_path, runtime_sha256)?;

    let local_dabs = localize_dabs(&request.dabs, &prepared, prepared.width, prepared.height);
    anyhow::ensure!(
        !local_dabs.is_empty(),
        "erase stroke did not intersect its source crop"
    );

    // Inpainting uses one strict binary mask end-to-end. There is no feather
    // ramp and no antialias coverage: every source pixel is either untouched
    // (0) or fully replaced (255). The same hard mask is fed to LaMa and
    // persisted for compositing, so the model and replacement boundary agree.
    let inference_mask = rasterize_inpaint_dabs_binary(
        prepared.width,
        prepared.height,
        prepared.width,
        prepared.height,
        &local_dabs,
    );
    // The prepared source is already the exact square full-resolution LaMa crop
    // downsampled on the GPU to 512x512, so the worker no longer needs a large
    // full-resolution Rec.2020 buffer just to shrink it again. This keeps the
    // LaMa boundary explicit while preserving full-resolution crop selection.
    let image_values = build_lama_image_tensor(&prepared);
    let mask_values = build_lama_mask_tensor(&inference_mask, prepared.width);
    let crop = inpaint_crop(&inference_mask, prepared.width, prepared.height)
        .context("erase stroke did not cover any image pixels")?;
    let image_tensor = Tensor::from_array((
        [1usize, 3, LAMA_EDGE as usize, LAMA_EDGE as usize],
        image_values,
    ))
    .context("create LaMa image tensor")?;
    let mask_tensor = Tensor::from_array((
        [1usize, 1, LAMA_EDGE as usize, LAMA_EDGE as usize],
        mask_values,
    ))
    .context("create LaMa mask tensor")?;

    #[cfg(target_os = "android")]
    let output = {
        let mut session = crate::ai_masks::create_session(model_path)?;
        run_lama_session(&mut session, image_tensor, mask_tensor)?
    };

    #[cfg(not(target_os = "android"))]
    let output = {
        let sessions = LAMA_SESSION.get_or_init(|| Mutex::new(None));
        let mut session = sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("LaMa session lock was poisoned"))?;
        if session.is_none() {
            *session = Some(crate::ai_masks::create_session(model_path)?);
        }
        let session = session
            .as_mut()
            .context("LaMa session initialization produced no session")?;
        run_lama_session(session, image_tensor, mask_tensor)?
    };

    // Persist generated pixels in scene-linear Rec.2020 RGBA16F with the same
    // strict binary replacement mask used by LaMa. RGB remains high precision;
    // compositing alpha is only 0 or 1.
    use half::f16;
    let patch_pixels = usize::try_from(crop.size)
        .ok()
        .and_then(|size| size.checked_mul(size))
        .context("inpainting patch dimensions overflow")?;
    let mut rgba16f = vec![0u16; patch_pixels.checked_mul(4).context("patch size overflow")?];
    let mut replacement_mask = vec![0u8; patch_pixels];
    for local_y in 0..crop.size {
        for local_x in 0..crop.size {
            let source_x = crop.x + local_x;
            let source_y = crop.y + local_y;
            let source_index = (source_y * prepared.width + source_x) as usize;
            let patch_index = (local_y * crop.size + local_x) as usize;
            let generated_encoded =
                sample_lama_bilinear(&output, source_x, source_y, prepared.width);
            let generated = srgb_encoded_to_rec2020_linear(generated_encoded);
            let out = patch_index * 4;
            rgba16f[out] = f16::from_f32(generated[0]).to_bits();
            rgba16f[out + 1] = f16::from_f32(generated[1]).to_bits();
            rgba16f[out + 2] = f16::from_f32(generated[2]).to_bits();
            rgba16f[out + 3] = f16::from_f32(1.0).to_bits();
            replacement_mask[patch_index] = inference_mask[source_index];
        }
    }

    InpaintPatch::new_linear(
        prepared.full_width,
        prepared.full_height,
        prepared.origin_x + crop.x,
        prepared.origin_y + crop.y,
        crop.size,
        crop.size,
        rgba16f,
        replacement_mask,
    )
    .context("LaMa result patch dimensions are invalid")
}

fn localize_dabs(
    dabs: &[BrushDab],
    source: &PreparedInpaintSource,
    local_width: u32,
    local_height: u32,
) -> Vec<BrushDab> {
    let full_min = source.full_width.min(source.full_height).max(1) as f32;
    let local_min = local_width.min(local_height).max(1) as f32;
    dabs.iter()
        .filter_map(|dab| {
            let center_x = dab.center[0] * source.full_width as f32 - source.origin_x as f32;
            let center_y = dab.center[1] * source.full_height as f32 - source.origin_y as f32;
            let radius = dab.size.clamp(0.0025, 0.5) * full_min;
            if center_x + radius < 0.0
                || center_y + radius < 0.0
                || center_x - radius > local_width as f32
                || center_y - radius > local_height as f32
            {
                return None;
            }
            Some(BrushDab {
                center: [
                    center_x / local_width.max(1) as f32,
                    center_y / local_height.max(1) as f32,
                ],
                opacity: dab.opacity,
                size: radius / local_min,
                feather: 0.0,
            })
        })
        .collect()
}

fn build_lama_image_tensor(source: &PreparedInpaintSource) -> Vec<f32> {
    let plane = (LAMA_EDGE * LAMA_EDGE) as usize;
    let mut output = vec![0.0f32; plane * 3];
    for index in 0..plane {
        let source_index = index * 3;
        let encoded = rec2020_linear_to_model_srgb([
            source.rgb_rec2020[source_index],
            source.rgb_rec2020[source_index + 1],
            source.rgb_rec2020[source_index + 2],
        ]);
        output[index] = encoded[0];
        output[plane + index] = encoded[1];
        output[plane * 2 + index] = encoded[2];
    }
    output
}

fn build_lama_mask_tensor(mask: &[u8], width: u32) -> Vec<f32> {
    let mut output = vec![0.0f32; (LAMA_EDGE * LAMA_EDGE) as usize];
    for y in 0..LAMA_EDGE {
        let src_y = (y * width / LAMA_EDGE).min(width - 1);
        for x in 0..LAMA_EDGE {
            let src_x = (x * width / LAMA_EDGE).min(width - 1);
            output[(y * LAMA_EDGE + x) as usize] = if mask[(src_y * width + src_x) as usize] >= 128
            {
                1.0
            } else {
                0.0
            };
        }
    }
    output
}

/// Converts AuRaw's scene-linear Rec.2020 working RGB to the encoded sRGB
/// domain expected by LaMa. Wide-gamut/negative values are mapped toward a
/// neutral luminance axis before clipping, which is substantially less prone to
/// false-colour speckles than independent per-channel clipping in dark RAW data.
fn rec2020_linear_to_model_srgb(rec2020: [f32; 3]) -> [f32; 3] {
    let mut rgb = [
        1.660_491 * rec2020[0] - 0.587_641_1 * rec2020[1] - 0.072_849_9 * rec2020[2],
        -0.124_550_5 * rec2020[0] + 1.132_899_9 * rec2020[1] - 0.008_349_4 * rec2020[2],
        -0.018_150_8 * rec2020[0] - 0.100_578_9 * rec2020[1] + 1.118_729_7 * rec2020[2],
    ];
    let luminance = (0.212_672_9 * rgb[0] + 0.715_152_2 * rgb[1] + 0.072_175_0 * rgb[2]).max(0.0);
    let min_channel = rgb[0].min(rgb[1]).min(rgb[2]);
    if min_channel < 0.0 {
        let denominator = (luminance - min_channel).max(1e-8);
        let chroma_scale = (luminance / denominator).clamp(0.0, 1.0);
        for channel in &mut rgb {
            *channel = luminance + (*channel - luminance) * chroma_scale;
        }
    }
    let max_channel = rgb[0].max(rgb[1]).max(rgb[2]);
    if max_channel > 1.0 {
        let denominator = (max_channel - luminance).max(1e-8);
        let chroma_scale = ((1.0 - luminance).max(0.0) / denominator).clamp(0.0, 1.0);
        for channel in &mut rgb {
            *channel = luminance + (*channel - luminance) * chroma_scale;
        }
    }
    rgb.map(|linear| linear_to_srgb(linear.clamp(0.0, 1.0)))
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_encoded_to_rec2020_linear(encoded: [f32; 3]) -> [f32; 3] {
    let decode = |value: f32| {
        let value = value.clamp(0.0, 1.0);
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let r = decode(encoded[0]);
    let g = decode(encoded[1]);
    let b = decode(encoded[2]);
    [
        0.627_403_9 * r + 0.329_283 * g + 0.043_313_1 * b,
        0.069_097_3 * r + 0.919_540_4 * g + 0.011_362_3 * b,
        0.016_391_4 * r + 0.088_013_3 * g + 0.895_595_3 * b,
    ]
}

fn sample_lama_bilinear(output: &[f32], x: u32, y: u32, target_edge: u32) -> [f32; 3] {
    let source_edge = LAMA_EDGE as usize;
    let fx = if target_edge > 1 {
        x as f32 * (LAMA_EDGE - 1) as f32 / (target_edge - 1) as f32
    } else {
        0.0
    };
    let fy = if target_edge > 1 {
        y as f32 * (LAMA_EDGE - 1) as f32 / (target_edge - 1) as f32
    } else {
        0.0
    };
    let x0 = fx.floor() as usize;
    let y0 = fy.floor() as usize;
    let x1 = (x0 + 1).min(source_edge - 1);
    let y1 = (y0 + 1).min(source_edge - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let plane = source_edge * source_edge;
    let sample = |channel: usize, sx: usize, sy: usize| {
        output[channel * plane + sy * source_edge + sx] / 255.0
    };
    let mut rgb = [0.0; 3];
    for (channel, value) in rgb.iter_mut().enumerate() {
        let top = sample(channel, x0, y0) * (1.0 - tx) + sample(channel, x1, y0) * tx;
        let bottom = sample(channel, x0, y1) * (1.0 - tx) + sample(channel, x1, y1) * tx;
        *value = top * (1.0 - ty) + bottom * ty;
    }
    rgb
}

fn run_lama_session(
    session: &mut Session,
    image: Tensor<f32>,
    mask: Tensor<f32>,
) -> Result<Vec<f32>> {
    let outputs = session
        .run(ort::inputs![image, mask])
        .context("run LaMa ONNX inference")?;
    let output = outputs
        .values()
        .next()
        .context("LaMa returned no output tensors")?;
    let (shape, values) = output
        .try_extract_tensor::<f32>()
        .context("read LaMa output tensor")?;
    let shape = &**shape;
    anyhow::ensure!(
        shape.len() == 4
            && shape[0] == 1
            && shape[1] == 3
            && shape[2] == LAMA_EDGE as i64
            && shape[3] == LAMA_EDGE as i64,
        "unexpected LaMa output shape {shape:?}"
    );
    anyhow::ensure!(
        values.len() == (3 * LAMA_EDGE * LAMA_EDGE) as usize,
        "LaMa returned {} values, expected {}",
        values.len(),
        3 * LAMA_EDGE * LAMA_EDGE
    );
    anyhow::ensure!(
        values.iter().all(|value| value.is_finite()),
        "LaMa output contains non-finite values"
    );
    Ok(values.to_vec())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SquareCrop {
    x: u32,
    y: u32,
    size: u32,
}

fn inpaint_crop(mask: &[u8], width: u32, height: u32) -> Option<SquareCrop> {
    if width == 0 || height == 0 || mask.len() != (width * height) as usize {
        return None;
    }
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    let mut found = false;
    for y in 0..height {
        for x in 0..width {
            if mask[(y * width + x) as usize] >= 4 {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if !found {
        return None;
    }
    let bounds_width = max_x - min_x + 1;
    let bounds_height = max_y - min_y + 1;
    let shorter = width.min(height);
    // AuRaw 1 used PAD_FRAC=0.25 on each side: total crop size ~= 1.5x
    // the painted bounds, with a 64px minimum. The old 2.0 code added 2x
    // the bounds as context (roughly a 3x crop), shrinking the target inside
    // LaMa's 512x512 input and visibly reducing reconstruction detail.
    let base = bounds_width.max(bounds_height);
    let size = ((base as f32 * 1.5).ceil() as u32).max(64).min(shorter);
    let center_x = (min_x as i64 + max_x as i64) / 2;
    let center_y = (min_y as i64 + max_y as i64) / 2;
    let x = (center_x - i64::from(size) / 2).clamp(0, i64::from(width.saturating_sub(size))) as u32;
    let y =
        (center_y - i64::from(size) / 2).clamp(0, i64::from(height.saturating_sub(size))) as u32;
    Some(SquareCrop { x, y, size })
}

#[cfg(test)]
mod tests {
    use super::{inpaint_crop, SquareCrop};

    #[test]
    fn crop_stays_square_and_inside_image_near_edge() {
        let mut mask = vec![0u8; 400 * 300];
        for y in 0..20 {
            for x in 0..20 {
                mask[y * 400 + x] = 255;
            }
        }
        let crop = inpaint_crop(&mask, 400, 300).unwrap();
        assert_eq!(crop.x, 0);
        assert_eq!(crop.y, 0);
        assert!(crop.size >= 64 && crop.size <= 300);
    }

    #[test]
    fn empty_mask_has_no_crop() {
        assert_eq!(inpaint_crop(&vec![0; 64 * 64], 64, 64), None);
    }

    #[test]
    fn crop_matches_auraw1_quarter_padding() {
        let mut mask = vec![0u8; 512 * 512];
        for y in 206..306 {
            for x in 206..306 {
                mask[y * 512 + x] = 255;
            }
        }
        let crop = inpaint_crop(&mask, 512, 512).unwrap();
        assert_eq!(crop.size, 150);
    }

    #[test]
    fn crop_type_is_copyable() {
        let crop = SquareCrop {
            x: 1,
            y: 2,
            size: 3,
        };
        assert_eq!(crop, crop);
    }
}
