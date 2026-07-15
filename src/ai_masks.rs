use anyhow::{Context, Result};
use image::{imageops::FilterType, ImageBuffer, Luma, Rgba};
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
static RUNTIME_INITIALIZED: OnceLock<()> = OnceLock::new();
static RUNTIME_INIT_LOCK: Mutex<()> = Mutex::new(());

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
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(10 * 60)))
            .build();
        let agent: ureq::Agent = config.into();
        let mut response = agent
            .get(BIREFNET_MODEL_URL)
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
        .with_context(|| format!("load ONNX model from {}", model_path.display()))
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
    let output = outputs
        .values()
        .next()
        .context("BiRefNet returned no output tensors")?;
    let (shape, logits) = output
        .try_extract_tensor::<f32>()
        .context("read BiRefNet output tensor")?;
    let (output_width, output_height, output_elements) =
        validate_birefnet_output_shape(&**shape, logits.len())?;
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

fn validate_birefnet_output_shape(shape: &[i64], logits_len: usize) -> Result<(u32, u32, usize)> {
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
        output_elements <= MODEL_SIZE as usize * MODEL_SIZE as usize * 4,
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
    let resized =
        image::imageops::resize(&cropped, target_width, target_height, FilterType::Lanczos3);
    Ok(resized
        .into_raw()
        .into_iter()
        .map(|probability| (probability.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect())
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
    use super::{sigmoid_probability, validate_birefnet_output_shape, Letterbox, MODEL_SIZE};

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

    #[test]
    fn birefnet_output_requires_single_batch_and_channel() {
        assert!(validate_birefnet_output_shape(&[1, 1, 1024, 1024], 1024 * 1024).is_ok());
        assert!(validate_birefnet_output_shape(&[1, 2, 1024, 1024], 2 * 1024 * 1024).is_err());
        assert!(validate_birefnet_output_shape(&[1, 1, -1, 1024], 0).is_err());
        assert!(validate_birefnet_output_shape(&[1, 1, 2, 2], 5).is_err());
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
const MAX_OBJECT_MASK_PIXELS: u64 = 16_000_000;

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
}

#[derive(Clone, Debug)]
pub struct ObjectMaskRequest {
    pub source_width: u32,
    pub source_height: u32,
    pub source_rgba: Vec<u8>,
    pub strokes: Vec<crate::pipeline::ObjectStroke>,
    pub edge_refine: f32,
    pub detailed_edges: bool,
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
    runtime_path: Option<PathBuf>,
    runtime_sha256: Option<String>,
    request: ObjectMaskRequest,
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
                    )?;
                    ensure_sam_model(
                        &decoder_path,
                        "SAM 2.1 decoder",
                        SAM21_DECODER_MODEL_URL,
                        SAM21_DECODER_SHA256_HEX,
                        &worker_sender,
                    )?;
                    let decoder_only = request.cache.is_some();
                    let _ = worker_sender.send(ObjectMaskEvent::Inferencing { decoder_only });
                    infer_object_mask(
                        &encoder_path,
                        &decoder_path,
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
    download_sam_model(path, label, url, expected_sha256, events)?;
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
    let result = (|| -> Result<()> {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(10 * 60)))
            .build();
        let agent: ureq::Agent = config.into();
        let mut response = agent
            .get(url)
            .call()
            .with_context(|| format!("download {label}"))?;
        let max_bytes = sam_model_max_bytes(label);
        let total = response.body().content_length().unwrap_or_else(|| {
            if label.contains("encoder") {
                109_000_000
            } else {
                16_500_000
            }
        });
        anyhow::ensure!(
            total <= max_bytes,
            "{label} response declares {total} bytes, above the {max_bytes}-byte limit"
        );
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
            let read = reader
                .read(&mut buffer)
                .with_context(|| format!("read {label}"))?;
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
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .with_context(|| format!("write {label}"))?;
            let _ = events.send(ObjectMaskEvent::DownloadProgress {
                label,
                downloaded,
                total: total.max(downloaded),
            });
        }
        file.sync_all().with_context(|| format!("flush {label}"))?;
        let actual = hasher
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        anyhow::ensure!(actual == expected_sha256, "{label} SHA-256 mismatch");
        fs::rename(&temporary, path)
            .with_context(|| format!("publish {label} to {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn infer_object_mask(
    encoder_path: &Path,
    decoder_path: &Path,
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

    let prompts = sampled_object_prompts(&request.strokes, SAM21_MAX_PROMPTS);
    let supplied_cache = request.cache;
    let mut last_result = None;

    for expansion in 0..3 {
        let crop = object_crop_for_prompts(
            request.source_width,
            request.source_height,
            &prompts,
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
            let can_reuse_logits = strokes_extend(&cache.prompt_strokes, &request.strokes);
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
        let decoded = decode_sam_mask(decoder_path, &features, &prompts, previous_logits)?;
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
        &prompts,
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
        request.detailed_edges,
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
    cache.low_res_logits = resize_f32(
        &decoded.selected_logits,
        decoded.width,
        decoded.height,
        SAM21_MASK_INPUT_SIZE,
        SAM21_MASK_INPUT_SIZE,
    )
    .into();
    cache.prompt_strokes = request.strokes.clone();

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

#[derive(Clone, Copy)]
struct ObjectPrompt {
    point: [f32; 2],
    positive: bool,
}

fn sampled_object_prompts(
    strokes: &[crate::pipeline::ObjectStroke],
    limit: usize,
) -> Vec<ObjectPrompt> {
    let mut all = Vec::new();
    for stroke in strokes {
        if stroke.points.is_empty() {
            continue;
        }
        let budget = if stroke.positive { 12 } else { 8 };
        let count = budget.min(stroke.points.len()).max(1);
        for index in 0..count {
            let source_index = if count == 1 {
                stroke.points.len() / 2
            } else {
                index * (stroke.points.len() - 1) / (count - 1)
            };
            all.push(ObjectPrompt {
                point: stroke.points[source_index],
                positive: stroke.positive,
            });
        }
    }
    if all.len() > limit {
        let positive = all
            .iter()
            .copied()
            .filter(|prompt| prompt.positive)
            .collect::<Vec<_>>();
        let negative = all
            .iter()
            .copied()
            .filter(|prompt| !prompt.positive)
            .collect::<Vec<_>>();
        let positive_budget = limit.saturating_sub(negative.len().min(limit / 3)).max(1);
        let mut reduced = evenly_sample(&positive, positive_budget);
        let remaining = limit.saturating_sub(reduced.len());
        reduced.extend(evenly_sample(&negative, remaining));
        reduced
    } else {
        all
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
    prompts: &[ObjectPrompt],
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
    let mut min_x = 1.0f32;
    let mut min_y = 1.0f32;
    let mut max_x = 0.0f32;
    let mut max_y = 0.0f32;
    // Include subtract prompts in the crop so corrections painted just outside
    // the current object are not clamped onto the crop edge.
    for prompt in prompts {
        min_x = min_x.min(prompt.point[0]);
        min_y = min_y.min(prompt.point[1]);
        max_x = max_x.max(prompt.point[0]);
        max_y = max_y.max(prompt.point[1]);
    }
    let center_x = ((min_x + max_x) * 0.5 * width as f32).clamp(0.0, width as f32);
    let center_y = ((min_y + max_y) * 0.5 * height as f32).clamp(0.0, height as f32);
    let bounds_w = (max_x - min_x).max(0.0) * width as f32;
    let bounds_h = (max_y - min_y).max(0.0) * height as f32;
    let minimum = width.min(height) as f32 * 0.16;
    let factor = if expansion == 0 { 1.7 } else { 2.5 };
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
        extract_f32_output(&outputs, 0, "high-resolution feature 0")?,
        extract_f32_output(&outputs, 1, "high-resolution feature 1")?,
        extract_f32_output(&outputs, 2, "image embedding")?,
    ))
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
    prompts: &[ObjectPrompt],
    use_previous_mask: bool,
) -> Result<DecodedSamMask> {
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
        labels[index] = if prompt.positive { 1.0 } else { 0.0 };
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
    };
    select_sam_candidate(masks, scores, prompts, cache)
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
    prompts: &[ObjectPrompt],
    cache: &ObjectInferenceCache,
) -> Result<DecodedSamMask> {
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
            score += if prompt.positive {
                probability * 0.12
            } else {
                (1.0 - probability) * 0.12
            };
        }
        let border = candidate_border_fraction(logits, width, height);
        score -= border * 0.18;
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

    for prompt in prompts.iter().filter(|prompt| prompt.positive) {
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

    // Preserve the model's soft boundary around the selected component. A
    // hard component cut at 0.5 would discard the very sub-threshold pixels
    // that edge-aware refinement needs for hair, fur, and anti-aliased edges.
    let keep_band = dilate_component_band(&keep, width_usize, height_usize, 16);
    probabilities
        .into_iter()
        .zip(keep_band)
        .map(|(probability, selected)| if selected { probability } else { 0.0 })
        .collect()
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
    detailed_edges: bool,
) -> Vec<f32> {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.001 || rgba.len() != width as usize * height as usize * 4 {
        return mask;
    }
    let radius = if detailed_edges {
        (2.0 + strength * 4.0).round() as i32
    } else {
        (1.0 + strength * 2.5).round() as i32
    };
    let iterations = if detailed_edges { 2 } else { 1 };
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

#[cfg(test)]
mod object_mask_tests {
    use super::*;
    use crate::pipeline::ObjectStroke;

    fn stroke(points: &[[f32; 2]], positive: bool) -> ObjectStroke {
        ObjectStroke {
            points: points.to_vec(),
            positive,
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
    fn prompt_sampling_keeps_foreground_and_background_within_limit() {
        let strokes = vec![
            stroke(&[[0.1, 0.1], [0.2, 0.2], [0.3, 0.3], [0.4, 0.4]], true),
            stroke(&[[0.8, 0.8], [0.7, 0.7], [0.6, 0.6]], false),
        ];
        let prompts = sampled_object_prompts(&strokes, 5);
        assert!(prompts.len() <= 5);
        assert!(prompts.iter().any(|prompt| prompt.positive));
        assert!(prompts.iter().any(|prompt| !prompt.positive));
    }

    #[test]
    fn subtract_prompts_are_inside_the_adaptive_crop() {
        let prompts = vec![
            ObjectPrompt {
                point: [0.50, 0.50],
                positive: true,
            },
            ObjectPrompt {
                point: [0.72, 0.50],
                positive: false,
            },
        ];
        let crop = object_crop_for_prompts(1000, 600, &prompts, 0);
        let negative_x = (prompts[1].point[0] * 1000.0) as u32;
        assert!(negative_x >= crop.x && negative_x < crop.x + crop.width);
        assert_eq!(
            object_crop_for_prompts(1000, 600, &prompts, 2),
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
            positive: true,
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
