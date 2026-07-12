use super::{
    extract_padded_tile, ExposureParams, GpuParams, LoadedRaw, ProcessingQuality,
    ProcessingStage, RawGpuPipeline, TilePlan, TileSpec,
};
use anyhow::{Context, Result};
use eframe::wgpu;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

#[derive(Debug)]
pub enum ExportEvent {
    Progress {
        completed_tiles: usize,
        total_tiles: usize,
    },
    Finished(Result<PathBuf, String>),
}

/// Runs full-resolution export on a worker thread. The worker first computes
/// global tone statistics from the cached preview RAW, then processes the full
/// sensor image as halo-padded high-quality tiles and streams completed rows to
/// the PNG encoder. No full-resolution rendered RGBA image is retained.
pub fn spawn_tiled_png_export(
    device: wgpu::Device,
    queue: wgpu::Queue,
    raw: Arc<LoadedRaw>,
    preview_raw: Arc<LoadedRaw>,
    exposure: ExposureParams,
    path: PathBuf,
    tile_spec: TileSpec,
) -> mpsc::Receiver<ExportEvent> {
    let (sender, receiver) = mpsc::channel();
    let worker_sender = sender.clone();
    let worker_path = path.clone();

    let spawn_result = std::thread::Builder::new()
        .name("auraw-tiled-export".to_owned())
        .spawn(move || {
            let result = export_tiled_png(
                &device,
                &queue,
                &raw,
                &preview_raw,
                &exposure,
                &worker_path,
                tile_spec,
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
    events: &mpsc::Sender<ExportEvent>,
) -> Result<()> {
    let global_params = GpuParams::new(exposure, preview_raw);
    // Export must not derive its global curve from a half-float analysis
    // pipeline. The source is only the bounded preview proxy, so RGBA32F has
    // modest memory cost while keeping desktop preview and export statistics
    // consistent.
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
    let mut encoder = png::Encoder::new(BufWriter::new(file), raw.width, raw.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .with_context(|| format!("write PNG header for {}", path.display()))?;
    let mut stream = writer
        .stream_writer_with_size(64 * 1024)
        .context("create streaming PNG writer")?;

    let total_tiles = plan.tile_count();
    let mut completed_tiles = 0usize;
    let mut tile_index = 0usize;

    // TilePlan is row-major. Holding one core-height band at a time allows PNG
    // scanlines to remain ordered while bounding CPU render memory to roughly
    // full_width * tile_height * 4 bytes.
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
    use super::stitch_tile_into_band;
    use crate::pipeline::ExportTile;

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
