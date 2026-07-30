//! RawNIND UtNet2 model acquisition and tiled RAW inference.
//!
//! The downloaded `.dtmodel` package is the published darktable-ai 5.6 release
//! asset. AuRaw pins both the archive and extracted ONNX graphs by SHA-256.

use anyhow::{Context, Result};
use eframe::wgpu;
use ort::{session::Session, value::Tensor};
use ring::digest::{Context as Sha256Context, SHA256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, RwLock,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::pipeline::{
    AiDenoisedImage, CfaKind, CompactPixelMap, ExposureParams, GpuParams,
    HighlightReconstructionMethod, LoadedRaw, MaskStack, ProcessingQuality, RawGpuPipeline,
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
const TILE_EDGE: usize = 512;
const OVERLAP: usize = 64;
const CORE_EDGE: usize = TILE_EDGE - 2 * OVERLAP;
const MAX_MODEL_ABS: f32 = 60_000.0;

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

pub fn model_cache_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        // The caller substitutes the application-private root on Android.
        std::env::temp_dir().join("auraw/models/rawdenoise-nind-1.0")
    }
    #[cfg(not(target_os = "android"))]
    {
        let root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("auraw/models/rawdenoise-nind-1.0")
    }
}

pub fn models_are_verified(model_dir: &Path) -> bool {
    verify_file(
        &model_dir.join("model_bayer.onnx"),
        BAYER_MODEL_BYTES,
        BAYER_MODEL_SHA256,
    )
    .is_ok()
        && verify_file(
            &model_dir.join("model_linear.onnx"),
            LINEAR_MODEL_BYTES,
            LINEAR_MODEL_SHA256,
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
                    ensure_models(&model_dir, &worker_sender, &cancellation)?;
                    ensure_not_cancelled(&cancellation)?;
                    crate::ai_masks::initialize_runtime(
                        runtime_path.as_deref(),
                        runtime_sha256.as_deref(),
                    )?;
                    match raw.cfa_kind {
                        CfaKind::Bayer => infer_bayer(
                            &model_dir.join("model_bayer.onnx"),
                            &raw,
                            device
                                .as_ref()
                                .context("Bayer AI denoise requires AuRaw's wgpu device")?,
                            queue
                                .as_ref()
                                .context("Bayer AI denoise requires AuRaw's wgpu queue")?,
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
                    }
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
    if verify_file(&bayer, BAYER_MODEL_BYTES, BAYER_MODEL_SHA256).is_ok()
        && verify_file(&linear, LINEAR_MODEL_BYTES, LINEAR_MODEL_SHA256).is_ok()
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
    if verify_file(&package, RAWNIND_PACKAGE_BYTES, RAWNIND_PACKAGE_SHA256).is_err() {
        if package.exists() {
            fs::remove_file(&package)
                .with_context(|| format!("remove invalid RawNIND package {}", package.display()))?;
        }
        download_package(&package, events, cancellation)?;
    }
    ensure_not_cancelled(cancellation)?;
    extract_model(
        &package,
        "rawdenoise-nind/model_bayer.onnx",
        &bayer,
        BAYER_MODEL_BYTES,
        BAYER_MODEL_SHA256,
    )?;
    extract_model(
        &package,
        "rawdenoise-nind/model_linear.onnx",
        &linear,
        LINEAR_MODEL_BYTES,
        LINEAR_MODEL_SHA256,
    )?;
    if let Err(error) = fs::remove_file(&package) {
        log::warn!(
            "could not remove verified RawNIND package {} after extraction: {error}",
            package.display()
        );
    }
    Ok(())
}

fn verify_file(path: &Path, expected_bytes: u64, expected_sha256: &str) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("read {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "{} is not a regular file",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() == expected_bytes,
        "{} has {} bytes, expected {expected_bytes}",
        path.display(),
        metadata.len()
    );
    let actual = sha256_file(path)?;
    anyhow::ensure!(
        actual == expected_sha256,
        "{} SHA-256 mismatch",
        path.display()
    );
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
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
    Ok(hex_digest(hasher.finish().as_ref()))
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn download_package(
    path: &Path,
    events: &mpsc::Sender<AiDenoiseEvent>,
    cancellation: &AtomicBool,
) -> Result<()> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("dtmodel.{}.{}.part", std::process::id(), nonce));
    let result = (|| -> Result<()> {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(30 * 60)))
            .build();
        let agent: ureq::Agent = config.into();
        let mut response = agent
            .get(RAWNIND_PACKAGE_URL)
            .call()
            .context("download darktable RawNIND model package")?;
        if let Some(length) = response.body().content_length() {
            anyhow::ensure!(
                length == RAWNIND_PACKAGE_BYTES,
                "RawNIND server declared {length} bytes, expected {RAWNIND_PACKAGE_BYTES}"
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
            ensure_not_cancelled(cancellation)?;
            let read = reader.read(&mut buffer).context("read RawNIND download")?;
            if read == 0 {
                break;
            }
            downloaded = downloaded
                .checked_add(read as u64)
                .context("RawNIND download byte count overflow")?;
            anyhow::ensure!(
                downloaded <= RAWNIND_PACKAGE_BYTES,
                "RawNIND download exceeded its pinned size"
            );
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .context("write RawNIND package")?;
            let _ = events.send(AiDenoiseEvent::DownloadProgress {
                downloaded,
                total: RAWNIND_PACKAGE_BYTES,
            });
        }
        file.sync_all().context("flush RawNIND package")?;
        anyhow::ensure!(
            downloaded == RAWNIND_PACKAGE_BYTES,
            "RawNIND package received {downloaded} bytes, expected {RAWNIND_PACKAGE_BYTES}"
        );
        anyhow::ensure!(
            hex_digest(hasher.finish().as_ref()) == RAWNIND_PACKAGE_SHA256,
            "RawNIND package SHA-256 mismatch"
        );
        ensure_not_cancelled(cancellation)?;
        fs::rename(&temporary, path)
            .with_context(|| format!("publish RawNIND package to {}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn extract_model(
    package: &Path,
    member: &str,
    destination: &Path,
    expected_bytes: u64,
    expected_sha256: &str,
) -> Result<()> {
    if verify_file(destination, expected_bytes, expected_sha256).is_ok() {
        return Ok(());
    }
    let file = File::open(package).with_context(|| format!("open {}", package.display()))?;
    let mut archive = zip::ZipArchive::new(file).context("read RawNIND dtmodel ZIP")?;
    let mut source = archive
        .by_name(member)
        .with_context(|| format!("find {member} in RawNIND package"))?;
    anyhow::ensure!(
        source.size() == expected_bytes,
        "{member} declares {} bytes, expected {expected_bytes}",
        source.size()
    );
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary =
        destination.with_extension(format!("onnx.{}.{}.part", std::process::id(), nonce));
    let result = (|| -> Result<()> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("create {}", temporary.display()))?;
        let copied = std::io::copy(&mut source, &mut output).context("extract RawNIND ONNX")?;
        anyhow::ensure!(
            copied == expected_bytes,
            "extracted {copied} bytes for {member}, expected {expected_bytes}"
        );
        output.sync_all().context("flush extracted RawNIND model")?;
        verify_file(&temporary, expected_bytes, expected_sha256)?;
        fs::rename(&temporary, destination)
            .with_context(|| format!("publish {}", destination.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn create_bayer_session(model_path: &Path) -> Result<Session> {
    // The published model card documents FP16 activation overflow on Apple
    // CoreML GPU/ANE for this graph. Keep Bayer on CPU there; the linear graph
    // does not have that limitation.
    #[cfg(target_os = "macos")]
    {
        crate::ai_masks::create_cpu_session(model_path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::ai_masks::create_session(model_path)
    }
}

fn run_model_tile(
    session: &mut Session,
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
    let outputs = session
        .run(ort::inputs![input])
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
}

fn infer_bayer(
    model_path: &Path,
    raw: &LoadedRaw,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
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
        .and_then(|pixels| pixels.checked_mul(3))
        .and_then(|elements| usize::try_from(elements).ok())
        .context("RawNIND Bayer output dimensions overflow")?;
    let mut stored = vec![0u16; output_elements];
    let mut session = create_bayer_session(model_path)?;
    let mut neutral = ExposureParams::scene_referred_default();
    neutral.ai_denoise_enabled = false;
    neutral.luminance_denoise = 0.0;
    neutral.chroma_denoise = 0.0;
    neutral.ca_red = 0.0;
    neutral.ca_blue = 0.0;
    neutral.highlight_method = HighlightReconstructionMethod::Off;
    neutral.highlight_reconstruction = 0.0;
    let masks = MaskStack::default();
    let mut demosaic_pipeline: Option<RawGpuPipeline> = None;
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
                    for (channel, [phase_x, phase_y]) in [[0u32, 0u32], [1, 0], [0, 1], [1, 1]]
                        .into_iter()
                        .enumerate()
                    {
                        let x = origin_x + packed_x as u32 * 2 + phase_x;
                        let y = origin_y + packed_y as u32 * 2 + phase_y;
                        input[channel * TILE_EDGE * TILE_EDGE
                            + tile_row * TILE_EDGE
                            + tile_column] = normalized_sensor_site(raw, x, y);
                    }
                }
            }
            let mut output = run_model_tile(&mut session, 4, input.clone(), TILE_EDGE * 2)?;
            match_gain_tile(&input, &mut output)?;
            let output_edge = TILE_EDGE * 2;
            let remosaicked = remosaic_bayer_model_tile(raw, &output, output_edge)?;
            let params = GpuParams::new(&neutral, &masks, &remosaicked);
            if let Some(existing) = &demosaic_pipeline {
                existing.upload_raw_tile(queue, &remosaicked)?;
            } else {
                demosaic_pipeline = Some(RawGpuPipeline::new_headless_with_quality(
                    device,
                    queue,
                    &remosaicked,
                    &params,
                    ProcessingQuality::High,
                )?);
            }
            let camera = demosaic_pipeline
                .as_ref()
                .context("Bayer remosaic pipeline was not created")?
                .render_camera_scene_blocking(device, queue, &params)?;
            for row in 0..core_height * 2 {
                let destination_y = origin_y as usize + core_y * 2 + row;
                for column in 0..core_width * 2 {
                    let destination_x = origin_x as usize + core_x * 2 + column;
                    let destination = (destination_y * raw.width as usize + destination_x) * 3;
                    let model_index = (OVERLAP * 2 + row) * output_edge + OVERLAP * 2 + column;
                    for channel in 0..3 {
                        let value = camera[model_index * 3 + channel];
                        anyhow::ensure!(
                            value.is_finite() && value.abs() <= half::f16::MAX.to_f32(),
                            "RawNIND Bayer remosaic/demosaic produced a divergent value"
                        );
                        stored[destination + channel] = half::f16::from_f32(value).to_bits();
                    }
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

    fill_bayer_crop_edges(
        &mut stored,
        raw.width,
        raw.height,
        origin_x,
        origin_y,
        packed_width * 2,
        packed_height * 2,
    );
    AiDenoisedImage::new(raw.width, raw.height, stored)
}

/// RawNIND's Bayer graph produces three-channel camera RGB, but darktable's
/// production path deliberately does not inject those channels directly into
/// the scene. It selects the channel belonging to each CFA site, remosaics the
/// result and runs the ordinary demosaic stage. Besides preserving the normal
/// RAW pipeline contract, that step prevents small independent RGB edge
/// offsets in the neural output from becoming visible colour fringes.
fn remosaic_bayer_model_tile(
    source: &LoadedRaw,
    model_rgb: &[f32],
    edge: usize,
) -> Result<LoadedRaw> {
    anyhow::ensure!(
        edge == TILE_EDGE * 2,
        "RawNIND remosaic received an unexpected model tensor"
    );
    let (raw_pixels, color_indices) = remosaic_bayer_pixels(model_rgb, edge)?;
    Ok(LoadedRaw {
        width: edge as u32,
        height: edge as u32,
        camera_make: source.camera_make.clone(),
        camera_model: source.camera_model.clone(),
        lens_make: source.lens_make.clone(),
        lens_model: source.lens_model.clone(),
        focal_length: source.focal_length,
        aperture: source.aperture,
        focus_distance: source.focus_distance,
        capture_metadata: source.capture_metadata.clone(),
        cfa_kind: CfaKind::Bayer,
        raw_pixels,
        color_indices: CompactPixelMap::compact_from_dense(
            edge as u32,
            edge as u32,
            color_indices,
            64,
        ),
        wb_coeffs: source.wb_coeffs,
        cam_to_srgb: source.cam_to_srgb,
        black_levels: [0.0; 4],
        black_levels_per_pixel: CompactPixelMap::repeating(
            edge as u32,
            edge as u32,
            1,
            1,
            vec![0.0],
        ),
        white_levels: [65_535.0; 4],
        noise_profile: source.noise_profile,
        camera_profile: source.camera_profile.clone(),
        camera_profile_source: source.camera_profile_source.clone(),
        available_camera_profiles: source.available_camera_profiles.clone(),
        white_balance_model: source.white_balance_model.clone(),
        lens_geometry: None,
        ai_denoised: Arc::new(RwLock::new(None)),
    })
}

fn remosaic_bayer_pixels(model_rgb: &[f32], edge: usize) -> Result<(Vec<u16>, Vec<u8>)> {
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
            let value = model_rgb[channel * plane + index];
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
    let mut session = crate::ai_masks::create_session(model_path)?;
    let mut neutral = ExposureParams::scene_referred_default();
    neutral.ai_denoise_enabled = false;
    neutral.luminance_denoise = 0.0;
    neutral.chroma_denoise = 0.0;
    neutral.ca_red = 0.0;
    neutral.ca_blue = 0.0;
    neutral.highlight_method = HighlightReconstructionMethod::Off;
    neutral.highlight_reconstruction = 0.0;
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
            for row in 0..core_height {
                let destination_y = core_y + row;
                for column in 0..core_width {
                    let destination_x = core_x + column;
                    let destination = (destination_y * raw.width as usize + destination_x) * 3;
                    let model_index = (OVERLAP + row) * TILE_EDGE + OVERLAP + column;
                    for channel in 0..3 {
                        let value = output[channel * TILE_EDGE * TILE_EDGE + model_index];
                        stored[destination + channel] = half::f16::from_f32(value).to_bits();
                    }
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

fn fill_bayer_crop_edges(
    values: &mut [u16],
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
            let source = ((source_y * width + source_x) * 3) as usize;
            let destination = ((y * width + x) * 3) as usize;
            values.copy_within(source..source + 3, destination);
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
        create_bayer_session, inverse3, match_gain_tile, mul3, reflect_index,
        remosaic_bayer_pixels, run_model_tile, SRGB_TO_REC2020, TILE_EDGE,
    };

    fn test_wgpu_device() -> (eframe::wgpu::Device, eframe::wgpu::Queue) {
        let instance = eframe::wgpu::Instance::default();
        let adapter = pollster::block_on(
            instance.request_adapter(&eframe::wgpu::RequestAdapterOptions::default()),
        )
        .expect("a wgpu adapter is required for RawNIND integration tests");
        pollster::block_on(adapter.request_device(&eframe::wgpu::DeviceDescriptor {
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
        let (mosaic, cfa) = remosaic_bayer_pixels(&model_rgb, 2).unwrap();
        let expected = [0.1f32, 0.6, 0.7, 0.2].map(|value| (value * 65_535.0).round() as u16);
        assert_eq!(mosaic, expected);
        assert_eq!(cfa, [0, 1, 3, 2]);
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
        let mut session = create_bayer_session(&model_dir.join("model_bayer.onnx")).unwrap();
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
        let mut session =
            crate::ai_masks::create_session(&model_dir.join("model_linear.onnx")).unwrap();
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
        let (device, queue) = test_wgpu_device();
        let (events, _receiver) = std::sync::mpsc::channel();
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let image = super::infer_bayer(
            &model_dir.join("model_bayer.onnx"),
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
