use super::{
    extract_padded_tile, ExposureParams, GpuParams, IccOutputTransform, LoadedRaw, MaskStack,
    ProcessingQuality, RawGpuPipeline, TilePlan, TileSpec, EXPORT_TILE_HALO, MAX_LOCAL_MASKS,
};
use anyhow::{Context, Result};
use eframe::wgpu;
use std::borrow::Cow;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            resize_mode: ExportResizeMode::Original,
            edge_or_dimension: 3000,
            percentage: 100.0,
            allow_upscale: false,
            keep_metadata: true,
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
    pub source_width: u32,
    pub source_height: u32,
}

impl ExportMetadata {
    pub fn from_raw(raw: &LoadedRaw, source_file_name: Option<String>) -> Self {
        Self {
            source_file_name,
            camera_make: raw.camera_make.clone(),
            camera_model: raw.camera_model.clone(),
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
pub fn spawn_tiled_png_export(
    device: wgpu::Device,
    queue: wgpu::Queue,
    raw: Arc<LoadedRaw>,
    exposure: ExposureParams,
    masks: MaskStack,
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
            let result = (|| -> Result<()> {
                let (output_width, output_height) =
                    settings.checked_output_dimensions(raw.width, raw.height)?;
                let tile_spec = bounded_tile_spec(tile_spec, raw.width)?;
                let temporary = temporary_export_path(&worker_path)?;
                let export_result = export_tiled_png(
                    &device,
                    &queue,
                    &raw,
                    &exposure,
                    &masks,
                    &temporary,
                    tile_spec,
                    output_width,
                    output_height,
                    settings.keep_metadata,
                    &metadata,
                    &worker_sender,
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

#[allow(clippy::too_many_arguments)]
fn export_tiled_png(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    masks: &MaskStack,
    path: &Path,
    tile_spec: TileSpec,
    output_width: u32,
    output_height: u32,
    keep_metadata: bool,
    metadata: &ExportMetadata,
    events: &mpsc::Sender<ExportEvent>,
) -> Result<()> {
    validate_export_dimensions(output_width, output_height)?;
    let plan = TilePlan::new(raw.width, raw.height, tile_spec);
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
    let tile_pipeline = RawGpuPipeline::new_headless_with_quality(
        device,
        queue,
        &first_raw,
        &first_params,
        ProcessingQuality::High,
    )
    .context("create reusable full-quality export pipeline")?;
    upload_mask_atlas(&tile_pipeline, queue, masks, raw.width, raw.height)?;

    // Establish one histogram from every full-resolution source pixel before
    // rendering output tiles. Reusing the tile pipeline keeps peak memory
    // bounded; restricting each dispatch to its core avoids counting halos.
    tile_pipeline.begin_export_tone_analysis(queue, device);
    for (index, tile) in plan.tiles.iter().copied().enumerate() {
        let tile_raw = if index == 0 {
            first_raw.clone()
        } else {
            extract_padded_tile(raw, tile)
        };
        tile_pipeline
            .upload_raw_tile(queue, &tile_raw)
            .with_context(|| format!("upload tone-analysis tile {}", index + 1))?;
        let params = GpuParams::new_for_tile(
            exposure,
            masks,
            &tile_raw,
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
        tile_pipeline.accumulate_export_tone_tile(queue, device, &params);
    }
    tile_pipeline.finish_export_tone_analysis(queue, device);

    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("create temporary export {}", path.display()))?;
    let mut info = png::Info::with_size(output_width, output_height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Eight;
    if keep_metadata {
        info.exif_metadata = Some(Cow::Owned(build_exif_payload(
            metadata,
            output_width,
            output_height,
        )));
    }
    let mut encoder =
        png::Encoder::with_info(BufWriter::new(file), info).context("configure PNG encoder")?;
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    if keep_metadata {
        add_png_text_metadata(&mut encoder, metadata, output_width, output_height)?;
    }
    let mut writer = encoder
        .write_header()
        .with_context(|| format!("write PNG header for {}", path.display()))?;
    let mut stream = writer
        .stream_writer_with_size(64 * 1024)
        .context("create streaming PNG writer")?;
    let output_transform = IccOutputTransform::srgb();
    let mut resizer = LinearLightResizer::new(raw.width, raw.height, output_width, output_height)?;

    let total_tiles = plan.tile_count();
    let mut completed_tiles = 0usize;
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
        for (absolute_index, tile) in plan.tiles[band_start..tile_index]
            .iter()
            .copied()
            .enumerate()
        {
            let global_index = band_start + absolute_index;
            let tile_raw = if global_index == 0 {
                first_raw.clone()
            } else {
                extract_padded_tile(raw, tile)
            };
            tile_pipeline
                .upload_raw_tile(queue, &tile_raw)
                .with_context(|| format!("upload export tile {}", global_index + 1))?;

            let params = GpuParams::new_for_tile(
                exposure,
                masks,
                &tile_raw,
                tile.global_origin_x,
                tile.global_origin_y,
                raw.width,
                raw.height,
            );
            tile_pipeline.dispatch_export_tile(queue, device, &params);
            let rgb = tile_pipeline
                .read_display_linear_region_blocking(
                    device,
                    queue,
                    tile.local_core_x,
                    tile.local_core_y,
                    tile.core_width,
                    tile.core_height,
                )
                .with_context(|| format!("read export tile {}", global_index + 1))?;

            stitch_linear_tile_into_band(&mut band, raw.width, band_y, tile, &rgb)?;
            completed_tiles += 1;
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
            resizer.push_source_row(
                band_y + local_y,
                &band[start..end],
                &output_transform,
                &mut stream,
            )?;
        }
    }

    resizer.finish(&output_transform, &mut stream)?;
    stream.finish().context("finish streaming PNG data")?;
    writer.finish().context("finish PNG file")?;
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
}

impl LinearLightResizer {
    fn new(
        source_width: u32,
        source_height: u32,
        output_width: u32,
        output_height: u32,
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
                let row = slot.as_mut().expect("pending output row was initialized");
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
            let encoded = encode_srgb_row(&row, output_transform)?;
            output
                .write_all(&encoded)
                .with_context(|| format!("write output PNG row {}", self.next_output_row))?;
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
    anyhow::ensure!(row.len() % 3 == 0, "linear RGB row has an invalid length");
    let pixels = row.len() / 3;
    let bytes = pixels.checked_mul(4).context("encoded row overflow")?;
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
        encoded.push(255);
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
    let maximum_core = if cfg!(target_os = "android") {
        768
    } else {
        1024
    };
    let scale = if cfg!(target_os = "android") { 8 } else { 4 };
    anyhow::ensure!(
        (64..=maximum_core).contains(&spec.core_edge),
        "export tile core must be between 64 and {maximum_core} pixels"
    );
    anyhow::ensure!(
        (EXPORT_TILE_HALO..=512).contains(&spec.halo),
        "export halo must be between {EXPORT_TILE_HALO} and 512 pixels"
    );
    anyhow::ensure!(
        spec.core_edge % scale == 0 && spec.halo % scale == 0,
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

fn temporary_export_path(destination: &Path) -> Result<PathBuf> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .with_context(|| format!("create export directory {}", parent.display()))?;
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .context("export path has no valid file name")?;
    cleanup_stale_export_parts(parent, name);
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
    #[cfg(windows)]
    if destination.exists() {
        fs::remove_file(destination)
            .with_context(|| format!("replace existing export {}", destination.display()))?;
    }
    fs::rename(temporary, destination).with_context(|| {
        format!(
            "publish completed export {} to {}",
            temporary.display(),
            destination.display()
        )
    })
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
        let bytes = masks.rasterize_layer(layer, edge, edge, image_width, image_height);
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
        .add_itxt_chunk("Software".to_owned(), "AuRaw".to_owned())
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
    let camera = format!("{} {}", metadata.camera_make, metadata.camera_model)
        .trim()
        .to_owned();
    if !camera.is_empty() {
        encoder
            .add_itxt_chunk("Camera".to_owned(), camera)
            .context("write PNG camera metadata")?;
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
    Ok(())
}

/// Builds a compact TIFF/EXIF IFD for PNG's eXIf chunk. The output image has
/// already been physically oriented, so Orientation is always written as 1.
fn build_exif_payload(metadata: &ExportMetadata, output_width: u32, output_height: u32) -> Vec<u8> {
    #[derive(Clone)]
    enum Value {
        Short(u16),
        Long(u32),
        Ascii(Vec<u8>),
    }
    #[derive(Clone)]
    struct Entry {
        tag: u16,
        value: Value,
    }

    let mut entries = vec![
        Entry {
            tag: 0x0100,
            value: Value::Long(output_width),
        },
        Entry {
            tag: 0x0101,
            value: Value::Long(output_height),
        },
        Entry {
            tag: 0x0112,
            value: Value::Short(1),
        },
        Entry {
            tag: 0x0131,
            value: Value::Ascii(b"AuRaw\0".to_vec()),
        },
    ];
    if !metadata.camera_make.is_empty() {
        let mut value = metadata.camera_make.as_bytes().to_vec();
        value.push(0);
        entries.push(Entry {
            tag: 0x010f,
            value: Value::Ascii(value),
        });
    }
    if !metadata.camera_model.is_empty() {
        let mut value = metadata.camera_model.as_bytes().to_vec();
        value.push(0);
        entries.push(Entry {
            tag: 0x0110,
            value: Value::Ascii(value),
        });
    }
    entries.sort_by_key(|entry| entry.tag);

    let ifd_offset = 8u32;
    let data_offset = ifd_offset + 2 + entries.len() as u32 * 12 + 4;
    let mut data = Vec::<u8>::new();
    let mut output = Vec::with_capacity(data_offset as usize + 128);
    output.extend_from_slice(b"II");
    output.extend_from_slice(&42u16.to_le_bytes());
    output.extend_from_slice(&ifd_offset.to_le_bytes());
    output.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    for entry in entries {
        output.extend_from_slice(&entry.tag.to_le_bytes());
        match entry.value {
            Value::Short(value) => {
                output.extend_from_slice(&3u16.to_le_bytes());
                output.extend_from_slice(&1u32.to_le_bytes());
                output.extend_from_slice(&value.to_le_bytes());
                output.extend_from_slice(&0u16.to_le_bytes());
            }
            Value::Long(value) => {
                output.extend_from_slice(&4u16.to_le_bytes());
                output.extend_from_slice(&1u32.to_le_bytes());
                output.extend_from_slice(&value.to_le_bytes());
            }
            Value::Ascii(value) => {
                output.extend_from_slice(&2u16.to_le_bytes());
                output.extend_from_slice(&(value.len() as u32).to_le_bytes());
                if value.len() <= 4 {
                    output.extend_from_slice(&value);
                    output.resize(output.len() + 4 - value.len(), 0);
                } else {
                    let offset = data_offset + data.len() as u32;
                    output.extend_from_slice(&offset.to_le_bytes());
                    data.extend_from_slice(&value);
                }
            }
        }
    }
    output.extend_from_slice(&0u32.to_le_bytes());
    output.extend_from_slice(&data);
    output
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_tile_spec, build_exif_payload, build_lanczos_contributions, encode_srgb_row,
        stitch_linear_tile_into_band, validate_export_dimensions, ExportMetadata, ExportResizeMode,
        ExportSettings, LinearLightResizer, EXPORT_TILE_HALO, MAX_EXPORT_EDGE,
    };
    use crate::pipeline::ExportTile;

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
    fn exif_payload_is_a_little_endian_tiff() {
        let metadata = ExportMetadata {
            camera_make: "CameraCo".to_owned(),
            camera_model: "Model X".to_owned(),
            source_width: 6000,
            source_height: 4000,
            ..ExportMetadata::default()
        };
        let exif = build_exif_payload(&metadata, 3000, 2000);
        assert_eq!(&exif[..4], &[b'I', b'I', 42, 0]);
        assert!(exif.windows(9).any(|window| window == b"CameraCo\0"));
        assert!(exif.windows(8).any(|window| window == b"Model X\0"));
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
}
