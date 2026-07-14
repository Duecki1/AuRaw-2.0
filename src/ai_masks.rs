use anyhow::{Context, Result};
use image::{imageops::FilterType, ImageBuffer, Luma, Rgba};
use ort::{session::Session, value::Tensor};
use ring::digest::{Context as Sha256Context, SHA256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{mpsc, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(target_os = "android"))]
use std::sync::Mutex;

pub const BIREFNET_MODEL_BYTES: u64 = 224_005_088;
pub const BIREFNET_MODEL_URL: &str = "https://github.com/danielgatis/rembg/releases/download/v0.0.0/BiRefNet-general-bb_swin_v1_tiny-epoch_232.onnx";
pub const BIREFNET_MODEL_SHA256_HEX: &str =
    "5600024376f572a557870a5eb0afb1e5961636bef4e1e22132025467d0f03333";
const BIREFNET_MODEL_SHA256: [u8; 32] = [
    0x56, 0x00, 0x02, 0x43, 0x76, 0xf5, 0x72, 0xa5, 0x57, 0x87, 0x0a, 0x5e, 0xb0, 0xaf, 0xb1, 0xe5,
    0x96, 0x16, 0x36, 0xbe, 0xf4, 0xe1, 0xe2, 0x21, 0x32, 0x02, 0x54, 0x67, 0xd0, 0xf0, 0x33, 0x33,
];
const MODEL_SIZE: u32 = 1024;
const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD: [f32; 3] = [0.229, 0.224, 0.225];

#[cfg(not(target_os = "android"))]
static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
static DESKTOP_RUNTIME_IDENTITY: OnceLock<(PathBuf, String)> = OnceLock::new();
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
    runtime_sha256: Option<String>,
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
                    ensure_model(&model_path, &worker_sender)?;
                    let _ = worker_sender.send(SubjectMaskEvent::Inferencing);
                    infer_subject(
                        &model_path,
                        runtime_path.as_deref(),
                        runtime_sha256.as_deref(),
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

fn ensure_model(path: &Path, events: &mpsc::Sender<SubjectMaskEvent>) -> Result<()> {
    match verify_model(path) {
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
    download_model(path, events)?;
    verify_model(path).context("verify published BiRefNet model")
}

fn verify_model(path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("read BiRefNet model metadata {}", path.display()))?;
    anyhow::ensure!(metadata.is_file(), "BiRefNet cache is not a regular file");
    anyhow::ensure!(
        metadata.len() == BIREFNET_MODEL_BYTES,
        "BiRefNet model size mismatch: found {}, expected {BIREFNET_MODEL_BYTES}",
        metadata.len()
    );
    let digest = sha256_file(path)?;
    anyhow::ensure!(
        digest == BIREFNET_MODEL_SHA256,
        "BiRefNet model SHA-256 mismatch (expected {BIREFNET_MODEL_SHA256_HEX})"
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

fn download_model(path: &Path, events: &mpsc::Sender<SubjectMaskEvent>) -> Result<()> {
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
        let mut response = ureq::get(BIREFNET_MODEL_URL)
            .call()
            .context("download BiRefNet ONNX model")?;
        if let Some(length) = response.body().content_length() {
            anyhow::ensure!(
                length == BIREFNET_MODEL_BYTES,
                "BiRefNet server declared {length} bytes, expected {BIREFNET_MODEL_BYTES}"
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
            let read = reader.read(&mut buffer).context("read BiRefNet download")?;
            if read == 0 {
                break;
            }
            downloaded = downloaded
                .checked_add(read as u64)
                .context("BiRefNet download byte count overflow")?;
            anyhow::ensure!(
                downloaded <= BIREFNET_MODEL_BYTES,
                "BiRefNet download exceeded its pinned {BIREFNET_MODEL_BYTES}-byte size"
            );
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .context("write BiRefNet ONNX model")?;
            let _ = events.send(SubjectMaskEvent::DownloadProgress {
                label: "BiRefNet model",
                downloaded,
                total: BIREFNET_MODEL_BYTES,
            });
        }
        file.sync_all().context("flush BiRefNet ONNX model")?;
        anyhow::ensure!(
            downloaded == BIREFNET_MODEL_BYTES,
            "BiRefNet model size mismatch: received {downloaded}, expected {BIREFNET_MODEL_BYTES}"
        );
        let digest: [u8; 32] = hasher.finish().as_ref().try_into().map_err(|_| {
            anyhow::anyhow!("SHA-256 implementation returned the wrong digest length")
        })?;
        anyhow::ensure!(
            digest == BIREFNET_MODEL_SHA256,
            "BiRefNet model SHA-256 mismatch (expected {BIREFNET_MODEL_SHA256_HEX})"
        );
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
fn initialize_runtime(runtime_path: Option<&Path>, expected_sha256: Option<&str>) -> Result<()> {
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
            .context("stage selected ONNX Runtime for race-free loading")?;
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
    let initialized = RUNTIME_INITIALIZED.get_or_init(|| match ort::init_from(&runtime_load_path) {
        Ok(builder) => {
            if builder.with_name("AuRaw").commit() {
                let _ =
                    DESKTOP_RUNTIME_IDENTITY.set((runtime_path.clone(), actual_sha256.clone()));
                Ok(())
            } else {
                Err(
                    "ONNX Runtime was already initialized before the selected pinned library could be committed"
                        .to_owned(),
                )
            }
        }
        Err(error) => Err(format!(
            "could not load ONNX Runtime from {}: {error}",
            runtime_path.display()
        )),
    });
    initialized.clone().map_err(anyhow::Error::msg)?;
    let (loaded_path, loaded_sha256) = DESKTOP_RUNTIME_IDENTITY
        .get()
        .context("ONNX Runtime initialized without a pinned desktop identity")?;
    anyhow::ensure!(
        loaded_path == &runtime_path && loaded_sha256 == &actual_sha256,
        "a different ONNX Runtime is already active in this process; restart AuRaw before changing the pinned runtime"
    );
    Ok(())
}

/// Returns a path whose bytes are the bytes that were hashed, plus an open
/// handle that must remain alive through `dlopen`/runtime initialization.
#[cfg(target_os = "linux")]
fn verified_runtime_load_path(path: &Path) -> Result<(PathBuf, Option<File>, String)> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::raw::{c_char, c_int, c_uint};

    unsafe extern "C" {
        fn memfd_create(name: *const c_char, flags: c_uint) -> c_int;
        fn fcntl(fd: c_int, command: c_int, ...) -> c_int;
    }

    const MFD_CLOEXEC: c_uint = 0x0001;
    const MFD_ALLOW_SEALING: c_uint = 0x0002;
    const F_ADD_SEALS: c_int = 1033;
    const F_SEAL_SEAL: c_int = 0x0001;
    const F_SEAL_SHRINK: c_int = 0x0002;
    const F_SEAL_GROW: c_int = 0x0004;
    const F_SEAL_WRITE: c_int = 0x0008;

    let mut source = File::open(path)
        .with_context(|| format!("open selected ONNX Runtime {}", path.display()))?;
    let name = CString::new("auraw-verified-onnx-runtime")
        .map_err(|_| anyhow::anyhow!("internal ONNX Runtime memfd name contains a NUL byte"))?;
    // SAFETY: `name` is a valid NUL-terminated CString and the flags are accepted by Linux memfd_create.
    let fd = unsafe { memfd_create(name.as_ptr(), MFD_CLOEXEC | MFD_ALLOW_SEALING) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("create sealed runtime file");
    }
    // SAFETY: `fd` was just returned as an owned descriptor and is transferred exactly once into `File`.
    let mut sealed = unsafe { File::from_raw_fd(fd) };
    let mut hasher = Sha256Context::new(&SHA256);
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let read = source
            .read(&mut buffer)
            .with_context(|| format!("read selected ONNX Runtime {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        sealed
            .write_all(&buffer[..read])
            .context("copy selected ONNX Runtime into sealed memory")?;
    }
    sealed.sync_all().context("flush sealed ONNX Runtime")?;
    let digest: [u8; 32] =
        hasher.finish().as_ref().try_into().map_err(|_| {
            anyhow::anyhow!("SHA-256 implementation returned the wrong digest length")
        })?;
    let actual_sha256 = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    let seals = F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE;
    // SAFETY: the descriptor remains open for the call and `F_ADD_SEALS` consumes the integer bitmask by value.
    if unsafe { fcntl(sealed.as_raw_fd(), F_ADD_SEALS, seals) } < 0 {
        return Err(std::io::Error::last_os_error()).context("seal verified ONNX Runtime bytes");
    }
    let load_path = PathBuf::from(format!("/proc/self/fd/{}", sealed.as_raw_fd()));
    Ok((load_path, Some(sealed), actual_sha256))
}

#[cfg(not(target_os = "linux"))]
fn verified_runtime_load_path(path: &Path) -> Result<(PathBuf, Option<File>, String)> {
    let digest = sha256_file_hex(path).context("verify selected ONNX Runtime SHA-256")?;
    Ok((path.to_path_buf(), None, digest))
}

#[cfg(target_os = "android")]
fn initialize_runtime(_runtime_path: Option<&Path>, _expected_sha256: Option<&str>) -> Result<()> {
    let initialized = RUNTIME_INITIALIZED.get_or_init(|| {
        ort::init().with_name("AuRaw").commit();
        Ok(())
    });
    initialized.clone().map_err(anyhow::Error::msg)
}

#[cfg(target_os = "android")]
fn create_session(model_path: &Path) -> Result<Session> {
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
            .context("compile BiRefNet for Android XNNPACK")
    })();
    let xnnpack_error = match xnnpack_result {
        Ok(session) => return Ok(session),
        Err(error) => {
            log::warn!("XNNPACK could not compile BiRefNet; trying CPU: {error:#}");
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
        format!("load BiRefNet with Android CPU fallback (XNNPACK failed: {xnnpack_error})")
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
    runtime_sha256: Option<&str>,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
) -> Result<SubjectMaskResult> {
    const MAX_SUBJECT_MASK_PIXELS: u64 = 16_000_000;
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
    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, rgba)
        .context("invalid preview image for BiRefNet")?;
    let letterbox = Letterbox::for_image(width, height)?;
    let resized = image::imageops::resize(
        &image,
        letterbox.width,
        letterbox.height,
        FilterType::Lanczos3,
    );
    let input = normalized_letterbox(&resized, letterbox)?;
    let input = Tensor::from_array(([1usize, 3, MODEL_SIZE as usize, MODEL_SIZE as usize], input))
        .context("create BiRefNet input tensor")?;

    #[cfg(target_os = "android")]
    let (output_width, output_height, logits) = {
        // Mobile memory is more important than avoiding session startup. Drop
        // all model weights and allocator state immediately after inference.
        let mut session = create_session(model_path)?;
        run_subject_session(&mut session, input)?
    };

    #[cfg(not(target_os = "android"))]
    let (output_width, output_height, logits) = {
        let sessions = SESSION.get_or_init(|| Mutex::new(None));
        let mut session = sessions
            .lock()
            .map_err(|_| anyhow::anyhow!("BiRefNet session lock was poisoned"))?;
        if session.is_none() {
            *session = Some(create_session(model_path)?);
        }
        let session = session.as_mut().ok_or_else(|| {
            anyhow::anyhow!("BiRefNet session initialization produced no session")
        })?;
        run_subject_session(session, input)?
    };

    let mask = restore_from_letterbox(
        &logits,
        output_width,
        output_height,
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

fn run_subject_session(session: &mut Session, input: Tensor<f32>) -> Result<(u32, u32, Vec<f32>)> {
    let outputs = session
        .run(ort::inputs![input])
        .context("run BiRefNet ONNX inference")?;
    let (shape, logits) = outputs[0]
        .try_extract_tensor::<f32>()
        .context("read BiRefNet output tensor")?;
    anyhow::ensure!(shape.len() >= 2, "unexpected BiRefNet output shape {shape}");
    let output_height = shape[shape.len() - 2] as usize;
    let output_width = shape[shape.len() - 1] as usize;
    let output_elements = output_width
        .checked_mul(output_height)
        .context("BiRefNet output dimensions overflow")?;
    anyhow::ensure!(
        output_width > 0
            && output_height > 0
            && output_elements <= (MODEL_SIZE as usize * MODEL_SIZE as usize * 4)
            && logits.len() >= output_elements,
        "invalid BiRefNet output shape {shape}"
    );
    let mut owned_logits = Vec::new();
    owned_logits
        .try_reserve_exact(output_elements)
        .context("reserve BiRefNet output logits")?;
    owned_logits.extend_from_slice(&logits[..output_elements]);
    Ok((output_width as u32, output_height as u32, owned_logits))
}

fn normalized_letterbox(
    resized: &ImageBuffer<Rgba<u8>, Vec<u8>>,
    letterbox: Letterbox,
) -> Result<Vec<f32>> {
    let plane = (MODEL_SIZE * MODEL_SIZE) as usize;
    let values = plane
        .checked_mul(3)
        .context("BiRefNet input size overflow")?;
    let mut input = Vec::new();
    input
        .try_reserve_exact(values)
        .context("reserve BiRefNet input tensor")?;
    input.resize(values, 0.0);
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
    Ok(input)
}

fn restore_from_letterbox(
    logits: &[f32],
    output_width: u32,
    output_height: u32,
    letterbox: Letterbox,
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
    let mut probabilities = Vec::new();
    probabilities
        .try_reserve_exact(output_elements)
        .context("reserve BiRefNet probability map")?;
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
}
