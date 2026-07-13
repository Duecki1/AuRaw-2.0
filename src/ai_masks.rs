use anyhow::{Context, Result};
use image::{imageops::FilterType, ImageBuffer, Luma, Rgba};
use ort::{session::Session, value::Tensor};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{mpsc, Mutex, OnceLock},
};

pub const BIREFNET_MODEL_BYTES: u64 = 224_005_088;
pub const BIREFNET_MODEL_URL: &str = "https://github.com/danielgatis/rembg/releases/download/v0.0.0/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx";
const MODEL_SIZE: u32 = 1024;
const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
static RUNTIME_INITIALIZED: OnceLock<Result<(), String>> = OnceLock::new();

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Letterbox {
    width: u32,
    height: u32,
    offset_x: u32,
    offset_y: u32,
}

impl Letterbox {
    fn for_image(width: u32, height: u32) -> Result<Self> {
        anyhow::ensure!(width > 0 && height > 0, "subject-mask input is empty");
        let scale = (MODEL_SIZE as f64 / width as f64).min(MODEL_SIZE as f64 / height as f64);
        let scaled_width = ((width as f64 * scale).round() as u32).clamp(1, MODEL_SIZE);
        let scaled_height = ((height as f64 * scale).round() as u32).clamp(1, MODEL_SIZE);
        Ok(Self {
            width: scaled_width,
            height: scaled_height,
            offset_x: (MODEL_SIZE - scaled_width) / 2,
            offset_y: (MODEL_SIZE - scaled_height) / 2,
        })
    }
}

pub fn spawn_subject_mask(
    model_path: PathBuf,
    runtime_path: Option<PathBuf>,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> mpsc::Receiver<SubjectMaskEvent> {
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let spawn = std::thread::Builder::new()
        .name("auraw-onnx-subject".to_owned())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (|| {
                    if !model_path.exists() {
                        download_model(&model_path, &worker_sender)?;
                    }
                    #[cfg(target_os = "linux")]
                    let runtime_path = match runtime_path {
                        Some(path) => Some(path),
                        None => Some(download_cpu_runtime(&model_path, &worker_sender)?),
                    };
                    let _ = worker_sender.send(SubjectMaskEvent::Inferencing);
                    infer_subject(&model_path, runtime_path.as_deref(), width, height, rgba)
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

fn download_model(path: &Path, events: &mpsc::Sender<SubjectMaskEvent>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create model cache {}", parent.display()))?;
    }
    let temporary = path.with_extension("onnx.part");
    let mut response = ureq::get(BIREFNET_MODEL_URL)
        .call()
        .context("download BiRefNet ONNX model")?;
    let total = response
        .body()
        .content_length()
        .unwrap_or(BIREFNET_MODEL_BYTES);
    let mut reader = response.body_mut().as_reader();
    let mut file =
        File::create(&temporary).with_context(|| format!("create {}", temporary.display()))?;
    let mut downloaded = 0u64;
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let read = reader.read(&mut buffer).context("read BiRefNet download")?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .context("write BiRefNet ONNX model")?;
        downloaded += read as u64;
        let _ = events.send(SubjectMaskEvent::DownloadProgress {
            label: "BiRefNet model",
            downloaded,
            total,
        });
    }
    file.sync_all().context("flush BiRefNet ONNX model")?;
    if downloaded != BIREFNET_MODEL_BYTES {
        let _ = fs::remove_file(&temporary);
        anyhow::bail!(
            "BiRefNet model size mismatch: received {downloaded}, expected {BIREFNET_MODEL_BYTES}"
        );
    }
    fs::rename(&temporary, path)
        .with_context(|| format!("publish BiRefNet model to {}", path.display()))?;
    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const CPU_RUNTIME_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-linux-x64-1.24.4.tgz";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const CPU_RUNTIME_URL: &str = "https://github.com/microsoft/onnxruntime/releases/download/v1.24.4/onnxruntime-linux-aarch64-1.24.4.tgz";
#[cfg(target_os = "linux")]
const CPU_RUNTIME_FILE: &str = "libonnxruntime.so.1.24.4";

#[cfg(target_os = "linux")]
fn download_cpu_runtime(
    model_path: &Path,
    events: &mpsc::Sender<SubjectMaskEvent>,
) -> Result<PathBuf> {
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    anyhow::bail!(
        "the automatic CPU ONNX Runtime is unavailable for this Linux architecture; select a runtime in Settings"
    );

    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let auraw_cache = model_path
            .parent()
            .and_then(Path::parent)
            .context("invalid AuRaw model cache path")?;
        let runtime_dir = auraw_cache.join("runtime/onnxruntime-1.24.4");
        let runtime_path = runtime_dir.join(CPU_RUNTIME_FILE);
        if runtime_path.is_file() {
            return Ok(runtime_path);
        }
        fs::create_dir_all(&runtime_dir)
            .with_context(|| format!("create CPU runtime cache {}", runtime_dir.display()))?;
        let archive_path = runtime_dir.join("onnxruntime.tgz.part");
        let temporary = runtime_path.with_extension("part");
        let result = (|| {
            let mut response = ureq::get(CPU_RUNTIME_URL)
                .call()
                .context("download CPU ONNX Runtime")?;
            let total = response.body().content_length().unwrap_or(8_200_000);
            let mut reader = response.body_mut().as_reader();
            let mut archive_file = File::create(&archive_path)
                .with_context(|| format!("create {}", archive_path.display()))?;
            let mut downloaded = 0u64;
            let mut buffer = [0u8; 256 * 1024];
            loop {
                let read = reader
                    .read(&mut buffer)
                    .context("read CPU runtime download")?;
                if read == 0 {
                    break;
                }
                archive_file
                    .write_all(&buffer[..read])
                    .context("write CPU runtime archive")?;
                downloaded += read as u64;
                let _ = events.send(SubjectMaskEvent::DownloadProgress {
                    label: "CPU ONNX Runtime",
                    downloaded,
                    total,
                });
            }
            archive_file
                .sync_all()
                .context("flush CPU runtime archive")?;

            let archive_file = File::open(&archive_path)
                .with_context(|| format!("open {}", archive_path.display()))?;
            let decoder = flate2::read::GzDecoder::new(archive_file);
            let mut archive = tar::Archive::new(decoder);
            let mut found = false;
            for entry in archive.entries().context("read CPU runtime archive")? {
                let mut entry = entry.context("read CPU runtime archive entry")?;
                let matches = entry
                    .path()
                    .ok()
                    .and_then(|path| path.file_name().map(|name| name == CPU_RUNTIME_FILE))
                    .unwrap_or(false);
                if matches {
                    let mut output = File::create(&temporary)
                        .with_context(|| format!("create {}", temporary.display()))?;
                    std::io::copy(&mut entry, &mut output).context("extract CPU ONNX Runtime")?;
                    output.sync_all().context("flush CPU ONNX Runtime")?;
                    found = true;
                    break;
                }
            }
            anyhow::ensure!(found, "CPU runtime library was missing from its archive");
            fs::rename(&temporary, &runtime_path).with_context(|| {
                format!("publish CPU ONNX Runtime to {}", runtime_path.display())
            })?;
            Ok(())
        })();
        let _ = fs::remove_file(&archive_path);
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        Ok(runtime_path)
    }
}

#[cfg(not(target_os = "android"))]
fn initialize_runtime(runtime_path: Option<&Path>) -> Result<()> {
    let runtime_path = runtime_path
        .context("no ONNX Runtime library is selected; choose one in Settings and try again")?;
    let initialized = RUNTIME_INITIALIZED.get_or_init(|| {
        ort::init_from(runtime_path)
            .map(|builder| {
                builder.with_name("AuRaw").commit();
            })
            .map_err(|error| {
                format!(
                    "could not load ONNX Runtime from {}: {error}",
                    runtime_path.display()
                )
            })
    });
    initialized.clone().map_err(anyhow::Error::msg)
}

#[cfg(target_os = "android")]
fn initialize_runtime(_runtime_path: Option<&Path>) -> Result<()> {
    let initialized = RUNTIME_INITIALIZED.get_or_init(|| {
        ort::init().with_name("AuRaw").commit();
        Ok(())
    });
    initialized.clone().map_err(anyhow::Error::msg)
}

#[cfg(target_os = "android")]
fn create_session(model_path: &Path) -> Result<Session> {
    let nnapi_result = (|| -> Result<Session> {
        let mut builder = Session::builder()
            .context("create NNAPI ONNX Runtime session")?
            .with_execution_providers([ort::ep::NNAPI::default().build()])
            .map_err(|error| anyhow::anyhow!("configure Android NNAPI: {error}"))?;
        builder
            .commit_from_file(model_path)
            .context("compile BiRefNet for Android NNAPI")
    })();
    let nnapi_error = match nnapi_result {
        Ok(session) => return Ok(session),
        Err(error) => {
            log::warn!("NNAPI could not compile BiRefNet; trying XNNPACK: {error:#}");
            format!("{error:#}")
        }
    };

    let xnnpack_result = (|| -> Result<Session> {
        let mut builder = Session::builder()
            .context("create XNNPACK ONNX Runtime session")?
            .with_execution_providers([ort::ep::XNNPACK::default().build()])
            .map_err(|error| anyhow::anyhow!("configure Android XNNPACK: {error}"))?;
        builder
            .commit_from_file(model_path)
            .context("compile BiRefNet for Android XNNPACK")
    })();
    let xnnpack_error = match xnnpack_result {
        Ok(session) => return Ok(session),
        Err(error) => {
            log::warn!("XNNPACK could not compile BiRefNet; trying CPU: {error:#}");
            format!("{error:#}")
        }
    };

    let mut builder = Session::builder().context("create CPU ONNX Runtime session")?;
    builder.commit_from_file(model_path).with_context(|| {
        format!(
            "load BiRefNet with Android CPU fallback (NNAPI failed: {nnapi_error}; XNNPACK failed: {xnnpack_error})"
        )
    })
}

#[cfg(not(target_os = "android"))]
fn create_session(model_path: &Path) -> Result<Session> {
    let builder = Session::builder().context("create ONNX Runtime session")?;

    #[cfg(target_os = "linux")]
    let mut builder = builder
        .with_execution_providers([
            ort::ep::TensorRT::default().build(),
            ort::ep::CUDA::default().build(),
            ort::ep::ROCm::default().build(),
            ort::ep::OpenVINO::default().build(),
            ort::ep::XNNPACK::default().build(),
        ])
        .map_err(|error| anyhow::anyhow!("configure Linux ONNX execution providers: {error}"))?;

    #[cfg(target_os = "windows")]
    let mut builder = builder
        .with_execution_providers([
            ort::ep::TensorRT::default().build(),
            ort::ep::CUDA::default().build(),
            ort::ep::DirectML::default().build(),
        ])
        .map_err(|error| anyhow::anyhow!("configure Windows ONNX execution providers: {error}"))?;

    #[cfg(target_os = "macos")]
    let mut builder = builder
        .with_execution_providers([ort::ep::CoreML::default().build()])
        .map_err(|error| anyhow::anyhow!("configure macOS ONNX execution provider: {error}"))?;

    builder
        .commit_from_file(model_path)
        .with_context(|| format!("load BiRefNet ONNX model from {}", model_path.display()))
}

fn infer_subject(
    model_path: &Path,
    runtime_path: Option<&Path>,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<SubjectMaskResult> {
    initialize_runtime(runtime_path)?;
    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, rgba)
        .context("invalid preview image for BiRefNet")?;
    let letterbox = Letterbox::for_image(width, height)?;
    let resized = image::imageops::resize(
        &image,
        letterbox.width,
        letterbox.height,
        FilterType::Lanczos3,
    );
    let input = normalized_letterbox(&resized, letterbox);
    let input = Tensor::from_array(([1usize, 3, MODEL_SIZE as usize, MODEL_SIZE as usize], input))
        .context("create BiRefNet input tensor")?;

    let sessions = SESSION.get_or_init(|| Mutex::new(None));
    let mut session = sessions
        .lock()
        .map_err(|_| anyhow::anyhow!("BiRefNet session lock was poisoned"))?;
    if session.is_none() {
        *session = Some(create_session(model_path)?);
    }
    let outputs = session
        .as_mut()
        .expect("session was initialized")
        .run(ort::inputs![input])
        .context("run BiRefNet ONNX inference")?;
    let (shape, logits) = outputs[0]
        .try_extract_tensor::<f32>()
        .context("read BiRefNet output tensor")?;
    anyhow::ensure!(shape.len() >= 2, "unexpected BiRefNet output shape {shape}");
    let output_height = shape[shape.len() - 2] as usize;
    let output_width = shape[shape.len() - 1] as usize;
    anyhow::ensure!(
        output_width > 0 && output_height > 0 && logits.len() >= output_width * output_height,
        "invalid BiRefNet output shape {shape}"
    );
    let mask = restore_from_letterbox(
        logits,
        output_width as u32,
        output_height as u32,
        letterbox,
        width,
        height,
    )?;
    Ok(SubjectMaskResult {
        width,
        height,
        mask,
    })
}

fn normalized_letterbox(
    resized: &ImageBuffer<Rgba<u8>, Vec<u8>>,
    letterbox: Letterbox,
) -> Vec<f32> {
    let plane = (MODEL_SIZE * MODEL_SIZE) as usize;
    let mut input = vec![0.0; plane * 3];
    for channel in 0..3 {
        let padding = (0.0 - IMAGENET_MEAN[channel]) / IMAGENET_STD[channel];
        input[channel * plane..(channel + 1) * plane].fill(padding);
    }
    for y in 0..letterbox.height {
        for x in 0..letterbox.width {
            let pixel = resized.get_pixel(x, y);
            let destination =
                ((y + letterbox.offset_y) * MODEL_SIZE + x + letterbox.offset_x) as usize;
            for channel in 0..3 {
                let value = pixel[channel] as f32 / 255.0;
                input[channel * plane + destination] =
                    (value - IMAGENET_MEAN[channel]) / IMAGENET_STD[channel];
            }
        }
    }
    input
}

fn restore_from_letterbox(
    logits: &[f32],
    output_width: u32,
    output_height: u32,
    letterbox: Letterbox,
    target_width: u32,
    target_height: u32,
) -> Result<Vec<u8>> {
    let output_elements = (output_width * output_height) as usize;
    let mut probabilities = Vec::with_capacity(output_elements);
    let mut minimum = f32::INFINITY;
    let mut maximum = f32::NEG_INFINITY;
    for &logit in logits.iter().take(output_elements) {
        let probability = if logit >= 0.0 {
            1.0 / (1.0 + (-logit).exp())
        } else {
            let exponential = logit.exp();
            exponential / (1.0 + exponential)
        };
        minimum = minimum.min(probability);
        maximum = maximum.max(probability);
        probabilities.push(probability);
    }
    let range = (maximum - minimum).max(1e-6);
    let pixels = probabilities
        .into_iter()
        .map(|value| (((value - minimum) / range).clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect();
    let output = ImageBuffer::<Luma<u8>, Vec<u8>>::from_raw(output_width, output_height, pixels)
        .context("invalid BiRefNet output buffer")?;

    let scale_x = output_width as f64 / MODEL_SIZE as f64;
    let scale_y = output_height as f64 / MODEL_SIZE as f64;
    let crop_x = (letterbox.offset_x as f64 * scale_x).round() as u32;
    let crop_y = (letterbox.offset_y as f64 * scale_y).round() as u32;
    let crop_width = (letterbox.width as f64 * scale_x).round().max(1.0) as u32;
    let crop_height = (letterbox.height as f64 * scale_y).round().max(1.0) as u32;
    let crop_width = crop_width.min(output_width.saturating_sub(crop_x));
    let crop_height = crop_height.min(output_height.saturating_sub(crop_y));
    anyhow::ensure!(crop_width > 0 && crop_height > 0, "invalid letterbox crop");
    let cropped =
        image::imageops::crop_imm(&output, crop_x, crop_y, crop_width, crop_height).to_image();
    Ok(
        image::imageops::resize(&cropped, target_width, target_height, FilterType::Lanczos3)
            .into_raw(),
    )
}

#[cfg(test)]
mod tests {
    use super::{Letterbox, MODEL_SIZE};

    #[test]
    fn letterbox_preserves_landscape_aspect_ratio() {
        let box_ = Letterbox::for_image(6000, 4000).unwrap();
        assert_eq!(box_.width, MODEL_SIZE);
        assert_eq!(box_.height, 683);
        assert_eq!(box_.offset_x, 0);
        assert_eq!(box_.offset_y, 170);
    }

    #[test]
    fn letterbox_preserves_portrait_aspect_ratio() {
        let box_ = Letterbox::for_image(3000, 6000).unwrap();
        assert_eq!(box_.width, 512);
        assert_eq!(box_.height, MODEL_SIZE);
        assert_eq!(box_.offset_x, 256);
        assert_eq!(box_.offset_y, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "downloads the official CPU ONNX Runtime"]
    fn cpu_runtime_download_extracts_a_shared_library() {
        let root =
            std::env::temp_dir().join(format!("auraw-onnx-runtime-test-{}", std::process::id()));
        let model_path = root.join("models/model.onnx");
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        let (sender, _receiver) = std::sync::mpsc::channel();
        let runtime = super::download_cpu_runtime(&model_path, &sender).unwrap();
        assert!(runtime.is_file());
        assert!(std::fs::metadata(&runtime).unwrap().len() > 1_000_000);
        std::fs::remove_dir_all(root).unwrap();
    }
}
