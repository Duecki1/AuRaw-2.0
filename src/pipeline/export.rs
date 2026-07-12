use super::{
    extract_padded_tile, resample_raw, ExposureParams, GpuParams, LoadedRaw, ProcessingQuality,
    ProcessingStage, RawGpuPipeline, TilePlan, TileSpec,
};
use anyhow::{Context, Result};
use eframe::wgpu;
use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

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

/// Runs export on a worker thread. The worker computes global tone statistics
/// from the cached preview RAW, optionally resamples the sensor mosaic to the
/// requested aspect-preserving dimensions, then processes halo-padded
/// high-quality tiles and streams completed rows to the PNG encoder.
pub fn spawn_tiled_png_export(
    device: wgpu::Device,
    queue: wgpu::Queue,
    raw: Arc<LoadedRaw>,
    preview_raw: Arc<LoadedRaw>,
    exposure: ExposureParams,
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
            let (output_width, output_height) =
                settings.output_dimensions(raw.width, raw.height);
            let resized_raw;
            let export_raw = if output_width == raw.width && output_height == raw.height {
                raw.as_ref()
            } else {
                resized_raw = resample_raw(raw.as_ref(), output_width, output_height);
                &resized_raw
            };

            let result = export_tiled_png(
                &device,
                &queue,
                export_raw,
                &preview_raw,
                &exposure,
                &worker_path,
                tile_spec,
                settings.keep_metadata,
                &metadata,
                &worker_sender,
            );
            if result.is_err() {
                let _ = std::fs::remove_file(&worker_path);
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

fn export_tiled_png(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: &LoadedRaw,
    preview_raw: &LoadedRaw,
    exposure: &ExposureParams,
    path: &Path,
    tile_spec: TileSpec,
    keep_metadata: bool,
    metadata: &ExportMetadata,
    events: &mpsc::Sender<ExportEvent>,
) -> Result<()> {
    let global_params = GpuParams::new(exposure, preview_raw);
    let global_tone_source = RawGpuPipeline::new_headless_with_quality(
        device,
        queue,
        preview_raw,
        &global_params,
        ProcessingQuality::High,
    )
    .context("create global tone-analysis pipeline")?;
    global_tone_source.dispatch_stage(queue, device, &global_params, ProcessingStage::Raw);
    global_tone_source.dispatch_stage(queue, device, &global_params, ProcessingStage::Tone);

    let plan = TilePlan::new(raw.width, raw.height, tile_spec);
    let first = *plan
        .tiles
        .first()
        .context("cannot export an empty RAW image")?;
    let first_raw = extract_padded_tile(raw, first);
    let first_params = GpuParams::new_for_tile(
        exposure,
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

    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut info = png::Info::with_size(raw.width, raw.height);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Eight;
    if keep_metadata {
        info.exif_metadata = Some(Cow::Owned(build_exif_payload(metadata, raw.width, raw.height)));
    }
    let mut encoder = png::Encoder::with_info(BufWriter::new(file), info)
        .context("configure PNG encoder")?;
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    if keep_metadata {
        add_png_text_metadata(&mut encoder, metadata, raw.width, raw.height)?;
    }
    let mut writer = encoder
        .write_header()
        .with_context(|| format!("write PNG header for {}", path.display()))?;
    let mut stream = writer
        .stream_writer_with_size(64 * 1024)
        .context("create streaming PNG writer")?;

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

        let mut band = vec![0u8; raw.width as usize * band_height as usize * 4];
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
                &tile_raw,
                tile.global_origin_x,
                tile.global_origin_y,
                raw.width,
                raw.height,
            );
            tile_pipeline.dispatch_export_tile(queue, device, &params, &global_tone_source);
            let rgba = tile_pipeline
                .read_output_region_blocking(
                    device,
                    queue,
                    tile.local_core_x,
                    tile.local_core_y,
                    tile.core_width,
                    tile.core_height,
                )
                .with_context(|| format!("read export tile {}", global_index + 1))?;

            stitch_tile_into_band(&mut band, raw.width, band_y, tile, &rgba);
            completed_tiles += 1;
            let _ = events.send(ExportEvent::Progress {
                completed_tiles,
                total_tiles,
            });
        }

        stream
            .write_all(&band)
            .with_context(|| format!("write PNG rows beginning at {band_y}"))?;
    }

    stream.finish().context("finish streaming PNG data")?;
    writer.finish().context("finish PNG file")?;
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
    if let Some(source) = metadata.source_file_name.as_deref().filter(|value| !value.is_empty()) {
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
fn build_exif_payload(
    metadata: &ExportMetadata,
    output_width: u32,
    output_height: u32,
) -> Vec<u8> {
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

fn stitch_tile_into_band(
    band: &mut [u8],
    output_width: u32,
    band_y: u32,
    tile: super::ExportTile,
    rgba: &[u8],
) {
    debug_assert_eq!(tile.core_y, band_y);
    let source_row_bytes = tile.core_width as usize * 4;
    for row in 0..tile.core_height as usize {
        let source_start = row * source_row_bytes;
        let destination_pixel = row * output_width as usize + tile.core_x as usize;
        let destination_start = destination_pixel * 4;
        band[destination_start..destination_start + source_row_bytes]
            .copy_from_slice(&rgba[source_start..source_start + source_row_bytes]);
    }
}

#[cfg(test)]
mod tests {
    use super::{build_exif_payload, stitch_tile_into_band, ExportMetadata, ExportResizeMode, ExportSettings};
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
        let mut band = vec![0u8; 4 * 4];
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
        stitch_tile_into_band(&mut band, 4, 2, tile, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&band[4..12], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
