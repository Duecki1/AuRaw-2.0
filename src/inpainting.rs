use anyhow::{Context, Result};
use image::{imageops::FilterType, ImageBuffer, Luma, Rgba};
use ort::{session::Session, value::Tensor};
use ring::digest::{Context as Sha256Context, SHA256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::pipeline::{rasterize_brush_dabs, BrushDab, InpaintLayer, MaskRgbImage};

pub const LAMA_MODEL_URL: &str =
    "https://huggingface.co/Carve/LaMa-ONNX/resolve/main/lama.onnx";
pub const LAMA_MODEL_BYTES: u64 = 207_479_252;
pub const LAMA_MODEL_SHA256_HEX: &str =
    "351e481e287f345b7fbfd026068cfb9ec0c7f24b440e6501458ebe54a833d1a1";
const LAMA_MODEL_SHA256: [u8; 32] = [
    0x35, 0x1e, 0x48, 0x1e, 0x28, 0x7f, 0x34, 0x5b, 0x7f, 0xbf, 0xd0, 0x26, 0x06, 0x8c,
    0xfb, 0x9e, 0xc0, 0xc7, 0xf2, 0x4b, 0x44, 0x0e, 0x65, 0x01, 0x45, 0x8e, 0xbe, 0x54,
    0xa8, 0x33, 0xd1, 0xa1,
];
const LAMA_EDGE: u32 = 512;
const MAX_INPAINT_PIXELS: u64 = 20_000_000;

#[cfg(not(target_os = "android"))]
static LAMA_SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct InpaintRequest {
    pub source: MaskRgbImage,
    pub dabs: Vec<BrushDab>,
}

#[derive(Debug)]
pub enum InpaintEvent {
    DownloadProgress { downloaded: u64, total: u64 },
    Inferencing,
    Finished(Result<InpaintLayer, String>),
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
    if verify_lama_model(path).is_ok() {
        return Ok(());
    }
    if path.exists() {
        log::warn!("discarding invalid LaMa cache {}", path.display());
        fs::remove_file(path)
            .with_context(|| format!("remove invalid LaMa model {}", path.display()))?;
    }
    download_lama_model(path, events)?;
    verify_lama_model(path).context("verify published LaMa model")
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
) -> Result<InpaintLayer> {
    let source = request.source;
    let pixels = u64::from(source.width)
        .checked_mul(u64::from(source.height))
        .context("inpainting input dimensions overflow")?;
    anyhow::ensure!(
        pixels > 0 && pixels <= MAX_INPAINT_PIXELS,
        "inpainting input {}x{} exceeds the {MAX_INPAINT_PIXELS}-pixel limit",
        source.width,
        source.height
    );
    let expected = pixels
        .checked_mul(4)
        .and_then(|bytes| usize::try_from(bytes).ok())
        .context("inpainting input byte count overflow")?;
    anyhow::ensure!(
        source.rgba.len() == expected,
        "inpainting RGBA buffer has {}, expected {expected}",
        source.rgba.len()
    );
    anyhow::ensure!(!request.dabs.is_empty(), "erase stroke is empty");

    crate::ai_masks::initialize_runtime(runtime_path, runtime_sha256)?;

    let soft_mask = rasterize_brush_dabs(
        source.width,
        source.height,
        source.width,
        source.height,
        &request.dabs,
    );
    let crop = inpaint_crop(&soft_mask, source.width, source.height)
        .context("erase stroke did not cover any image pixels")?;
    let source_image = ImageBuffer::<Rgba<u8>, _>::from_raw(
        source.width,
        source.height,
        source.rgba.to_vec(),
    )
    .context("invalid RGBA image for LaMa")?;
    let crop_image = image::imageops::crop_imm(
        &source_image,
        crop.x,
        crop.y,
        crop.size,
        crop.size,
    )
    .to_image();
    let resized_image = image::imageops::resize(
        &crop_image,
        LAMA_EDGE,
        LAMA_EDGE,
        FilterType::Lanczos3,
    );

    let crop_mask = crop_mask_image(&soft_mask, source.width, crop);
    let crop_mask_image = ImageBuffer::<Luma<u8>, _>::from_raw(crop.size, crop.size, crop_mask)
        .context("invalid erase mask crop")?;
    let resized_mask = image::imageops::resize(
        &crop_mask_image,
        LAMA_EDGE,
        LAMA_EDGE,
        FilterType::Triangle,
    );

    let image_values = rgba_to_chw(&resized_image);
    let mask_values = resized_mask
        .pixels()
        .map(|pixel| if pixel[0] >= 8 { 1.0 } else { 0.0 })
        .collect::<Vec<f32>>();
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

    let output_image = chw_to_rgba(&output)?;
    let restored = image::imageops::resize(
        &output_image,
        crop.size,
        crop.size,
        FilterType::Lanczos3,
    );
    let mut composited = source.rgba.to_vec();
    for local_y in 0..crop.size {
        for local_x in 0..crop.size {
            let source_index = ((crop.y + local_y) * source.width + crop.x + local_x) as usize;
            let alpha = soft_mask[source_index] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            let destination = source_index * 4;
            let generated = restored.get_pixel(local_x, local_y);
            for channel in 0..3 {
                let base = composited[destination + channel] as f32;
                composited[destination + channel] =
                    (base + (generated[channel] as f32 - base) * alpha)
                        .round()
                        .clamp(0.0, 255.0) as u8;
            }
            composited[destination + 3] = 255;
        }
    }

    // The feather has already been baked into `composited`. Use a binary
    // replacement mask for the display/export layer so the feather is not
    // applied a second time when the layer is composited over the live image.
    // Only mark pixels inside the context crop: a very long stroke can exceed
    // the largest square crop that fits inside a panoramic image, and pixels
    // outside that crop were not processed by this inference pass.
    let mut replacement_mask = vec![0u8; soft_mask.len()];
    for local_y in 0..crop.size {
        let start = ((crop.y + local_y) * source.width + crop.x) as usize;
        let end = start + crop.size as usize;
        for (output, coverage) in replacement_mask[start..end]
            .iter_mut()
            .zip(soft_mask[start..end].iter().copied())
        {
            if coverage != 0 {
                *output = 255;
            }
        }
    }
    InpaintLayer::new(source.width, source.height, composited, replacement_mask)
        .context("LaMa result dimensions are invalid")
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

fn rgba_to_chw(image: &ImageBuffer<Rgba<u8>, Vec<u8>>) -> Vec<f32> {
    let plane = (image.width() * image.height()) as usize;
    let mut output = vec![0.0f32; plane * 3];
    for (index, pixel) in image.pixels().enumerate() {
        output[index] = pixel[0] as f32 / 255.0;
        output[plane + index] = pixel[1] as f32 / 255.0;
        output[plane * 2 + index] = pixel[2] as f32 / 255.0;
    }
    output
}

fn chw_to_rgba(values: &[f32]) -> Result<ImageBuffer<Rgba<u8>, Vec<u8>>> {
    let plane = (LAMA_EDGE * LAMA_EDGE) as usize;
    anyhow::ensure!(values.len() == plane * 3, "invalid LaMa output length");
    // The pinned `lama.onnx` graph emits RGB values in the 0..255 range.
    // Do not apply the 0..1 scaling used by the original PyTorch checkpoint.
    let mut rgba = vec![255u8; plane * 4];
    for index in 0..plane {
        for channel in 0..3 {
            rgba[index * 4 + channel] = values[channel * plane + index]
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
    ImageBuffer::<Rgba<u8>, _>::from_raw(LAMA_EDGE, LAMA_EDGE, rgba)
        .context("invalid LaMa output image")
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
    let context = bounds_width.max(bounds_height).saturating_mul(2).max(96);
    let size = bounds_width
        .max(bounds_height)
        .saturating_add(context)
        .clamp(64, shorter.max(64))
        .min(shorter);
    let center_x = (min_x as i64 + max_x as i64) / 2;
    let center_y = (min_y as i64 + max_y as i64) / 2;
    let x = (center_x - i64::from(size) / 2)
        .clamp(0, i64::from(width.saturating_sub(size))) as u32;
    let y = (center_y - i64::from(size) / 2)
        .clamp(0, i64::from(height.saturating_sub(size))) as u32;
    Some(SquareCrop { x, y, size })
}

fn crop_mask_image(mask: &[u8], width: u32, crop: SquareCrop) -> Vec<u8> {
    let mut output = vec![0u8; (crop.size * crop.size) as usize];
    for local_y in 0..crop.size {
        let source_start = ((crop.y + local_y) * width + crop.x) as usize;
        let destination_start = (local_y * crop.size) as usize;
        output[destination_start..destination_start + crop.size as usize]
            .copy_from_slice(&mask[source_start..source_start + crop.size as usize]);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{SquareCrop, inpaint_crop};

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
    fn crop_type_is_copyable() {
        let crop = SquareCrop {
            x: 1,
            y: 2,
            size: 3,
        };
        assert_eq!(crop, crop);
    }
}
