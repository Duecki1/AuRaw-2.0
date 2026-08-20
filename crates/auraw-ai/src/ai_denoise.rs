//! RawNIND UtNet2 model acquisition and tiled RAW inference.
//!
//! The downloaded `.dtmodel` package is the published darktable-ai 5.6 release
//! asset. AuRaw pins both the archive and extracted ONNX graphs by SHA-256.

use crate::execution_provider::{FallbackSession, SessionOptions};
use crate::model_runtime::{acquire_model_session, AiModel, ModelRetention};
use crate::model_artifact::{
    ensure_artifact, install_artifact_from_reader, verify_artifact, ArtifactSize, DownloadOptions,
    ModelArtifact,
};
use anyhow::{Context, Result};
use auraw_gpu::wgpu;
use ort::value::Tensor;
use ring::digest::{Context as Sha256Context, SHA256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, RwLock,
    },
    time::Duration,
};
use zip::{write::FileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::pipeline::{
    AiDenoisedImage, CfaKind, CompactPixelMap, ExposureParams, GpuParams, LoadedRaw, MaskStack,
    ProcessingQuality, RawGpuPipeline,
};

pub const RAWNIND_PACKAGE_URL: &str =
    "https://github.com/darktable-org/darktable-ai/releases/download/release-5.6.0/rawdenoise-nind.dtmodel";
pub const RAWNIND_PACKAGE_BYTES: u64 = 57_700_134;
pub const RAWNIND_PACKAGE_SHA256: &str =
    "d71b5f1e727c85a359e6f74dca9e2016c9d8fc3e2f7ac3e9b347d80ceca969af";
const BAYER_MODEL_BYTES: u64 = 31_056_425;
const BAYER_MODEL_SHA256: &str = "da27509dab6a2915da67e988acd86cf71f9d5bbc8d1aa0ed32933578a887b901";
const LINEAR_MODEL_BYTES: u64 = 31_053_823;
const LINEAR_MODEL_SHA256: &str =
    "df957efadcc152c007d5d3b0917bdff9e41c0d4a0efe56584ef30b36393cd181";

const RAWNIND_PACKAGE_ARTIFACT: ModelArtifact = ModelArtifact {
    name: "RawNIND model package",
    url: Some(RAWNIND_PACKAGE_URL),
    sha256: RAWNIND_PACKAGE_SHA256,
    size: ArtifactSize::Exact(RAWNIND_PACKAGE_BYTES),
    progress_total: RAWNIND_PACKAGE_BYTES,
};
const BAYER_MODEL_ARTIFACT: ModelArtifact = ModelArtifact {
    name: "RawNIND Bayer model",
    url: None,
    sha256: BAYER_MODEL_SHA256,
    size: ArtifactSize::Exact(BAYER_MODEL_BYTES),
    progress_total: BAYER_MODEL_BYTES,
};
const LINEAR_MODEL_ARTIFACT: ModelArtifact = ModelArtifact {
    name: "RawNIND linear model",
    url: None,
    sha256: LINEAR_MODEL_SHA256,
    size: ArtifactSize::Exact(LINEAR_MODEL_BYTES),
    progress_total: LINEAR_MODEL_BYTES,
};
const RAWNIND_DOWNLOAD: DownloadOptions = DownloadOptions {
    connect_timeout: Duration::from_secs(30),
    response_timeout: Duration::from_secs(30),
    body_timeout: Duration::from_secs(30 * 60),
    attempts: 1,
    resume: false,
};
const TILE_EDGE: usize = 512;
const OVERLAP: usize = 64;
const CORE_EDGE: usize = TILE_EDGE - 2 * OVERLAP;
const MAX_MODEL_ABS: f32 = 60_000.0;
const RESULT_CACHE_MAGIC: [u8; 8] = *b"AURAWAI\0";
const RESULT_CACHE_VERSION: u32 = 2;
const RESULT_CACHE_MANIFEST: &str = "manifest.bin";
const RESULT_CACHE_PAYLOAD: &str = "denoised-pixels.bin";
const RESULT_CACHE_HEADER_BYTES: usize = 96;
const RESULT_CACHE_IO_CHUNK: usize = 1024 * 1024;
static NEXT_RESULT_CACHE_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum AiDenoiseEvent {
    DownloadProgress {
        downloaded: u64,
        total: u64,
    },
    Progress {
        phase: &'static str,
        completed: usize,
        total: usize,
    },
    Finished(Result<AiDenoisedImage, String>),
}

#[cfg(not(target_os = "android"))]
pub fn model_cache_dir() -> PathBuf {
    crate::desktop_model_cache_root().join("rawdenoise-nind-1.0")
}

pub fn result_cache_path(root: &Path, source_identity: &str) -> PathBuf {
    let digest = ring::digest::digest(&SHA256, source_identity.as_bytes());
    root.join(format!("{}.auraw-ai.zip", hex::encode(digest.as_ref())))
}

/// Loads a source- and process-validated derived RawNIND scene. A missing file
/// is an ordinary cache miss; malformed or stale files return an error so the
/// caller can discard them and regenerate from the original RAW.
pub fn load_result_cache(path: &Path, raw: &LoadedRaw) -> Result<Option<AiDenoisedImage>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("open {}", path.display())),
    };
    let mut archive = ZipArchive::new(file)
        .with_context(|| format!("read AI-denoise cache archive {}", path.display()))?;
    let header = {
        let mut entry = archive
            .by_name(RESULT_CACHE_MANIFEST)
            .context("AI-denoise cache has no manifest")?;
        anyhow::ensure!(
            entry.size() == RESULT_CACHE_HEADER_BYTES as u64,
            "AI-denoise cache manifest has an unexpected size"
        );
        let mut header = [0u8; RESULT_CACHE_HEADER_BYTES];
        entry
            .read_exact(&mut header)
            .context("read AI-denoise cache manifest")?;
        header
    };
    let manifest = ResultCacheManifest::decode(&header)?;
    anyhow::ensure!(
        manifest.width == raw.width && manifest.height == raw.height,
        "AI-denoise cache dimensions do not match the RAW"
    );
    anyhow::ensure!(
        manifest.cfa_kind == cfa_cache_code(raw.cfa_kind),
        "AI-denoise cache CFA type does not match the RAW"
    );
    let channels = match raw.cfa_kind {
        CfaKind::Bayer => 1,
        CfaKind::XTrans => 3,
    };
    let expected_elements = u64::from(raw.width)
        .checked_mul(u64::from(raw.height))
        .and_then(|pixels| pixels.checked_mul(channels))
        .and_then(|elements| usize::try_from(elements).ok())
        .context("AI-denoise cache dimensions overflow")?;
    let expected_bytes = expected_elements
        .checked_mul(std::mem::size_of::<u16>())
        .context("AI-denoise cache byte count overflow")?;
    anyhow::ensure!(
        manifest.payload_bytes == expected_bytes as u64,
        "AI-denoise cache payload size does not match the RAW"
    );
    anyhow::ensure!(
        manifest.source_sha256 == source_fingerprint(raw, None)?,
        "AI-denoise cache belongs to a different RAW reconstruction"
    );

    let mut payload = vec![0u16; expected_elements];
    {
        let mut entry = archive
            .by_name(RESULT_CACHE_PAYLOAD)
            .context("AI-denoise cache has no scene payload")?;
        anyhow::ensure!(
            entry.size() == expected_bytes as u64,
            "AI-denoise cache scene payload has an unexpected size"
        );
        entry
            .read_exact(bytemuck::cast_slice_mut(&mut payload))
            .context("read AI-denoise cache scene payload")?;
        let mut trailing = [0u8; 1];
        anyhow::ensure!(
            entry.read(&mut trailing)? == 0,
            "AI-denoise cache scene payload contains trailing data"
        );
    }
    let actual_payload = ring::digest::digest(&SHA256, bytemuck::cast_slice(&payload));
    anyhow::ensure!(
        actual_payload.as_ref() == manifest.payload_sha256,
        "AI-denoise cache scene checksum does not match"
    );
    match raw.cfa_kind {
        CfaKind::Bayer => AiDenoisedImage::new_bayer_cfa(raw.width, raw.height, payload),
        CfaKind::XTrans => AiDenoisedImage::new(raw.width, raw.height, payload),
    }
    .map(Some)
}

pub fn save_result_cache(
    path: &Path,
    raw: &LoadedRaw,
    image: &AiDenoisedImage,
    cancellation: &AtomicBool,
) -> Result<()> {
    anyhow::ensure!(
        image.is_valid_for(raw.width, raw.height),
        "cannot cache an AI-denoise result with mismatched dimensions"
    );
    ensure_not_cancelled(cancellation)?;
    let source_sha256 = source_fingerprint(raw, Some(cancellation))?;
    anyhow::ensure!(
        matches!(raw.cfa_kind, CfaKind::Bayer) == image.bayer_cfa().is_some(),
        "cannot cache an AI-denoise payload for a different CFA type"
    );
    let payload = bytemuck::cast_slice(image.payload());
    let payload_sha256 = digest_cancelable(payload, Some(cancellation))?;
    let manifest = ResultCacheManifest {
        width: raw.width,
        height: raw.height,
        cfa_kind: cfa_cache_code(raw.cfa_kind),
        payload_bytes: payload.len() as u64,
        source_sha256,
        payload_sha256,
    }
    .encode();

    let parent = path
        .parent()
        .context("AI-denoise cache path has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create AI-denoise cache directory {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("result.auraw-ai.zip");
    let temporary_id = NEXT_RESULT_CACHE_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.tmp-{}-{temporary_id}",
        std::process::id()
    ));
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        let mut archive = ZipWriter::new(file);
        let stored = FileOptions::default().compression_method(CompressionMethod::Stored);
        archive
            .start_file(RESULT_CACHE_MANIFEST, stored)
            .context("start AI-denoise cache manifest")?;
        archive
            .write_all(&manifest)
            .context("write AI-denoise cache manifest")?;
        let compressed = FileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(3));
        archive
            .start_file(RESULT_CACHE_PAYLOAD, compressed)
            .context("start AI-denoise cache scene payload")?;
        for chunk in payload.chunks(RESULT_CACHE_IO_CHUNK) {
            ensure_not_cancelled(cancellation)?;
            archive
                .write_all(chunk)
                .context("write AI-denoise cache scene payload")?;
        }
        let file = archive.finish().context("finalize AI-denoise cache")?;
        file.sync_all().context("flush AI-denoise cache")?;
        ensure_not_cancelled(cancellation)?;
        crate::file_ops::replace_file(&temporary, path)
            .with_context(|| format!("publish AI-denoise cache to {}", path.display()))?;
        crate::file_ops::sync_parent_directory(parent)
            .context("flush AI-denoise cache directory")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResultCacheManifest {
    width: u32,
    height: u32,
    cfa_kind: u32,
    payload_bytes: u64,
    source_sha256: [u8; 32],
    payload_sha256: [u8; 32],
}

impl ResultCacheManifest {
    fn encode(self) -> [u8; RESULT_CACHE_HEADER_BYTES] {
        let mut bytes = [0u8; RESULT_CACHE_HEADER_BYTES];
        bytes[0..8].copy_from_slice(&RESULT_CACHE_MAGIC);
        bytes[8..12].copy_from_slice(&RESULT_CACHE_VERSION.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.width.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.height.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.cfa_kind.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.payload_bytes.to_le_bytes());
        bytes[32..64].copy_from_slice(&self.source_sha256);
        bytes[64..96].copy_from_slice(&self.payload_sha256);
        bytes
    }

    fn decode(bytes: &[u8; RESULT_CACHE_HEADER_BYTES]) -> Result<Self> {
        anyhow::ensure!(
            bytes[0..8] == RESULT_CACHE_MAGIC,
            "invalid AI-denoise cache magic"
        );
        let read_u32 = |offset: usize| {
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("fixed header"))
        };
        let version = read_u32(8);
        anyhow::ensure!(
            version == RESULT_CACHE_VERSION,
            "AI-denoise cache format version {version} is stale"
        );
        Ok(Self {
            width: read_u32(12),
            height: read_u32(16),
            cfa_kind: read_u32(20),
            payload_bytes: u64::from_le_bytes(bytes[24..32].try_into().expect("fixed header")),
            source_sha256: bytes[32..64].try_into().expect("fixed header"),
            payload_sha256: bytes[64..96].try_into().expect("fixed header"),
        })
    }
}

fn cfa_cache_code(kind: CfaKind) -> u32 {
    match kind {
        CfaKind::Bayer => 1,
        CfaKind::XTrans => 2,
    }
}

fn source_fingerprint(raw: &LoadedRaw, cancellation: Option<&AtomicBool>) -> Result<[u8; 32]> {
    let mut digest = Sha256Context::new(&SHA256);
    digest.update(b"AuRaw RawNIND source fingerprint v1\0");
    digest.update(&raw.width.to_le_bytes());
    digest.update(&raw.height.to_le_bytes());
    digest.update(&cfa_cache_code(raw.cfa_kind).to_le_bytes());
    update_digest_cancelable(
        &mut digest,
        bytemuck::cast_slice(raw.raw_pixels.as_slice()),
        cancellation,
    )?;
    let (color_width, color_height, colors) = raw.color_indices.storage_parts();
    digest.update(&color_width.to_le_bytes());
    digest.update(&color_height.to_le_bytes());
    update_digest_cancelable(&mut digest, colors, cancellation)?;
    let (black_width, black_height, blacks) = raw.black_levels_per_pixel.storage_parts();
    digest.update(&black_width.to_le_bytes());
    digest.update(&black_height.to_le_bytes());
    update_digest_cancelable(&mut digest, bytemuck::cast_slice(blacks), cancellation)?;
    digest.update(bytemuck::bytes_of(&raw.wb_coeffs));
    digest.update(bytemuck::bytes_of(&raw.cam_to_srgb));
    digest.update(bytemuck::bytes_of(&raw.black_levels));
    digest.update(bytemuck::bytes_of(&raw.white_levels));
    Ok(digest
        .finish()
        .as_ref()
        .try_into()
        .expect("SHA-256 is always 32 bytes"))
}

fn digest_cancelable(bytes: &[u8], cancellation: Option<&AtomicBool>) -> Result<[u8; 32]> {
    let mut digest = Sha256Context::new(&SHA256);
    update_digest_cancelable(&mut digest, bytes, cancellation)?;
    Ok(digest
        .finish()
        .as_ref()
        .try_into()
        .expect("SHA-256 is always 32 bytes"))
}

fn update_digest_cancelable(
    digest: &mut Sha256Context,
    bytes: &[u8],
    cancellation: Option<&AtomicBool>,
) -> Result<()> {
    for chunk in bytes.chunks(RESULT_CACHE_IO_CHUNK) {
        if let Some(cancellation) = cancellation {
            ensure_not_cancelled(cancellation)?;
        }
        digest.update(chunk);
    }
    Ok(())
}

pub fn models_are_verified(model_dir: &Path) -> bool {
    verify_artifact(&model_dir.join("model_bayer.onnx"), BAYER_MODEL_ARTIFACT).is_ok()
        && verify_artifact(
            &model_dir.join("model_linear.onnx"),
            LINEAR_MODEL_ARTIFACT,
        )
        .is_ok()
}

#[allow(clippy::too_many_arguments)]
pub fn spawn_rawnind_denoise(
    model_dir: PathBuf,
    runtime_path: Option<PathBuf>,
    runtime_sha256: Option<String>,
    raw: Arc<LoadedRaw>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    result_cache_path: Option<PathBuf>,
    allow_model_download: bool,
    cancellation: Arc<AtomicBool>,
) -> mpsc::Receiver<AiDenoiseEvent> {
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let spawn = std::thread::Builder::new()
        .name("auraw-rawnind-denoise".to_owned())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (|| {
                    ensure_not_cancelled(&cancellation)?;
                    if let Some(path) = result_cache_path.as_deref() {
                        let _ = worker_sender.send(AiDenoiseEvent::Progress {
                            phase: "Restoring saved AI denoise",
                            completed: 0,
                            total: 0,
                        });
                        match load_result_cache(path, &raw) {
                            Ok(Some(image)) => {
                                crate::diagnostics::record(format!(
                                    "AI-denoise worker restored {} without model inference",
                                    path.display()
                                ));
                                return Ok(image);
                            }
                            Ok(None) => {}
                            Err(error) => {
                                log::warn!(
                                    "discarding invalid AI-denoise result cache {}: {error:#}",
                                    path.display()
                                );
                                crate::diagnostics::record(format!(
                                    "AI-denoise worker rejected saved result: {error:#}"
                                ));
                                if let Err(remove_error) = fs::remove_file(path) {
                                    if remove_error.kind() != std::io::ErrorKind::NotFound {
                                        log::warn!(
                                            "could not remove invalid AI-denoise cache {}: {remove_error}",
                                            path.display()
                                        );
                                    }
                                }
                            }
                        }
                        ensure_not_cancelled(&cancellation)?;
                    }
                    let _ = worker_sender.send(AiDenoiseEvent::Progress {
                        phase: "Checking RawNIND models",
                        completed: 0,
                        total: 0,
                    });
                    anyhow::ensure!(
                        allow_model_download || models_are_verified(&model_dir),
                        "the saved AI-denoise result is unavailable; enable AI denoise again to authorize any required model download"
                    );
                    ensure_models(&model_dir, &worker_sender, &cancellation)?;
                    ensure_not_cancelled(&cancellation)?;
                    let _ = worker_sender.send(AiDenoiseEvent::Progress {
                        phase: "Starting AI runtime",
                        completed: 0,
                        total: 0,
                    });
                    crate::ai_masks::initialize_runtime(
                        runtime_path.as_deref(),
                        runtime_sha256.as_deref(),
                    )?;
                    let image = match raw.cfa_kind {
                        CfaKind::Bayer => infer_bayer(
                            &model_dir.join("model_bayer.onnx"),
                            &raw,
                            &worker_sender,
                            &cancellation,
                        ),
                        CfaKind::XTrans => infer_linear(
                            &model_dir.join("model_linear.onnx"),
                            &raw,
                            device
                                .as_ref()
                                .context("X-Trans AI denoise requires AuRaw's wgpu device")?,
                            queue
                                .as_ref()
                                .context("X-Trans AI denoise requires AuRaw's wgpu queue")?,
                            &worker_sender,
                            &cancellation,
                        ),
                    }?;
                    if let Some(path) = result_cache_path.as_deref() {
                        ensure_not_cancelled(&cancellation)?;
                        let _ = worker_sender.send(AiDenoiseEvent::Progress {
                            phase: "Saving AI denoise result",
                            completed: 0,
                            total: 0,
                        });
                        if let Err(error) = save_result_cache(path, &raw, &image, &cancellation) {
                            ensure_not_cancelled(&cancellation)?;
                            log::warn!(
                                "could not persist AI-denoise result {}: {error:#}",
                                path.display()
                            );
                            crate::diagnostics::record(format!(
                                "AI-denoise result cache write failed for {}: {error:#}",
                                path.display()
                            ));
                        }
                    }
                    ensure_not_cancelled(&cancellation)?;
                    Ok(image)
                })()
            }))
            .unwrap_or_else(|panic| {
                let message = panic
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("unknown ONNX Runtime failure");
                Err(anyhow::anyhow!(
                    "ONNX Runtime terminated RawNIND denoise: {message}"
                ))
            });
            let _ = worker_sender.send(AiDenoiseEvent::Finished(
                result.map_err(|error| format!("{error:#}")),
            ));
        });
    if let Err(error) = spawn {
        let _ = sender.send(AiDenoiseEvent::Finished(Err(format!(
            "could not start RawNIND worker: {error}"
        ))));
    }
    receiver
}

fn ensure_not_cancelled(cancellation: &AtomicBool) -> Result<()> {
    anyhow::ensure!(
        !cancellation.load(Ordering::Acquire),
        "AI denoise cancelled"
    );
    Ok(())
}

fn ensure_models(
    model_dir: &Path,
    events: &mpsc::Sender<AiDenoiseEvent>,
    cancellation: &AtomicBool,
) -> Result<()> {
    let bayer = model_dir.join("model_bayer.onnx");
    let linear = model_dir.join("model_linear.onnx");
    if verify_artifact(&bayer, BAYER_MODEL_ARTIFACT).is_ok()
        && verify_artifact(&linear, LINEAR_MODEL_ARTIFACT).is_ok()
    {
        return Ok(());
    }
    fs::create_dir_all(model_dir)
        .with_context(|| format!("create RawNIND model cache {}", model_dir.display()))?;
    for path in [&bayer, &linear] {
        if path.exists() {
            fs::remove_file(path)
                .with_context(|| format!("remove invalid RawNIND model {}", path.display()))?;
        }
    }

    let package = model_dir.join("rawdenoise-nind.dtmodel");
    ensure_artifact(
        &package,
        RAWNIND_PACKAGE_ARTIFACT,
        RAWNIND_DOWNLOAD,
        |downloaded, total| {
            let _ = events.send(AiDenoiseEvent::DownloadProgress { downloaded, total });
        },
        || ensure_not_cancelled(cancellation),
    )?;
    ensure_not_cancelled(cancellation)?;
    extract_model(
        &package,
        "rawdenoise-nind/model_bayer.onnx",
        &bayer,
        BAYER_MODEL_ARTIFACT,
    )?;
    extract_model(
        &package,
        "rawdenoise-nind/model_linear.onnx",
        &linear,
        LINEAR_MODEL_ARTIFACT,
    )?;
    if let Err(error) = fs::remove_file(&package) {
        log::warn!(
            "could not remove verified RawNIND package {} after extraction: {error}",
            package.display()
        );
    }
    Ok(())
}

fn extract_model(
    package: &Path,
    member: &str,
    destination: &Path,
    artifact: ModelArtifact,
) -> Result<()> {
    if verify_artifact(destination, artifact).is_ok() {
        return Ok(());
    }
    let file = File::open(package).with_context(|| format!("open {}", package.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("read RawNIND dtmodel ZIP")?;
    let mut source = archive
        .by_name(member)
        .with_context(|| format!("find {member} in RawNIND package"))?;
    let ArtifactSize::Exact(expected_bytes) = artifact.size else {
        anyhow::bail!("RawNIND archive members must have an exact pinned size");
    };
    anyhow::ensure!(
        source.size() == expected_bytes,
        "{member} declares {} bytes, expected {expected_bytes}",
        source.size()
    );
    install_artifact_from_reader(destination, artifact, &mut source, || Ok(()))
        .with_context(|| format!("extract {member} from RawNIND package"))
}

fn run_model_tile(
    session: &mut FallbackSession,
    channels: usize,
    values: Vec<f32>,
    output_edge: usize,
) -> Result<Vec<f32>> {
    anyhow::ensure!(
        values.len() == channels * TILE_EDGE * TILE_EDGE,
        "RawNIND input tensor has the wrong length"
    );
    let input = Tensor::from_array(([1usize, channels, TILE_EDGE, TILE_EDGE], values))
        .context("create RawNIND input tensor")?;
    session.run_with_fallback(
        "RawNIND ONNX tile inference",
        |ort_session, _accelerated| {
            let outputs = ort_session
                .run(ort::inputs![&input])
                .context("run RawNIND ONNX inference")?;
            let output = outputs
                .values()
                .next()
                .context("RawNIND returned no output tensor")?;
            let (shape, values) = output
                .try_extract_tensor::<f32>()
                .context("read RawNIND output tensor")?;
            anyhow::ensure!(
                shape.as_ref() == [1, 3, output_edge as i64, output_edge as i64],
                "unexpected RawNIND output shape {shape:?}"
            );
            let non_finite = values.iter().filter(|value| !value.is_finite()).count();
            anyhow::ensure!(
                non_finite == 0,
                "RawNIND produced {non_finite} non-finite output values"
            );
            Ok(values.to_vec())
        },
    )
}

fn infer_bayer(
    model_path: &Path,
    raw: &LoadedRaw,
    events: &mpsc::Sender<AiDenoiseEvent>,
    cancellation: &AtomicBool,
) -> Result<AiDenoisedImage> {
    let (origin_x, origin_y) = bayer_rggb_origin(raw)?;
    let packed_width = (raw.width - origin_x) / 2;
    let packed_height = (raw.height - origin_y) / 2;
    anyhow::ensure!(
        packed_width > 0 && packed_height > 0,
        "Bayer RAW is too small for RawNIND"
    );
    let tiles_x = packed_width.div_ceil(CORE_EDGE as u32) as usize;
    let tiles_y = packed_height.div_ceil(CORE_EDGE as u32) as usize;
    let total_tiles = tiles_x * tiles_y;
    let output_elements = u64::from(raw.width)
        .checked_mul(u64::from(raw.height))
        .and_then(|elements| usize::try_from(elements).ok())
        .context("RawNIND Bayer output dimensions overflow")?;
    // Accumulate one denoised value per photosite. Keeping the model result as
    // CFA until the interactive/export pipeline is the crucial contract: the
    // full image is highlight-reconstructed and demosaicked only once, after
    // all tiles have been blended, and those stages remain responsive to edits.
    let mut normalized_cfa = vec![0.0f32; output_elements];
    let mut session = acquire_model_session(
        AiModel::RawNindBayer,
        model_path,
        SessionOptions::new("RawNIND Bayer"),
        ModelRetention::OneShot,
    )?;
    let model_white_balance = raw.rawnind_daylight_white_balance();
    for tile_y in 0..tiles_y {
        for tile_x in 0..tiles_x {
            ensure_not_cancelled(cancellation)?;
            let core_x = tile_x * CORE_EDGE;
            let core_y = tile_y * CORE_EDGE;
            let core_width = CORE_EDGE.min(packed_width as usize - core_x);
            let core_height = CORE_EDGE.min(packed_height as usize - core_y);
            let mut input = vec![0.0f32; 4 * TILE_EDGE * TILE_EDGE];
            for tile_row in 0..TILE_EDGE {
                let packed_y = reflect_index(
                    core_y as i64 + tile_row as i64 - OVERLAP as i64,
                    packed_height as usize,
                );
                for tile_column in 0..TILE_EDGE {
                    let packed_x = reflect_index(
                        core_x as i64 + tile_column as i64 - OVERLAP as i64,
                        packed_width as usize,
                    );
                    for (channel, ([phase_x, phase_y], wb_channel)) in [
                        ([0u32, 0u32], 0usize),
                        ([1, 0], 1),
                        ([0, 1], 1),
                        ([1, 1], 2),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        let x = origin_x + packed_x as u32 * 2 + phase_x;
                        let y = origin_y + packed_y as u32 * 2 + phase_y;
                        input[channel * TILE_EDGE * TILE_EDGE
                            + tile_row * TILE_EDGE
                            + tile_column] =
                            normalized_sensor_site(raw, x, y) * model_white_balance[wb_channel];
                    }
                }
            }
            let mut output = run_model_tile(&mut session, 4, input.clone(), TILE_EDGE * 2)?;
            match_gain_tile(&input, &mut output)?;
            let output_edge = TILE_EDGE * 2;
            let output_plane = output_edge * output_edge;
            let sensor_overlap = OVERLAP * 2;
            let core_start_x = origin_x as usize + core_x * 2;
            let core_start_y = origin_y as usize + core_y * 2;
            let core_end_x = core_start_x + core_width * 2;
            let core_end_y = core_start_y + core_height * 2;
            let has_left = tile_x > 0;
            let has_right = tile_x + 1 < tiles_x;
            let has_top = tile_y > 0;
            let has_bottom = tile_y + 1 < tiles_y;
            let working_end_x = origin_x as usize + packed_width as usize * 2;
            let working_end_y = origin_y as usize + packed_height as usize * 2;
            let extended_start_x = if has_left {
                core_start_x - sensor_overlap
            } else {
                core_start_x
            };
            let extended_start_y = if has_top {
                core_start_y - sensor_overlap
            } else {
                core_start_y
            };
            let extended_end_x = if has_right {
                core_end_x + sensor_overlap
            } else {
                core_end_x
            }
            .min(working_end_x);
            let extended_end_y = if has_bottom {
                core_end_y + sensor_overlap
            } else {
                core_end_y
            }
            .min(working_end_y);
            for destination_y in extended_start_y..extended_end_y {
                let model_y = sensor_overlap + destination_y - core_start_y;
                let weight_y = seam_weight(
                    destination_y,
                    core_start_y,
                    core_end_y,
                    sensor_overlap,
                    has_top,
                    has_bottom,
                );
                for destination_x in extended_start_x..extended_end_x {
                    let destination = destination_y * raw.width as usize + destination_x;
                    let model_x = sensor_overlap + destination_x - core_start_x;
                    let model_index = model_y * output_edge + model_x;
                    let weight_x = seam_weight(
                        destination_x,
                        core_start_x,
                        core_end_x,
                        sensor_overlap,
                        has_left,
                        has_right,
                    );
                    let channel = match raw.color_indices[destination] {
                        0 => 0,
                        2 => 2,
                        _ => 1,
                    };
                    let value =
                        output[channel * output_plane + model_index] / model_white_balance[channel];
                    anyhow::ensure!(
                        value.is_finite(),
                        "RawNIND Bayer remosaic produced a non-finite value"
                    );
                    normalized_cfa[destination] += value.clamp(0.0, 1.0) * weight_x * weight_y;
                }
            }
            let completed = tile_y * tiles_x + tile_x + 1;
            let _ = events.send(AiDenoiseEvent::Progress {
                phase: "Denoising Bayer mosaic",
                completed,
                total: total_tiles,
            });
        }
    }
    // RawNIND is one-shot in the common runtime manager, so its weights and
    // execution-provider allocations are released before this CPU-only work.
    drop(session);

    fill_bayer_crop_edges(
        &mut normalized_cfa,
        raw.width,
        raw.height,
        origin_x,
        origin_y,
        packed_width * 2,
        packed_height * 2,
    );
    // RawNIND can regress a saturated photosite slightly below one. If that
    // value replaced the sensor code verbatim, the downstream highlight stage
    // would lose the fact that the channel clipped and could reveal a false
    // pink/magenta model ratio when Exposure is reduced. Retain the original
    // high-SNR shoulder, with a smooth neighbourhood guard around actual
    // clipping. Noise reduction is preserved throughout shadows and midtones.
    const CLIP_THRESHOLD: f32 = 0.98;
    const HIGH_SNR_SHOULDER_START: f32 = 0.72;
    const CLIP_CORE_RADIUS: u8 = 4;
    const CLIP_FEATHER_RADIUS: u8 = 32;
    let width = raw.width as usize;
    let height = raw.height as usize;
    let mut clip_distance = vec![u8::MAX; output_elements];
    for y in 0..height {
        for x in 0..width {
            if normalized_sensor_site(raw, x as u32, y as u32) >= CLIP_THRESHOLD {
                clip_distance[y * width + x] = 0;
            }
        }
    }
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let mut distance = clip_distance[index];
            if x > 0 {
                distance = distance.min(clip_distance[index - 1].saturating_add(1));
            }
            if y > 0 {
                distance = distance.min(clip_distance[index - width].saturating_add(1));
            }
            clip_distance[index] = distance;
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let index = y * width + x;
            let mut distance = clip_distance[index];
            if x + 1 < width {
                distance = distance.min(clip_distance[index + 1].saturating_add(1));
            }
            if y + 1 < height {
                distance = distance.min(clip_distance[index + width].saturating_add(1));
            }
            clip_distance[index] = distance;
        }
    }
    let mut stored = vec![0u16; output_elements];
    for (index, destination) in stored.iter_mut().enumerate() {
        let cfa = raw.color_indices[index] as usize;
        let black = raw.black_levels_per_pixel[index];
        let white = raw.white_levels[cfa].max(black + 1.0);
        let original =
            ((f32::from(raw.raw_pixels[index]) - black) / (white - black)).clamp(0.0, 1.0);
        let distance = clip_distance[index];
        let proximity = if distance <= CLIP_CORE_RADIUS {
            1.0
        } else if distance < CLIP_FEATHER_RADIUS {
            let t = f32::from(CLIP_FEATHER_RADIUS - distance)
                / f32::from(CLIP_FEATHER_RADIUS - CLIP_CORE_RADIUS);
            t * t * (3.0 - 2.0 * t)
        } else {
            0.0
        };
        let shoulder_t = ((original - HIGH_SNR_SHOULDER_START)
            / (CLIP_THRESHOLD - HIGH_SNR_SHOULDER_START))
            .clamp(0.0, 1.0);
        let shoulder = shoulder_t * shoulder_t * (3.0 - 2.0 * shoulder_t);
        let source_weight = proximity.max(shoulder);
        let normalized = normalized_cfa[index].clamp(0.0, 1.0) * (1.0 - source_weight)
            + original * source_weight;
        let code = black + normalized * (white - black);
        *destination = code.round().clamp(0.0, f32::from(u16::MAX)) as u16;
    }
    AiDenoisedImage::new_bayer_cfa(raw.width, raw.height, stored)
}

/// RawNIND's Bayer graph produces three-channel camera RGB, but darktable's
/// production path deliberately does not inject those channels directly into
/// the scene. It selects the channel belonging to each CFA site, remosaics the
/// result and runs the ordinary demosaic stage. Besides preserving the normal
/// RAW pipeline contract, that step prevents small independent RGB edge
/// offsets in the neural output from becoming visible colour fringes.
#[cfg(test)]
fn remosaic_bayer_pixels(
    model_rgb: &[f32],
    edge: usize,
    model_white_balance: [f32; 3],
) -> Result<(Vec<u16>, Vec<u8>)> {
    let plane = edge
        .checked_mul(edge)
        .context("RawNIND remosaic tile dimensions overflow")?;
    anyhow::ensure!(
        edge > 0 && model_rgb.len() == plane * 3,
        "RawNIND remosaic received an unexpected model tensor"
    );
    let mut raw_pixels = vec![0u16; plane];
    let mut color_indices = vec![0u8; plane];
    for y in 0..edge {
        for x in 0..edge {
            let index = y * edge + x;
            let channel = match (x & 1, y & 1) {
                (0, 0) => 0,
                (1, 0) => 1,
                (0, 1) => 1,
                _ => 2,
            };
            color_indices[index] = match (x & 1, y & 1) {
                (0, 0) => 0,
                (1, 0) => 1,
                (0, 1) => 3,
                _ => 2,
            };
            let white_balance = model_white_balance[channel];
            anyhow::ensure!(
                white_balance.is_finite() && white_balance > 0.0,
                "RawNIND Bayer white balance is invalid"
            );
            let value = model_rgb[channel * plane + index] / white_balance;
            anyhow::ensure!(value.is_finite(), "RawNIND Bayer output is non-finite");
            raw_pixels[index] = (value.clamp(0.0, 1.0) * 65_535.0).round() as u16;
        }
    }
    Ok((raw_pixels, color_indices))
}

fn infer_linear(
    model_path: &Path,
    raw: &LoadedRaw,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    events: &mpsc::Sender<AiDenoiseEvent>,
    cancellation: &AtomicBool,
) -> Result<AiDenoisedImage> {
    let tiles_x = raw.width.div_ceil(CORE_EDGE as u32) as usize;
    let tiles_y = raw.height.div_ceil(CORE_EDGE as u32) as usize;
    let total_tiles = tiles_x * tiles_y;
    let output_elements = u64::from(raw.width)
        .checked_mul(u64::from(raw.height))
        .and_then(|pixels| pixels.checked_mul(3))
        .and_then(|elements| usize::try_from(elements).ok())
        .context("RawNIND linear output dimensions overflow")?;
    let mut stored = vec![0u16; output_elements];
    let mut session = acquire_model_session(
        AiModel::RawNindLinear,
        model_path,
        SessionOptions::new("RawNIND linear"),
        ModelRetention::OneShot,
    )?;
    let mut neutral = ExposureParams::scene_referred_default();
    neutral.ai_denoise_enabled = false;
    neutral.luminance_denoise = 0.0;
    neutral.chroma_denoise = 0.0;
    neutral.ca_red = 0.0;
    neutral.ca_blue = 0.0;
    let masks = MaskStack::default();
    let mut pipeline: Option<RawGpuPipeline> = None;
    let cam_to_rec2020 = multiply3(SRGB_TO_REC2020, rows3(raw.cam_to_srgb));
    let rec2020_to_cam =
        inverse3(cam_to_rec2020).context("camera-to-Rec.2020 matrix is singular")?;
    for tile_y in 0..tiles_y {
        for tile_x in 0..tiles_x {
            ensure_not_cancelled(cancellation)?;
            let core_x = tile_x * CORE_EDGE;
            let core_y = tile_y * CORE_EDGE;
            let core_width = CORE_EDGE.min(raw.width as usize - core_x);
            let core_height = CORE_EDGE.min(raw.height as usize - core_y);
            let origin_x = core_x as i32 - OVERLAP as i32;
            let origin_y = core_y as i32 - OVERLAP as i32;
            let tile_raw = reflected_raw_tile(raw, origin_x, origin_y)?;
            let params = GpuParams::new_for_tile(
                &neutral, &masks, &tile_raw, origin_x, origin_y, raw.width, raw.height,
            );
            if let Some(existing) = &pipeline {
                existing.upload_raw_tile(queue, &tile_raw)?;
            } else {
                pipeline = Some(RawGpuPipeline::new_headless_with_quality(
                    device,
                    queue,
                    &tile_raw,
                    &params,
                    ProcessingQuality::High,
                )?);
            }
            let camera = pipeline
                .as_ref()
                .context("X-Trans demosaic pipeline was not created")?
                .render_camera_scene_blocking(device, queue, &params)?;
            let mut input = vec![0.0f32; 3 * TILE_EDGE * TILE_EDGE];
            for pixel_index in 0..TILE_EDGE * TILE_EDGE {
                let rgb = mul3(
                    cam_to_rec2020,
                    [
                        camera[pixel_index * 3],
                        camera[pixel_index * 3 + 1],
                        camera[pixel_index * 3 + 2],
                    ],
                );
                for channel in 0..3 {
                    input[channel * TILE_EDGE * TILE_EDGE + pixel_index] = rgb[channel];
                }
            }
            let mut output = run_model_tile(&mut session, 3, input.clone(), TILE_EDGE)?;
            match_gain_tile(&input, &mut output)?;
            let has_left = tile_x > 0;
            let has_right = tile_x + 1 < tiles_x;
            let has_top = tile_y > 0;
            let has_bottom = tile_y + 1 < tiles_y;
            let core_end_x = core_x + core_width;
            let core_end_y = core_y + core_height;
            let extended_start_x = if has_left { core_x - OVERLAP } else { core_x };
            let extended_start_y = if has_top { core_y - OVERLAP } else { core_y };
            let extended_end_x = if has_right {
                core_end_x + OVERLAP
            } else {
                core_end_x
            }
            .min(raw.width as usize);
            let extended_end_y = if has_bottom {
                core_end_y + OVERLAP
            } else {
                core_end_y
            }
            .min(raw.height as usize);
            for destination_y in extended_start_y..extended_end_y {
                let model_y = OVERLAP + destination_y - core_y;
                let weight_y = seam_weight(
                    destination_y,
                    core_y,
                    core_end_y,
                    OVERLAP,
                    has_top,
                    has_bottom,
                );
                for destination_x in extended_start_x..extended_end_x {
                    let destination = (destination_y * raw.width as usize + destination_x) * 3;
                    let model_x = OVERLAP + destination_x - core_x;
                    let model_index = model_y * TILE_EDGE + model_x;
                    let weight_x = seam_weight(
                        destination_x,
                        core_x,
                        core_end_x,
                        OVERLAP,
                        has_left,
                        has_right,
                    );
                    accumulate_half_rgb(
                        &mut stored,
                        destination,
                        [
                            output[model_index],
                            output[TILE_EDGE * TILE_EDGE + model_index],
                            output[2 * TILE_EDGE * TILE_EDGE + model_index],
                        ],
                        weight_x * weight_y,
                        "RawNIND linear output",
                    )?;
                }
            }
            let completed = tile_y * tiles_x + tile_x + 1;
            let _ = events.send(AiDenoiseEvent::Progress {
                phase: "Demosaicing and denoising X-Trans",
                completed,
                total: total_tiles,
            });
        }
    }
    // The one-shot RawNIND session is gone before the remaining CPU-only
    // Rec.2020-to-camera conversion.
    drop(session);

    for pixel in stored.chunks_exact_mut(3) {
        let rec2020 = [
            half::f16::from_bits(pixel[0]).to_f32(),
            half::f16::from_bits(pixel[1]).to_f32(),
            half::f16::from_bits(pixel[2]).to_f32(),
        ];
        let camera = mul3(rec2020_to_cam, rec2020);
        for channel in 0..3 {
            anyhow::ensure!(
                camera[channel].is_finite() && camera[channel].abs() <= half::f16::MAX.to_f32(),
                "RawNIND linear colour conversion overflowed"
            );
            pixel[channel] = half::f16::from_f32(camera[channel]).to_bits();
        }
    }
    AiDenoisedImage::new(raw.width, raw.height, stored)
}

fn match_gain_tile(input: &[f32], output: &mut [f32]) -> Result<f32> {
    anyhow::ensure!(
        !input.is_empty() && !output.is_empty(),
        "RawNIND gain matching received no pixels"
    );
    let input_mean = input.iter().map(|value| f64::from(*value)).sum::<f64>() / input.len() as f64;
    let output_mean =
        output.iter().map(|value| f64::from(*value)).sum::<f64>() / output.len() as f64;
    let threshold = 1e-6 * input_mean.abs();
    anyhow::ensure!(
        input_mean.is_finite() && output_mean.is_finite() && output_mean.abs() > threshold,
        "RawNIND returned a degenerate mean"
    );
    let gain = (input_mean / output_mean) as f32;
    anyhow::ensure!(
        gain.is_finite() && gain.abs() <= 10_000.0,
        "RawNIND returned an implausible gain {gain}"
    );
    let mut maximum_abs = 0.0f32;
    for value in output {
        *value *= gain;
        maximum_abs = maximum_abs.max(value.abs());
    }
    anyhow::ensure!(
        maximum_abs.is_finite() && maximum_abs <= MAX_MODEL_ABS,
        "RawNIND gain-matched output is divergent (max |value| {maximum_abs})"
    );
    Ok(gain)
}

fn bayer_rggb_origin(raw: &LoadedRaw) -> Result<(u32, u32)> {
    anyhow::ensure!(
        raw.width >= 2 && raw.height >= 2,
        "Bayer RAW dimensions are too small"
    );
    for y in 0..2 {
        for x in 0..2 {
            if raw.color_indices[(y * raw.width + x) as usize] != 0 {
                continue;
            }
            let color = |dx: u32, dy: u32| {
                raw.color_indices[(((y + dy) % 2) * raw.width + (x + dx) % 2) as usize]
            };
            if matches!(color(1, 0), 1 | 3) && matches!(color(0, 1), 1 | 3) && color(1, 1) == 2 {
                return Ok((x, y));
            }
        }
    }
    Err(anyhow::anyhow!(
        "RawNIND Bayer path requires a canonical R/G/B 2x2 CFA"
    ))
}

fn normalized_sensor_site(raw: &LoadedRaw, x: u32, y: u32) -> f32 {
    let index = (y * raw.width + x) as usize;
    let channel = usize::from(raw.color_indices[index]).min(3);
    let black = raw.black_levels_per_pixel[index];
    let white = raw.white_levels[channel].max(black + 1.0);
    ((f32::from(raw.raw_pixels[index]) - black) / (white - black)).clamp(0.0, 1.0)
}

fn reflect_index(index: i64, length: usize) -> usize {
    if length <= 1 {
        return 0;
    }
    let period = (length as i64 - 1) * 2;
    let folded = index.rem_euclid(period);
    if folded < length as i64 {
        folded as usize
    } else {
        (period - folded) as usize
    }
}

fn seam_ramp(distance: usize, overlap: usize) -> f32 {
    (distance as f32 + 0.5) / (2 * overlap) as f32
}

/// Weight for one axis of darktable's RawNIND overlap blend. Neighboring
/// ramps sum to exactly one throughout the 2*overlap-wide seam.
fn seam_weight(
    coordinate: usize,
    core_start: usize,
    core_end: usize,
    overlap: usize,
    has_before: bool,
    has_after: bool,
) -> f32 {
    if has_before && coordinate < core_start + overlap {
        return seam_ramp(coordinate - (core_start - overlap), overlap);
    }
    if has_after && coordinate >= core_end - overlap {
        return 1.0 - seam_ramp(coordinate - (core_end - overlap), overlap);
    }
    1.0
}

fn accumulate_half_rgb(
    stored: &mut [u16],
    destination: usize,
    rgb: [f32; 3],
    weight: f32,
    phase: &str,
) -> Result<()> {
    anyhow::ensure!(
        weight.is_finite() && (0.0..=1.0).contains(&weight),
        "{phase} produced an invalid overlap weight"
    );
    for channel in 0..3 {
        let contribution = rgb[channel] * weight;
        let previous = half::f16::from_bits(stored[destination + channel]).to_f32();
        let value = previous + contribution;
        anyhow::ensure!(
            value.is_finite() && value.abs() <= half::f16::MAX.to_f32(),
            "{phase} produced a divergent value"
        );
        stored[destination + channel] = half::f16::from_f32(value).to_bits();
    }
    Ok(())
}

fn fill_bayer_crop_edges(
    values: &mut [f32],
    width: u32,
    height: u32,
    origin_x: u32,
    origin_y: u32,
    interior_width: u32,
    interior_height: u32,
) {
    let max_x = origin_x + interior_width - 1;
    let max_y = origin_y + interior_height - 1;
    for y in 0..height {
        for x in 0..width {
            if x >= origin_x && x <= max_x && y >= origin_y && y <= max_y {
                continue;
            }
            let source_x = x.clamp(origin_x, max_x);
            let source_y = y.clamp(origin_y, max_y);
            let source = (source_y * width + source_x) as usize;
            let destination = (y * width + x) as usize;
            values[destination] = values[source];
        }
    }
}

fn reflected_raw_tile(raw: &LoadedRaw, origin_x: i32, origin_y: i32) -> Result<LoadedRaw> {
    let pixels = TILE_EDGE * TILE_EDGE;
    let mut raw_pixels = vec![0u16; pixels];
    let mut colors = vec![0u8; pixels];
    let mut blacks = vec![0.0f32; pixels];
    for y in 0..TILE_EDGE {
        let source_y = reflect_index(i64::from(origin_y) + y as i64, raw.height as usize);
        for x in 0..TILE_EDGE {
            let source_x = reflect_index(i64::from(origin_x) + x as i64, raw.width as usize);
            let source = source_y * raw.width as usize + source_x;
            let destination = y * TILE_EDGE + x;
            raw_pixels[destination] = raw.raw_pixels[source];
            colors[destination] = raw.color_indices[source];
            blacks[destination] = raw.black_levels_per_pixel[source];
        }
    }
    Ok(LoadedRaw {
        width: TILE_EDGE as u32,
        height: TILE_EDGE as u32,
        camera_make: raw.camera_make.clone(),
        camera_model: raw.camera_model.clone(),
        lens_make: raw.lens_make.clone(),
        lens_model: raw.lens_model.clone(),
        focal_length: raw.focal_length,
        aperture: raw.aperture,
        focus_distance: raw.focus_distance,
        capture_metadata: raw.capture_metadata.clone(),
        cfa_kind: raw.cfa_kind,
        raw_pixels,
        scene_linear_raster: None,
        color_indices: CompactPixelMap::compact_from_dense(
            TILE_EDGE as u32,
            TILE_EDGE as u32,
            colors,
            64,
        ),
        wb_coeffs: raw.wb_coeffs,
        cam_to_srgb: raw.cam_to_srgb,
        black_levels: raw.black_levels,
        black_levels_per_pixel: CompactPixelMap::compact_from_dense(
            TILE_EDGE as u32,
            TILE_EDGE as u32,
            blacks,
            64,
        ),
        white_levels: raw.white_levels,
        noise_profile: raw.noise_profile,
        camera_profile: raw.camera_profile.clone(),
        camera_profile_source: raw.camera_profile_source.clone(),
        available_camera_profiles: raw.available_camera_profiles.clone(),
        white_balance_model: raw.white_balance_model.clone(),
        lens_geometry: None,
        ai_denoised: Arc::new(RwLock::new(None)),
        opposed_chroma_cache: Default::default(),
    })
}

type Matrix3 = [[f32; 3]; 3];

const SRGB_TO_REC2020: Matrix3 = [
    [0.627_403_9, 0.329_283, 0.043_313_1],
    [0.069_097_3, 0.919_540_4, 0.011_362_3],
    [0.016_391_4, 0.088_013_3, 0.895_595_3],
];

fn rows3(rows: [[f32; 4]; 3]) -> Matrix3 {
    rows.map(|row| [row[0], row[1], row[2]])
}

fn mul3(matrix: Matrix3, value: [f32; 3]) -> [f32; 3] {
    matrix.map(|row| row[0] * value[0] + row[1] * value[1] + row[2] * value[2])
}

fn multiply3(left: Matrix3, right: Matrix3) -> Matrix3 {
    std::array::from_fn(|row| {
        std::array::from_fn(|column| {
            (0..3)
                .map(|index| left[row][index] * right[index][column])
                .sum()
        })
    })
}

fn inverse3(matrix: Matrix3) -> Option<Matrix3> {
    let determinant = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    if !determinant.is_finite() || determinant.abs() < 1e-12 {
        return None;
    }
    let inverse = 1.0 / determinant;
    Some([
        [
            (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1]) * inverse,
            (matrix[0][2] * matrix[2][1] - matrix[0][1] * matrix[2][2]) * inverse,
            (matrix[0][1] * matrix[1][2] - matrix[0][2] * matrix[1][1]) * inverse,
        ],
        [
            (matrix[1][2] * matrix[2][0] - matrix[1][0] * matrix[2][2]) * inverse,
            (matrix[0][0] * matrix[2][2] - matrix[0][2] * matrix[2][0]) * inverse,
            (matrix[0][2] * matrix[1][0] - matrix[0][0] * matrix[1][2]) * inverse,
        ],
        [
            (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]) * inverse,
            (matrix[0][1] * matrix[2][0] - matrix[0][0] * matrix[2][1]) * inverse,
            (matrix[0][0] * matrix[1][1] - matrix[0][1] * matrix[1][0]) * inverse,
        ],
    ])
}

#[cfg(test)]
mod tests {
    use super::{
        bayer_rggb_origin, inverse3, load_result_cache, match_gain_tile, mul3, reflect_index,
        remosaic_bayer_pixels, result_cache_path, run_model_tile, save_result_cache, seam_weight,
        spawn_rawnind_denoise, AiDenoiseEvent, CORE_EDGE, SRGB_TO_REC2020, TILE_EDGE,
    };
    use crate::execution_provider::SessionOptions;
    use crate::model_runtime::{acquire_model_session, AiModel, ModelRetention};
    use crate::pipeline::{
        build_proxy, AiDenoisedImage, CameraProfile, CfaKind, CompactPixelMap, ExposureParams,
        GpuParams, LoadedRaw, MaskStack, NoiseProfile, ProcessingQuality, ProxySpec,
        RawGpuPipeline,
    };

    fn cache_test_raw(width: u32, height: u32) -> LoadedRaw {
        let pixels = (width * height) as usize;
        LoadedRaw {
            width,
            height,
            camera_make: "Cache Test".to_owned(),
            camera_model: "Synthetic".to_owned(),
            lens_make: String::new(),
            lens_model: String::new(),
            focal_length: 0.0,
            aperture: 0.0,
            focus_distance: 0.0,
            capture_metadata: Default::default(),
            cfa_kind: CfaKind::Bayer,
            raw_pixels: (0..pixels).map(|index| index as u16 * 17).collect(),
            scene_linear_raster: None,
            color_indices: CompactPixelMap::repeating(width, height, 2, 2, vec![0, 1, 3, 2]),
            wb_coeffs: [2.0, 1.0, 1.5, 1.0],
            cam_to_srgb: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            black_levels: [64.0; 4],
            black_levels_per_pixel: CompactPixelMap::repeating(width, height, 1, 1, vec![64.0]),
            white_levels: [16_383.0; 4],
            noise_profile: NoiseProfile::default(),
            camera_profile: CameraProfile::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
            lens_geometry: None,
            ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
            opposed_chroma_cache: Default::default(),
        }
    }

    fn adjacent_cfa_difference(image: &AiDenoisedImage, a: (u32, u32), b: (u32, u32)) -> f64 {
        let cfa = image.bayer_cfa().expect("Bayer integration result");
        let a = (a.1 * image.width + a.0) as usize;
        let b = (b.1 * image.width + b.0) as usize;
        f64::from(cfa[a].abs_diff(cfa[b]))
    }

    fn bayer_seam_gradient_ratio(raw: &LoadedRaw, image: &AiDenoisedImage) -> f64 {
        let (origin_x, origin_y) = bayer_rggb_origin(raw).unwrap();
        let packed_width = (raw.width - origin_x) / 2;
        let packed_height = (raw.height - origin_y) / 2;
        let mut seam = 0.0;
        let mut nearby = 0.0;
        let mut samples = 0u64;
        for packed_x in (CORE_EDGE as u32..packed_width).step_by(CORE_EDGE) {
            let x = origin_x + packed_x * 2;
            if x < 34 || x + 33 >= raw.width {
                continue;
            }
            for y in (origin_y..origin_y + packed_height * 2).step_by(8) {
                seam += adjacent_cfa_difference(image, (x - 2, y), (x, y));
                nearby += adjacent_cfa_difference(image, (x - 34, y), (x - 32, y));
                nearby += adjacent_cfa_difference(image, (x + 30, y), (x + 32, y));
                samples += 2;
            }
        }
        for packed_y in (CORE_EDGE as u32..packed_height).step_by(CORE_EDGE) {
            let y = origin_y + packed_y * 2;
            if y < 34 || y + 33 >= raw.height {
                continue;
            }
            for x in (origin_x..origin_x + packed_width * 2).step_by(8) {
                seam += adjacent_cfa_difference(image, (x, y - 2), (x, y));
                nearby += adjacent_cfa_difference(image, (x, y - 34), (x, y - 32));
                nearby += adjacent_cfa_difference(image, (x, y + 30), (x, y + 32));
                samples += 2;
            }
        }
        // `samples` counts the two nearby gradients per seam gradient.
        let seam_mean = seam / (samples.max(1) as f64 * 0.5);
        let nearby_mean = nearby / samples.max(1) as f64;
        seam_mean / nearby_mean.max(1e-12)
    }

    fn test_wgpu_device() -> (auraw_gpu::wgpu::Device, auraw_gpu::wgpu::Queue) {
        let instance = auraw_gpu::wgpu::Instance::default();
        let adapter = pollster::block_on(
            instance.request_adapter(&auraw_gpu::wgpu::RequestAdapterOptions::default()),
        )
        .expect("a wgpu adapter is required for RawNIND integration tests");
        pollster::block_on(adapter.request_device(&auraw_gpu::wgpu::DeviceDescriptor {
            label: Some("RawNIND integration test"),
            ..Default::default()
        }))
        .unwrap()
    }

    #[test]
    fn mirror_padding_matches_numpy_reflect_without_repeating_edges() {
        let values = (-5..=8)
            .map(|index| reflect_index(index, 4))
            .collect::<Vec<_>>();
        assert_eq!(values, [1, 2, 3, 2, 1, 0, 1, 2, 3, 2, 1, 0, 1, 2]);
    }

    #[test]
    fn rec2020_matrix_inverse_round_trips() {
        let inverse = inverse3(SRGB_TO_REC2020).unwrap();
        let sample = [0.13, 0.42, 0.91];
        let round_trip = mul3(inverse, mul3(SRGB_TO_REC2020, sample));
        for channel in 0..3 {
            assert!((round_trip[channel] - sample[channel]).abs() < 1e-5);
        }
    }

    #[test]
    fn bayer_remosaic_selects_only_the_channel_at_each_rggb_site() {
        let model_rgb = [
            0.1, 0.2, 0.3, 0.4, // R plane
            0.5, 0.6, 0.7, 0.8, // G plane
            0.9, 1.0, 0.4, 0.2, // B plane
        ];
        let (mosaic, cfa) = remosaic_bayer_pixels(&model_rgb, 2, [1.0; 3]).unwrap();
        let expected = [0.1f32, 0.6, 0.7, 0.2].map(|value| (value * 65_535.0).round() as u16);
        assert_eq!(mosaic, expected);
        assert_eq!(cfa, [0, 1, 3, 2]);
    }

    #[test]
    fn bayer_remosaic_reverses_model_daylight_white_balance() {
        let model_rgb = [
            0.8, 0.8, 0.8, 0.8, // R plane
            0.6, 0.6, 0.6, 0.6, // G plane
            0.4, 0.4, 0.4, 0.4, // B plane
        ];
        let (mosaic, _) = remosaic_bayer_pixels(&model_rgb, 2, [2.0, 1.0, 4.0]).unwrap();
        let expected = [0.4f32, 0.6, 0.6, 0.1].map(|value| (value * 65_535.0).round() as u16);
        assert_eq!(mosaic, expected);
    }

    #[test]
    fn neighboring_overlap_weights_sum_to_one() {
        const OVERLAP: usize = 64;
        const BOUNDARY: usize = 1_000;
        for coordinate in BOUNDARY - OVERLAP..BOUNDARY + OVERLAP {
            let left = seam_weight(coordinate, BOUNDARY - 384, BOUNDARY, OVERLAP, false, true);
            let right = seam_weight(coordinate, BOUNDARY, BOUNDARY + 384, OVERLAP, true, false);
            assert!((left + right - 1.0).abs() < f32::EPSILON);
            assert!(left > 0.0 && right > 0.0);
        }
    }

    #[test]
    fn result_cache_round_trips_and_rejects_changed_source() {
        let directory = std::env::temp_dir().join(format!(
            "auraw-ai-cache-test-{}-{}",
            std::process::id(),
            super::NEXT_RESULT_CACHE_TEMPORARY_ID
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed,)
        ));
        let path = result_cache_path(&directory, "synthetic-source");
        let mut raw = cache_test_raw(4, 4);
        let values = (0..4 * 4).map(|index| index as u16 * 97).collect();
        let expected = AiDenoisedImage::new_bayer_cfa(4, 4, values).unwrap();
        let cancellation = std::sync::atomic::AtomicBool::new(false);

        save_result_cache(&path, &raw, &expected, &cancellation).unwrap();
        let restored = load_result_cache(&path, &raw).unwrap().unwrap();
        assert_eq!(restored.raw_cfa16.as_ref(), expected.raw_cfa16.as_ref());

        raw.raw_pixels[3] ^= 1;
        assert!(load_result_cache(&path, &raw).is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn saved_result_worker_finishes_without_models_runtime_or_gpu() {
        let directory = std::env::temp_dir().join(format!(
            "auraw-ai-restore-test-{}-{}",
            std::process::id(),
            super::NEXT_RESULT_CACHE_TEMPORARY_ID
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let path = result_cache_path(&directory, "restore-only-source");
        let raw = std::sync::Arc::new(cache_test_raw(4, 4));
        let values = (0..4 * 4).map(|index| index as u16 * 97).collect();
        let expected = AiDenoisedImage::new_bayer_cfa(4, 4, values).unwrap();
        let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        save_result_cache(&path, &raw, &expected, &cancellation).unwrap();

        let receiver = spawn_rawnind_denoise(
            directory.join("models-do-not-exist"),
            None,
            None,
            std::sync::Arc::clone(&raw),
            None,
            None,
            Some(path),
            false,
            cancellation,
        );
        let restored = receiver
            .into_iter()
            .find_map(|event| match event {
                AiDenoiseEvent::Finished(result) => Some(result.unwrap()),
                _ => None,
            })
            .expect("restore worker must send a terminal event");
        assert_eq!(restored.raw_cfa16.as_ref(), expected.raw_cfa16.as_ref());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn cache_miss_cannot_download_models_without_consent() {
        let directory = std::env::temp_dir().join(format!(
            "auraw-ai-no-download-test-{}-{}",
            std::process::id(),
            super::NEXT_RESULT_CACHE_TEMPORARY_ID
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let raw = std::sync::Arc::new(cache_test_raw(4, 4));
        let cancellation = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let receiver = spawn_rawnind_denoise(
            directory.join("models-do-not-exist"),
            None,
            None,
            raw,
            None,
            None,
            Some(result_cache_path(&directory, "missing-source")),
            false,
            cancellation,
        );
        let error = receiver
            .into_iter()
            .find_map(|event| match event {
                AiDenoiseEvent::Finished(Err(error)) => Some(error),
                _ => None,
            })
            .expect("cache miss must terminate before model acquisition");
        assert!(error.contains("authorize any required model download"));
        assert!(!directory.join("models-do-not-exist").exists());
    }

    #[test]
    fn daylight_white_balance_falls_back_to_as_shot_coefficients() {
        assert_eq!(
            cache_test_raw(2, 2).rawnind_daylight_white_balance(),
            [2.0, 1.0, 1.5]
        );
    }

    /// Opt-in contract check for the pinned published graph. CI does not carry
    /// native ONNX Runtime or optional model downloads, so maintainers can run:
    /// `AURAW_RAWNIND_MODEL_DIR=... AURAW_ONNX_RUNTIME=... cargo test
    /// raw_nind_published_bayer_graph_contract -- --ignored`.
    #[test]
    #[ignore]
    fn raw_nind_published_bayer_graph_contract() {
        let model_dir = std::path::PathBuf::from(
            std::env::var_os("AURAW_RAWNIND_MODEL_DIR")
                .expect("set AURAW_RAWNIND_MODEL_DIR to the extracted dtmodel directory"),
        );
        let runtime = std::path::PathBuf::from(
            std::env::var_os("AURAW_ONNX_RUNTIME")
                .expect("set AURAW_ONNX_RUNTIME to libonnxruntime"),
        );
        let sha = crate::ai_masks::sha256_file_hex(&runtime).unwrap();
        crate::ai_masks::initialize_runtime(Some(&runtime), Some(&sha)).unwrap();
        let mut session = acquire_model_session(
            AiModel::RawNindBayer,
            model_dir.join("model_bayer.onnx"),
            SessionOptions::new("RawNIND Bayer"),
            ModelRetention::OneShot,
        )
        .unwrap();
        let mut output =
            run_model_tile(&mut session, 4, vec![0.1; 4 * TILE_EDGE * TILE_EDGE], 1024).unwrap();
        match_gain_tile(&vec![0.1; 4 * TILE_EDGE * TILE_EDGE], &mut output).unwrap();
        assert_eq!(output.len(), 3 * 1024 * 1024);
    }

    #[test]
    #[ignore]
    fn raw_nind_published_linear_graph_contract() {
        let model_dir = std::path::PathBuf::from(
            std::env::var_os("AURAW_RAWNIND_MODEL_DIR")
                .expect("set AURAW_RAWNIND_MODEL_DIR to the extracted dtmodel directory"),
        );
        let runtime = std::path::PathBuf::from(
            std::env::var_os("AURAW_ONNX_RUNTIME")
                .expect("set AURAW_ONNX_RUNTIME to libonnxruntime"),
        );
        let sha = crate::ai_masks::sha256_file_hex(&runtime).unwrap();
        crate::ai_masks::initialize_runtime(Some(&runtime), Some(&sha)).unwrap();
        let mut session = acquire_model_session(
            AiModel::RawNindLinear,
            model_dir.join("model_linear.onnx"),
            SessionOptions::new("RawNIND linear"),
            ModelRetention::OneShot,
        )
        .unwrap();
        let input = vec![0.1; 3 * TILE_EDGE * TILE_EDGE];
        let mut output = run_model_tile(&mut session, 3, input.clone(), TILE_EDGE).unwrap();
        match_gain_tile(&input, &mut output).unwrap();
        assert_eq!(output.len(), 3 * TILE_EDGE * TILE_EDGE);
    }

    #[test]
    #[ignore]
    fn raw_nind_bayer_end_to_end_fixture() {
        let model_dir = std::path::PathBuf::from(
            std::env::var_os("AURAW_RAWNIND_MODEL_DIR")
                .expect("set AURAW_RAWNIND_MODEL_DIR to the extracted dtmodel directory"),
        );
        let runtime = std::path::PathBuf::from(
            std::env::var_os("AURAW_ONNX_RUNTIME")
                .expect("set AURAW_ONNX_RUNTIME to libonnxruntime"),
        );
        let raw_path = std::path::PathBuf::from(
            std::env::var_os("AURAW_RAWNIND_TEST_RAW")
                .expect("set AURAW_RAWNIND_TEST_RAW to a Bayer RAW fixture"),
        );
        let sha = crate::ai_masks::sha256_file_hex(&runtime).unwrap();
        crate::ai_masks::initialize_runtime(Some(&runtime), Some(&sha)).unwrap();
        let raw = crate::pipeline::load_raw_file(&raw_path).unwrap();
        assert_eq!(raw.cfa_kind, crate::pipeline::CfaKind::Bayer);
        let (events, _receiver) = std::sync::mpsc::channel();
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let image = super::infer_bayer(
            &model_dir.join("model_bayer.onnx"),
            &raw,
            &events,
            &cancellation,
        )
        .unwrap();
        assert!(image.is_valid_for(raw.width, raw.height));
        assert_eq!(image.raw_cfa16.len(), raw.raw_pixels.len());
        let seam_ratio = bayer_seam_gradient_ratio(&raw, &image);
        eprintln!("RawNIND seam/nearby-gradient ratio: {seam_ratio:.4}");
        assert!(
            seam_ratio < 1.5,
            "RawNIND tile boundaries are stronger than nearby image gradients"
        );

        raw.set_ai_denoised_image(image).unwrap();
        let proxy = build_proxy(&raw, ProxySpec { max_edge: 1600 });
        let (device, queue) = test_wgpu_device();
        let render = |exposure_stops: f32, ai_enabled: bool| {
            let exposure = ExposureParams {
                exposure: exposure_stops,
                ai_denoise_enabled: ai_enabled,
                ..ExposureParams::default()
            };
            let params = GpuParams::new(&exposure, &MaskStack::default(), &proxy);
            let pipeline = RawGpuPipeline::new_headless_with_quality(
                &device,
                &queue,
                &proxy,
                &params,
                ProcessingQuality::Preview,
            )
            .unwrap();
            pipeline.recompute(&queue, &device, &params);
            pipeline
                .read_output_region_blocking(&device, &queue, 0, 0, proxy.width, proxy.height)
                .unwrap()
        };
        let is_pink = |pixel: &[u8]| {
            let red_blue = pixel[0].min(pixel[2]);
            red_blue > 24 && u16::from(pixel[1]) * 3 < u16::from(red_blue) * 2
        };
        for exposure_stops in [-1.0, -5.0] {
            let normal = render(exposure_stops, false);
            let denoised = render(exposure_stops, true);
            let false_pink_mask = normal
                .chunks_exact(4)
                .zip(denoised.chunks_exact(4))
                .map(|(normal, denoised)| !is_pink(normal) && is_pink(denoised))
                .collect::<Vec<_>>();
            let false_pink = false_pink_mask.iter().filter(|value| **value).count();
            let width = proxy.width as usize;
            let height = proxy.height as usize;
            let mut visited = vec![false; false_pink_mask.len()];
            let mut largest_region = 0usize;
            for seed in 0..false_pink_mask.len() {
                if !false_pink_mask[seed] || visited[seed] {
                    continue;
                }
                visited[seed] = true;
                let mut stack = vec![seed];
                let mut region = 0usize;
                while let Some(index) = stack.pop() {
                    region += 1;
                    let x = index % width;
                    let y = index / width;
                    for neighbor in [
                        (x > 0).then_some(index - 1),
                        (x + 1 < width).then_some(index + 1),
                        (y > 0).then_some(index - width),
                        (y + 1 < height).then_some(index + width),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if false_pink_mask[neighbor] && !visited[neighbor] {
                            visited[neighbor] = true;
                            stack.push(neighbor);
                        }
                    }
                }
                largest_region = largest_region.max(region);
            }
            eprintln!(
                "RawNIND {exposure_stops} EV newly pink pixels: {false_pink}/{}, largest region {largest_region}",
                normal.len() / 4
            );
            assert!(
                false_pink * 10_000 <= normal.len() / 4,
                "AI denoise creates visible false-pink highlight regions at {exposure_stops} EV"
            );
            assert!(
                largest_region <= 16,
                "AI denoise creates a contiguous false-pink region at {exposure_stops} EV"
            );
        }
    }

    #[test]
    #[ignore]
    fn raw_nind_xtrans_end_to_end_fixture() {
        let model_dir = std::path::PathBuf::from(
            std::env::var_os("AURAW_RAWNIND_MODEL_DIR")
                .expect("set AURAW_RAWNIND_MODEL_DIR to the extracted dtmodel directory"),
        );
        let runtime = std::path::PathBuf::from(
            std::env::var_os("AURAW_ONNX_RUNTIME")
                .expect("set AURAW_ONNX_RUNTIME to libonnxruntime"),
        );
        let raw_path = std::path::PathBuf::from(
            std::env::var_os("AURAW_RAWNIND_TEST_RAW")
                .expect("set AURAW_RAWNIND_TEST_RAW to an X-Trans RAW fixture"),
        );
        let sha = crate::ai_masks::sha256_file_hex(&runtime).unwrap();
        crate::ai_masks::initialize_runtime(Some(&runtime), Some(&sha)).unwrap();
        let raw = crate::pipeline::load_raw_file(&raw_path).unwrap();
        assert_eq!(raw.cfa_kind, crate::pipeline::CfaKind::XTrans);
        let (device, queue) = test_wgpu_device();
        let (events, _receiver) = std::sync::mpsc::channel();
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let image = super::infer_linear(
            &model_dir.join("model_linear.onnx"),
            &raw,
            &device,
            &queue,
            &events,
            &cancellation,
        )
        .unwrap();
        assert!(image.is_valid_for(raw.width, raw.height));
        assert!(image
            .rgb16f
            .iter()
            .all(|value| { half::f16::from_bits(*value).to_f32().is_finite() }));
    }
}
