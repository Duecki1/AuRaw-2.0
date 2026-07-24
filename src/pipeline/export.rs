use super::{
    export_mask_atlas_edge, extract_padded_tile, extract_padded_tile_into, mask_atlas_edge,
    required_export_tile_halo, ExposureParams, GpuParams, IccOutputTransform, InpaintLayer,
    LoadedRaw, MaskStack, ProcessingQuality, RawGpuPipeline, TilePlan,
    TileSpec, EXPORT_TILE_HALO, MIN_EXPORT_TILE_HALO, MAX_LOCAL_MASKS,
};
use anyhow::{Context, Result};
use eframe::wgpu;
use std::borrow::Cow;
use std::fs::{self, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportFormat {
    #[default]
    Png,
    Jpeg,
}

impl ExportFormat {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
        }
    }

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }

    pub const fn mime_type(self) -> &'static str {
        match self {
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExportResizeMode {
    #[default]
    Original,
    LongEdge,
    ShortEdge,
    Width,
    Height,
    Percentage,
}

impl ExportResizeMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Original => "Original size",
            Self::LongEdge => "Long edge",
            Self::ShortEdge => "Short edge",
            Self::Width => "Width",
            Self::Height => "Height",
            Self::Percentage => "Percentage",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportSettings {
    pub resize_mode: ExportResizeMode,
    pub edge_or_dimension: u32,
    pub percentage: f32,
    pub allow_upscale: bool,
    pub keep_metadata: bool,
    pub jpeg_quality: u8,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            resize_mode: ExportResizeMode::Original,
            edge_or_dimension: 3000,
            percentage: 100.0,
            allow_upscale: false,
            keep_metadata: true,
            jpeg_quality: 90,
        }
    }
}

pub const MAX_EXPORT_EDGE: u32 = 32_768;
#[cfg(target_os = "android")]
pub const MAX_EXPORT_PIXELS: u64 = 50_000_000;
#[cfg(not(target_os = "android"))]
pub const MAX_EXPORT_PIXELS: u64 = 120_000_000;
#[cfg(target_os = "android")]
const MAX_EXPORT_BAND_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(not(target_os = "android"))]
const MAX_EXPORT_BAND_BYTES: u64 = 192 * 1024 * 1024;
const STALE_EXPORT_PART_AGE: Duration = Duration::from_secs(24 * 60 * 60);

impl ExportSettings {
    pub fn output_dimensions(self, source_width: u32, source_height: u32) -> (u32, u32) {
        let source_width = source_width.max(1);
        let source_height = source_height.max(1);
        if self.resize_mode == ExportResizeMode::Original {
            return (source_width, source_height);
        }

        let width = source_width as f64;
        let height = source_height as f64;
        let requested = self.edge_or_dimension.max(1) as f64;
        let mut scale = match self.resize_mode {
            ExportResizeMode::Original => 1.0,
            ExportResizeMode::LongEdge => requested / width.max(height),
            ExportResizeMode::ShortEdge => requested / width.min(height),
            ExportResizeMode::Width => requested / width,
            ExportResizeMode::Height => requested / height,
            ExportResizeMode::Percentage => f64::from(self.percentage.clamp(1.0, 400.0)) / 100.0,
        };
        if !self.allow_upscale {
            scale = scale.min(1.0);
        }
        scale = scale.max(1.0 / width.max(height));

        let output_width = (width * scale).round().clamp(1.0, u32::MAX as f64) as u32;
        let output_height = (height * scale).round().clamp(1.0, u32::MAX as f64) as u32;
        (output_width, output_height)
    }

    pub fn checked_output_dimensions(
        self,
        source_width: u32,
        source_height: u32,
    ) -> Result<(u32, u32)> {
        let dimensions = self.output_dimensions(source_width, source_height);
        validate_export_dimensions(dimensions.0, dimensions.1)?;
        Ok(dimensions)
    }
}

fn validate_export_dimensions(width: u32, height: u32) -> Result<()> {
    anyhow::ensure!(
        width > 0 && height > 0,
        "export dimensions must be non-zero"
    );
    anyhow::ensure!(
        width <= MAX_EXPORT_EDGE && height <= MAX_EXPORT_EDGE,
        "export dimensions {width}x{height} exceed the {MAX_EXPORT_EDGE}-pixel edge limit"
    );
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("export pixel count overflow")?;
    anyhow::ensure!(
        pixels <= MAX_EXPORT_PIXELS,
        "export dimensions {width}x{height} contain {pixels} pixels; the limit is {MAX_EXPORT_PIXELS}"
    );
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct ExportMetadata {
    pub source_file_name: Option<String>,
    pub camera_make: String,
    pub camera_model: String,
    pub lens_make: String,
    pub lens_model: String,
    pub focal_length: f32,
    pub aperture: f32,
    pub focus_distance: f32,
    pub iso_speed: f32,
    pub shutter_seconds: f32,
    pub description: String,
    pub artist: String,
    pub source_width: u32,
    pub source_height: u32,
}

impl ExportMetadata {
    pub fn from_raw(raw: &LoadedRaw, source_file_name: Option<String>) -> Self {
        Self {
            source_file_name,
            camera_make: raw.camera_make.clone(),
            camera_model: raw.camera_model.clone(),
            lens_make: raw.lens_make.clone(),
            lens_model: raw.lens_model.clone(),
            focal_length: raw.focal_length,
            aperture: raw.aperture,
            focus_distance: raw.focus_distance,
            iso_speed: raw.capture_metadata.iso_speed,
            shutter_seconds: raw.capture_metadata.shutter_seconds,
            description: raw.capture_metadata.description.clone(),
            artist: raw.capture_metadata.artist.clone(),
            source_width: raw.width,
            source_height: raw.height,
        }
    }
}

#[derive(Debug)]
pub enum ExportEvent {
    Progress {
        completed_tiles: usize,
        total_tiles: usize,
    },
    Finished(Result<PathBuf, String>),
}

/// Runs export on a worker thread. The source mosaic always remains at its
/// native dimensions. Each halo-padded tile is demosaiced and tone-mapped on
/// the GPU, read back as display-linear Rec.2020, stitched into source rows,
/// resized in linear light, then encoded to sRGB. The destination is published
/// only after the PNG has completed successfully.
#[allow(clippy::too_many_arguments)]
pub fn spawn_tiled_png_export(
    device: wgpu::Device,
    queue: wgpu::Queue,
    raw: Arc<LoadedRaw>,
    exposure: ExposureParams,
    masks: MaskStack,
    inpaint: Option<InpaintLayer>,
    path: PathBuf,
    tile_spec: TileSpec,
    settings: ExportSettings,
    metadata: ExportMetadata,
) -> mpsc::Receiver<ExportEvent> {
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let worker_path = path.clone();

    let spawn_result = std::thread::Builder::new()
        .name("auraw-tiled-export".to_owned())
        .spawn(move || {
            let worker_started = Instant::now();
            crate::diagnostics::record(format!(
                "PNG export worker started: source={}x{} cfa={:?} requested_tile_core={} halo={} exposure={:.3} temperature={:.3} tint={:.3} demosaic={:?} highlight={:?}",
                raw.width,
                raw.height,
                raw.cfa_kind,
                tile_spec.core_edge,
                tile_spec.halo,
                exposure.exposure,
                exposure.temperature,
                exposure.tint,
                exposure.demosaic_mode,
                exposure.highlight_method,
            ));
            let result = (|| -> Result<()> {
                let (output_width, output_height) =
                    settings.checked_output_dimensions(raw.width, raw.height)?;
                let mut tile_spec = tile_spec;
                let required_halo = required_export_tile_halo(&exposure, &masks);
                tile_spec.halo = if tile_spec.halo == EXPORT_TILE_HALO {
                    required_halo
                } else {
                    tile_spec.halo.max(required_halo)
                };
                if cfg!(target_os = "android")
                    && tile_spec.core_edge == 768
                    && tile_spec.halo <= 192
                {
                    // Neutral/low-radius edits can amortize the halo with a
                    // larger core while keeping the padded allocation close to
                    // the old worst-case footprint; wide-radius edits stay at 768 px.
                    tile_spec.core_edge = 1024;
                }
                let tile_spec = bounded_tile_spec(tile_spec, raw.width)?;
                if is_direct_export_destination(&worker_path) {
                    return export_tiled_png(
                        ExportContext {
                            device: &device,
                            queue: &queue,
                            events: &worker_sender,
                        },
                        ExportRequest {
                            raw: &raw,
                            exposure: &exposure,
                            masks: &masks,
                            inpaint: inpaint.as_ref(),
                            path: &worker_path,
                            tile_spec,
                            output_width,
                            output_height,
                            keep_metadata: settings.keep_metadata,
                            metadata: &metadata,
                        },
                    );
                }
                let temporary = temporary_export_path(&worker_path)?;
                let export_result = export_tiled_png(
                    ExportContext {
                        device: &device,
                        queue: &queue,
                        events: &worker_sender,
                    },
                    ExportRequest {
                        raw: &raw,
                        exposure: &exposure,
                        masks: &masks,
                        inpaint: inpaint.as_ref(),
                        path: &temporary,
                        tile_spec,
                        output_width,
                        output_height,
                        keep_metadata: settings.keep_metadata,
                        metadata: &metadata,
                    },
                );
                if let Err(error) = export_result {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error);
                }
                if let Err(error) = publish_completed_export(&temporary, &worker_path) {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error);
                }
                Ok(())
            })();
            match &result {
                Ok(()) => crate::diagnostics::record(format!(
                    "PNG export worker finished successfully in {:.3}s",
                    worker_started.elapsed().as_secs_f64()
                )),
                Err(error) => crate::diagnostics::record(format!(
                    "PNG export worker failed after {:.3}s: {error:#}",
                    worker_started.elapsed().as_secs_f64()
                )),
            }
            let _ = worker_sender.send(ExportEvent::Finished(
                result
                    .map(|_| worker_path)
                    .map_err(|error| format!("{error:#}")),
            ));
        });

    if let Err(error) = spawn_result {
        let _ = sender.send(ExportEvent::Finished(Err(format!(
            "could not start export worker: {error}"
        ))));
    }

    receiver
}

/// JPEG export uses the exact same full-quality tiled render as PNG, writing
/// the rendered RGB8 rows into a bounded disk-backed staging raster before
/// JPEG compression. Keeping the render path shared ensures both formats match.
#[allow(clippy::too_many_arguments)]
pub fn spawn_tiled_jpeg_export(
    device: wgpu::Device,
    queue: wgpu::Queue,
    raw: Arc<LoadedRaw>,
    exposure: ExposureParams,
    masks: MaskStack,
    inpaint: Option<InpaintLayer>,
    path: PathBuf,
    tile_spec: TileSpec,
    settings: ExportSettings,
    metadata: ExportMetadata,
) -> mpsc::Receiver<ExportEvent> {
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let worker_path = path.clone();

    let spawn_result = std::thread::Builder::new()
        .name("auraw-tiled-jpeg-export".to_owned())
        .spawn(move || {
            let worker_started = Instant::now();
            crate::diagnostics::record(format!(
                "JPEG export worker started: source={}x{} quality={} cfa={:?} requested_tile_core={} halo={}",
                raw.width,
                raw.height,
                settings.jpeg_quality,
                raw.cfa_kind,
                tile_spec.core_edge,
                tile_spec.halo,
            ));
            let result = (|| -> Result<()> {
                let (output_width, output_height) =
                    settings.checked_output_dimensions(raw.width, raw.height)?;
                let mut tile_spec = tile_spec;
                let required_halo = required_export_tile_halo(&exposure, &masks);
                tile_spec.halo = if tile_spec.halo == EXPORT_TILE_HALO {
                    required_halo
                } else {
                    tile_spec.halo.max(required_halo)
                };
                if cfg!(target_os = "android")
                    && tile_spec.core_edge == 768
                    && tile_spec.halo <= 192
                {
                    // Neutral/low-radius edits can amortize the halo with a
                    // larger core while keeping the padded allocation close to
                    // the old worst-case footprint; wide-radius edits stay at 768 px.
                    tile_spec.core_edge = 1024;
                }
                let tile_spec = bounded_tile_spec(tile_spec, raw.width)?;
                if is_direct_export_destination(&worker_path) {
                    return export_tiled_jpeg(
                        ExportContext {
                            device: &device,
                            queue: &queue,
                            events: &worker_sender,
                        },
                        ExportRequest {
                            raw: &raw,
                            exposure: &exposure,
                            masks: &masks,
                            inpaint: inpaint.as_ref(),
                            path: &worker_path,
                            tile_spec,
                            output_width,
                            output_height,
                            keep_metadata: settings.keep_metadata,
                            metadata: &metadata,
                        },
                        settings.jpeg_quality,
                    );
                }
                let temporary = temporary_export_path(&worker_path)?;
                let export_result = export_tiled_jpeg(
                    ExportContext {
                        device: &device,
                        queue: &queue,
                        events: &worker_sender,
                    },
                    ExportRequest {
                        raw: &raw,
                        exposure: &exposure,
                        masks: &masks,
                        inpaint: inpaint.as_ref(),
                        path: &temporary,
                        tile_spec,
                        output_width,
                        output_height,
                        keep_metadata: settings.keep_metadata,
                        metadata: &metadata,
                    },
                    settings.jpeg_quality,
                );
                if let Err(error) = export_result {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error);
                }
                if let Err(error) = publish_completed_export(&temporary, &worker_path) {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error);
                }
                Ok(())
            })();
            match &result {
                Ok(()) => crate::diagnostics::record(format!(
                    "JPEG export worker finished successfully in {:.3}s",
                    worker_started.elapsed().as_secs_f64()
                )),
                Err(error) => crate::diagnostics::record(format!(
                    "JPEG export worker failed after {:.3}s: {error:#}",
                    worker_started.elapsed().as_secs_f64()
                )),
            }
            let _ = worker_sender.send(ExportEvent::Finished(
                result
                    .map(|_| worker_path)
                    .map_err(|error| format!("{error:#}")),
            ));
        });

    if let Err(error) = spawn_result {
        let _ = sender.send(ExportEvent::Finished(Err(format!(
            "could not start JPEG export worker: {error}"
        ))));
    }

    receiver
}

#[derive(Clone, Copy)]
struct ExportContext<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    events: &'a mpsc::Sender<ExportEvent>,
}

#[derive(Clone, Copy)]
struct ExportRequest<'a> {
    raw: &'a LoadedRaw,
    exposure: &'a ExposureParams,
    masks: &'a MaskStack,
    inpaint: Option<&'a InpaintLayer>,
    path: &'a Path,
    tile_spec: TileSpec,
    output_width: u32,
    output_height: u32,
    keep_metadata: bool,
    metadata: &'a ExportMetadata,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportRowFormat {
    Rgb8,
    Rgba8,
}

fn export_tiled_png(context: ExportContext<'_>, request: ExportRequest<'_>) -> Result<()> {
    validate_export_dimensions(request.output_width, request.output_height)?;
    let file = open_export_destination(request.path)
        .with_context(|| format!("create export {}", request.path.display()))?;
    let mut info = png::Info::with_size(request.output_width, request.output_height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Eight;
    if request.keep_metadata {
        info.exif_metadata = Some(Cow::Owned(build_exif_payload(
            request.metadata,
            request.output_width,
            request.output_height,
        )));
    }
    let mut encoder =
        png::Encoder::with_info(BufWriter::new(file), info).context("configure PNG encoder")?;
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    if request.keep_metadata {
        add_png_text_metadata(
            &mut encoder,
            request.metadata,
            request.output_width,
            request.output_height,
        )?;
    }
    let mut writer = encoder
        .write_header()
        .with_context(|| format!("write PNG header for {}", request.path.display()))?;
    let mut stream = writer
        .stream_writer_with_size(64 * 1024)
        .context("create streaming PNG writer")?;
    render_tiled_srgb(context, request, &mut stream, ExportRowFormat::Rgba8)?;
    stream.finish().context("finish streaming PNG data")?;
    writer.finish().context("finish PNG file")?;
    Ok(())
}

fn render_tiled_srgb<W: Write>(
    context: ExportContext<'_>,
    request: ExportRequest<'_>,
    output: &mut W,
    row_format: ExportRowFormat,
) -> Result<()> {
    let ExportContext {
        device,
        queue,
        events,
    } = context;
    let ExportRequest {
        raw,
        exposure,
        masks,
        inpaint,
        path: _,
        tile_spec,
        output_width,
        output_height,
        keep_metadata: _,
        metadata: _,
    } = request;
    let export_started = Instant::now();
    validate_export_dimensions(output_width, output_height)?;
    let plan = TilePlan::new(raw.width, raw.height, tile_spec);
    crate::diagnostics::record(format!(
        "Tiled export plan: output={}x{} tiles={} core={} halo={} row_format={row_format:?}",
        output_width,
        output_height,
        plan.tile_count(),
        tile_spec.core_edge,
        tile_spec.halo,
    ));
    let first = *plan
        .tiles
        .first()
        .context("cannot export an empty RAW image")?;
    let first_raw = extract_padded_tile(raw, first);
    let first_params = GpuParams::new_for_tile(
        exposure,
        masks,
        &first_raw,
        first.global_origin_x,
        first.global_origin_y,
        raw.width,
        raw.height,
    );
    let pipeline_started = Instant::now();
    let export_mask_edge = if masks.masks.is_empty() {
        mask_atlas_edge()
    } else {
        export_mask_atlas_edge(raw.width, raw.height)
    };
    let tile_pipeline = RawGpuPipeline::new_headless_with_quality_and_mask_edge(
        device,
        queue,
        &first_raw,
        &first_params,
        ProcessingQuality::High,
        export_mask_edge,
    )
    .context("create reusable full-quality export pipeline")?;
    upload_mask_atlas(&tile_pipeline, queue, masks, raw.width, raw.height)?;
    crate::diagnostics::record(format!(
        "Full-quality export pipeline prepared in {:.3}s; padded_tile={}x{} mask_atlas={}x{} R16F",
        pipeline_started.elapsed().as_secs_f64(),
        first_raw.width,
        first_raw.height,
        export_mask_edge,
        export_mask_edge
    ));

    // Preserve the original full-resolution tone statistics exactly. Every
    // native source pixel contributes once, while halo pixels are excluded via
    // the per-tile histogram bounds. Reuse one padded RAW allocation so this
    // exact prepass still benefits from the allocation/copy optimizations.
    let tone_analysis_started = Instant::now();
    let mut tone_scratch = first_raw.clone();
    tile_pipeline.begin_export_tone_analysis(queue, device);
    for (index, tile) in plan.tiles.iter().copied().enumerate() {
        if index != 0 {
            extract_padded_tile_into(raw, tile, &mut tone_scratch);
        }
        tile_pipeline
            .upload_raw_tile(queue, &tone_scratch)
            .with_context(|| format!("upload tone-analysis tile {}", index + 1))?;
        let tone_params = GpuParams::new_for_tile(
            exposure,
            masks,
            &tone_scratch,
            tile.global_origin_x,
            tile.global_origin_y,
            raw.width,
            raw.height,
        )
        .with_tone_histogram_bounds(
            tile.local_core_x,
            tile.local_core_y,
            tile.core_width,
            tile.core_height,
        );
        tile_pipeline.accumulate_export_tone_tile(queue, device, &tone_params);
    }
    tile_pipeline.finish_export_tone_analysis(queue, device);
    crate::diagnostics::record(format!(
        "Exact full-resolution tone-analysis prepass queued in {:.3}s across {} tiles",
        tone_analysis_started.elapsed().as_secs_f64(),
        plan.tile_count()
    ));

    let output_transform = IccOutputTransform::srgb();
    let mut resizer = LinearLightResizer::new_with_format(
        raw.width,
        raw.height,
        output_width,
        output_height,
        row_format,
    )?;
    // Reuse one padded RAW allocation for the entire export. Queue::write_texture
    // copies each tile before this buffer is rewritten, so reuse is safe and
    // avoids allocator churn proportional to tile count.
    let mut tile_scratch = first_raw.clone();

    let total_tiles = plan.tile_count();
    let mut completed_tiles = 0usize;
    let mut first_progress_logged = false;
    let mut tile_index = 0usize;

    while tile_index < plan.tiles.len() {
        let band_y = plan.tiles[tile_index].core_y;
        let band_height = plan.tiles[tile_index].core_height;
        let band_start = tile_index;
        while tile_index < plan.tiles.len() && plan.tiles[tile_index].core_y == band_y {
            tile_index += 1;
        }

        let band_values = checked_rgb_len(raw.width, band_height)?;
        let mut band = Vec::new();
        band.try_reserve_exact(band_values)
            .context("reserve bounded export source band")?;
        band.resize(band_values, 0.0f32);
        let mut pending_readback = None;
        for (absolute_index, tile) in plan.tiles[band_start..tile_index]
            .iter()
            .copied()
            .enumerate()
        {
            let global_index = band_start + absolute_index;
            extract_padded_tile_into(raw, tile, &mut tile_scratch);
            tile_pipeline
                .upload_raw_tile(queue, &tile_scratch)
                .with_context(|| format!("upload export tile {}", global_index + 1))?;
            tile_pipeline
                .update_inpaint_layer(
                    queue,
                    inpaint,
                    tile.global_origin_x,
                    tile.global_origin_y,
                    raw.width,
                    raw.height,
                )
                .with_context(|| {
                    format!("upload inpainting for export tile {}", global_index + 1)
                })?;

            let params = GpuParams::new_for_tile(
                exposure,
                masks,
                &tile_scratch,
                tile.global_origin_x,
                tile.global_origin_y,
                raw.width,
                raw.height,
            );
            tile_pipeline.dispatch_export_tile(queue, device, &params);
            let readback = tile_pipeline
                .begin_display_linear_region_readback(
                    device,
                    queue,
                    tile.local_core_x,
                    tile.local_core_y,
                    tile.core_width,
                    tile.core_height,
                )
                .with_context(|| format!("queue export tile readback {}", global_index + 1))?;

            // Queue this tile before consuming the previous tile. Using
            // `replace` here also lets Rust infer the concrete pending-readback
            // handle type from `readback` before we call methods on the older
            // handle. GPU rendering/copy can therefore overlap CPU stitching,
            // resizing, and encoding without requiring a public type annotation.
            let previous = pending_readback.replace((tile, global_index, readback));
            if let Some((previous_tile, previous_index, previous_readback)) = previous {
                let rgb = previous_readback
                    .finish(device)
                    .with_context(|| format!("read export tile {}", previous_index + 1))?;
                stitch_linear_tile_into_band(
                    &mut band,
                    raw.width,
                    band_y,
                    previous_tile,
                    &rgb,
                )?;
                completed_tiles += 1;
                if !first_progress_logged {
                    first_progress_logged = true;
                    crate::diagnostics::record(format!(
                        "First export tile completed after {:.3}s; pipelined GPU readback is active",
                        export_started.elapsed().as_secs_f64()
                    ));
                }
                let _ = events.send(ExportEvent::Progress {
                    completed_tiles,
                    total_tiles,
                });
            }
        }

        if let Some((last_tile, last_index, last_readback)) = pending_readback.take() {
            let rgb = last_readback
                .finish(device)
                .with_context(|| format!("read export tile {}", last_index + 1))?;
            stitch_linear_tile_into_band(&mut band, raw.width, band_y, last_tile, &rgb)?;
            completed_tiles += 1;
            if !first_progress_logged {
                first_progress_logged = true;
                crate::diagnostics::record(format!(
                    "First export tile completed after {:.3}s; pipelined GPU readback is active",
                    export_started.elapsed().as_secs_f64()
                ));
            }
            let _ = events.send(ExportEvent::Progress {
                completed_tiles,
                total_tiles,
            });
        }

        let source_row_values = checked_rgb_len(raw.width, 1)?;
        for local_y in 0..band_height {
            let start = usize::try_from(local_y)
                .ok()
                .and_then(|row| row.checked_mul(source_row_values))
                .context("source export row offset overflow")?;
            let end = start
                .checked_add(source_row_values)
                .context("source export row end overflow")?;
            let source_y = band_y + local_y;
            resizer.push_source_row(source_y, &band[start..end], &output_transform, output)?;
        }
    }

    resizer.finish(&output_transform, output)?;
    Ok(())
}



fn export_tiled_jpeg(
    context: ExportContext<'_>,
    request: ExportRequest<'_>,
    quality: u8,
) -> Result<()> {
    let quality = quality.clamp(1, 100);
    let staged_rgb = temporary_export_path(request.path)?;
    let encoded_jpeg = temporary_export_path(request.path)?;
    let encode_result = (|| -> Result<()> {
        // Render directly into a disk-backed RGB8 raster. This removes the old
        // full PNG encode -> PNG decode -> RGB staging round trip while keeping
        // peak Android heap use bounded.
        {
            let rgb_file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged_rgb)
                .with_context(|| format!("create staged RGB raster {}", staged_rgb.display()))?;
            let mut rgb_writer = BufWriter::new(rgb_file);
            render_tiled_srgb(context, request, &mut rgb_writer, ExportRowFormat::Rgb8)?;
            rgb_writer.flush().context("flush staged RGB raster")?;
        }

        let rgb_file = fs::File::open(&staged_rgb)
            .with_context(|| format!("open staged RGB raster {}", staged_rgb.display()))?;
        // SAFETY: the file remains open and immutable for the lifetime of the
        // read-only mapping; it is deleted only after JPEG encoding completes.
        let mapped = unsafe { memmap2::MmapOptions::new().map(&rgb_file) }
            .with_context(|| format!("map staged RGB raster {}", staged_rgb.display()))?;
        let expected = u64::from(request.output_width)
            .checked_mul(u64::from(request.output_height))
            .and_then(|pixels| pixels.checked_mul(3))
            .context("staged RGB image size overflow")?;
        anyhow::ensure!(
            u64::try_from(mapped.len()).unwrap_or(u64::MAX) == expected,
            "staged RGB raster length does not match its dimensions"
        );

        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&encoded_jpeg)
            .with_context(|| format!("create staged JPEG {}", encoded_jpeg.display()))?;
        let mut writer = BufWriter::new(file);
        {
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
                &mut writer,
                quality,
            );
            encoder
                .encode(
                    &mapped[..],
                    request.output_width,
                    request.output_height,
                    image::ExtendedColorType::Rgb8,
                )
                .with_context(|| format!("encode JPEG {}", request.path.display()))?;
        }
        writer.flush().context("flush staged JPEG")?;
        drop(mapped);
        drop(rgb_file);

        write_final_jpeg(
            &encoded_jpeg,
            request.path,
            request.keep_metadata,
            request.metadata,
            request.output_width,
            request.output_height,
        )?;
        Ok(())
    })();
    let _ = fs::remove_file(&staged_rgb);
    let _ = fs::remove_file(&encoded_jpeg);
    if encode_result.is_err() {
        let _ = fs::remove_file(request.path);
    }
    encode_result
}

fn write_final_jpeg(
    encoded_path: &Path,
    output_path: &Path,
    keep_metadata: bool,
    metadata: &ExportMetadata,
    output_width: u32,
    output_height: u32,
) -> Result<()> {
    if !keep_metadata {
        if is_direct_export_destination(output_path) {
            let mut input = BufReader::new(
                fs::File::open(encoded_path)
                    .with_context(|| format!("open staged JPEG {}", encoded_path.display()))?,
            );
            let mut output = BufWriter::new(open_export_destination(output_path)?);
            std::io::copy(&mut input, &mut output).context("copy JPEG to direct export destination")?;
            output.flush().context("flush direct JPEG export")?;
            return Ok(());
        }
        return fs::rename(encoded_path, output_path).with_context(|| {
            format!(
                "publish staged JPEG {} to {}",
                encoded_path.display(),
                output_path.display()
            )
        });
    }

    let mut input = BufReader::new(
        fs::File::open(encoded_path)
            .with_context(|| format!("open staged JPEG {}", encoded_path.display()))?,
    );
    let mut soi = [0u8; 2];
    input.read_exact(&mut soi).context("read JPEG SOI marker")?;
    anyhow::ensure!(soi == [0xff, 0xd8], "staged JPEG is missing its SOI marker");

    let tiff = build_exif_payload(metadata, output_width, output_height);
    let payload_len = 6usize
        .checked_add(tiff.len())
        .context("JPEG EXIF payload length overflow")?;
    let segment_len = payload_len
        .checked_add(2)
        .context("JPEG EXIF segment length overflow")?;
    let segment_len = u16::try_from(segment_len)
        .context("JPEG EXIF metadata exceeds the APP1 segment limit")?;

    let output = open_export_destination(output_path)
        .with_context(|| format!("create final JPEG {}", output_path.display()))?;
    let mut output = BufWriter::new(output);
    output.write_all(&soi).context("write JPEG SOI marker")?;
    output.write_all(&[0xff, 0xe1]).context("write JPEG APP1 marker")?;
    output
        .write_all(&segment_len.to_be_bytes())
        .context("write JPEG APP1 length")?;
    output
        .write_all(b"Exif\0\0")
        .context("write JPEG EXIF signature")?;
    output.write_all(&tiff).context("write JPEG EXIF payload")?;
    std::io::copy(&mut input, &mut output).context("copy JPEG image data")?;
    output.flush().context("flush final JPEG")?;
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct SampleWeight {
    index: u32,
    weight: f32,
}

#[derive(Clone, Copy, Debug)]
struct OutputSampleWeight {
    output_index: u32,
    weight: f32,
}

struct LinearLightResizer {
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    horizontal: Vec<Vec<SampleWeight>>,
    vertical_by_source: Vec<Vec<OutputSampleWeight>>,
    output_last_source: Vec<u32>,
    pending_rows: Vec<Option<Vec<f32>>>,
    next_source_row: u32,
    next_output_row: u32,
    row_format: ExportRowFormat,
}

impl LinearLightResizer {
    fn new(
        source_width: u32,
        source_height: u32,
        output_width: u32,
        output_height: u32,
    ) -> Result<Self> {
        Self::new_with_format(
            source_width,
            source_height,
            output_width,
            output_height,
            ExportRowFormat::Rgba8,
        )
    }

    fn new_with_format(
        source_width: u32,
        source_height: u32,
        output_width: u32,
        output_height: u32,
        row_format: ExportRowFormat,
    ) -> Result<Self> {
        validate_export_dimensions(output_width, output_height)?;
        anyhow::ensure!(
            source_width > 0 && source_height > 0,
            "source image is empty"
        );
        let vertical = build_lanczos_contributions(source_height, output_height)?;
        let (vertical_by_source, output_last_source) =
            invert_vertical_contributions(source_height, &vertical)?;
        let mut pending_rows = Vec::new();
        pending_rows
            .try_reserve_exact(output_height as usize)
            .context("reserve vertical resize row slots")?;
        pending_rows.resize_with(output_height as usize, || None);
        Ok(Self {
            source_width,
            source_height,
            output_width,
            output_height,
            horizontal: build_lanczos_contributions(source_width, output_width)?,
            vertical_by_source,
            output_last_source,
            pending_rows,
            next_source_row: 0,
            next_output_row: 0,
            row_format,
        })
    }

    fn push_source_row<W: Write>(
        &mut self,
        source_y: u32,
        source: &[f32],
        output_transform: &IccOutputTransform,
        output: &mut W,
    ) -> Result<()> {
        anyhow::ensure!(
            source_y == self.next_source_row,
            "source rows arrived out of order: got {source_y}, expected {}",
            self.next_source_row
        );
        anyhow::ensure!(
            source.len() == checked_rgb_len(self.source_width, 1)?,
            "source row length does not match export width"
        );
        let horizontal = resize_horizontal_row(source, &self.horizontal)?;
        let contribution_count = self
            .vertical_by_source
            .get(source_y as usize)
            .context("source resize row is outside the contribution table")?
            .len();
        for contribution_index in 0..contribution_count {
            let contribution = self.vertical_by_source[source_y as usize][contribution_index];
            {
                let output_index = contribution.output_index as usize;
                let slot = self
                    .pending_rows
                    .get_mut(output_index)
                    .context("output resize row is outside the pending table")?;
                if slot.is_none() {
                    let row_values = checked_rgb_len(self.output_width, 1)?;
                    let mut row = Vec::new();
                    row.try_reserve_exact(row_values)
                        .context("reserve active vertical resize row")?;
                    row.resize(row_values, 0.0f32);
                    *slot = Some(row);
                }
                let row = slot
                    .as_mut()
                    .context("pending output row failed to initialize")?;
                for (destination, value) in row.iter_mut().zip(&horizontal) {
                    *destination += *value * contribution.weight;
                }
            }
            // Contributions for a source row are ordered by output index. Do
            // not flush a later row until its contribution from this source
            // has actually been accumulated.
            self.write_ready_rows_through(
                source_y,
                contribution.output_index,
                output_transform,
                output,
            )?;
        }
        self.next_source_row += 1;
        Ok(())
    }

    fn finish<W: Write>(
        &mut self,
        output_transform: &IccOutputTransform,
        output: &mut W,
    ) -> Result<()> {
        anyhow::ensure!(
            self.next_source_row == self.source_height,
            "linear resizer received {} of {} source rows",
            self.next_source_row,
            self.source_height
        );
        self.write_ready_rows_through(
            self.source_height - 1,
            self.output_height - 1,
            output_transform,
            output,
        )?;
        anyhow::ensure!(
            self.next_output_row == self.output_height,
            "linear resizer produced {} of {} output rows",
            self.next_output_row,
            self.output_height
        );
        anyhow::ensure!(
            self.pending_rows.iter().all(Option::is_none),
            "linear resizer retained incomplete output rows"
        );
        Ok(())
    }

    fn write_ready_rows_through<W: Write>(
        &mut self,
        source_y: u32,
        completed_through_output: u32,
        output_transform: &IccOutputTransform,
        output: &mut W,
    ) -> Result<()> {
        while self.next_output_row < self.output_height
            && self.next_output_row <= completed_through_output
            && self.output_last_source[self.next_output_row as usize] <= source_y
        {
            let row = self.pending_rows[self.next_output_row as usize]
                .take()
                .context("completed resize row has no accumulated pixels")?;
            let encoded = encode_srgb_row_with_format(&row, output_transform, self.row_format)?;
            output
                .write_all(&encoded)
                .with_context(|| format!("write output row {}", self.next_output_row))?;
            self.next_output_row += 1;
        }
        Ok(())
    }
}

fn invert_vertical_contributions(
    source_height: u32,
    vertical: &[Vec<SampleWeight>],
) -> Result<(Vec<Vec<OutputSampleWeight>>, Vec<u32>)> {
    let source_rows = source_height as usize;
    let mut counts = Vec::new();
    counts
        .try_reserve_exact(source_rows)
        .context("reserve vertical contribution counts")?;
    counts.resize(source_rows, 0usize);
    let mut output_last_source = Vec::new();
    output_last_source
        .try_reserve_exact(vertical.len())
        .context("reserve output resize boundaries")?;
    for samples in vertical {
        let last = samples
            .last()
            .map(|sample| sample.index)
            .context("vertical resampling kernel is empty")?;
        output_last_source.push(last);
        for sample in samples {
            let count = counts
                .get_mut(sample.index as usize)
                .context("vertical contribution references an invalid source row")?;
            *count = count
                .checked_add(1)
                .context("vertical contribution count overflow")?;
        }
    }

    let mut by_source = Vec::new();
    by_source
        .try_reserve_exact(source_rows)
        .context("reserve vertical contribution rows")?;
    for count in counts {
        let mut row = Vec::new();
        row.try_reserve_exact(count)
            .context("reserve vertical source contributions")?;
        by_source.push(row);
    }
    for (output_index, samples) in vertical.iter().enumerate() {
        let output_index =
            u32::try_from(output_index).context("output resize index does not fit in u32")?;
        for sample in samples {
            by_source[sample.index as usize].push(OutputSampleWeight {
                output_index,
                weight: sample.weight,
            });
        }
    }
    Ok((by_source, output_last_source))
}

fn build_lanczos_contributions(source: u32, output: u32) -> Result<Vec<Vec<SampleWeight>>> {
    anyhow::ensure!(
        source > 0 && output > 0,
        "resize dimensions must be non-zero"
    );
    let mut all = Vec::new();
    all.try_reserve_exact(output as usize)
        .context("reserve resize contribution table")?;
    if source == output {
        for index in 0..output {
            all.push(vec![SampleWeight { index, weight: 1.0 }]);
        }
        return Ok(all);
    }

    let scale = source as f64 / output as f64;
    let filter_scale = scale.max(1.0);
    let support = 3.0 * filter_scale;
    for destination in 0..output {
        let center = (destination as f64 + 0.5) * scale - 0.5;
        // Truncate to real source samples and renormalize. Iterating virtual
        // clamped taps can be O(source^2) for extreme reductions.
        let first = ((center - support).floor() as i64 + 1).max(0);
        let last = ((center + support).ceil() as i64 - 1).min(i64::from(source) - 1);
        anyhow::ensure!(first <= last, "resize kernel contains no source samples");
        let capacity = usize::try_from(last - first + 1)
            .context("resize kernel is too large for this platform")?;
        let mut samples = Vec::<SampleWeight>::new();
        samples
            .try_reserve_exact(capacity)
            .context("reserve resize kernel")?;
        for source_index in first..=last {
            let distance = (center - source_index as f64) / filter_scale;
            let weight = lanczos3(distance) / filter_scale;
            if weight.abs() <= 1e-15 {
                continue;
            }
            samples.push(SampleWeight {
                index: source_index as u32,
                weight: weight as f32,
            });
        }
        let sum: f32 = samples.iter().map(|sample| sample.weight).sum();
        anyhow::ensure!(
            sum.is_finite() && sum.abs() > 1e-12,
            "invalid resize kernel"
        );
        for sample in &mut samples {
            sample.weight /= sum;
        }
        all.push(samples);
    }
    Ok(all)
}

fn lanczos3(value: f64) -> f64 {
    let value = value.abs();
    if value < 1e-12 {
        return 1.0;
    }
    if value >= 3.0 {
        return 0.0;
    }
    let pi_value = std::f64::consts::PI * value;
    (pi_value.sin() / pi_value) * ((pi_value / 3.0).sin() / (pi_value / 3.0))
}

fn resize_horizontal_row(source: &[f32], weights: &[Vec<SampleWeight>]) -> Result<Vec<f32>> {
    let values = weights
        .len()
        .checked_mul(3)
        .context("resize row overflow")?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(values)
        .context("reserve horizontal resize row")?;
    output.resize(values, 0.0f32);
    for (destination, samples) in weights.iter().enumerate() {
        for sample in samples {
            let source_start = usize::try_from(sample.index)
                .ok()
                .and_then(|index| index.checked_mul(3))
                .context("source resize index overflow")?;
            let destination_start = destination
                .checked_mul(3)
                .context("destination resize index overflow")?;
            for channel in 0..3 {
                output[destination_start + channel] +=
                    source[source_start + channel] * sample.weight;
            }
        }
    }
    Ok(output)
}

fn encode_srgb_row(row: &[f32], transform: &IccOutputTransform) -> Result<Vec<u8>> {
    encode_srgb_row_with_format(row, transform, ExportRowFormat::Rgba8)
}

fn encode_srgb_row_with_format(
    row: &[f32],
    transform: &IccOutputTransform,
    row_format: ExportRowFormat,
) -> Result<Vec<u8>> {
    anyhow::ensure!(
        row.len().is_multiple_of(3),
        "linear RGB row has an invalid length"
    );
    let pixels = row.len() / 3;
    let channels = match row_format {
        ExportRowFormat::Rgb8 => 3,
        ExportRowFormat::Rgba8 => 4,
    };
    let bytes = pixels
        .checked_mul(channels)
        .context("encoded row overflow")?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(bytes)
        .context("reserve encoded export row")?;
    for rgb in row.chunks_exact(3) {
        anyhow::ensure!(
            rgb.iter().all(|value| value.is_finite()),
            "export contains NaN or infinity"
        );
        let device = transform.transform_rgb([rgb[0], rgb[1], rgb[2]]);
        for value in device {
            encoded.push((value.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        if row_format == ExportRowFormat::Rgba8 {
            encoded.push(255);
        }
    }
    Ok(encoded)
}

fn stitch_linear_tile_into_band(
    band: &mut [f32],
    source_width: u32,
    band_y: u32,
    tile: super::ExportTile,
    rgb: &[f32],
) -> Result<()> {
    anyhow::ensure!(tile.core_y == band_y, "tile is in the wrong export band");
    let source_row_values = checked_rgb_len(tile.core_width, 1)?;
    anyhow::ensure!(
        rgb.len() == checked_rgb_len(tile.core_width, tile.core_height)?,
        "GPU tile readback length does not match tile dimensions"
    );
    for row in 0..tile.core_height as usize {
        let source_start = row
            .checked_mul(source_row_values)
            .context("tile source row overflow")?;
        let destination_pixel = row
            .checked_mul(source_width as usize)
            .and_then(|value| value.checked_add(tile.core_x as usize))
            .context("tile destination row overflow")?;
        let destination_start = destination_pixel
            .checked_mul(3)
            .context("tile destination channel overflow")?;
        let destination_end = destination_start
            .checked_add(source_row_values)
            .context("tile destination end overflow")?;
        band.get_mut(destination_start..destination_end)
            .context("tile destination is outside its export band")?
            .copy_from_slice(&rgb[source_start..source_start + source_row_values]);
    }
    Ok(())
}

fn checked_rgb_len(width: u32, height: u32) -> Result<usize> {
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("image pixel count overflow")?;
    let values = pixels.checked_mul(3).context("RGB value count overflow")?;
    usize::try_from(values).context("RGB allocation does not fit this platform")
}

fn validate_tile_spec(spec: TileSpec) -> Result<()> {
    let maximum_core = 1024;
    let scale = if cfg!(target_os = "android") { 8 } else { 4 };
    anyhow::ensure!(
        (64..=maximum_core).contains(&spec.core_edge),
        "export tile core must be between 64 and {maximum_core} pixels"
    );
    anyhow::ensure!(
        (MIN_EXPORT_TILE_HALO..=512).contains(&spec.halo),
        "export halo must be between {MIN_EXPORT_TILE_HALO} and 512 pixels"
    );
    anyhow::ensure!(
        spec.core_edge.is_multiple_of(scale) && spec.halo.is_multiple_of(scale),
        "export tile core and halo must align to the global tone-guide grid"
    );
    spec.core_edge
        .checked_add(spec.halo.checked_mul(2).context("export halo overflow")?)
        .context("padded export tile overflow")?;
    Ok(())
}

fn bounded_tile_spec(mut spec: TileSpec, source_width: u32) -> Result<TileSpec> {
    validate_tile_spec(spec)?;
    let bytes_per_source_row = u64::from(source_width)
        .checked_mul(3)
        .and_then(|value| value.checked_mul(std::mem::size_of::<f32>() as u64))
        .context("export source-band row size overflow")?;
    anyhow::ensure!(bytes_per_source_row > 0, "export source width is zero");
    let alignment = if cfg!(target_os = "android") { 8 } else { 4 };
    let maximum_rows =
        (MAX_EXPORT_BAND_BYTES / bytes_per_source_row).min(u64::from(spec.core_edge)) as u32;
    let aligned_rows = maximum_rows - maximum_rows % alignment;
    anyhow::ensure!(
        aligned_rows >= 64,
        "the source image is too wide for the bounded export memory budget"
    );
    spec.core_edge = spec.core_edge.min(aligned_rows);
    validate_tile_spec(spec)?;
    Ok(spec)
}

fn is_direct_export_destination(path: &Path) -> bool {
    #[cfg(target_os = "android")]
    {
        crate::android::is_direct_export_path(path)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = path;
        false
    }
}

fn open_export_destination(destination: &Path) -> Result<fs::File> {
    let mut options = OpenOptions::new();
    options.write(true);
    if is_direct_export_destination(destination) {
        options.truncate(true);
    } else {
        options.create_new(true);
    }
    options
        .open(destination)
        .with_context(|| format!("open export destination {}", destination.display()))
}

fn temporary_export_path(destination: &Path) -> Result<PathBuf> {
    let direct = is_direct_export_destination(destination);
    let parent = if direct {
        #[cfg(target_os = "android")]
        {
            crate::android::direct_export_temp_dir(destination)
                .context("Android direct export has no temporary staging directory")?
        }
        #[cfg(not(target_os = "android"))]
        {
            unreachable!("direct export destinations only exist on Android")
        }
    } else {
        destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    };
    fs::create_dir_all(&parent)
        .with_context(|| format!("create export directory {}", parent.display()))?;
    let name = if direct {
        "auraw-direct-export"
    } else {
        destination
            .file_name()
            .and_then(|value| value.to_str())
            .context("export path has no valid file name")?
    };
    cleanup_stale_export_parts(&parent, name);
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(".{name}.{}.{}.part", std::process::id(), nonce)))
}

fn cleanup_stale_export_parts(parent: &Path, destination_name: &str) {
    let prefix = format!(".{destination_name}.");
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&prefix) || !name.ends_with(".part") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let old_enough = metadata
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_EXPORT_PART_AGE);
        if old_enough {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn publish_completed_export(temporary: &Path, destination: &Path) -> Result<()> {
    replace_export_file(temporary, destination).with_context(|| {
        format!(
            "publish completed export {} to {}",
            temporary.display(),
            destination.display()
        )
    })
}

#[cfg(not(windows))]
fn replace_export_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_export_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both buffers are NUL-terminated and remain alive for the call.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn upload_mask_atlas(
    pipeline: &RawGpuPipeline,
    queue: &wgpu::Queue,
    masks: &MaskStack,
    image_width: u32,
    image_height: u32,
) -> Result<()> {
    let edge = pipeline.mask_atlas_edge();
    for layer in 0..MAX_LOCAL_MASKS {
        let bytes = masks.rasterize_layer_f16(layer, edge, edge, image_width, image_height);
        pipeline
            .update_mask_layer(queue, layer, &bytes)
            .with_context(|| format!("upload local-mask layer {}", layer + 1))?;
    }
    Ok(())
}

fn add_png_text_metadata<W: Write>(
    encoder: &mut png::Encoder<'_, W>,
    metadata: &ExportMetadata,
    output_width: u32,
    output_height: u32,
) -> Result<()> {
    encoder
        .add_itxt_chunk("Software".to_owned(), "AuRaw 2.0".to_owned())
        .context("write PNG software metadata")?;
    if let Some(source) = metadata
        .source_file_name
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        encoder
            .add_itxt_chunk("Source".to_owned(), source.to_owned())
            .context("write PNG source metadata")?;
    }
    let camera = joined_metadata_label(&metadata.camera_make, &metadata.camera_model);
    if !camera.is_empty() {
        encoder
            .add_itxt_chunk("Camera".to_owned(), camera)
            .context("write PNG camera metadata")?;
    }
    let lens = joined_metadata_label(&metadata.lens_make, &metadata.lens_model);
    if !lens.is_empty() {
        encoder
            .add_itxt_chunk("Lens".to_owned(), lens)
            .context("write PNG lens metadata")?;
    }
    if metadata.focal_length.is_finite() && metadata.focal_length > 0.0 {
        encoder
            .add_itxt_chunk(
                "Focal length".to_owned(),
                format!("{:.1} mm", metadata.focal_length),
            )
            .context("write PNG focal-length metadata")?;
    }
    if metadata.aperture.is_finite() && metadata.aperture > 0.0 {
        encoder
            .add_itxt_chunk("Aperture".to_owned(), format!("f/{:.1}", metadata.aperture))
            .context("write PNG aperture metadata")?;
    }
    if metadata.focus_distance.is_finite() && metadata.focus_distance > 0.0 {
        encoder
            .add_itxt_chunk(
                "Focus distance".to_owned(),
                format!("{:.2} m", metadata.focus_distance),
            )
            .context("write PNG focus-distance metadata")?;
    }
    if metadata.iso_speed.is_finite() && metadata.iso_speed > 0.0 {
        encoder
            .add_itxt_chunk("ISO speed".to_owned(), format!("{:.0}", metadata.iso_speed))
            .context("write PNG ISO metadata")?;
    }
    if metadata.shutter_seconds.is_finite() && metadata.shutter_seconds > 0.0 {
        encoder
            .add_itxt_chunk(
                "Exposure time".to_owned(),
                format_exposure_time(metadata.shutter_seconds),
            )
            .context("write PNG exposure-time metadata")?;
    }
    if !metadata.artist.trim().is_empty() {
        encoder
            .add_itxt_chunk("Artist".to_owned(), metadata.artist.trim().to_owned())
            .context("write PNG artist metadata")?;
    }
    if !metadata.description.trim().is_empty() {
        encoder
            .add_itxt_chunk(
                "Image description".to_owned(),
                metadata.description.trim().to_owned(),
            )
            .context("write PNG image-description metadata")?;
    }
    encoder
        .add_itxt_chunk(
            "Original dimensions".to_owned(),
            format!("{}x{}", metadata.source_width, metadata.source_height),
        )
        .context("write original dimensions metadata")?;
    encoder
        .add_itxt_chunk(
            "Export dimensions".to_owned(),
            format!("{output_width}x{output_height}"),
        )
        .context("write export dimensions metadata")?;
    encoder
        .add_itxt_chunk("Orientation".to_owned(), "1 (normal)".to_owned())
        .context("write PNG orientation metadata")?;
    Ok(())
}

fn format_exposure_time(seconds: f32) -> String {
    if seconds > 0.0 && seconds < 1.0 {
        let reciprocal = (1.0 / seconds).round().max(1.0);
        if ((1.0 / reciprocal) - seconds).abs() <= seconds * 0.02 {
            return format!("1/{reciprocal:.0} s");
        }
    }
    format!("{seconds:.4} s")
}

fn joined_metadata_label(make: &str, model: &str) -> String {
    match (make.trim(), model.trim()) {
        ("", "") => String::new(),
        ("", model) => model.to_owned(),
        (make, "") => make.to_owned(),
        (make, model) if model.starts_with(make) => model.to_owned(),
        (make, model) => format!("{make} {model}"),
    }
}

fn export_metadata_description(metadata: &ExportMetadata) -> String {
    let mut parts = Vec::with_capacity(3);
    if let Some(source) = metadata
        .source_file_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("Processed from {source}"));
    } else {
        parts.push("Processed from a RAW image".to_owned());
    }
    if metadata.source_width > 0 && metadata.source_height > 0 {
        parts.push(format!(
            "original dimensions {}x{}",
            metadata.source_width, metadata.source_height
        ));
    }
    parts.push("exported by AuRaw 2.0".to_owned());
    parts.join("; ")
}

fn combined_image_description(metadata: &ExportMetadata) -> String {
    let export_description = export_metadata_description(metadata);
    match metadata.description.trim() {
        "" => export_description,
        original => format!("{original}; {export_description}"),
    }
}

#[derive(Clone)]
enum ExifValue {
    Short(u16),
    Long(u32),
    Ascii(Vec<u8>),
    Rational(u32, u32),
    Undefined(Vec<u8>),
}

#[derive(Clone)]
struct ExifEntry {
    tag: u16,
    value: ExifValue,
}

fn nul_terminated_exif_ascii(value: &str) -> Vec<u8> {
    let mut output = value
        .chars()
        .map(|character| {
            if character.is_ascii() && character != '\0' {
                character as u8
            } else {
                b'?'
            }
        })
        .collect::<Vec<_>>();
    output.push(0);
    output
}

fn exif_rational(value: f32) -> Option<(u32, u32)> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let denominator = 10_000u32;
    let numerator = (f64::from(value) * f64::from(denominator))
        .round()
        .clamp(1.0, f64::from(u32::MAX)) as u32;
    let divisor = greatest_common_divisor(numerator, denominator);
    Some((numerator / divisor, denominator / divisor))
}

fn greatest_common_divisor(mut left: u32, mut right: u32) -> u32 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}

fn exif_value_parts(value: &ExifValue) -> (u16, u32, Vec<u8>) {
    match value {
        ExifValue::Short(value) => (3, 1, value.to_le_bytes().to_vec()),
        ExifValue::Long(value) => (4, 1, value.to_le_bytes().to_vec()),
        ExifValue::Ascii(value) => (2, value.len() as u32, value.clone()),
        ExifValue::Rational(numerator, denominator) => {
            let mut bytes = Vec::with_capacity(8);
            bytes.extend_from_slice(&numerator.to_le_bytes());
            bytes.extend_from_slice(&(*denominator).max(1).to_le_bytes());
            (5, 1, bytes)
        }
        ExifValue::Undefined(value) => (7, value.len() as u32, value.clone()),
    }
}

fn encoded_ifd_block_len(entries: &[ExifEntry]) -> u32 {
    let directory_len = 2usize
        .saturating_add(entries.len().saturating_mul(12))
        .saturating_add(4);
    let data_len = entries
        .iter()
        .map(|entry| {
            let (_, _, bytes) = exif_value_parts(&entry.value);
            if bytes.len() <= 4 {
                0
            } else {
                bytes.len() + (bytes.len() & 1)
            }
        })
        .sum::<usize>();
    u32::try_from(directory_len.saturating_add(data_len)).unwrap_or(u32::MAX)
}

fn encode_ifd_block(entries: &[ExifEntry], ifd_offset: u32) -> Vec<u8> {
    let directory_len = 2usize + entries.len() * 12 + 4;
    let data_offset = ifd_offset.saturating_add(directory_len as u32);
    let mut directory = Vec::with_capacity(directory_len);
    let mut data = Vec::new();
    directory.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    for entry in entries {
        directory.extend_from_slice(&entry.tag.to_le_bytes());
        let (field_type, count, bytes) = exif_value_parts(&entry.value);
        directory.extend_from_slice(&field_type.to_le_bytes());
        directory.extend_from_slice(&count.to_le_bytes());
        if bytes.len() <= 4 {
            directory.extend_from_slice(&bytes);
            directory.resize(directory.len() + 4 - bytes.len(), 0);
        } else {
            let value_offset = data_offset.saturating_add(data.len() as u32);
            directory.extend_from_slice(&value_offset.to_le_bytes());
            data.extend_from_slice(&bytes);
            if data.len() & 1 != 0 {
                data.push(0);
            }
        }
    }
    directory.extend_from_slice(&0u32.to_le_bytes());
    directory.extend_from_slice(&data);
    directory
}

/// Builds a compact, standards-shaped TIFF/EXIF payload used by both JPEG's
/// APP1 segment and PNG's eXIf chunk. The output pixels have already been
/// physically oriented, so Orientation is always normalized to 1.
fn build_exif_payload(metadata: &ExportMetadata, output_width: u32, output_height: u32) -> Vec<u8> {
    let mut ifd0_entries = vec![
        ExifEntry {
            tag: 0x0100,
            value: ExifValue::Long(output_width),
        },
        ExifEntry {
            tag: 0x0101,
            value: ExifValue::Long(output_height),
        },
        ExifEntry {
            tag: 0x010e,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&combined_image_description(
                metadata,
            ))),
        },
        ExifEntry {
            tag: 0x0112,
            value: ExifValue::Short(1),
        },
        ExifEntry {
            tag: 0x0131,
            value: ExifValue::Ascii(nul_terminated_exif_ascii("AuRaw 2.0")),
        },
    ];
    if !metadata.camera_make.trim().is_empty() {
        ifd0_entries.push(ExifEntry {
            tag: 0x010f,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.camera_make)),
        });
    }
    if !metadata.camera_model.trim().is_empty() {
        ifd0_entries.push(ExifEntry {
            tag: 0x0110,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.camera_model)),
        });
    }
    if !metadata.artist.trim().is_empty() {
        ifd0_entries.push(ExifEntry {
            tag: 0x013b,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.artist)),
        });
    }

    let mut exif_entries = vec![
        ExifEntry {
            tag: 0x9000,
            value: ExifValue::Undefined(b"0232".to_vec()),
        },
        ExifEntry {
            tag: 0xa002,
            value: ExifValue::Long(output_width),
        },
        ExifEntry {
            tag: 0xa003,
            value: ExifValue::Long(output_height),
        },
    ];
    if let Some((numerator, denominator)) = exif_rational(metadata.shutter_seconds) {
        exif_entries.push(ExifEntry {
            tag: 0x829a,
            value: ExifValue::Rational(numerator, denominator),
        });
    }
    if metadata.iso_speed.is_finite() && metadata.iso_speed > 0.0 {
        let iso = metadata.iso_speed.round().clamp(1.0, u32::MAX as f32) as u32;
        exif_entries.push(ExifEntry {
            tag: 0x8827,
            value: if iso <= u32::from(u16::MAX) {
                ExifValue::Short(iso as u16)
            } else {
                ExifValue::Long(iso)
            },
        });
    }
    if let Some((numerator, denominator)) = exif_rational(metadata.aperture) {
        exif_entries.push(ExifEntry {
            tag: 0x829d,
            value: ExifValue::Rational(numerator, denominator),
        });
    }
    if let Some((numerator, denominator)) = exif_rational(metadata.focal_length) {
        exif_entries.push(ExifEntry {
            tag: 0x920a,
            value: ExifValue::Rational(numerator, denominator),
        });
    }
    if let Some((numerator, denominator)) = exif_rational(metadata.focus_distance) {
        exif_entries.push(ExifEntry {
            tag: 0x9206,
            value: ExifValue::Rational(numerator, denominator),
        });
    }
    if !metadata.lens_make.trim().is_empty() {
        exif_entries.push(ExifEntry {
            tag: 0xa433,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.lens_make)),
        });
    }
    if !metadata.lens_model.trim().is_empty() {
        exif_entries.push(ExifEntry {
            tag: 0xa434,
            value: ExifValue::Ascii(nul_terminated_exif_ascii(&metadata.lens_model)),
        });
    }
    let mut user_comment = b"ASCII\0\0\0".to_vec();
    user_comment.extend_from_slice(
        &nul_terminated_exif_ascii(&combined_image_description(metadata))[..],
    );
    exif_entries.push(ExifEntry {
        tag: 0x9286,
        value: ExifValue::Undefined(user_comment),
    });

    ifd0_entries.sort_by_key(|entry| entry.tag);
    exif_entries.sort_by_key(|entry| entry.tag);

    // Adding the ExifIFD pointer changes IFD0's directory length, so include a
    // placeholder before calculating the nested IFD's final TIFF-relative offset.
    ifd0_entries.push(ExifEntry {
        tag: 0x8769,
        value: ExifValue::Long(0),
    });
    ifd0_entries.sort_by_key(|entry| entry.tag);
    let ifd0_offset = 8u32;
    let exif_ifd_offset = ifd0_offset.saturating_add(encoded_ifd_block_len(&ifd0_entries));
    if let Some(pointer) = ifd0_entries.iter_mut().find(|entry| entry.tag == 0x8769) {
        pointer.value = ExifValue::Long(exif_ifd_offset);
    }

    let ifd0 = encode_ifd_block(&ifd0_entries, ifd0_offset);
    let exif_ifd = encode_ifd_block(&exif_entries, exif_ifd_offset);
    let mut output = Vec::with_capacity(8 + ifd0.len() + exif_ifd.len());
    output.extend_from_slice(b"II");
    output.extend_from_slice(&42u16.to_le_bytes());
    output.extend_from_slice(&ifd0_offset.to_le_bytes());
    output.extend_from_slice(&ifd0);
    output.extend_from_slice(&exif_ifd);
    output
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_tile_spec, build_exif_payload, build_lanczos_contributions, encode_srgb_row,
        encode_srgb_row_with_format, publish_completed_export, stitch_linear_tile_into_band,
        validate_export_dimensions, ExportMetadata, ExportResizeMode, ExportRowFormat,
        ExportSettings, LinearLightResizer, EXPORT_TILE_HALO, MAX_EXPORT_EDGE,
    };
    use crate::pipeline::ExportTile;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn resize_modes_preserve_aspect_ratio() {
        let base = ExportSettings::default();
        let cases = [
            (ExportResizeMode::LongEdge, 3000, (3000, 2000)),
            (ExportResizeMode::ShortEdge, 1000, (1500, 1000)),
            (ExportResizeMode::Width, 1200, (1200, 800)),
            (ExportResizeMode::Height, 800, (1200, 800)),
        ];
        for (resize_mode, edge_or_dimension, expected) in cases {
            let settings = ExportSettings {
                resize_mode,
                edge_or_dimension,
                ..base
            };
            assert_eq!(settings.output_dimensions(6000, 4000), expected);
        }
    }

    #[test]
    fn resizing_does_not_enlarge_by_default() {
        let settings = ExportSettings {
            resize_mode: ExportResizeMode::LongEdge,
            edge_or_dimension: 12000,
            ..ExportSettings::default()
        };
        assert_eq!(settings.output_dimensions(6000, 4000), (6000, 4000));
    }

    #[test]
    fn exif_payload_contains_source_camera_lens_and_exposure_metadata() {
        let metadata = ExportMetadata {
            source_file_name: Some("IMG_0042.CR3".to_owned()),
            camera_make: "CameraCo".to_owned(),
            camera_model: "Model X".to_owned(),
            lens_make: "LensCo".to_owned(),
            lens_model: "Prime 50".to_owned(),
            focal_length: 50.0,
            aperture: 2.8,
            iso_speed: 640.0,
            shutter_seconds: 1.0 / 125.0,
            description: "Studio portrait".to_owned(),
            artist: "Photographer".to_owned(),
            source_width: 6000,
            source_height: 4000,
            ..ExportMetadata::default()
        };
        let exif = build_exif_payload(&metadata, 3000, 2000);
        assert_eq!(&exif[..4], &[b'I', b'I', 42, 0]);
        for expected in [
            b"CameraCo\0".as_slice(),
            b"Model X\0".as_slice(),
            b"LensCo\0".as_slice(),
            b"Prime 50\0".as_slice(),
            b"IMG_0042.CR3\0".as_slice(),
            b"Studio portrait".as_slice(),
            b"Photographer\0".as_slice(),
        ] {
            assert!(exif.windows(expected.len()).any(|window| window == expected));
        }

        let read_u16 = |offset: usize| u16::from_le_bytes([exif[offset], exif[offset + 1]]);
        let read_u32 = |offset: usize| {
            u32::from_le_bytes([
                exif[offset],
                exif[offset + 1],
                exif[offset + 2],
                exif[offset + 3],
            ])
        };
        let ifd0_offset = read_u32(4) as usize;
        let ifd0_count = read_u16(ifd0_offset) as usize;
        let mut exif_ifd_offset = None;
        let mut ifd0_tags = Vec::new();
        for index in 0..ifd0_count {
            let entry = ifd0_offset + 2 + index * 12;
            let tag = read_u16(entry);
            ifd0_tags.push(tag);
            if tag == 0x8769 {
                exif_ifd_offset = Some(read_u32(entry + 8) as usize);
            }
        }
        assert!(ifd0_tags.contains(&0x010e));
        assert!(ifd0_tags.contains(&0x010f));
        assert!(ifd0_tags.contains(&0x0110));
        assert!(ifd0_tags.contains(&0x013b));

        let exif_ifd_offset = exif_ifd_offset.expect("ExifIFD pointer");
        let exif_count = read_u16(exif_ifd_offset) as usize;
        let exif_tags = (0..exif_count)
            .map(|index| read_u16(exif_ifd_offset + 2 + index * 12))
            .collect::<Vec<_>>();
        for tag in [
            0x829a, 0x829d, 0x8827, 0x920a, 0x9286, 0xa002, 0xa003, 0xa433, 0xa434,
        ] {
            assert!(exif_tags.contains(&tag), "missing EXIF tag {tag:#06x}");
        }
    }

    #[test]
    fn jpeg_rows_omit_png_alpha_bytes() {
        let transform = crate::pipeline::IccOutputTransform::srgb();
        let rgba = encode_srgb_row(&[0.18, 0.18, 0.18], &transform).unwrap();
        let rgb = encode_srgb_row_with_format(
            &[0.18, 0.18, 0.18],
            &transform,
            ExportRowFormat::Rgb8,
        )
        .unwrap();
        assert_eq!(rgba.len(), 4);
        assert_eq!(rgb.len(), 3);
        assert_eq!(&rgba[..3], &rgb);
    }

    #[test]
    fn tile_rows_land_at_their_band_offset() {
        let mut band = vec![0.0f32; 4 * 3];
        let tile = ExportTile {
            core_x: 1,
            core_y: 2,
            core_width: 2,
            core_height: 1,
            local_core_x: 48,
            local_core_y: 48,
            padded_width: 100,
            padded_height: 100,
            global_origin_x: -47,
            global_origin_y: -46,
        };
        stitch_linear_tile_into_band(&mut band, 4, 2, tile, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0])
            .unwrap();
        assert_eq!(&band[3..9], &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn resize_kernels_are_bounded_and_normalized() {
        let kernels = build_lanczos_contributions(32_768, 1).unwrap();
        assert_eq!(kernels.len(), 1);
        assert!(kernels[0].len() <= 32_768);
        let sum: f32 = kernels[0].iter().map(|sample| sample.weight).sum();
        assert!((sum - 1.0).abs() < 1e-5);
    }

    #[test]
    fn identity_resize_uses_one_exact_sample_per_pixel() {
        let kernels = build_lanczos_contributions(8, 8).unwrap();
        for (index, kernel) in kernels.iter().enumerate() {
            assert_eq!(kernel.len(), 1);
            assert_eq!(kernel[0].index, index as u32);
            assert_eq!(kernel[0].weight, 1.0);
        }
    }

    #[test]
    fn export_dimension_limits_reject_oversized_images() {
        assert!(validate_export_dimensions(MAX_EXPORT_EDGE + 1, 1).is_err());
        assert!(validate_export_dimensions(MAX_EXPORT_EDGE, MAX_EXPORT_EDGE).is_err());
    }

    #[test]
    fn wide_sources_reduce_band_height_to_stay_within_budget() {
        let requested = crate::pipeline::TileSpec {
            core_edge: if cfg!(target_os = "android") {
                768
            } else {
                1024
            },
            halo: EXPORT_TILE_HALO,
        };
        let bounded = bounded_tile_spec(requested, MAX_EXPORT_EDGE).unwrap();
        assert!(bounded.core_edge <= requested.core_edge);
        assert!(bounded.core_edge >= 64);
    }

    #[test]
    fn vertical_resize_streams_extreme_upscales_without_retaining_rows() {
        let transform = crate::pipeline::IccOutputTransform::srgb();
        let mut output = Vec::new();
        let mut resizer = LinearLightResizer::new(1, 1, 1, 128).unwrap();
        resizer
            .push_source_row(0, &[0.18, 0.18, 0.18], &transform, &mut output)
            .unwrap();
        assert_eq!(output.len(), 128 * 4);
        assert!(resizer.pending_rows.iter().all(Option::is_none));
        resizer.finish(&transform, &mut output).unwrap();
    }

    #[test]
    fn vertical_resize_streams_extreme_downscales_with_one_active_row() {
        let transform = crate::pipeline::IccOutputTransform::srgb();
        let mut output = Vec::new();
        let mut resizer = LinearLightResizer::new(1, 128, 1, 1).unwrap();
        for source_y in 0..128 {
            resizer
                .push_source_row(source_y, &[0.18, 0.18, 0.18], &transform, &mut output)
                .unwrap();
            assert!(
                resizer
                    .pending_rows
                    .iter()
                    .filter(|row| row.is_some())
                    .count()
                    <= 1
            );
        }
        resizer.finish(&transform, &mut output).unwrap();
        assert_eq!(output.len(), 4);
    }

    #[test]
    fn srgb_encoding_outputs_opaque_rgba_and_rejects_non_finite_values() {
        let transform = crate::pipeline::IccOutputTransform::srgb();
        let encoded = encode_srgb_row(&[0.0, 0.18, 1.0], &transform).unwrap();
        assert_eq!(encoded.len(), 4);
        assert_eq!(encoded[3], 255);
        assert!(encode_srgb_row(&[f32::NAN, 0.0, 0.0], &transform).is_err());
    }

    #[test]
    fn failed_export_publish_preserves_existing_destination() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "auraw-export-publish-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let destination = directory.join("photo.png");
        let missing_temporary = directory.join("missing.part");
        std::fs::write(&destination, b"previous export").unwrap();

        assert!(publish_completed_export(&missing_temporary, &destination).is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"previous export");

        std::fs::remove_dir_all(directory).unwrap();
    }
}
