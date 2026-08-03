use super::{CompactPixelMap, DenoiseQuality, ExposureParams, LoadedRaw, MaskStack};
use rayon::prelude::*;

/// Earliest pipeline stage that must be executed after a parameter change.
/// Stages are ordered from most expensive/upstream to cheapest/downstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProcessingStage {
    Raw,
    Tone,
    Output,
}

impl ProcessingStage {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Raw => "RAW reconstruction",
            Self::Tone => "tone analysis",
            Self::Output => "display rendering",
        }
    }
}

/// Returns the earliest affected stage. RAW-space controls invalidate the
/// cached demosaic result and every downstream stage. Global white balance
/// invalidates tone analysis because it changes scene-working luminance;
/// ordinary Develop controls only invalidate the final render.
pub fn affected_stage(before: &ExposureParams, after: &ExposureParams) -> Option<ProcessingStage> {
    if before == after {
        return None;
    }

    if raw_controls_changed(before, after) {
        Some(ProcessingStage::Raw)
    } else if before.temperature != after.temperature || before.tint != after.tint {
        Some(ProcessingStage::Tone)
    } else {
        Some(ProcessingStage::Output)
    }
}

fn raw_controls_changed(before: &ExposureParams, after: &ExposureParams) -> bool {
    before.black_point != after.black_point
        || before.chroma_denoise != after.chroma_denoise
        || before.luminance_denoise != after.luminance_denoise
        || before.denoise_detail != after.denoise_detail
        || before.denoise_quality != after.denoise_quality
        || before.ai_denoise_enabled != after.ai_denoise_enabled
        || before.demosaic_mode != after.demosaic_mode
        || before.dual_threshold != after.dual_threshold
        || before.frequency_chroma != after.frequency_chroma
        || before.ca_red != after.ca_red
        || before.ca_blue != after.ca_blue
        || before.highlight_method != after.highlight_method
        || before.highlight_clip != after.highlight_clip
        || before.highlight_reconstruction != after.highlight_reconstruction
}

#[derive(Clone, Copy, Debug)]
pub struct ProxySpec {
    pub max_edge: u32,
}

impl Default for ProxySpec {
    fn default() -> Self {
        Self {
            max_edge: if cfg!(target_os = "android") {
                1280
            } else {
                2048
            },
        }
    }
}

/// Copies a rectangular RAW region while retaining camera metadata and the
/// explicit CFA/black-level maps. Coordinates are clamped to the source image.
pub fn crop_raw(raw: &LoadedRaw, x: u32, y: u32, width: u32, height: u32) -> LoadedRaw {
    let x = x.min(raw.width.saturating_sub(1));
    let y = y.min(raw.height.saturating_sub(1));
    let width = width.max(1).min(raw.width - x);
    let height = height.max(1).min(raw.height - y);
    let mut raw_pixels = Vec::with_capacity((width * height) as usize);
    for row in y..y + height {
        let start = (row * raw.width + x) as usize;
        let end = start + width as usize;
        raw_pixels.extend_from_slice(&raw.raw_pixels[start..end]);
    }
    let color_indices =
        raw.color_indices
            .subregion_clamped(i64::from(x), i64::from(y), width, height);
    let black_levels_per_pixel =
        raw.black_levels_per_pixel
            .subregion_clamped(i64::from(x), i64::from(y), width, height);

    LoadedRaw {
        width,
        height,
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
        color_indices,
        wb_coeffs: raw.wb_coeffs,
        cam_to_srgb: raw.cam_to_srgb,
        black_levels: raw.black_levels,
        black_levels_per_pixel,
        white_levels: raw.white_levels,
        noise_profile: raw.noise_profile,
        camera_profile: raw.camera_profile.clone(),
        camera_profile_source: raw.camera_profile_source.clone(),
        available_camera_profiles: raw.available_camera_profiles.clone(),
        white_balance_model: raw.white_balance_model.clone(),
        // A lens map is normalized to the full sensor raster. A spatial crop
        // needs an origin-aware view of that map, so do not attach the full-map
        // normalization to cropped scratch RAWs where it would be misleading.
        lens_geometry: (x == 0 && y == 0 && width == raw.width && height == raw.height)
            .then(|| raw.lens_geometry.clone())
            .flatten(),
        ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(crop_ai_denoised(
            raw, x, y, width, height,
        ))),
        opposed_chroma_cache: Default::default(),
    }
}

/// Builds a compact RAW proxy while preserving the explicit per-pixel CFA
/// map. Each proxy photosite averages only source samples from the same CFA
/// plane, preventing colour-plane cross-contamination before demosaic.
pub fn build_proxy(raw: &LoadedRaw, spec: ProxySpec) -> LoadedRaw {
    build_region_proxy(raw, 0, 0, raw.width, raw.height, spec)
}

/// Builds a proxy directly from one source region. Unlike `crop_raw` followed
/// by `build_proxy`, this visits and allocates only the final proxy data when
/// reduction is required, which is important for repeated zoom previews on
/// memory-constrained phones.
pub fn build_region_proxy(
    raw: &LoadedRaw,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    spec: ProxySpec,
) -> LoadedRaw {
    let x = x.min(raw.width.saturating_sub(1));
    let y = y.min(raw.height.saturating_sub(1));
    let region_width = width.max(1).min(raw.width - x);
    let region_height = height.max(1).min(raw.height - y);
    let max_edge = spec.max_edge.max(1);
    let longest = region_width.max(region_height);
    if longest <= max_edge {
        if x == 0 && y == 0 && region_width == raw.width && region_height == raw.height {
            return raw.clone();
        }
        return crop_raw(raw, x, y, region_width, region_height);
    }

    let cfa_period = match raw.cfa_kind {
        super::CfaKind::Bayer => 2,
        super::CfaKind::XTrans => 6,
    };
    let target_scale = max_edge as f64 / longest as f64;
    let target_width = (region_width as f64 * target_scale).floor().max(1.0) as u32;
    let target_height = (region_height as f64 * target_scale).floor().max(1.0) as u32;
    let phase_aligned_dimension = |source: u32, target: u32| {
        if target >= source {
            source
        } else {
            target
                .div_euclid(cfa_period)
                .max(1)
                .saturating_mul(cfa_period)
                .min(source)
        }
    };
    let width = phase_aligned_dimension(region_width, target_width);
    let height = phase_aligned_dimension(region_height, target_height);
    let len = (width * height) as usize;
    let row_stride = width as usize;
    let mut raw_pixels = vec![0u16; len];
    let mut color_indices = vec![0u8; len];
    let mut black_levels_per_pixel = vec![0.0f32; len];
    let proportional_partition = |output_index: u32, source_count: u32, output_count: u32| {
        let start =
            (u64::from(output_index) * u64::from(source_count) / u64::from(output_count)) as u32;
        let end = (u64::from(output_index + 1) * u64::from(source_count) / u64::from(output_count))
            as u32;
        (start, end.max(start + 1).min(source_count))
    };

    raw_pixels
        .par_chunks_mut(row_stride)
        .zip(color_indices.par_chunks_mut(row_stride))
        .zip(black_levels_per_pixel.par_chunks_mut(row_stride))
        .enumerate()
        .for_each(|(py, ((raw_row, cfa_row), black_row))| {
            let py = py as u32;
            let output_phase_y = py % cfa_period;
            let (source_y0, source_y1) = proportional_partition(py, region_height, height);
            let footprint_y0 = y + source_y0;
            let footprint_y1 = (y + source_y1).min(y + region_height);

            for px in 0..width {
                let output_phase_x = px % cfa_period;
                let (source_x0, source_x1) = proportional_partition(px, region_width, width);
                let footprint_x0 = x + source_x0;
                let footprint_x1 = (x + source_x1).min(x + region_width);
                let center_x = (footprint_x0 + (footprint_x1.saturating_sub(footprint_x0)) / 2)
                    .min(raw.width - 1);
                let center_y = (footprint_y0 + (footprint_y1.saturating_sub(footprint_y0)) / 2)
                    .min(raw.height - 1);

                let phase_x = (x + output_phase_x).min(raw.width - 1);
                let phase_y = (y + output_phase_y).min(raw.height - 1);
                let phase_index = (phase_y * raw.width + phase_x) as usize;
                let cfa = raw.color_indices[phase_index];

                let mut pixel_sum = 0u64;
                let mut black_sum = 0.0f64;
                let mut count = 0u32;

                for sy in footprint_y0..footprint_y1 {
                    let row = sy * raw.width;
                    for sx in footprint_x0..footprint_x1 {
                        let index = (row + sx) as usize;
                        if raw.color_indices[index] == cfa {
                            pixel_sum += u64::from(raw.raw_pixels[index]);
                            black_sum += f64::from(raw.black_levels_per_pixel[index]);
                            count += 1;
                        }
                    }
                }

                let px = px as usize;
                if count == 0 {
                    let fallback = nearest_cfa_sample(raw, center_x, center_y, cfa, cfa_period);
                    raw_row[px] = raw.raw_pixels[fallback];
                    black_row[px] = raw.black_levels_per_pixel[fallback];
                } else {
                    raw_row[px] = (pixel_sum / u64::from(count)) as u16;
                    black_row[px] = (black_sum / f64::from(count)) as f32;
                }
                cfa_row[px] = cfa;
            }
        });

    LoadedRaw {
        width,
        height,
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
        color_indices: CompactPixelMap::compact_from_dense(width, height, color_indices, 64),
        wb_coeffs: raw.wb_coeffs,
        cam_to_srgb: raw.cam_to_srgb,
        black_levels: raw.black_levels,
        black_levels_per_pixel: CompactPixelMap::compact_from_dense(
            width,
            height,
            black_levels_per_pixel,
            64,
        ),
        white_levels: raw.white_levels,
        noise_profile: raw.noise_profile.scaled_variance(
            ((u64::from(width) * u64::from(height)) as f64
                / (u64::from(region_width) * u64::from(region_height)) as f64)
                .clamp(0.0, 1.0) as f32,
        ),
        camera_profile: raw.camera_profile.clone(),
        camera_profile_source: raw.camera_profile_source.clone(),
        available_camera_profiles: raw.available_camera_profiles.clone(),
        white_balance_model: raw.white_balance_model.clone(),
        lens_geometry: (x == 0
            && y == 0
            && region_width == raw.width
            && region_height == raw.height)
            .then(|| raw.lens_geometry.clone())
            .flatten(),
        ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(proxy_ai_denoised(
            raw,
            x,
            y,
            region_width,
            region_height,
            width,
            height,
        ))),
        opposed_chroma_cache: Default::default(),
    }
}

fn nearest_cfa_sample(
    raw: &LoadedRaw,
    center_x: u32,
    center_y: u32,
    cfa: u8,
    search_radius: u32,
) -> usize {
    let max_x = raw.width.saturating_sub(1) as i64;
    let max_y = raw.height.saturating_sub(1) as i64;
    for radius in 0..=search_radius.max(1) {
        let r = i64::from(radius);
        for dy in -r..=r {
            for dx in -r..=r {
                if radius > 0 && dx.abs() != r && dy.abs() != r {
                    continue;
                }
                let x = (i64::from(center_x) + dx).clamp(0, max_x) as u32;
                let y = (i64::from(center_y) + dy).clamp(0, max_y) as u32;
                let index = (y * raw.width + x) as usize;
                if raw.color_indices[index] == cfa {
                    return index;
                }
            }
        }
    }

    (center_y * raw.width + center_x) as usize
}

fn crop_ai_denoised(
    raw: &LoadedRaw,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
) -> Option<super::AiDenoisedImage> {
    let source = raw.ai_denoised_image()?;
    if let Some(source_cfa) = source.bayer_cfa() {
        let elements = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|count| usize::try_from(count).ok())?;
        let mut raw_cfa16 = Vec::new();
        raw_cfa16.try_reserve_exact(elements).ok()?;
        for row in y..y + height {
            let start = (row * raw.width + x) as usize;
            let end = start + width as usize;
            raw_cfa16.extend_from_slice(&source_cfa[start..end]);
        }
        return super::AiDenoisedImage::new_bayer_cfa(width, height, raw_cfa16).ok();
    }
    let source_rgb = source.camera_rgb16f()?;
    let elements = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(3)
        .and_then(|count| usize::try_from(count).ok())?;
    let mut rgb16f = Vec::new();
    rgb16f.try_reserve_exact(elements).ok()?;
    for row in y..y + height {
        let start = ((row * raw.width + x) * 3) as usize;
        let end = start + width as usize * 3;
        rgb16f.extend_from_slice(&source_rgb[start..end]);
    }
    super::AiDenoisedImage::new(width, height, rgb16f).ok()
}

fn proxy_ai_denoised(
    raw: &LoadedRaw,
    x: u32,
    y: u32,
    region_width: u32,
    region_height: u32,
    output_width: u32,
    output_height: u32,
) -> Option<super::AiDenoisedImage> {
    let source = raw.ai_denoised_image()?;
    if let Some(source_cfa) = source.bayer_cfa() {
        let elements = u64::from(output_width)
            .checked_mul(u64::from(output_height))
            .and_then(|count| usize::try_from(count).ok())?;
        let mut raw_cfa16 = vec![0u16; elements];
        let partition = |output_index: u32, source_count: u32, output_count: u32| {
            let start = (u64::from(output_index) * u64::from(source_count)
                / u64::from(output_count)) as u32;
            let end = (u64::from(output_index + 1) * u64::from(source_count)
                / u64::from(output_count)) as u32;
            (start, end.max(start + 1).min(source_count))
        };
        raw_cfa16
            .par_chunks_mut(output_width as usize)
            .enumerate()
            .for_each(|(output_y, row)| {
                let output_y = output_y as u32;
                let phase_y = output_y % 2;
                let (source_y0, source_y1) = partition(output_y, region_height, output_height);
                let footprint_y0 = y + source_y0;
                let footprint_y1 = (y + source_y1).min(y + region_height);
                for output_x in 0..output_width {
                    let phase_x = output_x % 2;
                    let (source_x0, source_x1) = partition(output_x, region_width, output_width);
                    let footprint_x0 = x + source_x0;
                    let footprint_x1 = (x + source_x1).min(x + region_width);
                    let phase_index = (((y + phase_y).min(raw.height - 1) * raw.width)
                        + (x + phase_x).min(raw.width - 1))
                        as usize;
                    let cfa = raw.color_indices[phase_index];
                    let mut sum = 0u64;
                    let mut count = 0u64;
                    for source_y in footprint_y0..footprint_y1 {
                        for source_x in footprint_x0..footprint_x1 {
                            let source_index = (source_y * raw.width + source_x) as usize;
                            if raw.color_indices[source_index] == cfa {
                                sum += u64::from(source_cfa[source_index]);
                                count += 1;
                            }
                        }
                    }
                    row[output_x as usize] = if count > 0 {
                        (sum / count) as u16
                    } else {
                        let center_x = (footprint_x0
                            + footprint_x1.saturating_sub(footprint_x0) / 2)
                            .min(raw.width - 1);
                        let center_y = (footprint_y0
                            + footprint_y1.saturating_sub(footprint_y0) / 2)
                            .min(raw.height - 1);
                        source_cfa[nearest_cfa_sample(raw, center_x, center_y, cfa, 2)]
                    };
                }
            });
        return super::AiDenoisedImage::new_bayer_cfa(output_width, output_height, raw_cfa16).ok();
    }
    let source_rgb = source.camera_rgb16f()?;
    let elements = u64::from(output_width)
        .checked_mul(u64::from(output_height))?
        .checked_mul(3)
        .and_then(|count| usize::try_from(count).ok())?;
    let mut rgb16f = vec![0u16; elements];
    use half::f16;
    rgb16f
        .par_chunks_mut(output_width as usize * 3)
        .enumerate()
        .for_each(|(output_y, row)| {
            let source_y0 = y
                + ((output_y as u64 * u64::from(region_height)) / u64::from(output_height)) as u32;
            let source_y1 = y
                + (((output_y as u64 + 1) * u64::from(region_height))
                    .div_ceil(u64::from(output_height))) as u32;
            let source_y1 = source_y1.min(y + region_height).max(source_y0 + 1);
            for output_x in 0..output_width {
                let source_x0 = x
                    + ((u64::from(output_x) * u64::from(region_width)) / u64::from(output_width))
                        as u32;
                let source_x1 = x
                    + ((u64::from(output_x + 1) * u64::from(region_width))
                        .div_ceil(u64::from(output_width))) as u32;
                let source_x1 = source_x1.min(x + region_width).max(source_x0 + 1);
                let mut sum = [0.0f64; 3];
                let mut count = 0u32;
                for source_y in source_y0..source_y1 {
                    for source_x in source_x0..source_x1 {
                        let index = ((source_y * raw.width + source_x) * 3) as usize;
                        for channel in 0..3 {
                            sum[channel] +=
                                f64::from(f16::from_bits(source_rgb[index + channel]).to_f32());
                        }
                        count += 1;
                    }
                }
                let destination = output_x as usize * 3;
                for channel in 0..3 {
                    row[destination + channel] =
                        f16::from_f32((sum[channel] / f64::from(count.max(1))) as f32).to_bits();
                }
            }
        });
    super::AiDenoisedImage::new(output_width, output_height, rgb16f).ok()
}

fn padded_tile_ai_denoised(raw: &LoadedRaw, tile: ExportTile) -> Option<super::AiDenoisedImage> {
    let source = raw.ai_denoised_image()?;
    if let Some(source_cfa) = source.bayer_cfa() {
        let elements = u64::from(tile.padded_width)
            .checked_mul(u64::from(tile.padded_height))
            .and_then(|count| usize::try_from(count).ok())?;
        let mut raw_cfa16 = vec![0u16; elements];
        let max_x = i64::from(raw.width.saturating_sub(1));
        let max_y = i64::from(raw.height.saturating_sub(1));
        for local_y in 0..tile.padded_height {
            let source_y =
                (i64::from(tile.global_origin_y) + i64::from(local_y)).clamp(0, max_y) as u32;
            for local_x in 0..tile.padded_width {
                let source_x =
                    (i64::from(tile.global_origin_x) + i64::from(local_x)).clamp(0, max_x) as u32;
                raw_cfa16[(local_y * tile.padded_width + local_x) as usize] =
                    source_cfa[(source_y * raw.width + source_x) as usize];
            }
        }
        return super::AiDenoisedImage::new_bayer_cfa(
            tile.padded_width,
            tile.padded_height,
            raw_cfa16,
        )
        .ok();
    }
    let source_rgb = source.camera_rgb16f()?;
    let elements = u64::from(tile.padded_width)
        .checked_mul(u64::from(tile.padded_height))?
        .checked_mul(3)
        .and_then(|count| usize::try_from(count).ok())?;
    let mut rgb16f = vec![0u16; elements];
    let max_x = i64::from(raw.width.saturating_sub(1));
    let max_y = i64::from(raw.height.saturating_sub(1));
    for local_y in 0..tile.padded_height {
        let source_y =
            (i64::from(tile.global_origin_y) + i64::from(local_y)).clamp(0, max_y) as u32;
        for local_x in 0..tile.padded_width {
            let source_x =
                (i64::from(tile.global_origin_x) + i64::from(local_x)).clamp(0, max_x) as u32;
            let source_index = ((source_y * raw.width + source_x) * 3) as usize;
            let destination_index = ((local_y * tile.padded_width + local_x) * 3) as usize;
            rgb16f[destination_index..destination_index + 3]
                .copy_from_slice(&source_rgb[source_index..source_index + 3]);
        }
    }
    super::AiDenoisedImage::new(tile.padded_width, tile.padded_height, rgb16f).ok()
}

/// Cumulative input support of every spatial stage used by an export tile.
/// These are deliberately separate constants: taking only the maximum stage
/// radius is incorrect when one spatial pass consumes another pass's output.
// Inpaint opposed reads only the local 3x3 CFA cube. Its full-image
// chrominance offsets are calculated before export tiling begins.
const HIGHLIGHT_RECONSTRUCTION_SUPPORT: u32 = 1;
// Conservative bound over the complete Bayer RCD and X-Trans Markesteijn-3
// pass chains, including their final detail-recovery neighbourhoods.
const DEMOSAIC_CHAIN_SUPPORT: u32 = 32;
// High-quality color denoise cascades dense B3 scales at radii 1/2/4/8 and
// compact binomial scales at 16/32. Because each pass consumes the previous
// scale, dependency support is 2*(1+2+4+8)+16+32 = 78 pixels.
const COLOR_DENOISE_SUPPORT_FAST: u32 = 2;
const COLOR_DENOISE_SUPPORT_BALANCED: u32 = 2 * (1 + 2 + 4 + 8);
const COLOR_DENOISE_SUPPORT_HIGH: u32 = COLOR_DENOISE_SUPPORT_BALANCED + 16 + 32;
// The edge-aware tone guide is blurred by five guide texels at desktop's 4x
// reduction and three texels at Android's 8x reduction. Bilinear lookup can
// reach one additional guide cell, so its raw-pixel support is 24/32.
const TONE_GUIDE_SUPPORT: u32 = if cfg!(target_os = "android") { 32 } else { 24 };
// Scale-aware Clarity has the widest presence footprint: the B3 kernel reaches
// +/-2 times a step capped at 14 pixels. Texture and Dehaze remain inside it.
const LOCAL_EFFECTS_SUPPORT: u32 = 28;
// Glow cascades five B3 diffusion stages. At the capped 3x reference scale the
// steps are 3+3+6+12+24, and each 5x5 stage reaches +/-2*step. Support therefore
// accumulates to 96 pixels from the extracted highlight source.
const GLOW_SUPPORT: u32 = 96;
const COLOR_MIXER_SUPPORT: u32 = 4;
const EXPORT_CUMULATIVE_SUPPORT: u32 = HIGHLIGHT_RECONSTRUCTION_SUPPORT
    + DEMOSAIC_CHAIN_SUPPORT
    + COLOR_DENOISE_SUPPORT_HIGH
    + TONE_GUIDE_SUPPORT
    + LOCAL_EFFECTS_SUPPORT
    + GLOW_SUPPORT
    + COLOR_MIXER_SUPPORT;

/// Rounded up to the 8-pixel guide/workgroup alignment.
pub const EXPORT_TILE_HALO: u32 = EXPORT_CUMULATIVE_SUPPORT.div_ceil(8) * 8;

/// Smallest safe export halo when optional spatial effects are neutral.
/// Highlight reconstruction, demosaic, and the tone guide remain active.
pub const MIN_EXPORT_TILE_HALO: u32 = (HIGHLIGHT_RECONSTRUCTION_SUPPORT
    + DEMOSAIC_CHAIN_SUPPORT
    + TONE_GUIDE_SUPPORT
    + COLOR_MIXER_SUPPORT)
    .div_ceil(8)
    * 8;

/// Returns the halo actually required by the current edit. Neutral Glow and
/// local spatial controls should not force every tile to process their full
/// support radius. This is especially important on Android, where a 280 px
/// halo around a 768 px core nearly triples the processed area.
pub fn required_export_tile_halo(exposure: &ExposureParams, masks: &MaskStack) -> u32 {
    let mut support = HIGHLIGHT_RECONSTRUCTION_SUPPORT
        + DEMOSAIC_CHAIN_SUPPORT
        + TONE_GUIDE_SUPPORT
        + COLOR_MIXER_SUPPORT;

    if exposure.chroma_denoise > 1e-6 {
        support += match exposure.denoise_quality {
            DenoiseQuality::Fast => COLOR_DENOISE_SUPPORT_FAST,
            DenoiseQuality::Balanced => COLOR_DENOISE_SUPPORT_BALANCED,
            DenoiseQuality::High => COLOR_DENOISE_SUPPORT_HIGH,
        };
    }

    let local_spatial_active = exposure.sharpen_amount.abs() > 1e-6
        || exposure.texture.abs() > 1e-6
        || exposure.clarity.abs() > 1e-6
        || exposure.dehaze.abs() > 1e-6
        || masks.masks.iter().any(|mask| {
            mask.enabled
                && (mask.adjustments.texture.abs() > 1e-6
                    || mask.adjustments.clarity.abs() > 1e-6
                    || mask.adjustments.dehaze.abs() > 1e-6)
        });
    if local_spatial_active {
        support += LOCAL_EFFECTS_SUPPORT;
    }

    if exposure.glow_amount.abs() > 1e-6 {
        support += GLOW_SUPPORT;
    }

    support.div_ceil(8) * 8
}

#[derive(Clone, Copy, Debug)]
pub struct TileSpec {
    pub core_edge: u32,
    pub halo: u32,
}

impl Default for TileSpec {
    fn default() -> Self {
        Self {
            core_edge: if cfg!(target_os = "android") {
                768
            } else {
                1024
            },
            // Must cover every spatial operation executed inside a tile.
            halo: EXPORT_TILE_HALO,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportTile {
    pub core_x: u32,
    pub core_y: u32,
    pub core_width: u32,
    pub core_height: u32,
    pub local_core_x: u32,
    pub local_core_y: u32,
    pub padded_width: u32,
    pub padded_height: u32,
    pub global_origin_x: i32,
    pub global_origin_y: i32,
}

#[derive(Clone, Debug)]
pub struct TilePlan {
    pub full_width: u32,
    pub full_height: u32,
    pub spec: TileSpec,
    pub tiles: Vec<ExportTile>,
}

impl TilePlan {
    pub fn new(full_width: u32, full_height: u32, spec: TileSpec) -> Self {
        let core_edge = spec.core_edge.max(64);
        let halo = spec.halo;
        let padded_width = core_edge.saturating_add(halo.saturating_mul(2));
        let padded_height = padded_width;
        let mut tiles = Vec::new();

        let mut y = 0;
        while y < full_height {
            let core_height = core_edge.min(full_height - y);
            let mut x = 0;
            while x < full_width {
                let core_width = core_edge.min(full_width - x);
                tiles.push(ExportTile {
                    core_x: x,
                    core_y: y,
                    core_width,
                    core_height,
                    local_core_x: halo,
                    local_core_y: halo,
                    padded_width,
                    padded_height,
                    global_origin_x: x as i32 - halo as i32,
                    global_origin_y: y as i32 - halo as i32,
                });
                x = x.saturating_add(core_edge);
            }
            y = y.saturating_add(core_edge);
        }

        Self {
            full_width,
            full_height,
            spec: TileSpec { core_edge, halo },
            tiles,
        }
    }

    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }
}

/// Extracts a fixed-size, halo-padded RAW tile. Out-of-image samples clamp to
/// the nearest sensor edge, allowing one reusable GPU allocation for all tiles.
pub fn extract_padded_tile(raw: &LoadedRaw, tile: ExportTile) -> LoadedRaw {
    let mut tile_raw = LoadedRaw {
        width: tile.padded_width,
        height: tile.padded_height,
        camera_make: raw.camera_make.clone(),
        camera_model: raw.camera_model.clone(),
        lens_make: raw.lens_make.clone(),
        lens_model: raw.lens_model.clone(),
        focal_length: raw.focal_length,
        aperture: raw.aperture,
        focus_distance: raw.focus_distance,
        capture_metadata: raw.capture_metadata.clone(),
        cfa_kind: raw.cfa_kind,
        raw_pixels: Vec::new(),
        color_indices: raw.color_indices.subregion_clamped(
            i64::from(tile.global_origin_x),
            i64::from(tile.global_origin_y),
            tile.padded_width,
            tile.padded_height,
        ),
        wb_coeffs: raw.wb_coeffs,
        cam_to_srgb: raw.cam_to_srgb,
        black_levels: raw.black_levels,
        black_levels_per_pixel: raw.black_levels_per_pixel.subregion_clamped(
            i64::from(tile.global_origin_x),
            i64::from(tile.global_origin_y),
            tile.padded_width,
            tile.padded_height,
        ),
        white_levels: raw.white_levels,
        noise_profile: raw.noise_profile,
        camera_profile: raw.camera_profile.clone(),
        camera_profile_source: raw.camera_profile_source.clone(),
        available_camera_profiles: raw.available_camera_profiles.clone(),
        white_balance_model: raw.white_balance_model.clone(),
        // Export tiles use global coordinates for masks and are stitched back
        // into one native raster before the deferred lens map is applied.
        lens_geometry: None,
        ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
        opposed_chroma_cache: std::sync::Arc::clone(&raw.opposed_chroma_cache),
    };
    fill_padded_tile(raw, tile, &mut tile_raw);
    tile_raw
}

/// Reuses the allocation and metadata clones of an existing tile buffer. The
/// hot export loop only rewrites the mosaic pixels and compact calibration
/// maps, avoiding three fresh full-tile allocations per tile.
pub fn extract_padded_tile_into(raw: &LoadedRaw, tile: ExportTile, tile_raw: &mut LoadedRaw) {
    tile_raw.width = tile.padded_width;
    tile_raw.height = tile.padded_height;
    tile_raw.color_indices = raw.color_indices.subregion_clamped(
        i64::from(tile.global_origin_x),
        i64::from(tile.global_origin_y),
        tile.padded_width,
        tile.padded_height,
    );
    tile_raw.black_levels_per_pixel = raw.black_levels_per_pixel.subregion_clamped(
        i64::from(tile.global_origin_x),
        i64::from(tile.global_origin_y),
        tile.padded_width,
        tile.padded_height,
    );
    fill_padded_tile(raw, tile, tile_raw);
}

fn fill_padded_tile(raw: &LoadedRaw, tile: ExportTile, tile_raw: &mut LoadedRaw) {
    if let Ok(mut cached) = tile_raw.ai_denoised.write() {
        *cached = padded_tile_ai_denoised(raw, tile);
    }
    let width = tile.padded_width as usize;
    let height = tile.padded_height as usize;
    // Keep the reusable tile buffer fully allocated before row slices are taken.
    tile_raw.raw_pixels.resize(width.saturating_mul(height), 0);

    let source_width = raw.width as i64;
    let max_x = source_width.saturating_sub(1);
    let max_y = i64::from(raw.height.saturating_sub(1));

    for local_y in 0..tile.padded_height {
        let global_y = (i64::from(tile.global_origin_y) + i64::from(local_y)).clamp(0, max_y);
        let destination_start = local_y as usize * width;
        let destination = &mut tile_raw.raw_pixels[destination_start..destination_start + width];
        let origin_x = i64::from(tile.global_origin_x);
        let end_x = origin_x + i64::from(tile.padded_width);
        let source_row = global_y as usize * raw.width as usize;

        if origin_x >= 0 && end_x <= source_width {
            let source_start = source_row + origin_x as usize;
            destination.copy_from_slice(&raw.raw_pixels[source_start..source_start + width]);
            continue;
        }

        let left = (-origin_x).clamp(0, i64::from(tile.padded_width)) as usize;
        let right = (end_x - source_width).clamp(0, i64::from(tile.padded_width)) as usize;
        if left > 0 {
            destination[..left].fill(raw.raw_pixels[source_row]);
        }
        let middle_start_global = origin_x.max(0);
        let middle_len = width.saturating_sub(left + right);
        if middle_len > 0 {
            let source_start = source_row + middle_start_global as usize;
            destination[left..left + middle_len]
                .copy_from_slice(&raw.raw_pixels[source_start..source_start + middle_len]);
        }
        if right > 0 {
            let edge = raw.raw_pixels[source_row + max_x as usize];
            destination[width - right..].fill(edge);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        affected_stage, build_proxy, crop_raw, extract_padded_tile, extract_padded_tile_into,
        required_export_tile_halo, ExportTile, ProcessingStage, ProxySpec, TilePlan, TileSpec,
        EXPORT_TILE_HALO, MIN_EXPORT_TILE_HALO,
    };
    use crate::pipeline::{
        AiDenoisedImage, CameraProfile, CfaKind, CompactPixelMap, DenoiseQuality, ExposureParams,
        LoadedRaw, MaskStack,
    };

    fn test_raw(width: u32, height: u32) -> LoadedRaw {
        let pixels = (0..width * height)
            .map(|value| value as u16)
            .collect::<Vec<_>>();
        LoadedRaw {
            width,
            height,
            camera_make: "Test".to_owned(),
            camera_model: String::new(),
            lens_make: String::new(),
            lens_model: String::new(),
            focal_length: 0.0,
            aperture: 0.0,
            focus_distance: 0.0,
            capture_metadata: Default::default(),
            cfa_kind: CfaKind::Bayer,
            raw_pixels: pixels,
            color_indices: CompactPixelMap::dense(
                width,
                height,
                vec![0; (width * height) as usize],
            ),
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [[0.0; 4]; 3],
            black_levels: [0.0; 4],
            black_levels_per_pixel: CompactPixelMap::dense(
                width,
                height,
                vec![0.0; (width * height) as usize],
            ),
            white_levels: [1023.0; 4],
            noise_profile: crate::pipeline::NoiseProfile::default(),
            camera_profile: CameraProfile::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
            lens_geometry: None,
            ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
            opposed_chroma_cache: Default::default(),
        }
    }

    #[test]
    fn padded_tile_allocates_first_export_buffer_and_clamps_edges() {
        let raw = test_raw(3, 2);
        let tile = ExportTile {
            core_x: 0,
            core_y: 0,
            core_width: 3,
            core_height: 2,
            local_core_x: 1,
            local_core_y: 1,
            padded_width: 5,
            padded_height: 4,
            global_origin_x: -1,
            global_origin_y: -1,
        };

        let extracted = extract_padded_tile(&raw, tile);

        assert_eq!(extracted.raw_pixels.len(), 20);
        assert_eq!(&extracted.raw_pixels[0..5], &[0, 0, 1, 2, 2]);
        assert_eq!(&extracted.raw_pixels[5..10], &[0, 0, 1, 2, 2]);
        assert_eq!(&extracted.raw_pixels[10..15], &[3, 3, 4, 5, 5]);
        assert_eq!(&extracted.raw_pixels[15..20], &[3, 3, 4, 5, 5]);
    }

    #[test]
    fn padded_tile_reuse_resizes_buffer_for_new_tile_shape() {
        let raw = test_raw(4, 3);
        let first = ExportTile {
            core_x: 0,
            core_y: 0,
            core_width: 2,
            core_height: 2,
            local_core_x: 0,
            local_core_y: 0,
            padded_width: 2,
            padded_height: 2,
            global_origin_x: 0,
            global_origin_y: 0,
        };
        let second = ExportTile {
            core_x: 0,
            core_y: 0,
            core_width: 4,
            core_height: 3,
            local_core_x: 1,
            local_core_y: 1,
            padded_width: 6,
            padded_height: 5,
            global_origin_x: -1,
            global_origin_y: -1,
        };

        let mut scratch = extract_padded_tile(&raw, first);
        extract_padded_tile_into(&raw, second, &mut scratch);

        assert_eq!(scratch.raw_pixels.len(), 30);
        assert_eq!(&scratch.raw_pixels[0..6], &[0, 0, 1, 2, 3, 3]);
        assert_eq!(&scratch.raw_pixels[24..30], &[8, 8, 9, 10, 11, 11]);
    }

    #[test]
    fn ai_denoise_cache_tracks_crop_proxy_and_export_tile_geometry() {
        let raw = test_raw(4, 2);
        let raw_cfa16 = (0..8).map(|pixel| pixel as u16).collect();
        raw.set_ai_denoised_image(AiDenoisedImage::new_bayer_cfa(4, 2, raw_cfa16).unwrap())
            .unwrap();

        let crop = crop_raw(&raw, 1, 0, 2, 2)
            .ai_denoised_image()
            .expect("crop retains aligned AI output");
        assert_eq!(crop.raw_cfa16.as_ref(), &[1, 2, 5, 6]);

        let proxy = build_proxy(&raw, ProxySpec { max_edge: 2 })
            .ai_denoised_image()
            .expect("proxy derives AI output");
        assert_eq!(proxy.raw_cfa16.as_ref(), &[0, 2, 4, 6]);

        let tile = extract_padded_tile(
            &raw,
            ExportTile {
                core_x: 0,
                core_y: 0,
                core_width: 4,
                core_height: 2,
                local_core_x: 1,
                local_core_y: 1,
                padded_width: 6,
                padded_height: 4,
                global_origin_x: -1,
                global_origin_y: -1,
            },
        )
        .ai_denoised_image()
        .expect("export tile retains aligned AI output");
        assert_eq!(tile.raw_cfa16[0], 0);
        assert_eq!(tile.raw_cfa16[tile.raw_cfa16.len() - 1], 7);
    }

    #[test]
    fn develop_adjustments_only_invalidate_output() {
        let before = ExposureParams::default();
        let mut after = before;
        after.exposure = 1.0;
        assert_eq!(
            affected_stage(&before, &after),
            Some(ProcessingStage::Output)
        );
    }

    #[test]
    fn raw_controls_invalidate_every_downstream_stage() {
        let before = ExposureParams::default();

        let mut black_point = before;
        black_point.black_point = 0.01;
        assert_eq!(
            affected_stage(&before, &black_point),
            Some(ProcessingStage::Raw)
        );

        let mut luminance_denoise = before;
        luminance_denoise.luminance_denoise = 25.0;
        assert_eq!(
            affected_stage(&before, &luminance_denoise),
            Some(ProcessingStage::Raw)
        );

        let mut denoise_quality = before;
        denoise_quality.denoise_quality = crate::pipeline::DenoiseQuality::High;
        assert_eq!(
            affected_stage(&before, &denoise_quality),
            Some(ProcessingStage::Raw)
        );
    }

    #[test]
    fn global_wb_invalidates_tone_analysis_and_output() {
        let before = ExposureParams::default();
        for after in [
            ExposureParams {
                temperature: 1.0,
                ..before
            },
            ExposureParams {
                tint: 1.0,
                ..before
            },
        ] {
            assert_eq!(affected_stage(&before, &after), Some(ProcessingStage::Tone));
        }
    }

    #[test]
    fn export_halo_shrinks_when_wide_radius_effects_are_neutral() {
        let masks = MaskStack::default();
        let mut exposure = ExposureParams {
            sharpen_amount: 0.0,
            ..Default::default()
        };
        assert_eq!(
            required_export_tile_halo(&exposure, &masks),
            MIN_EXPORT_TILE_HALO
        );

        exposure.glow_amount = 1.0;
        assert!(required_export_tile_halo(&exposure, &masks) > MIN_EXPORT_TILE_HALO);
        exposure.clarity = 1.0;
        exposure.chroma_denoise = 1.0;
        exposure.denoise_quality = DenoiseQuality::High;
        assert_eq!(
            required_export_tile_halo(&exposure, &masks),
            EXPORT_TILE_HALO
        );
    }

    #[test]
    fn tile_plan_covers_partial_edges() {
        let plan = TilePlan::new(
            2500,
            1300,
            TileSpec {
                core_edge: 1024,
                halo: 48,
            },
        );
        assert_eq!(plan.tile_count(), 6);
        assert_eq!(plan.tiles.last().unwrap().core_width, 452);
        assert_eq!(plan.tiles.last().unwrap().core_height, 276);
    }

    #[test]
    fn crop_raw_copies_only_the_requested_sensor_region() {
        let width = 4;
        let height = 3;
        let raw = LoadedRaw {
            width,
            height,
            camera_make: "Test".to_owned(),
            camera_model: String::new(),
            lens_make: String::new(),
            lens_model: String::new(),
            focal_length: 0.0,
            aperture: 0.0,
            focus_distance: 0.0,
            capture_metadata: Default::default(),
            cfa_kind: CfaKind::Bayer,
            raw_pixels: (0..width * height).map(|value| value as u16).collect(),
            color_indices: CompactPixelMap::dense(
                width,
                height,
                (0..width * height).map(|value| (value % 4) as u8).collect(),
            ),
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [[0.0; 4]; 3],
            black_levels: [0.0; 4],
            black_levels_per_pixel: CompactPixelMap::dense(
                width,
                height,
                (0..width * height).map(|value| value as f32).collect(),
            ),
            white_levels: [1023.0; 4],
            noise_profile: crate::pipeline::NoiseProfile::default(),
            camera_profile: CameraProfile::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
            lens_geometry: None,
            ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
            opposed_chroma_cache: Default::default(),
        };

        let cropped = crop_raw(&raw, 1, 1, 2, 2);
        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(cropped.raw_pixels, vec![5, 6, 9, 10]);
        assert_eq!(
            cropped.color_indices.iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 1, 2]
        );
        assert_eq!(
            cropped
                .black_levels_per_pixel
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![5.0, 6.0, 9.0, 10.0]
        );
        assert_eq!(cropped.camera_make, "Test");
    }

    #[test]
    fn proxy_preserves_bayer_phase_when_scale_is_even() {
        let width = 8;
        let height = 8;
        let mut color_indices = Vec::new();
        for y in 0..height {
            for x in 0..width {
                color_indices.push(match (x % 2, y % 2) {
                    (0, 0) => 0,
                    (1, 0) => 1,
                    (0, 1) => 3,
                    _ => 2,
                });
            }
        }
        let raw = LoadedRaw {
            width,
            height,
            camera_make: String::new(),
            camera_model: String::new(),
            lens_make: String::new(),
            lens_model: String::new(),
            focal_length: 0.0,
            aperture: 0.0,
            focus_distance: 0.0,
            capture_metadata: Default::default(),
            cfa_kind: CfaKind::Bayer,
            raw_pixels: vec![100; (width * height) as usize],
            color_indices: CompactPixelMap::dense(width, height, color_indices),
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [[0.0; 4]; 3],
            black_levels: [0.0; 4],
            black_levels_per_pixel: CompactPixelMap::dense(
                width,
                height,
                vec![0.0; (width * height) as usize],
            ),
            white_levels: [1023.0; 4],
            noise_profile: crate::pipeline::NoiseProfile::default(),
            camera_profile: CameraProfile::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: None,
            lens_geometry: None,
            ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
            opposed_chroma_cache: Default::default(),
        };

        let proxy = build_proxy(&raw, ProxySpec { max_edge: 4 });
        assert_eq!(proxy.width, 4);
        assert_eq!(proxy.height, 4);
        let proxy_cfa = proxy.color_indices.iter().copied().collect::<Vec<_>>();
        assert_eq!(&proxy_cfa[..4], &[0, 1, 0, 1]);
        assert_eq!(&proxy_cfa[4..8], &[3, 2, 3, 2]);
    }

    #[test]
    fn proxy_long_edge_does_not_drop_at_integer_scale_thresholds() {
        // These small rasters mirror the aspect ratios and scale thresholds
        // that previously made a 45 MP RAW produce a lower-resolution preview
        // than a 33 MP RAW for the same max_edge request.
        let larger = build_proxy(&test_raw(82, 54), ProxySpec { max_edge: 26 });
        let smaller = build_proxy(&test_raw(70, 46), ProxySpec { max_edge: 26 });
        let portrait = build_proxy(&test_raw(54, 82), ProxySpec { max_edge: 26 });

        assert_eq!(larger.width.max(larger.height), 26);
        assert_eq!(smaller.width.max(smaller.height), 26);
        assert_eq!(portrait.width.max(portrait.height), 26);
        assert_eq!(larger.width % 2, 0);
        assert_eq!(larger.height % 2, 0);
    }

    #[test]
    fn fractional_xtrans_proxy_keeps_complete_six_by_six_phases() {
        let pattern = vec![
            0, 1, 0, 0, 1, 0, 1, 2, 1, 2, 1, 2, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 1, 2, 1, 2, 1,
            2, 0, 1, 0, 0, 1, 0,
        ];
        let mut raw = test_raw(98, 66);
        raw.cfa_kind = CfaKind::XTrans;
        raw.color_indices = CompactPixelMap::repeating(98, 66, 6, 6, pattern.clone());

        // Six guard pixels are sufficient for a 32 px physical viewport even
        // after the output is aligned down to a complete X-Trans period.
        let proxy = build_proxy(&raw, ProxySpec { max_edge: 38 });
        assert!(proxy.width.max(proxy.height) >= 32);
        assert_eq!(proxy.width % 6, 0);
        assert_eq!(proxy.height % 6, 0);
        let proxy_cfa = &proxy.color_indices;
        let proxy_width = proxy.width;
        let first_period = (0..6)
            .flat_map(|y| (0..6).map(move |x| proxy_cfa[(y * proxy_width + x) as usize]))
            .collect::<Vec<_>>();
        assert_eq!(first_period, pattern);
    }

    #[test]
    fn xtrans_proxy_filters_each_output_pixel_not_a_whole_six_pixel_macrocell() {
        let pattern = vec![
            0, 1, 0, 0, 1, 0, 1, 2, 1, 2, 1, 2, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 1, 2, 1, 2, 1,
            2, 0, 1, 0, 0, 1, 0,
        ];
        let mut raw = test_raw(120, 120);
        raw.cfa_kind = CfaKind::XTrans;
        raw.color_indices = CompactPixelMap::repeating(120, 120, 6, 6, pattern);
        raw.raw_pixels = (0..120)
            .flat_map(|_| (0..120).map(|x| (x * 100) as u16))
            .collect();

        let proxy = build_proxy(&raw, ProxySpec { max_edge: 60 });
        // A 2:1 reduction maps the first six output pixels across source
        // columns 0..12. They must retain that spatial progression. The old
        // macrocell implementation averaged all twelve columns into every one
        // of these samples, which is the sixfold blur seen in Fuji previews.
        assert!(proxy.raw_pixels[0] <= 200, "left sample was over-blurred");
        assert!(proxy.raw_pixels[5] >= 900, "right sample was over-blurred");
    }
}
