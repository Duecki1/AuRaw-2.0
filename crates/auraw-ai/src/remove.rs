use crate::execution_provider::SessionOptions;
use crate::model_runtime::{
    with_model_session, AiModel, AiRuntimeContext, ModelRetention,
};
use crate::model_artifact::{ArtifactSize, DownloadOptions, ModelArtifact};
use crate::model_install::ModelInstallSpec;
use crate::ModelDownloadProgress;
use crate::pipeline::{
    adaptive_remove_dilation, pipeline_scene_to_canonical_remove_scene, plan_remove_context_crops,
    rasterize_remove_brush, remove_model_srgb_to_canonical_scene, remove_model_view_gain,
    remove_scene_to_model_srgb, render_remove_scene_crop, DevelopedCropJob, ExposureParams,
    GeometryTransform, GpuProgramPrewarm, LoadedRaw, MaskStack, NativeRect, RemoveBrushStroke,
    RemoveContextCrop, RemoveEditState, RemoveMask, RemovePatch, RemoveStroke,
    ToneStatisticsSnapshot, BIG_LAMA_INPUT_EDGE,
};
use anyhow::{Context, Result};
use image::{imageops::FilterType, GrayImage, ImageBuffer, Luma, Rgb, Rgb32FImage};
use ort::value::Tensor;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

pub const BIG_LAMA_MODEL_FILENAME: &str = "big-lama-places2-fp32-512.onnx";
pub const BIG_LAMA_MODEL_URL: &str =
    "https://huggingface.co/Carve/LaMa-ONNX/resolve/a3ee2fca54baebec351b8fa7786154ffa7555aa6/lama_fp32.onnx";
pub const BIG_LAMA_MODEL_SHA256_HEX: &str =
    "1faef5301d78db7dda502fe59966957ec4b79dd64e16f03ed96913c7a4eb68d6";
pub const BIG_LAMA_MODEL_BYTES: u64 = 208_044_816;
pub const BIG_LAMA_MODEL_LICENSE: &str = "Apache-2.0";
pub const BIG_LAMA_MODEL_PROVENANCE: &str =
    "Carve/LaMa-ONNX port of the original PyTorch big-lama inpainting model";

const BIG_LAMA_ARTIFACT: ModelArtifact = ModelArtifact {
    name: "Big-LaMa Places2 ONNX",
    url: Some(BIG_LAMA_MODEL_URL),
    sha256: BIG_LAMA_MODEL_SHA256_HEX,
    size: ArtifactSize::Exact(BIG_LAMA_MODEL_BYTES),
    progress_total: BIG_LAMA_MODEL_BYTES,
};
const BIG_LAMA_DOWNLOAD: DownloadOptions = DownloadOptions {
    connect_timeout: Duration::from_secs(30),
    response_timeout: Duration::from_secs(60),
    body_timeout: Duration::from_secs(30 * 60),
    attempts: 5,
    resume: true,
};
const BIG_LAMA_INSTALL: ModelInstallSpec = ModelInstallSpec {
    artifact: BIG_LAMA_ARTIFACT,
    download: BIG_LAMA_DOWNLOAD,
    progress_label: "Big-LaMa Remove model",
};

#[derive(Clone)]
pub struct RemoveRequest {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub raw: Arc<LoadedRaw>,
    pub geometry: GeometryTransform,
    pub exposure: ExposureParams,
    pub masks: MaskStack,
    pub existing: RemoveEditState,
    pub brush: RemoveBrushStroke,
    pub model_path: PathBuf,
    pub allow_download: bool,
    pub runtime_path: Option<PathBuf>,
    pub runtime_sha256: Option<String>,
    pub tone_statistics: Option<Arc<ToneStatisticsSnapshot>>,
    pub program_prewarm: Option<Arc<GpuProgramPrewarm>>,
    pub cancellation: Arc<AtomicBool>,
}

#[derive(Debug)]
pub enum RemoveEvent {
    DownloadProgress(ModelDownloadProgress),
    Processing { completed: usize, total: usize },
    Finished(Result<RemoveStroke, String>),
}

pub fn big_lama_model_is_verified(path: &Path) -> bool {
    BIG_LAMA_INSTALL.is_installed(path)
}

pub fn spawn_remove(request: RemoveRequest) -> mpsc::Receiver<RemoveEvent> {
    let (sender, receiver) = mpsc::channel();
    let worker = sender.clone();
    let spawn = std::thread::Builder::new()
        .name("auraw-big-lama-remove".to_owned())
        .spawn(move || {
            let result = run_remove(request, &worker).map_err(|error| format!("{error:#}"));
            let _ = worker.send(RemoveEvent::Finished(result));
        });
    if let Err(error) = spawn {
        let _ = sender.send(RemoveEvent::Finished(Err(format!(
            "could not start Remove worker: {error}"
        ))));
    }
    receiver
}

fn ensure_not_cancelled(cancellation: &AtomicBool) -> Result<()> {
    anyhow::ensure!(
        !cancellation.load(Ordering::Acquire),
        "Remove operation cancelled"
    );
    Ok(())
}

fn run_remove(request: RemoveRequest, events: &mpsc::Sender<RemoveEvent>) -> Result<RemoveStroke> {
    ensure_not_cancelled(&request.cancellation)?;
    crate::ai_masks::initialize_runtime(
        request.runtime_path.as_deref(),
        request.runtime_sha256.as_deref(),
    )?;
    BIG_LAMA_INSTALL.ensure_installed(
        &request.model_path,
        request.allow_download,
        |progress| {
            let _ = events.send(RemoveEvent::DownloadProgress(progress));
        },
        || ensure_not_cancelled(&request.cancellation),
    )?;
    ensure_not_cancelled(&request.cancellation)?;

    let mut brush = request.brush;
    if brush.dilation_radius == 0 {
        brush.dilation_radius = adaptive_remove_dilation(&brush.points);
    }
    let mask = rasterize_remove_brush(request.raw.width, request.raw.height, &brush)
        .context("Remove brush produced no native image mask")?;
    let crops = plan_remove_context_crops(request.raw.width, request.raw.height, &mask);
    anyhow::ensure!(!crops.is_empty(), "Remove mask produced no local context crops");

    let mut working = request.existing;
    let mut patches = Vec::with_capacity(crops.len());
    for (index, planned) in crops.iter().copied().enumerate() {
        ensure_not_cancelled(&request.cancellation)?;
        let scene = render_remove_scene_crop(DevelopedCropJob {
            device: request.device.clone(),
            queue: request.queue.clone(),
            raw: Arc::clone(&request.raw),
            geometry: request.geometry,
            exposure: request.exposure,
            masks: request.masks.clone(),
            remove: working.clone(),
            crop: planned.context,
            tone_statistics: request.tone_statistics.clone(),
            program_prewarm: request.program_prewarm.clone(),
        })
        .with_context(|| {
            format!(
                "render native scene Remove context {}x{} at {},{}",
                planned.context.width,
                planned.context.height,
                planned.context.x,
                planned.context.y
            )
        })?;
        ensure_not_cancelled(&request.cancellation)?;

        let patch = infer_crop(
            &request.model_path,
            planned,
            request.raw.width,
            request.raw.height,
            &request.raw,
            &request.exposure,
            &scene,
            &mask,
        )?;
        let partial = RemoveStroke {
            brush: brush.clone(),
            patches: vec![patch.clone()],
        };
        working.strokes.push(partial);
        patches.push(patch);
        let _ = events.send(RemoveEvent::Processing {
            completed: index + 1,
            total: crops.len(),
        });
    }

    Ok(RemoveStroke { brush, patches })
}

fn infer_crop(
    model_path: &Path,
    planned: RemoveContextCrop,
    image_width: u32,
    image_height: u32,
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    scene: &[f32],
    mask: &RemoveMask,
) -> Result<RemovePatch> {
    let crop = planned.context;
    let expected = crop.width as usize * crop.height as usize * 3;
    anyhow::ensure!(
        scene.len() == expected,
        "scene Remove crop has an invalid RGB length"
    );

    let view_gain = remove_model_view_gain(raw, scene);
    let mut srgb = Vec::with_capacity(expected);
    for pixel in scene.chunks_exact(3) {
        let converted = remove_scene_to_model_srgb(raw, [pixel[0], pixel[1], pixel[2]], view_gain);
        srgb.extend_from_slice(&converted);
    }
    let source: Rgb32FImage = ImageBuffer::from_raw(crop.width, crop.height, srgb)
        .context("construct developed Remove crop")?;
    let resized = image::imageops::resize(
        &source,
        BIG_LAMA_INPUT_EDGE,
        BIG_LAMA_INPUT_EDGE,
        FilterType::Lanczos3,
    );
    let source_mask = crop_binary_mask(crop, mask, planned.target);
    let blend_mask = crop_binary_mask(crop, mask, crop);
    let resized_mask = image::imageops::resize(
        &source_mask,
        BIG_LAMA_INPUT_EDGE,
        BIG_LAMA_INPUT_EDGE,
        FilterType::Nearest,
    );

    let plane = (BIG_LAMA_INPUT_EDGE * BIG_LAMA_INPUT_EDGE) as usize;
    let mut image_values = vec![0.0f32; plane * 3];
    let mut mask_values = vec![0.0f32; plane];
    for y in 0..BIG_LAMA_INPUT_EDGE {
        for x in 0..BIG_LAMA_INPUT_EDGE {
            let index = (y * BIG_LAMA_INPUT_EDGE + x) as usize;
            let pixel = resized.get_pixel(x, y);
            image_values[index] = pixel[0].clamp(0.0, 1.0);
            image_values[plane + index] = pixel[1].clamp(0.0, 1.0);
            image_values[plane * 2 + index] = pixel[2].clamp(0.0, 1.0);
            // Big-LaMa/IOPaint polarity: 1 means the pixel is inpainted.
            mask_values[index] = if resized_mask.get_pixel(x, y)[0] >= 128 {
                1.0
            } else {
                0.0
            };
        }
    }
    anyhow::ensure!(
        mask_values.iter().any(|value| *value > 0.5),
        "Remove mask vanished during resize"
    );

    let image_tensor = Tensor::from_array((
        [
            1usize,
            3,
            BIG_LAMA_INPUT_EDGE as usize,
            BIG_LAMA_INPUT_EDGE as usize,
        ],
        image_values,
    ))
    .context("create Big-LaMa image tensor")?;
    let mask_tensor = Tensor::from_array((
        [
            1usize,
            1,
            BIG_LAMA_INPUT_EDGE as usize,
            BIG_LAMA_INPUT_EDGE as usize,
        ],
        mask_values,
    ))
    .context("create Big-LaMa mask tensor")?;

    let output_values = with_model_session(
        AiModel::BigLama,
        model_path,
        SessionOptions::new("Big-LaMa Remove"),
        ModelRetention::Interactive(AiRuntimeContext::Remove),
        |session| {
            session.run_with_fallback(
                "Big-LaMa Remove ONNX inference",
                |ort_session, _accelerated| {
                    let outputs = ort_session
                        .run(ort::inputs![&image_tensor, &mask_tensor])
                        .context("run Big-LaMa ONNX inference")?;
                    let output = outputs
                        .values()
                        .next()
                        .context("Big-LaMa returned no output tensor")?;
                    let (shape, values) = output
                        .try_extract_tensor::<f32>()
                        .context("read Big-LaMa output tensor")?;
                    anyhow::ensure!(
                        shape.as_ref()
                            == [
                                1,
                                3,
                                BIG_LAMA_INPUT_EDGE as i64,
                                BIG_LAMA_INPUT_EDGE as i64,
                            ],
                        "unexpected Big-LaMa output shape {shape:?}"
                    );
                    anyhow::ensure!(
                        values.len() == plane * 3
                            && values.iter().all(|value| value.is_finite()),
                        "Big-LaMa output tensor is invalid"
                    );
                    Ok(values.to_vec())
                },
            )
        },
    )?;

    let mut output_interleaved = vec![0.0f32; plane * 3];
    for index in 0..plane {
        // Carve's fixed-shape fp32 export deliberately bakes the final *255
        // into the ONNX graph. Inputs are still RGB / 255 in [0, 1], but the
        // ONNX output tensor is RGB in [0, 255]. Convert it back to normalized
        // photographic RGB before high-quality resize/compositing.
        output_interleaved[index * 3] = (output_values[index] / 255.0).clamp(0.0, 1.0);
        output_interleaved[index * 3 + 1] =
            (output_values[plane + index] / 255.0).clamp(0.0, 1.0);
        output_interleaved[index * 3 + 2] =
            (output_values[plane * 2 + index] / 255.0).clamp(0.0, 1.0);
    }
    let model_output: Rgb32FImage = ImageBuffer::from_raw(
        BIG_LAMA_INPUT_EDGE,
        BIG_LAMA_INPUT_EDGE,
        output_interleaved,
    )
    .context("construct Big-LaMa output image")?;
    let restored = image::imageops::resize(
        &model_output,
        crop.width,
        crop.height,
        FilterType::Lanczos3,
    );

    build_cached_patch(
        planned,
        mask.bounds,
        image_width,
        image_height,
        raw,
        exposure,
        scene,
        view_gain,
        &restored,
        &source_mask,
        &blend_mask,
    )
}

fn crop_binary_mask(crop: NativeRect, mask: &RemoveMask, target: NativeRect) -> GrayImage {
    let mut out = GrayImage::new(crop.width, crop.height);
    let active = mask.bounds.intersect(target);
    if let Some(intersection) = active.and_then(|active| crop.intersect(active)) {
        for y in intersection.y..intersection.bottom() {
            for x in intersection.x..intersection.right() {
                if mask.contains_global(x, y) {
                    out.put_pixel(x - crop.x, y - crop.y, Luma([255]));
                }
            }
        }
    }
    out
}

fn build_cached_patch(
    planned: RemoveContextCrop,
    mask_bounds: NativeRect,
    image_width: u32,
    image_height: u32,
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    source_scene: &[f32],
    view_gain: f32,
    restored: &Rgb32FImage,
    binary_mask: &GrayImage,
    blend_mask: &GrayImage,
) -> Result<RemovePatch> {
    let crop = planned.context;
    let scale = crop.width as f32 / BIG_LAMA_INPUT_EDGE as f32;
    let sigma = (1.25 * scale).clamp(1.25, 5.0);
    let blurred = image::imageops::blur(blend_mask, sigma);
    let mut left = crop.width;
    let mut top = crop.height;
    let mut right = 0u32;
    let mut bottom = 0u32;
    for y in 0..crop.height {
        for x in 0..crop.width {
            let binary = binary_mask.get_pixel(x, y)[0];
            let soft = blurred.get_pixel(x, y)[0];
            // Feather inward only. The dilated binary model mask defines the
            // entire editable region; original pixels outside it stay exact.
            let base_alpha = if binary != 0 { soft } else { 0 };
            let alpha = apply_tile_transition(
                base_alpha,
                planned,
                mask_bounds,
                image_width,
                image_height,
                x,
                y,
            );
            if alpha >= 2 {
                left = left.min(x);
                top = top.min(y);
                right = right.max(x + 1);
                bottom = bottom.max(y + 1);
            }
        }
    }
    anyhow::ensure!(
        right > left && bottom > top,
        "Big-LaMa patch has no compositing coverage"
    );
    let bounds = NativeRect {
        x: crop.x + left,
        y: crop.y + top,
        width: right - left,
        height: bottom - top,
    };
    let pixels = bounds.width as usize * bounds.height as usize;
    let mut rgb16f = Vec::with_capacity(pixels * 3);
    let mut alpha = Vec::with_capacity(pixels);
    for y in top..bottom {
        for x in left..right {
            let pixel: &Rgb<f32> = restored.get_pixel(x, y);
            let generated = remove_model_srgb_to_canonical_scene(
                raw,
                exposure,
                [pixel[0], pixel[1], pixel[2]],
                view_gain,
            );
            let source_index = (y as usize * crop.width as usize + x as usize) * 3;
            let source = pipeline_scene_to_canonical_remove_scene(
                raw,
                exposure,
                [
                    source_scene[source_index],
                    source_scene[source_index + 1],
                    source_scene[source_index + 2],
                ],
            );
            let binary = binary_mask.get_pixel(x, y)[0];
            let soft = blurred.get_pixel(x, y)[0];
            // Feather inward only. The feather is composited at the scene
            // boundary now, before any mutable Develop adjustment.
            let base_alpha = if binary != 0 { soft } else { 0 };
            let coverage = apply_tile_transition(
                base_alpha,
                planned,
                mask_bounds,
                image_width,
                image_height,
                x,
                y,
            );
            let mix = coverage as f32 / 255.0;
            for channel in 0..3 {
                let value = source[channel] * (1.0 - mix) + generated[channel] * mix;
                let finite = if value.is_finite() { value } else { source[channel] };
                rgb16f.push(half::f16::from_f32(finite.clamp(-65_504.0, 65_504.0)).to_bits());
            }
            alpha.push(coverage);
        }
    }
    RemovePatch::new_scene(bounds, rgb16f, alpha).map_err(anyhow::Error::msg)
}

fn apply_tile_transition(
    alpha: u8,
    planned: RemoveContextCrop,
    mask_bounds: NativeRect,
    image_width: u32,
    image_height: u32,
    local_x: u32,
    local_y: u32,
) -> u8 {
    if alpha == 0 {
        return 0;
    }
    let crop = planned.context;
    let global_x = crop.x.saturating_add(local_x);
    let global_y = crop.y.saturating_add(local_y);

    // Large Remove masks use overlapping target cores inside larger context
    // crops. Process tiles left-to-right/top-to-bottom and feather only the
    // leading internal target edges. Earlier tiles therefore remain full
    // coverage underneath while the new inference transitions in, avoiding
    // both hard tile seams and alpha holes. Real user-mask/image boundaries
    // are controlled only by the inward mask feather above.
    let span = (planned.target.width.min(planned.target.height) as f32 * 0.20)
        .clamp(24.0, 96.0);
    let smooth = |distance: f32| {
        let t = (distance / span).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    let mut weight = 1.0f32;
    if planned.target != crop && planned.target.x > mask_bounds.x {
        weight = weight.min(smooth(global_x as f32 + 0.5 - planned.target.x as f32));
    }
    if planned.target != crop && planned.target.y > mask_bounds.y {
        weight = weight.min(smooth(global_y as f32 + 0.5 - planned.target.y as f32));
    }

    // These arguments make the intended native-image boundary contract
    // explicit and guard future planning changes from producing out-of-image
    // target coordinates.
    if global_x >= image_width || global_y >= image_height {
        return 0;
    }
    (alpha as f32 * weight).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{RemoveBrushPoint, RemoveBrushStroke};

    #[test]
    fn cached_patch_feather_stays_inside_binary_mask() {
        let crop = NativeRect {
            x: 100,
            y: 200,
            width: 64,
            height: 64,
        };
        let mut binary = GrayImage::new(64, 64);
        for y in 20..44 {
            for x in 18..46 {
                binary.put_pixel(x, y, Luma([255]));
            }
        }
        let restored = Rgb32FImage::from_pixel(64, 64, Rgb([0.4, 0.5, 0.6]));
        let planned = RemoveContextCrop {
            context: crop,
            target: crop,
        };
        let raw = LoadedRaw::from_scene_linear_rec2020(1, 1, vec![0.18, 0.18, 0.18]).unwrap();
        let exposure = ExposureParams::default();
        let source_scene = vec![0.18f32; 64 * 64 * 3];
        let patch = build_cached_patch(
            planned,
            crop,
            1_000,
            1_000,
            &raw,
            &exposure,
            &source_scene,
            1.0,
            &restored,
            &binary,
            &binary,
        )
        .unwrap();
        assert_eq!(patch.bounds.x, crop.x + 18);
        assert_eq!(patch.bounds.y, crop.y + 20);
        assert_eq!(patch.bounds.right(), crop.x + 46);
        assert_eq!(patch.bounds.bottom(), crop.y + 44);
        assert!(patch.alpha.iter().all(|alpha| *alpha > 0));
    }

    #[test]
    fn large_tile_model_mask_is_limited_to_target_core() {
        let mask = RemoveMask {
            bounds: NativeRect {
                x: 0,
                y: 0,
                width: 1024,
                height: 1024,
            },
            pixels: vec![255; 1024 * 1024],
        };
        let crop = NativeRect {
            x: 0,
            y: 0,
            width: 1024,
            height: 1024,
        };
        let target = NativeRect {
            x: 0,
            y: 0,
            width: 512,
            height: 512,
        };
        let source = crop_binary_mask(crop, &mask, target);
        assert_eq!(source.get_pixel(100, 100)[0], 255);
        assert_eq!(source.get_pixel(700, 100)[0], 0);
        assert_eq!(source.get_pixel(100, 700)[0], 0);
    }

    #[test]
    fn nearest_model_mask_stays_binary() {
        let brush = RemoveBrushStroke {
            points: vec![RemoveBrushPoint {
                x: 50.0,
                y: 40.0,
                radius: 8.0,
            }],
            dilation_radius: 2,
        };
        let mask = rasterize_remove_brush(100, 80, &brush).unwrap();
        let crop = NativeRect {
            x: 10,
            y: 0,
            width: 80,
            height: 80,
        };
        let source = crop_binary_mask(crop, &mask, crop);
        let resized = image::imageops::resize(
            &source,
            BIG_LAMA_INPUT_EDGE,
            BIG_LAMA_INPUT_EDGE,
            FilterType::Nearest,
        );
        assert!(resized.pixels().all(|pixel| pixel[0] == 0 || pixel[0] == 255));
    }
}
