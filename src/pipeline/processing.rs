use super::{ExposureParams, LoadedRaw};

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
        || before.demosaic_mode != after.demosaic_mode
        || before.dual_threshold != after.dual_threshold
        || before.frequency_chroma != after.frequency_chroma
        || before.ca_red != after.ca_red
        || before.ca_blue != after.ca_blue
        || before.highlight_method != after.highlight_method
        || before.highlight_clip != after.highlight_clip
        || before.highlight_reconstruction != after.highlight_reconstruction
        || before.highlight_iterations != after.highlight_iterations
        || before.highlight_color_adaptation != after.highlight_color_adaptation
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
    let mut color_indices = Vec::with_capacity((width * height) as usize);
    let mut black_levels_per_pixel = Vec::with_capacity((width * height) as usize);

    for row in y..y + height {
        let start = (row * raw.width + x) as usize;
        let end = start + width as usize;
        raw_pixels.extend_from_slice(&raw.raw_pixels[start..end]);
        color_indices.extend_from_slice(&raw.color_indices[start..end]);
        black_levels_per_pixel.extend_from_slice(&raw.black_levels_per_pixel[start..end]);
    }

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
        cfa_kind: raw.cfa_kind,
        raw_pixels,
        color_indices,
        wb_coeffs: raw.wb_coeffs,
        cam_to_srgb: raw.cam_to_srgb,
        black_levels: raw.black_levels,
        black_levels_per_pixel,
        white_levels: raw.white_levels,
        camera_profile: raw.camera_profile.clone(),
        white_balance_model: raw.white_balance_model.clone(),
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
    let scale = longest.div_ceil(max_edge).max(1);
    if scale == 1 {
        if x == 0 && y == 0 && region_width == raw.width && region_height == raw.height {
            return raw.clone();
        }
        return crop_raw(raw, x, y, region_width, region_height);
    }

    let width = region_width.div_ceil(scale);
    let height = region_height.div_ceil(scale);
    let mut raw_pixels = Vec::with_capacity((width * height) as usize);
    let mut color_indices = Vec::with_capacity((width * height) as usize);
    let mut black_levels_per_pixel = Vec::with_capacity((width * height) as usize);

    let cfa_period = match raw.cfa_kind {
        super::CfaKind::Bayer => 2,
        super::CfaKind::XTrans => 6,
    };

    for py in 0..height {
        let source_y0 = y + py * scale;
        let source_y1 = (y + (py + 1) * scale).min(y + region_height);
        for px in 0..width {
            let source_x0 = x + px * scale;
            let source_x1 = (x + (px + 1) * scale).min(x + region_width);
            let center_x = (source_x0 + (source_x1 - source_x0) / 2).min(raw.width - 1);
            let center_y = (source_y0 + (source_y1 - source_y0) / 2).min(raw.height - 1);

            // Anchor the synthetic proxy mosaic to the source region's real CFA
            // phase. Detail crops are aligned to the sensor period, preventing
            // a phase jump from appearing as coloured horizontal/vertical lines.
            let phase_x = (x + px % cfa_period).min(raw.width - 1);
            let phase_y = (y + py % cfa_period).min(raw.height - 1);
            let phase_index = (phase_y * raw.width + phase_x) as usize;
            let cfa = raw.color_indices[phase_index];

            let mut pixel_sum = 0u64;
            let mut black_sum = 0.0f64;
            let mut count = 0u32;
            for sy in source_y0..source_y1 {
                let row = sy * raw.width;
                for sx in source_x0..source_x1 {
                    let index = (row + sx) as usize;
                    if raw.color_indices[index] == cfa {
                        pixel_sum += u64::from(raw.raw_pixels[index]);
                        black_sum += f64::from(raw.black_levels_per_pixel[index]);
                        count += 1;
                    }
                }
            }

            if count == 0 {
                let fallback = nearest_cfa_sample(raw, center_x, center_y, cfa, cfa_period);
                raw_pixels.push(raw.raw_pixels[fallback]);
                black_levels_per_pixel.push(raw.black_levels_per_pixel[fallback]);
            } else {
                raw_pixels.push((pixel_sum / u64::from(count)) as u16);
                black_levels_per_pixel.push((black_sum / f64::from(count)) as f32);
            }
            color_indices.push(cfa);
        }
    }

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
        cfa_kind: raw.cfa_kind,
        raw_pixels,
        color_indices,
        wb_coeffs: raw.wb_coeffs,
        cam_to_srgb: raw.cam_to_srgb,
        black_levels: raw.black_levels,
        black_levels_per_pixel,
        white_levels: raw.white_levels,
        camera_profile: raw.camera_profile.clone(),
        white_balance_model: raw.white_balance_model.clone(),
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

/// Cumulative input support of every spatial stage used by an export tile.
/// These are deliberately separate constants: taking only the maximum stage
/// radius is incorrect when one spatial pass consumes another pass's output.
const HIGHLIGHT_PREP_SUPPORT: u32 = 4;
// The quality-4 guided sequence has radii 16+8+4+2+1+4+2+1+2+1+1 = 42.
// Each pass also reads the outward sample at twice its radius, so its
// dependency support accumulates to 84 pixels across the ping-pong chain.
const HIGHLIGHT_GUIDED_SUPPORT: u32 = 2 * (16 + 8 + 4 + 2 + 1 + 4 + 2 + 1 + 2 + 1 + 1);
// Conservative bound over the complete Bayer RCD and X-Trans Markesteijn-3
// pass chains, including their final detail-recovery neighbourhoods.
const DEMOSAIC_CHAIN_SUPPORT: u32 = 32;
// The edge-aware tone guide is blurred by five guide texels at desktop's 4x
// reduction and three texels at Android's 8x reduction. Bilinear lookup can
// reach one additional guide cell, so its raw-pixel support is 24/32.
const TONE_GUIDE_SUPPORT: u32 = if cfg!(target_os = "android") { 32 } else { 24 };
// Scale-aware Clarity has the widest presence footprint: the B3 kernel reaches
// +/-2 times a step capped at 12 pixels. Texture and Dehaze remain inside it.
const LOCAL_EFFECTS_SUPPORT: u32 = 24;
// Glow cascades five B3 diffusion stages. At the capped 3x reference scale the
// steps are 3+3+6+12+24, and each 5x5 stage reaches +/-2*step. Support therefore
// accumulates to 96 pixels from the extracted highlight source.
const GLOW_SUPPORT: u32 = 96;
const COLOR_MIXER_SUPPORT: u32 = 4;
const EXPORT_CUMULATIVE_SUPPORT: u32 = HIGHLIGHT_PREP_SUPPORT
    + HIGHLIGHT_GUIDED_SUPPORT
    + DEMOSAIC_CHAIN_SUPPORT
    + TONE_GUIDE_SUPPORT
    + LOCAL_EFFECTS_SUPPORT
    + GLOW_SUPPORT
    + COLOR_MIXER_SUPPORT;

/// Rounded up to the 8-pixel guide/workgroup alignment.
pub const EXPORT_TILE_HALO: u32 = EXPORT_CUMULATIVE_SUPPORT.div_ceil(8) * 8;

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
    let pixel_count = (tile.padded_width * tile.padded_height) as usize;
    let mut raw_pixels = Vec::with_capacity(pixel_count);
    let mut color_indices = Vec::with_capacity(pixel_count);
    let mut black_levels_per_pixel = Vec::with_capacity(pixel_count);

    let max_x = raw.width.saturating_sub(1) as i64;
    let max_y = raw.height.saturating_sub(1) as i64;
    for local_y in 0..tile.padded_height {
        let global_y = (i64::from(tile.global_origin_y) + i64::from(local_y)).clamp(0, max_y);
        for local_x in 0..tile.padded_width {
            let global_x = (i64::from(tile.global_origin_x) + i64::from(local_x)).clamp(0, max_x);
            let index = (global_y as u32 * raw.width + global_x as u32) as usize;
            raw_pixels.push(raw.raw_pixels[index]);
            color_indices.push(raw.color_indices[index]);
            black_levels_per_pixel.push(raw.black_levels_per_pixel[index]);
        }
    }

    LoadedRaw {
        width: tile.padded_width,
        height: tile.padded_height,
        camera_make: raw.camera_make.clone(),
        camera_model: raw.camera_model.clone(),
        lens_make: raw.lens_make.clone(),
        lens_model: raw.lens_model.clone(),
        focal_length: raw.focal_length,
        aperture: raw.aperture,
        focus_distance: raw.focus_distance,
        cfa_kind: raw.cfa_kind,
        raw_pixels,
        color_indices,
        wb_coeffs: raw.wb_coeffs,
        cam_to_srgb: raw.cam_to_srgb,
        black_levels: raw.black_levels,
        black_levels_per_pixel,
        white_levels: raw.white_levels,
        camera_profile: raw.camera_profile.clone(),
        white_balance_model: raw.white_balance_model.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        affected_stage, build_proxy, crop_raw, ProcessingStage, ProxySpec, TilePlan, TileSpec,
    };
    use crate::pipeline::{CameraProfile, CfaKind, ExposureParams, LoadedRaw};

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
        let mut after = before;
        after.black_point = 0.01;
        assert_eq!(affected_stage(&before, &after), Some(ProcessingStage::Raw));
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
            cfa_kind: CfaKind::Bayer,
            raw_pixels: (0..width * height).map(|value| value as u16).collect(),
            color_indices: (0..width * height).map(|value| (value % 4) as u8).collect(),
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [[0.0; 4]; 3],
            black_levels: [0.0; 4],
            black_levels_per_pixel: (0..width * height).map(|value| value as f32).collect(),
            white_levels: [1023.0; 4],
            camera_profile: CameraProfile::default(),
            white_balance_model: None,
        };

        let cropped = crop_raw(&raw, 1, 1, 2, 2);
        assert_eq!((cropped.width, cropped.height), (2, 2));
        assert_eq!(cropped.raw_pixels, vec![5, 6, 9, 10]);
        assert_eq!(cropped.color_indices, vec![1, 2, 1, 2]);
        assert_eq!(cropped.black_levels_per_pixel, vec![5.0, 6.0, 9.0, 10.0]);
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
            cfa_kind: CfaKind::Bayer,
            raw_pixels: vec![100; (width * height) as usize],
            color_indices,
            wb_coeffs: [1.0; 4],
            cam_to_srgb: [[0.0; 4]; 3],
            black_levels: [0.0; 4],
            black_levels_per_pixel: vec![0.0; (width * height) as usize],
            white_levels: [1023.0; 4],
            camera_profile: CameraProfile::default(),
            white_balance_model: None,
        };

        let proxy = build_proxy(&raw, ProxySpec { max_edge: 4 });
        assert_eq!(proxy.width, 4);
        assert_eq!(proxy.height, 4);
        assert_eq!(&proxy.color_indices[..4], &[0, 1, 0, 1]);
        assert_eq!(&proxy.color_indices[4..8], &[3, 2, 3, 2]);
    }
}
