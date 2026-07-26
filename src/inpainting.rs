use anyhow::{Context, Result};
use ort::{session::Session, value::Tensor};
use rayon::prelude::*;
use ring::digest::{Context as Sha256Context, SHA256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::pipeline::{
    rasterize_brush_dabs, rasterize_inpaint_dabs_binary, BrushDab, InpaintPatch,
};

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
const REC2020_LUMA: [f32; 3] = [0.262_700_2, 0.677_998_1, 0.059_301_7];

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
        let radius = dab.size.clamp(f32::EPSILON, 0.5) * image_min + 2.0;
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
            && prepared
                .origin_x
                .checked_add(prepared.width)
                .is_some_and(|right| right <= prepared.full_width)
            && prepared
                .origin_y
                .checked_add(prepared.height)
                .is_some_and(|bottom| bottom <= prepared.full_height),
        "invalid full-resolution inpainting source coordinates"
    );

    crate::ai_masks::initialize_runtime(runtime_path, runtime_sha256)?;

    let local_dabs = localize_dabs(&request.dabs, &prepared, prepared.width, prepared.height);
    anyhow::ensure!(
        !local_dabs.is_empty(),
        "erase stroke did not intersect its source crop"
    );

    let painted_mask = rasterize_inpaint_dabs_binary(
        prepared.width,
        prepared.height,
        prepared.width,
        prepared.height,
        &local_dabs,
    );
    let composite_dabs = feathered_composite_dabs(&local_dabs, prepared.width);
    let composite_mask = rasterize_brush_dabs(
        prepared.width,
        prepared.height,
        prepared.width,
        prepared.height,
        &composite_dabs,
    );
    let inference_mask = composite_mask
        .iter()
        .zip(&painted_mask)
        .map(|(&soft, &painted)| u8::from(painted >= 128 || soft > 0) * 255)
        .collect::<Vec<_>>();
    // The prepared source is already the exact square full-resolution LaMa crop
    // downsampled on the GPU to 512x512, so the worker no longer needs a large
    // full-resolution Rec.2020 buffer just to shrink it again. This keeps the
    // LaMa boundary explicit while preserving full-resolution crop selection.
    let mask_values = build_lama_mask_tensor(&inference_mask, prepared.width);
    let painted_values = build_lama_mask_tensor(&painted_mask, prepared.width);
    let scene_scale = lama_model_scene_scale(&prepared.rgb_rec2020, &mask_values);
    let image_values = build_lama_image_tensor(&prepared, scene_scale);
    let storage_bounds = inpaint_storage_bounds(&composite_mask, prepared.width, prepared.height)
        .context("erase stroke did not cover any image pixels")?;
    let image_tensor = Tensor::from_array((
        [1usize, 3, LAMA_EDGE as usize, LAMA_EDGE as usize],
        image_values,
    ))
    .context("create LaMa image tensor")?;
    let mask_tensor = Tensor::from_array((
        [1usize, 1, LAMA_EDGE as usize, LAMA_EDGE as usize],
        mask_values.clone(),
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

    // Convert the model output back through the inverse of its temporary
    // viewing exposure. Matching the generated boundary in the same neutral
    // scene-linear domain keeps the fill attached to its surroundings when
    // Develop Exposure or the display transform is changed later.
    let mut generated_scene = decode_lama_output_scene(&output, scene_scale);
    suppress_lama_checkerboard(&mut generated_scene, &painted_values, LAMA_EDGE);
    match_lama_boundary_color(
        &mut generated_scene,
        &prepared.rgb_rec2020,
        &mask_values,
        &painted_values,
        LAMA_EDGE,
    );

    build_resampled_inpaint_patch(
        &prepared,
        &generated_scene,
        &composite_mask,
        &painted_mask,
        storage_bounds,
    )
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
            let radius = dab.size.clamp(f32::EPSILON, 0.5) * full_min;
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

fn feathered_composite_dabs(dabs: &[BrushDab], patch_edge: u32) -> Vec<BrushDab> {
    let edge = patch_edge.max(1) as f32;
    let feather_pixels = (3.0 * edge / LAMA_EDGE as f32).clamp(1.5, 12.0);
    dabs.iter()
        .map(|dab| {
            let inner_radius = dab.size.clamp(f32::EPSILON, 0.5) * edge;
            let outer_radius = inner_radius + feather_pixels;
            BrushDab {
                center: dab.center,
                opacity: if dab.opacity > 0.0 { 1.0 } else { 0.0 },
                size: outer_radius / edge,
                feather: (feather_pixels / outer_radius).clamp(0.0, 1.0),
            }
        })
        .collect()
}

fn build_lama_image_tensor(source: &PreparedInpaintSource, scene_scale: f32) -> Vec<f32> {
    let plane = (LAMA_EDGE * LAMA_EDGE) as usize;
    let mut output = vec![0.0f32; plane * 3];
    for index in 0..plane {
        let source_index = index * 3;
        let encoded = rec2020_linear_to_model_srgb([
            source.rgb_rec2020[source_index] * scene_scale,
            source.rgb_rec2020[source_index + 1] * scene_scale,
            source.rgb_rec2020[source_index + 2] * scene_scale,
        ]);
        output[index] = encoded[0];
        output[plane + index] = encoded[1];
        output[plane * 2 + index] = encoded[2];
    }
    output
}

fn lama_model_scene_scale(rgb_rec2020: &[f32], model_mask: &[f32]) -> f32 {
    let pixels = rgb_rec2020.len() / 3;
    if pixels == 0 || rgb_rec2020.len() != pixels * 3 || model_mask.len() != pixels {
        return 1.0;
    }

    let collect_levels = |outside_only: bool| {
        let mut luminance = Vec::new();
        let mut maximum = Vec::new();
        for (rgb, mask) in rgb_rec2020.chunks_exact(3).zip(model_mask.iter().copied()) {
            if outside_only && mask >= 0.5 {
                continue;
            }
            let y = REC2020_LUMA[0] * rgb[0] + REC2020_LUMA[1] * rgb[1] + REC2020_LUMA[2] * rgb[2];
            let srgb = rec2020_to_linear_srgb([rgb[0], rgb[1], rgb[2]]);
            let peak = srgb[0].max(srgb[1]).max(srgb[2]);
            if y.is_finite() && peak.is_finite() && y > 1e-6 && peak > 1e-6 {
                luminance.push(y);
                maximum.push(peak);
            }
        }
        (luminance, maximum)
    };

    let (mut luminance, mut maximum) = collect_levels(true);
    if luminance.len() < 256 {
        (luminance, maximum) = collect_levels(false);
    }
    if luminance.is_empty() {
        return 1.0;
    }
    luminance.sort_unstable_by(f32::total_cmp);
    maximum.sort_unstable_by(f32::total_cmp);
    let percentile = |values: &[f32], fraction: f32| {
        let index = ((values.len() - 1) as f32 * fraction).round() as usize;
        values[index]
    };
    let middle = percentile(&luminance, 0.50);
    let highlight = percentile(&maximum, 0.99);
    let middle_scale = 0.18 / middle.max(1e-6);
    let highlight_scale = 0.90 / highlight.max(1e-6);
    middle_scale.min(highlight_scale).clamp(0.25, 64.0)
}

fn decode_lama_output_scene(output: &[f32], scene_scale: f32) -> Vec<f32> {
    let plane = (LAMA_EDGE * LAMA_EDGE) as usize;
    let inverse_scale = 1.0 / scene_scale.max(1e-6);
    let mut scene = vec![0.0; plane * 3];
    for index in 0..plane {
        let generated = srgb_encoded_to_rec2020_linear([
            output[index] / 255.0,
            output[plane + index] / 255.0,
            output[plane * 2 + index] / 255.0,
        ]);
        let destination = index * 3;
        scene[destination] = generated[0] * inverse_scale;
        scene[destination + 1] = generated[1] * inverse_scale;
        scene[destination + 2] = generated[2] * inverse_scale;
    }
    scene
}

/// Reduces the periodic transpose-convolution texture produced by LaMa in
/// smooth fills. A small binomial filter is blended only into the generated
/// region and is normalized by mask coverage, so real pixels outside the
/// stroke cannot bleed into the replacement or soften its boundary.
fn suppress_lama_checkerboard(generated: &mut [f32], painted_mask: &[f32], edge: u32) {
    let edge = edge as usize;
    let pixels = edge.saturating_mul(edge);
    if edge < 5 || generated.len() != pixels.saturating_mul(3) || painted_mask.len() != pixels {
        return;
    }

    const WEIGHTS: [f32; 5] = [1.0, 4.0, 6.0, 4.0, 1.0];
    const BLEND: f32 = 0.42;
    let source = generated.to_vec();
    generated
        .par_chunks_mut(3)
        .enumerate()
        .for_each(|(index, destination)| {
            if painted_mask[index] < 0.5 {
                return;
            }
            let x = index % edge;
            let y = index / edge;
            let mut sum = [0.0f32; 3];
            let mut weight_sum = 0.0f32;
            for (kernel_y, y_weight) in WEIGHTS.iter().copied().enumerate() {
                let sample_y = (y as isize + kernel_y as isize - 2)
                    .clamp(0, edge.saturating_sub(1) as isize)
                    as usize;
                for (kernel_x, x_weight) in WEIGHTS.iter().copied().enumerate() {
                    let sample_x = (x as isize + kernel_x as isize - 2)
                        .clamp(0, edge.saturating_sub(1) as isize)
                        as usize;
                    let sample_index = sample_y * edge + sample_x;
                    let mask_weight = painted_mask[sample_index].clamp(0.0, 1.0);
                    let weight = x_weight * y_weight * mask_weight;
                    let source_index = sample_index * 3;
                    for channel in 0..3 {
                        sum[channel] += source[source_index + channel] * weight;
                    }
                    weight_sum += weight;
                }
            }
            if weight_sum > 1e-5 {
                for channel in 0..3 {
                    let filtered = sum[channel] / weight_sum;
                    destination[channel] += (filtered - destination[channel]) * BLEND;
                }
            }
        });
}

fn median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable_by(f32::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        Some((values[middle - 1] + values[middle]) * 0.5)
    } else {
        Some(values[middle])
    }
}

fn robust_median_correction(
    values: &mut [f32],
    minimum_samples: usize,
    limit_ev: f32,
) -> Option<f32> {
    if values.len() < minimum_samples {
        return None;
    }
    let center = median(values)?;
    let mut deviations = values
        .iter()
        .map(|value| (value - center).abs())
        .collect::<Vec<_>>();
    let deviation = median(&mut deviations)?;
    // A systematic model bias produces a compact ratio distribution. Content
    // changes within the generated margin do not, so fade out the correction
    // instead of letting a heterogeneous edge dictate the whole patch color.
    let confidence = ((0.75 - deviation) / 0.50).clamp(0.0, 1.0);
    (confidence > 1e-3).then_some(center.clamp(-limit_ev, limit_ev) * confidence)
}

fn match_lama_boundary_color(
    generated: &mut [f32],
    source: &[f32],
    inference_mask: &[f32],
    painted_mask: &[f32],
    edge: u32,
) {
    let pixels = edge as usize * edge as usize;
    if edge < 2
        || generated.len() != pixels * 3
        || source.len() != pixels * 3
        || inference_mask.len() != pixels
        || painted_mask.len() != pixels
    {
        return;
    }

    let mut luminance_ratios = Vec::new();
    let mut chroma_ratios: [Vec<f32>; 3] = std::array::from_fn(|_| Vec::new());
    for index in 0..pixels {
        // The expanded model mask contains a narrow generated margin outside
        // the user's opaque erase target. Source and generated RGB in this
        // margin describe the same coordinates, making it a reliable color
        // calibration region without comparing across real image edges.
        if inference_mask[index] < 0.5 || painted_mask[index] >= 0.5 {
            continue;
        }
        let generated_index = index * 3;
        let boundary_rgb = [
            source[generated_index].max(0.0),
            source[generated_index + 1].max(0.0),
            source[generated_index + 2].max(0.0),
        ];
        let generated_rgb = [
            generated[generated_index].max(0.0),
            generated[generated_index + 1].max(0.0),
            generated[generated_index + 2].max(0.0),
        ];
        let source_luminance = REC2020_LUMA[0] * boundary_rgb[0]
            + REC2020_LUMA[1] * boundary_rgb[1]
            + REC2020_LUMA[2] * boundary_rgb[2];
        let generated_luminance = REC2020_LUMA[0] * generated_rgb[0]
            + REC2020_LUMA[1] * generated_rgb[1]
            + REC2020_LUMA[2] * generated_rgb[2];
        if source_luminance <= 1e-5 || generated_luminance <= 1e-5 {
            continue;
        }
        luminance_ratios.push((source_luminance / generated_luminance).log2());
        for channel in 0..3 {
            let source_chroma = boundary_rgb[channel] / source_luminance;
            let generated_chroma = generated_rgb[channel] / generated_luminance;
            if source_chroma > 1e-4 && generated_chroma > 1e-4 {
                chroma_ratios[channel].push((source_chroma / generated_chroma).log2());
            }
        }
    }

    let minimum_chroma_samples = (luminance_ratios.len() / 4).max(32);
    let Some(luminance_ev) = robust_median_correction(&mut luminance_ratios, 32, 0.75) else {
        return;
    };
    let channel_gain = chroma_ratios.map(|mut ratios| {
        let chroma_ev = robust_median_correction(&mut ratios, minimum_chroma_samples, 0.201_633_86)
            .unwrap_or(0.0);
        (luminance_ev + chroma_ev).exp2()
    });
    for (index, mask) in inference_mask.iter().copied().enumerate() {
        if mask < 0.5 {
            continue;
        }
        let pixel = index * 3;
        for channel in 0..3 {
            generated[pixel + channel] *= channel_gain[channel];
        }
    }
}

fn build_lama_mask_tensor(mask: &[u8], width: u32) -> Vec<f32> {
    let mut output = vec![0.0f32; (LAMA_EDGE * LAMA_EDGE) as usize];
    for y in 0..LAMA_EDGE {
        let src_y0 = (u64::from(y) * u64::from(width) / u64::from(LAMA_EDGE)) as u32;
        let src_y1 = ((u64::from(y + 1) * u64::from(width)).div_ceil(u64::from(LAMA_EDGE)) as u32)
            .max(src_y0 + 1)
            .min(width);
        for x in 0..LAMA_EDGE {
            let src_x0 = (u64::from(x) * u64::from(width) / u64::from(LAMA_EDGE)) as u32;
            let src_x1 = ((u64::from(x + 1) * u64::from(width)).div_ceil(u64::from(LAMA_EDGE))
                as u32)
                .max(src_x0 + 1)
                .min(width);
            let covered = (src_y0..src_y1).any(|source_y| {
                (src_x0..src_x1).any(|source_x| mask[(source_y * width + source_x) as usize] >= 128)
            });
            output[(y * LAMA_EDGE + x) as usize] = f32::from(covered);
        }
    }
    output
}

/// Converts AuRaw's scene-linear Rec.2020 working RGB to the encoded sRGB
/// domain expected by LaMa. Wide-gamut/negative values are mapped toward a
/// neutral luminance axis before clipping, which is substantially less prone to
/// false-colour speckles than independent per-channel clipping in dark RAW data.
fn rec2020_linear_to_model_srgb(rec2020: [f32; 3]) -> [f32; 3] {
    let mut rgb = rec2020_to_linear_srgb(rec2020);
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

fn rec2020_to_linear_srgb(rec2020: [f32; 3]) -> [f32; 3] {
    [
        1.660_491 * rec2020[0] - 0.587_641_1 * rec2020[1] - 0.072_849_9 * rec2020[2],
        -0.124_550_5 * rec2020[0] + 1.132_899_9 * rec2020[1] - 0.008_349_4 * rec2020[2],
        -0.018_150_8 * rec2020[0] - 0.100_578_9 * rec2020[1] + 1.118_729_7 * rec2020[2],
    ]
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

fn sample_scene_bilinear(output: &[f32], x: f32, y: f32, target_edge: u32) -> [f32; 3] {
    let source_edge = LAMA_EDGE as usize;
    let map_coordinate = |coordinate: f32| {
        (((coordinate + 0.5) * LAMA_EDGE as f32 / target_edge.max(1) as f32) - 0.5)
            .clamp(0.0, (LAMA_EDGE - 1) as f32)
    };
    let fx = map_coordinate(x);
    let fy = map_coordinate(y);
    let x0 = fx.floor() as usize;
    let y0 = fy.floor() as usize;
    let x1 = (x0 + 1).min(source_edge - 1);
    let y1 = (y0 + 1).min(source_edge - 1);
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let sample =
        |channel: usize, sx: usize, sy: usize| output[(sy * source_edge + sx) * 3 + channel];
    let mut rgb = [0.0; 3];
    for (channel, value) in rgb.iter_mut().enumerate() {
        let top = sample(channel, x0, y0) * (1.0 - tx) + sample(channel, x1, y0) * tx;
        let bottom = sample(channel, x0, y1) * (1.0 - tx) + sample(channel, x1, y1) * tx;
        *value = top * (1.0 - ty) + bottom * ty;
    }
    rgb
}

fn resample_composite_mask(
    mask: &[u8],
    source_width: u32,
    bounds: PixelRect,
    raster_dimensions: [u32; 2],
) -> Vec<u8> {
    let [raster_width, raster_height] = raster_dimensions;
    let mut output = vec![0u8; (raster_width * raster_height) as usize];
    for raster_y in 0..raster_height {
        let y_start = f64::from(raster_y) * f64::from(bounds.height) / f64::from(raster_height);
        let y_end = f64::from(raster_y + 1) * f64::from(bounds.height) / f64::from(raster_height);
        let source_y_start = y_start.floor() as u32;
        let source_y_end = (y_end.ceil() as u32).min(bounds.height);
        for raster_x in 0..raster_width {
            let x_start = f64::from(raster_x) * f64::from(bounds.width) / f64::from(raster_width);
            let x_end = f64::from(raster_x + 1) * f64::from(bounds.width) / f64::from(raster_width);
            let source_x_start = x_start.floor() as u32;
            let source_x_end = (x_end.ceil() as u32).min(bounds.width);
            let mut weighted_alpha = 0.0;
            for local_y in source_y_start..source_y_end {
                let y_weight =
                    (y_end.min(f64::from(local_y + 1)) - y_start.max(f64::from(local_y))).max(0.0);
                let row = (bounds.y + local_y) as usize * source_width as usize;
                for local_x in source_x_start..source_x_end {
                    let x_weight = (x_end.min(f64::from(local_x + 1))
                        - x_start.max(f64::from(local_x)))
                    .max(0.0);
                    let source_index = row + (bounds.x + local_x) as usize;
                    weighted_alpha += f64::from(mask[source_index]) * x_weight * y_weight;
                }
            }
            let area = (x_end - x_start) * (y_end - y_start);
            output[(raster_y * raster_width + raster_x) as usize] =
                (weighted_alpha / area).round().clamp(0.0, 255.0) as u8;
        }
    }
    output
}

fn build_resampled_inpaint_patch(
    prepared: &PreparedInpaintSource,
    generated_scene: &[f32],
    composite_mask: &[u8],
    painted_mask: &[u8],
    bounds: PixelRect,
) -> Result<InpaintPatch> {
    anyhow::ensure!(
        prepared.width > 0
            && prepared.height > 0
            && bounds.width > 0
            && bounds.height > 0
            && bounds
                .x
                .checked_add(bounds.width)
                .is_some_and(|right| right <= prepared.width)
            && bounds
                .y
                .checked_add(bounds.height)
                .is_some_and(|bottom| bottom <= prepared.height),
        "inpainting storage bounds are invalid"
    );
    let model_pixels = (LAMA_EDGE * LAMA_EDGE) as usize;
    let prepared_pixels = usize::try_from(prepared.width)
        .ok()
        .and_then(|width| {
            usize::try_from(prepared.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .context("inpainting source dimensions overflow")?;
    anyhow::ensure!(
        generated_scene.len() == model_pixels * 3
            && composite_mask.len() == prepared_pixels
            && painted_mask.len() == prepared_pixels,
        "inpainting result buffers are incomplete"
    );

    let raster_width = ((u64::from(bounds.width) * u64::from(LAMA_EDGE))
        .div_ceil(u64::from(prepared.width)))
    .clamp(1, u64::from(LAMA_EDGE)) as u32;
    let raster_height = ((u64::from(bounds.height) * u64::from(LAMA_EDGE))
        .div_ceil(u64::from(prepared.height)))
    .clamp(1, u64::from(LAMA_EDGE)) as u32;
    let raster_pixels = usize::try_from(raster_width)
        .ok()
        .and_then(|width| {
            usize::try_from(raster_height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .context("inpainting raster dimensions overflow")?;

    use half::f16;
    let mut rgba16f = vec![
        0u16;
        raster_pixels
            .checked_mul(4)
            .context("patch size overflow")?
    ];
    let mut replacement_mask = resample_composite_mask(
        composite_mask,
        prepared.width,
        bounds,
        [raster_width, raster_height],
    );
    let painted_coverage = resample_composite_mask(
        painted_mask,
        prepared.width,
        bounds,
        [raster_width, raster_height],
    );
    // Every raster cell touched by the user's hard brush is a true removal,
    // not a translucent blend. Keep the soft composite mask only outside that
    // painted core so tone/highlight changes can never reveal the old pixels.
    for (replacement, painted) in replacement_mask.iter_mut().zip(painted_coverage) {
        if painted > 0 {
            *replacement = 255;
        }
    }
    for raster_y in 0..raster_height {
        let source_y = bounds.y as f32
            + ((raster_y as f32 + 0.5) * bounds.height as f32 / raster_height as f32)
            - 0.5;
        for raster_x in 0..raster_width {
            let source_x = bounds.x as f32
                + ((raster_x as f32 + 0.5) * bounds.width as f32 / raster_width as f32)
                - 0.5;
            let generated =
                sample_scene_bilinear(generated_scene, source_x, source_y, prepared.width);
            let patch_index = (raster_y * raster_width + raster_x) as usize;
            let out = patch_index * 4;
            rgba16f[out] = f16::from_f32(generated[0]).to_bits();
            rgba16f[out + 1] = f16::from_f32(generated[1]).to_bits();
            rgba16f[out + 2] = f16::from_f32(generated[2]).to_bits();
            rgba16f[out + 3] = f16::from_f32(1.0).to_bits();
        }
    }

    InpaintPatch::new_linear_resampled(
        [prepared.full_width, prepared.full_height],
        [prepared.origin_x + bounds.x, prepared.origin_y + bounds.y],
        [bounds.width, bounds.height],
        [raster_width, raster_height],
        rgba16f,
        replacement_mask,
    )
    .context("LaMa result patch dimensions are invalid")
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
struct PixelRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn inpaint_storage_bounds(mask: &[u8], width: u32, height: u32) -> Option<PixelRect> {
    let expected = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    if width == 0 || height == 0 || mask.len() != expected {
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
    let guard_x = width.div_ceil(LAMA_EDGE).max(1);
    let guard_y = height.div_ceil(LAMA_EDGE).max(1);
    let x = min_x.saturating_sub(guard_x);
    let y = min_y.saturating_sub(guard_y);
    let right = max_x.saturating_add(1).saturating_add(guard_x).min(width);
    let bottom = max_y.saturating_add(1).saturating_add(guard_y).min(height);
    Some(PixelRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        build_lama_image_tensor, build_lama_mask_tensor, build_resampled_inpaint_patch,
        decode_lama_output_scene, feathered_composite_dabs, inpaint_storage_bounds,
        lama_model_scene_scale, match_lama_boundary_color, resample_composite_mask,
        sample_scene_bilinear, suppress_lama_checkerboard, PixelRect, PreparedInpaintSource,
        LAMA_EDGE,
    };
    use crate::pipeline::BrushDab;

    #[test]
    fn storage_bounds_stay_inside_image_near_edge() {
        let mut mask = vec![0u8; 400 * 300];
        for y in 0..20 {
            for x in 0..20 {
                mask[y * 400 + x] = 255;
            }
        }
        let bounds = inpaint_storage_bounds(&mask, 400, 300).unwrap();
        assert_eq!(bounds.x, 0);
        assert_eq!(bounds.y, 0);
        assert_eq!(bounds.width, 21);
        assert_eq!(bounds.height, 21);
    }

    #[test]
    fn empty_mask_has_no_crop() {
        assert_eq!(inpaint_storage_bounds(&vec![0; 64 * 64], 64, 64), None);
    }

    #[test]
    fn storage_bounds_do_not_square_or_repad_an_elongated_stroke() {
        let mut mask = vec![0u8; 400 * 300];
        for y in 140..160 {
            for x in 100..200 {
                mask[y * 400 + x] = 255;
            }
        }
        let bounds = inpaint_storage_bounds(&mask, 400, 300).unwrap();
        assert_eq!([bounds.x, bounds.y], [99, 139]);
        assert_eq!([bounds.width, bounds.height], [102, 22]);
    }

    #[test]
    fn composite_mask_downsampling_preserves_area_without_max_pool_expansion() {
        let bounds = PixelRect {
            x: 0,
            y: 0,
            width: 4,
            height: 1,
        };
        let averaged = resample_composite_mask(&[0, 64, 128, 192], 4, bounds, [2, 1]);
        assert_eq!(averaged, [32, 160]);

        let split_bounds = PixelRect { width: 5, ..bounds };
        let split = resample_composite_mask(&[0, 0, 255, 0, 0], 5, split_bounds, [2, 1]);
        assert_eq!(split, [51, 51]);
    }

    #[test]
    fn persisted_patch_keeps_full_resolution_extent_but_not_redundant_upscale() {
        let source_edge = 1024;
        let mut mask = vec![0u8; (source_edge * source_edge) as usize];
        for y in 450..550 {
            for x in 100..900 {
                mask[(y * source_edge + x) as usize] = 255;
            }
        }
        let bounds = inpaint_storage_bounds(&mask, source_edge, source_edge).unwrap();
        let generated = [0.2, 0.3, 0.4].repeat((LAMA_EDGE * LAMA_EDGE) as usize);
        let prepared = PreparedInpaintSource {
            rgb_rec2020: generated.clone(),
            width: source_edge,
            height: source_edge,
            origin_x: 200,
            origin_y: 300,
            full_width: 2000,
            full_height: 2000,
        };
        let patch =
            build_resampled_inpaint_patch(&prepared, &generated, &mask, &mask, bounds).unwrap();
        assert_eq!([patch.width, patch.height], [804, 104]);
        assert_eq!(patch.raster_dimensions(), [402, 52]);
        assert_eq!(patch.mask.len(), 402 * 52);
        assert_eq!(patch.rgba16f.len(), 402 * 52 * 4);
        assert!(patch.mask[..402].iter().all(|&alpha| alpha == 0));
        assert!(patch.mask[402 * 51..].iter().all(|&alpha| alpha == 0));
        assert!(patch.mask.contains(&255));
        assert!(patch.rgba16f.len() < patch.width as usize * patch.height as usize * 4);
    }

    #[test]
    fn model_mask_max_pools_thin_full_resolution_coverage() {
        let width = LAMA_EDGE * 2;
        let mut mask = vec![0u8; (width * width) as usize];
        mask[(width + 1) as usize] = 255;
        let model_mask = build_lama_mask_tensor(&mask, width);
        assert_eq!(model_mask[0], 1.0);
    }

    #[test]
    fn lama_output_sampling_uses_shared_pixel_centers() {
        let plane = (LAMA_EDGE * LAMA_EDGE) as usize;
        let mut output = vec![0.0f32; plane * 3];
        for channel in 0..3 {
            for y in 0..LAMA_EDGE as usize {
                for x in 0..LAMA_EDGE as usize {
                    output[(y * LAMA_EDGE as usize + x) * 3 + channel] = x as f32;
                }
            }
        }
        let target_edge = LAMA_EDGE * 4;
        let x = 1000;
        let expected_model_x = ((x as f32 + 0.5) * LAMA_EDGE as f32 / target_edge as f32 - 0.5)
            .clamp(0.0, (LAMA_EDGE - 1) as f32);
        let sampled = sample_scene_bilinear(&output, x as f32, 777.0, target_edge);
        for channel in sampled {
            assert!((channel - expected_model_x).abs() < 1e-6);
        }
    }

    #[test]
    fn dark_scene_is_temporarily_exposed_for_lama_and_round_trips() {
        let plane = (LAMA_EDGE * LAMA_EDGE) as usize;
        let source_rgb = [0.03, 0.04, 0.05];
        let source = PreparedInpaintSource {
            rgb_rec2020: source_rgb.repeat(plane),
            width: LAMA_EDGE,
            height: LAMA_EDGE,
            origin_x: 0,
            origin_y: 0,
            full_width: LAMA_EDGE,
            full_height: LAMA_EDGE,
        };
        let mask = vec![0.0; plane];
        let scale = lama_model_scene_scale(&source.rgb_rec2020, &mask);
        assert!(scale > 2.0);

        let mut model_output = build_lama_image_tensor(&source, scale);
        for value in &mut model_output {
            *value *= 255.0;
        }
        let round_trip = decode_lama_output_scene(&model_output, scale);
        for channel in 0..3 {
            assert!((round_trip[channel] - source_rgb[channel]).abs() < 1e-5);
        }
    }

    #[test]
    fn generated_feather_margin_matches_scene_luminance_and_chroma() {
        let edge = 16;
        let pixels = (edge * edge) as usize;
        let source_rgb = [0.20, 0.30, 0.40];
        let generated_rgb = [0.14, 0.21, 0.28];
        let source = source_rgb.repeat(pixels);
        let mut generated = generated_rgb.repeat(pixels);
        let inference_mask = vec![1.0; pixels];
        let mut painted_mask = vec![0.0; pixels];
        for y in 4..12 {
            for x in 4..12 {
                painted_mask[(y * edge + x) as usize] = 1.0;
            }
        }

        match_lama_boundary_color(
            &mut generated,
            &source,
            &inference_mask,
            &painted_mask,
            edge,
        );
        let center = ((8 * edge + 8) * 3) as usize;
        for channel in 0..3 {
            assert!((generated[center + channel] - source_rgb[channel]).abs() < 1e-5);
        }
    }

    #[test]
    fn checkerboard_suppression_reduces_periodic_generated_texture() {
        let edge = 16;
        let pixels = edge * edge;
        let mut generated = Vec::with_capacity(pixels * 3);
        for y in 0..edge {
            for x in 0..edge {
                let value = if (x + y) % 2 == 0 { 0.4 } else { 0.6 };
                generated.extend_from_slice(&[value; 3]);
            }
        }
        let mask = vec![1.0; pixels];
        let contrast = |values: &[f32]| {
            values
                .chunks_exact(3)
                .map(|rgb| (rgb[0] - 0.5).abs())
                .sum::<f32>()
        };
        let before = contrast(&generated);
        suppress_lama_checkerboard(&mut generated, &mask, edge as u32);
        assert!(contrast(&generated) < before * 0.7);
    }

    #[test]
    fn composite_feather_preserves_the_painted_core() {
        let edge = 1024;
        let original = BrushDab {
            center: [0.5, 0.5],
            opacity: 1.0,
            size: 0.2,
            feather: 0.0,
        };
        let feathered = feathered_composite_dabs(&[original], edge)[0];
        let outer_radius = feathered.size * edge as f32;
        let opaque_radius = outer_radius * (1.0 - feathered.feather);
        assert!((opaque_radius - original.size * edge as f32).abs() < 1e-4);
        assert!(outer_radius > opaque_radius);
    }
}
