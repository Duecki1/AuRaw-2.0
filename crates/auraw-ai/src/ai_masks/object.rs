//! Promptable object-mask inference using SAM 2.1 Hiera Tiny.

use super::*;

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
pub(super) const SAM21_ENCODER_MAX_BYTES: u64 = 160_000_000;
pub(super) const SAM21_DECODER_MAX_BYTES: u64 = 32_000_000;

pub(super) const SAM21_ENCODER_ARTIFACT: ModelArtifact = ModelArtifact {
    name: "SAM 2.1 encoder",
    url: Some(SAM21_ENCODER_MODEL_URL),
    sha256: SAM21_ENCODER_SHA256_HEX,
    size: ArtifactSize::Max(SAM21_ENCODER_MAX_BYTES),
    progress_total: 109_000_000,
};
pub(super) const SAM21_DECODER_ARTIFACT: ModelArtifact = ModelArtifact {
    name: "SAM 2.1 decoder",
    url: Some(SAM21_DECODER_MODEL_URL),
    sha256: SAM21_DECODER_SHA256_HEX,
    size: ArtifactSize::Max(SAM21_DECODER_MAX_BYTES),
    progress_total: 16_500_000,
};
const SAM_DOWNLOAD: DownloadOptions = DownloadOptions {
    connect_timeout: Duration::from_secs(45),
    response_timeout: Duration::from_secs(60),
    body_timeout: Duration::from_secs(30 * 60),
    attempts: 5,
    resume: true,
};
pub(super) const SAM21_ENCODER_INSTALL: ModelInstallSpec = ModelInstallSpec {
    artifact: SAM21_ENCODER_ARTIFACT,
    download: SAM_DOWNLOAD,
    progress_label: "SAM 2.1 encoder",
};
pub(super) const SAM21_DECODER_INSTALL: ModelInstallSpec = ModelInstallSpec {
    artifact: SAM21_DECODER_ARTIFACT,
    download: SAM_DOWNLOAD,
    progress_label: "SAM 2.1 decoder",
};
const MAX_OBJECT_MASK_PIXELS: u64 = 17_000_000;

#[cfg(not(target_os = "android"))]
pub(super) static SAM_ENCODER_SESSION: OnceLock<Mutex<Option<FallbackSession>>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
pub(super) static SAM_DECODER_SESSION: OnceLock<Mutex<Option<FallbackSession>>> = OnceLock::new();

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
    pub prompt_brush_size: f32,
}

#[derive(Clone, Debug)]
pub struct ObjectMaskRequest {
    pub source_width: u32,
    pub source_height: u32,
    pub source_rgba: Vec<u8>,
    pub strokes: Vec<crate::pipeline::ObjectStroke>,
    pub brush_size: f32,
    pub edge_refine: f32,
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
    DownloadProgress(ModelDownloadProgress),
    Inferencing {
        decoder_only: bool,
    },
    Finished(Result<ObjectMaskResult, String>),
}

pub fn spawn_object_mask(
    encoder_path: PathBuf,
    decoder_path: PathBuf,
    vitmatte_path: PathBuf,
    allow_download: bool,
    runtime_path: Option<PathBuf>,
    runtime_sha256: Option<String>,
    request: ObjectMaskRequest,
    cancellation: Arc<AtomicBool>,
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
                        SAM21_ENCODER_INSTALL,
                        allow_download,
                        &worker_sender,
                        &cancellation,
                    )?;
                    ensure_sam_model(
                        &decoder_path,
                        SAM21_DECODER_INSTALL,
                        allow_download,
                        &worker_sender,
                        &cancellation,
                    )?;
                    ensure_vitmatte_model(
                        &vitmatte_path,
                        allow_download,
                        &cancellation,
                        |progress| {
                            let _ = worker_sender.send(ObjectMaskEvent::DownloadProgress(progress));
                        },
                    )?;
                    ensure_ai_not_cancelled(&cancellation)?;
                    let decoder_only = request.cache.is_some();
                    let _ = worker_sender.send(ObjectMaskEvent::Inferencing { decoder_only });
                    infer_object_mask(
                        &encoder_path,
                        &decoder_path,
                        &vitmatte_path,
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
    install: ModelInstallSpec,
    allow_download: bool,
    events: &mpsc::Sender<ObjectMaskEvent>,
    cancellation: &AtomicBool,
) -> Result<()> {
    install.ensure_installed(
        path,
        allow_download,
        |progress| {
            let _ = events.send(ObjectMaskEvent::DownloadProgress(progress));
        },
        || ensure_ai_not_cancelled(cancellation),
    )
}

fn infer_object_mask(
    encoder_path: &Path,
    decoder_path: &Path,
    vitmatte_path: &Path,
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

    let prompt_set = sampled_object_prompts(
        &request.strokes,
        request.brush_size,
        request.source_width,
        request.source_height,
        SAM21_MAX_PROMPTS,
    );
    let prompts = &prompt_set.prompts;
    let supplied_cache = request.cache;
    let mut last_result = None;

    for expansion in 0..3 {
        let crop = object_crop_for_prompts(
            request.source_width,
            request.source_height,
            prompt_set.focus,
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
            let can_reuse_logits = strokes_extend(&cache.prompt_strokes, &request.strokes)
                && (cache.prompt_brush_size - request.brush_size).abs() <= f32::EPSILON;
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
        let decoded = decode_sam_mask(decoder_path, &features, &prompt_set, previous_logits)?;
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
        prompts,
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
    full_mask = match refine_mask_with_vitmatte(
        vitmatte_path,
        source.as_raw(),
        request.source_width,
        request.source_height,
        &full_mask,
        (0.65 + request.edge_refine.clamp(0.0, 1.0) * 0.35).clamp(0.0, 1.0),
    ) {
        Ok(refined) => refined,
        Err(error) => {
            // ViTMatte is an edge refinement stage, not the source of the
            // object selection. A provider/model failure must not discard a
            // perfectly usable SAM mask and look like a cancelled operation.
            log::warn!(
                "ViTMatte object-edge refinement failed; using the cleaned SAM mask: {error:#}"
            );
            full_mask
        }
    };
    cache.low_res_logits = resize_f32(
        &decoded.selected_logits,
        decoded.width,
        decoded.height,
        SAM21_MASK_INPUT_SIZE,
        SAM21_MASK_INPUT_SIZE,
    )
    .into();
    cache.prompt_strokes = request.strokes.clone();
    cache.prompt_brush_size = request.brush_size;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjectPromptKind {
    Foreground,
    Background,
    BoxTopLeft,
    BoxBottomRight,
}

impl ObjectPromptKind {
    const fn sam_label(self) -> f32 {
        match self {
            Self::Foreground => 1.0,
            Self::Background => 0.0,
            Self::BoxTopLeft => 2.0,
            Self::BoxBottomRight => 3.0,
        }
    }

    const fn is_foreground(self) -> bool {
        matches!(self, Self::Foreground)
    }

    const fn is_background(self) -> bool {
        matches!(self, Self::Background)
    }
}

#[derive(Clone, Copy, Debug)]
struct ObjectPrompt {
    point: [f32; 2],
    kind: ObjectPromptKind,
}

#[derive(Clone, Copy, Debug)]
struct ObjectPromptFocus {
    min: [f32; 2],
    max: [f32; 2],
}

impl ObjectPromptFocus {
    fn contains(self, point: [f32; 2]) -> bool {
        point[0] >= self.min[0]
            && point[0] <= self.max[0]
            && point[1] >= self.min[1]
            && point[1] <= self.max[1]
    }
}

#[derive(Clone, Debug)]
struct ObjectPromptSet {
    prompts: Vec<ObjectPrompt>,
    focus: ObjectPromptFocus,
}

fn sampled_object_prompts(
    strokes: &[crate::pipeline::ObjectStroke],
    brush_size: f32,
    source_width: u32,
    source_height: u32,
    limit: usize,
) -> ObjectPromptSet {
    let mut foreground_points = Vec::new();
    let mut explicit_background_points = Vec::new();
    for stroke in strokes {
        let target = if stroke.positive {
            &mut foreground_points
        } else {
            &mut explicit_background_points
        };
        target.extend(
            stroke
                .points
                .iter()
                .map(|point| [point[0].clamp(0.0, 1.0), point[1].clamp(0.0, 1.0)]),
        );
    }

    let focus = object_prompt_focus(&foreground_points, brush_size, source_width, source_height);
    let foreground_budget = limit.saturating_sub(10).clamp(1, 16);
    let background_budget = limit.saturating_sub(foreground_budget + 2).min(6);
    let mut prompts = evenly_sample(&foreground_points, foreground_budget)
        .into_iter()
        .map(|point| ObjectPrompt {
            point,
            kind: ObjectPromptKind::Foreground,
        })
        .collect::<Vec<_>>();
    prompts.extend(
        evenly_sample(&explicit_background_points, background_budget)
            .into_iter()
            .map(|point| ObjectPrompt {
                point,
                kind: ObjectPromptKind::Background,
            }),
    );

    // A box tells SAM that the painted region is the intended object part,
    // rather than merely one foreground sample somewhere on a larger person or
    // connected object. Labels 2 and 3 are SAM's top-left/bottom-right box
    // prompts.
    if prompts.len() + 2 <= limit {
        prompts.push(ObjectPrompt {
            point: focus.min,
            kind: ObjectPromptKind::BoxTopLeft,
        });
        prompts.push(ObjectPrompt {
            point: focus.max,
            kind: ObjectPromptKind::BoxBottomRight,
        });
    }

    // Automatic background guards just outside the brush-sized focus box make
    // a stroke through an arm prefer the arm instead of the nearby shoulder and
    // torso. They remain outside the painted area and are omitted at image
    // boundaries where clamping would move them back into the focus region.
    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let image_min = source_width.min(source_height).max(1) as f32;
    let radius_x = brush_size.clamp(f32::EPSILON, 0.5) * image_min / width;
    let radius_y = brush_size.clamp(f32::EPSILON, 0.5) * image_min / height;
    let gap_x = (radius_x * 0.85).max(6.0 / width);
    let gap_y = (radius_y * 0.85).max(6.0 / height);
    let center = [
        (focus.min[0] + focus.max[0]) * 0.5,
        (focus.min[1] + focus.max[1]) * 0.5,
    ];
    let guards = [
        [focus.min[0] - gap_x, focus.min[1] - gap_y],
        [center[0], focus.min[1] - gap_y],
        [focus.max[0] + gap_x, focus.min[1] - gap_y],
        [focus.min[0] - gap_x, center[1]],
        [focus.max[0] + gap_x, center[1]],
        [focus.min[0] - gap_x, focus.max[1] + gap_y],
        [center[0], focus.max[1] + gap_y],
        [focus.max[0] + gap_x, focus.max[1] + gap_y],
    ];
    for guard in guards {
        if prompts.len() >= limit {
            break;
        }
        let point = [guard[0].clamp(0.0, 1.0), guard[1].clamp(0.0, 1.0)];
        if !focus.contains(point) {
            prompts.push(ObjectPrompt {
                point,
                kind: ObjectPromptKind::Background,
            });
        }
    }

    ObjectPromptSet { prompts, focus }
}

fn object_prompt_focus(
    foreground_points: &[[f32; 2]],
    brush_size: f32,
    source_width: u32,
    source_height: u32,
) -> ObjectPromptFocus {
    let mut min = [1.0f32, 1.0f32];
    let mut max = [0.0f32, 0.0f32];
    for point in foreground_points {
        min[0] = min[0].min(point[0]);
        min[1] = min[1].min(point[1]);
        max[0] = max[0].max(point[0]);
        max[1] = max[1].max(point[1]);
    }
    if foreground_points.is_empty() {
        min = [0.45, 0.45];
        max = [0.55, 0.55];
    }

    let width = source_width.max(1) as f32;
    let height = source_height.max(1) as f32;
    let image_min = source_width.min(source_height).max(1) as f32;
    let radius = brush_size.clamp(f32::EPSILON, 0.5) * image_min;
    let padding_x = (radius * 1.35 + 8.0) / width;
    let padding_y = (radius * 1.35 + 8.0) / height;
    ObjectPromptFocus {
        min: [
            (min[0] - padding_x).clamp(0.0, 1.0),
            (min[1] - padding_y).clamp(0.0, 1.0),
        ],
        max: [
            (max[0] + padding_x).clamp(0.0, 1.0),
            (max[1] + padding_y).clamp(0.0, 1.0),
        ],
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
    focus: ObjectPromptFocus,
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
    let center_x = ((focus.min[0] + focus.max[0]) * 0.5 * width as f32).clamp(0.0, width as f32);
    let center_y = ((focus.min[1] + focus.max[1]) * 0.5 * height as f32).clamp(0.0, height as f32);
    let bounds_w = (focus.max[0] - focus.min[0]).max(0.0) * width as f32;
    let bounds_h = (focus.max[1] - focus.min[1]).max(0.0) * height as f32;
    let minimum = width.min(height) as f32 * 0.16;
    let factor = if expansion == 0 { 1.5 } else { 2.3 };
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
        let _model_guard = prepare_model(AiMaskModel::SamEncoder)?;
        let mut session = create_session_with_fallback(
            encoder_path,
            SessionOptions::new("SAM 2.1 encoder")
                .with_cpu_fallback_profile(CpuFallbackProfile::WindowsSamEncoder),
        )?;
        run_sam_encoder(&mut session, input)?
    };
    #[cfg(not(target_os = "android"))]
    let tensors = {
        let _model_guard = prepare_model(AiMaskModel::SamEncoder)?;
        if model_cache_enabled() && cache_object_ai_sessions() {
            let sessions = SAM_ENCODER_SESSION.get_or_init(|| Mutex::new(None));
            let mut guard = sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("SAM encoder session lock was poisoned"))?;
            if guard.is_none() {
                *guard = Some(create_session_with_fallback(
                    encoder_path,
                    SessionOptions::new("SAM 2.1 encoder")
                        .with_cpu_fallback_profile(CpuFallbackProfile::WindowsSamEncoder),
                )?);
            }
            let result = run_sam_encoder(
                guard
                    .as_mut()
                    .context("SAM encoder session is unavailable")?,
                input,
            );
            if !model_cache_enabled() {
                *guard = None;
            }
            result?
        } else {
            let mut session = create_session_with_fallback(
                encoder_path,
                SessionOptions::new("SAM 2.1 encoder")
                    .with_cpu_fallback_profile(CpuFallbackProfile::WindowsSamEncoder),
            )?;
            run_sam_encoder(&mut session, input)?
        }
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
        prompt_brush_size: 0.0,
    })
}

fn run_sam_encoder(
    session: &mut FallbackSession,
    input: Tensor<f32>,
) -> Result<(SamTensorData, SamTensorData, SamTensorData)> {
    session.run_with_fallback(
        "SAM 2.1 image encoder inference",
        |ort_session, accelerated| {
            let outputs = ort_session
                .run(ort::inputs![&input])
                .context("run SAM 2.1 image encoder")?;
            Ok((
                extract_sam_encoder_output(&outputs, 0, "high-resolution feature 0", accelerated)?,
                extract_sam_encoder_output(&outputs, 1, "high-resolution feature 1", accelerated)?,
                extract_sam_encoder_output(&outputs, 2, "image embedding", accelerated)?,
            ))
        },
    )
}

fn extract_sam_encoder_output(
    outputs: &ort::session::SessionOutputs<'_>,
    index: usize,
    label: &str,
    _accelerated: bool,
) -> Result<SamTensorData> {
    let value = outputs
        .values()
        .nth(index)
        .with_context(|| format!("SAM 2.1 returned no {label}"))?;
    let (shape, data) = value
        .try_extract_tensor::<f32>()
        .with_context(|| format!("read SAM 2.1 {label}"))?;

    let non_finite = data.iter().filter(|value| !value.is_finite()).count();
    #[cfg(target_os = "windows")]
    let values = if non_finite > 0 {
        anyhow::ensure!(
            !_accelerated,
            "SAM 2.1 {label} produced {non_finite} non-finite values on the accelerated execution provider"
        );
        // A very small number of isolated NaN/Inf values has been observed from
        // third-party Windows ORT CPU DLLs even with conservative session
        // settings. Replacing a handful with neutral zeros is safer than making
        // Object Mask unusable, but never accept broadly-corrupted CPU features.
        let repair_limit = 64usize.max(data.len() / 100_000);
        anyhow::ensure!(
            non_finite <= repair_limit,
            "SAM 2.1 {label} is numerically corrupted on CPU: {non_finite} of {} values are non-finite. Select a current Microsoft x64 CPU onnxruntime.dll and restart AuRaw",
            data.len()
        );
        log::warn!(
            "SAM 2.1 {label} contained {non_finite} isolated non-finite values on Windows CPU; replacing them with zero"
        );
        data.iter()
            .map(|value| if value.is_finite() { *value } else { 0.0 })
            .collect::<Vec<_>>()
    } else {
        data.to_vec()
    };
    #[cfg(not(target_os = "windows"))]
    let values = {
        anyhow::ensure!(
            non_finite == 0,
            "SAM 2.1 {label} contains non-finite values"
        );
        data.to_vec()
    };

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
        expected == values.len(),
        "SAM tensor shape does not match its data"
    );
    Ok(SamTensorData {
        shape,
        values: values.into(),
    })
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
    prompt_set: &ObjectPromptSet,
    use_previous_mask: bool,
) -> Result<DecodedSamMask> {
    let prompts = &prompt_set.prompts;
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
        labels[index] = prompt.kind.sam_label();
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
        let _model_guard = prepare_model(AiMaskModel::SamDecoder)?;
        let mut session =
            create_session_with_fallback(decoder_path, SessionOptions::new("SAM 2.1 decoder"))?;
        run_sam_decoder(
            &mut session,
            SamDecoderInputs {
                image_embedding,
                high_res_0,
                high_res_1,
                point_coords,
                point_labels,
                mask_input,
                has_mask,
            },
        )?
    };
    #[cfg(not(target_os = "android"))]
    let (masks, scores) = {
        let _model_guard = prepare_model(AiMaskModel::SamDecoder)?;
        if model_cache_enabled() && cache_object_ai_sessions() {
            let sessions = SAM_DECODER_SESSION.get_or_init(|| Mutex::new(None));
            let mut guard = sessions
                .lock()
                .map_err(|_| anyhow::anyhow!("SAM decoder session lock was poisoned"))?;
            if guard.is_none() {
                *guard = Some(create_session_with_fallback(
                    decoder_path,
                    SessionOptions::new("SAM 2.1 decoder"),
                )?);
            }
            let result = run_sam_decoder(
                guard
                    .as_mut()
                    .context("SAM decoder session is unavailable")?,
                SamDecoderInputs {
                    image_embedding,
                    high_res_0,
                    high_res_1,
                    point_coords,
                    point_labels,
                    mask_input,
                    has_mask,
                },
            );
            if !model_cache_enabled() {
                *guard = None;
            }
            result?
        } else {
            let mut session =
                create_session_with_fallback(decoder_path, SessionOptions::new("SAM 2.1 decoder"))?;
            run_sam_decoder(
                &mut session,
                SamDecoderInputs {
                    image_embedding,
                    high_res_0,
                    high_res_1,
                    point_coords,
                    point_labels,
                    mask_input,
                    has_mask,
                },
            )?
        }
    };
    select_sam_candidate(masks, scores, prompt_set, cache)
}

fn tensor_from_sam_data(data: &SamTensorData, label: &str) -> Result<Tensor<f32>> {
    Tensor::from_array((data.shape.clone(), data.values.to_vec()))
        .with_context(|| format!("create SAM {label} input"))
}

struct SamDecoderInputs {
    image_embedding: Tensor<f32>,
    high_res_0: Tensor<f32>,
    high_res_1: Tensor<f32>,
    point_coords: Tensor<f32>,
    point_labels: Tensor<f32>,
    mask_input: Tensor<f32>,
    has_mask: Tensor<f32>,
}

fn run_sam_decoder(
    session: &mut FallbackSession,
    inputs: SamDecoderInputs,
) -> Result<(SamTensorData, SamTensorData)> {
    let SamDecoderInputs {
        image_embedding,
        high_res_0,
        high_res_1,
        point_coords,
        point_labels,
        mask_input,
        has_mask,
    } = inputs;
    session.run_with_fallback(
        "SAM 2.1 mask decoder inference",
        |ort_session, _accelerated| {
            let outputs = ort_session
                .run(ort::inputs![
                    &image_embedding,
                    &high_res_0,
                    &high_res_1,
                    &point_coords,
                    &point_labels,
                    &mask_input,
                    &has_mask
                ])
                .context("run SAM 2.1 mask decoder")?;
            Ok((
                extract_f32_output(&outputs, 0, "mask logits")?,
                extract_f32_output(&outputs, 1, "mask scores")?,
            ))
        },
    )
}

fn select_sam_candidate(
    masks: SamTensorData,
    scores: SamTensorData,
    prompt_set: &ObjectPromptSet,
    cache: &ObjectInferenceCache,
) -> Result<DecodedSamMask> {
    let prompts = &prompt_set.prompts;
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
            if !prompt.kind.is_foreground() && !prompt.kind.is_background() {
                continue;
            }
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
            score += if prompt.kind.is_foreground() {
                probability * 0.14
            } else {
                (1.0 - probability) * 0.16
            };
        }
        let (outside_focus, focus_fill, area_ratio) =
            candidate_focus_statistics(logits, width, height, prompt_set.focus, cache);
        score -= outside_focus * 0.95;
        score += focus_fill.min(0.75) * 0.22;
        score -= (area_ratio - 2.0).clamp(0.0, 5.0) * 0.10;
        let border = candidate_border_fraction(logits, width, height);
        score -= border * 0.20;
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

fn candidate_focus_statistics(
    logits: &[f32],
    width: usize,
    height: usize,
    focus: ObjectPromptFocus,
    cache: &ObjectInferenceCache,
) -> (f32, f32, f32) {
    let to_output_x = |normalized: f32| {
        ((normalized.clamp(0.0, 1.0) * cache.source_width as f32 - cache.crop.x as f32)
            / cache.crop.width.max(1) as f32
            * width as f32)
            .clamp(0.0, width as f32)
    };
    let to_output_y = |normalized: f32| {
        ((normalized.clamp(0.0, 1.0) * cache.source_height as f32 - cache.crop.y as f32)
            / cache.crop.height.max(1) as f32
            * height as f32)
            .clamp(0.0, height as f32)
    };
    let min_x = to_output_x(focus.min[0]);
    let max_x = to_output_x(focus.max[0]);
    let min_y = to_output_y(focus.min[1]);
    let max_y = to_output_y(focus.max[1]);
    let focus_area = ((max_x - min_x).max(1.0) * (max_y - min_y).max(1.0)).max(1.0);

    let mut active = 0usize;
    let mut inside = 0usize;
    for y in 0..height {
        for x in 0..width {
            if logits[y * width + x] <= 0.0 {
                continue;
            }
            active += 1;
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            if px >= min_x && px <= max_x && py >= min_y && py <= max_y {
                inside += 1;
            }
        }
    }
    if active == 0 {
        return (1.0, 0.0, 0.0);
    }
    let outside_fraction = (active - inside) as f32 / active as f32;
    let focus_fill = inside as f32 / focus_area;
    let area_ratio = active as f32 / focus_area;
    (outside_fraction, focus_fill, area_ratio)
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

    for prompt in prompts.iter().filter(|prompt| prompt.kind.is_foreground()) {
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

    // SAM probabilities can contain small sub-threshold islands inside a
    // visually solid object. Fill enclosed holes in the selected silhouette,
    // then make the deep interior definitively opaque while keeping a narrow
    // soft band for edge-aware/ViTMatte refinement. This prevents texture from
    // appearing as pinholes without sacrificing hair or anti-aliased edges.
    fill_enclosed_component_holes(&mut keep, width_usize, height_usize);
    let background = keep.iter().map(|selected| !*selected).collect::<Vec<_>>();
    let near_background = dilate_component_band(&background, width_usize, height_usize, 3);
    let soft_band = dilate_component_band(&keep, width_usize, height_usize, 10);
    probabilities
        .into_iter()
        .enumerate()
        .map(|(index, probability)| {
            if keep[index] {
                if near_background[index] {
                    probability.max(0.82)
                } else {
                    1.0
                }
            } else if soft_band[index] {
                probability.min(0.49)
            } else {
                0.0
            }
        })
        .collect()
}

fn fill_enclosed_component_holes(selected: &mut [bool], width: usize, height: usize) {
    use std::collections::VecDeque;

    if width == 0 || height == 0 || selected.len() != width.saturating_mul(height) {
        return;
    }
    let mut exterior = vec![false; selected.len()];
    let mut queue = VecDeque::new();
    let seed = |x: usize, y: usize, exterior: &mut [bool], queue: &mut VecDeque<usize>| {
        let index = y * width + x;
        if !selected[index] && !exterior[index] {
            exterior[index] = true;
            queue.push_back(index);
        }
    };
    for x in 0..width {
        seed(x, 0, &mut exterior, &mut queue);
        if height > 1 {
            seed(x, height - 1, &mut exterior, &mut queue);
        }
    }
    for y in 0..height {
        seed(0, y, &mut exterior, &mut queue);
        if width > 1 {
            seed(width - 1, y, &mut exterior, &mut queue);
        }
    }
    while let Some(index) = queue.pop_front() {
        let x = index % width;
        let y = index / width;
        for (nx, ny) in neighbors4(x, y, width, height) {
            let next = ny * width + nx;
            if !selected[next] && !exterior[next] {
                exterior[next] = true;
                queue.push_back(next);
            }
        }
    }
    // Preserve genuine large holes (for example the opening inside a handle)
    // while removing tiny enclosed probability pinholes.
    let max_hole_area = (selected.len() / 512).clamp(32, 2048);
    let mut visited_holes = vec![false; selected.len()];
    for start in 0..selected.len() {
        if selected[start] || exterior[start] || visited_holes[start] {
            continue;
        }
        let mut component = Vec::new();
        visited_holes[start] = true;
        queue.push_back(start);
        while let Some(index) = queue.pop_front() {
            component.push(index);
            let x = index % width;
            let y = index / width;
            for (nx, ny) in neighbors4(x, y, width, height) {
                let next = ny * width + nx;
                if !selected[next] && !exterior[next] && !visited_holes[next] {
                    visited_holes[next] = true;
                    queue.push_back(next);
                }
            }
        }
        if component.len() <= max_hole_area {
            for index in component {
                selected[index] = true;
            }
        }
    }
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
) -> Vec<f32> {
    let strength = strength.clamp(0.0, 1.0);
    if strength <= 0.001 || rgba.len() != width as usize * height as usize * 4 {
        return mask;
    }
    let radius = (2.0 + strength * 4.0).round() as i32;
    let iterations = 2;
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

pub(super) fn resize_probability_u8(
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
            brush_size: 0.0,
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
    fn prompt_sampling_adds_box_and_background_guards_within_limit() {
        let strokes = vec![
            stroke(&[[0.1, 0.1], [0.2, 0.2], [0.3, 0.3], [0.4, 0.4]], true),
            stroke(&[[0.8, 0.8], [0.7, 0.7], [0.6, 0.6]], false),
        ];
        let set = sampled_object_prompts(&strokes, 0.04, 1000, 600, SAM21_MAX_PROMPTS);
        assert!(set.prompts.len() <= SAM21_MAX_PROMPTS);
        assert!(set
            .prompts
            .iter()
            .any(|prompt| prompt.kind == ObjectPromptKind::Foreground));
        assert!(set
            .prompts
            .iter()
            .any(|prompt| prompt.kind == ObjectPromptKind::Background));
        assert!(set
            .prompts
            .iter()
            .any(|prompt| prompt.kind == ObjectPromptKind::BoxTopLeft));
        assert!(set
            .prompts
            .iter()
            .any(|prompt| prompt.kind == ObjectPromptKind::BoxBottomRight));
    }

    #[test]
    fn focus_and_guard_prompts_are_inside_the_adaptive_crop() {
        let strokes = vec![stroke(&[[0.50, 0.50], [0.62, 0.50]], true)];
        let set = sampled_object_prompts(&strokes, 0.035, 1000, 600, SAM21_MAX_PROMPTS);
        let crop = object_crop_for_prompts(1000, 600, set.focus, 0);
        for prompt in &set.prompts {
            let x = (prompt.point[0] * 1000.0) as u32;
            let y = (prompt.point[1] * 600.0) as u32;
            assert!(x >= crop.x && x <= crop.x + crop.width);
            assert!(y >= crop.y && y <= crop.y + crop.height);
        }
        assert_eq!(
            object_crop_for_prompts(1000, 600, set.focus, 2),
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
            kind: ObjectPromptKind::Foreground,
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
