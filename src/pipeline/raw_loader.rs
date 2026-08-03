use super::basicadj::{ExposureParams, GLOBAL_TEMPERATURE_LIMIT};
use super::color_profile::CameraProfile;
use super::geometry::LensGeometryMap;
use super::noise::NoiseProfile;
#[cfg(not(libraw_available))]
use anyhow::anyhow;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Index;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CameraProfileMode {
    /// Ignore DCP creative stages and use only camera/DNG/LibRaw matrices.
    MatrixOnly,
    /// Use a matching external DCP from the configured folder, otherwise
    /// fall back to the camera matrix without using an embedded DCP.
    DcpProfiles,
    /// Use the embedded camera matrix unless an external DCP was explicitly
    /// selected for the image.
    #[default]
    Automatic,
}

impl CameraProfileMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::MatrixOnly => "Embedded matrix only",
            Self::DcpProfiles => "Use DCP profiles",
            Self::Automatic => "Automatic",
        }
    }

    pub(crate) const fn cache_key(self) -> &'static str {
        match self {
            Self::MatrixOnly => "matrix",
            Self::DcpProfiles => "dcp",
            Self::Automatic => "auto",
        }
    }

    pub(crate) const fn prefers_external_dcp(self) -> bool {
        matches!(self, Self::DcpProfiles)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraProfileCandidate {
    pub path: PathBuf,
    pub name: String,
}

pub const SUPPORTED_RAW_EXTENSIONS: &[&str] = &[
    "3fr", "ari", "arw", "bay", "bmq", "cap", "cine", "cr2", "cr3", "crw", "cs1", "dc2", "dcr",
    "dcs", "dng", "drf", "eip", "erf", "fff", "gpr", "iiq", "k25", "kc2", "kdc", "mdc", "mef",
    "mos", "mrw", "nef", "nrw", "obm", "orf", "pef", "ptx", "pxn", "qtk", "r3d", "raf", "raw",
    "rdc", "rw2", "rwl", "rwz", "sr2", "srf", "srw", "sti", "x3f",
];

pub fn is_supported_raw_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            SUPPORTED_RAW_EXTENSIONS
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

#[derive(Clone, Debug)]
pub struct RawThumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub const MAX_RAW_EDGE: u32 = 32_768;
#[cfg(target_os = "android")]
pub const MAX_RAW_PIXELS: u64 = 50_000_000;
#[cfg(not(target_os = "android"))]
pub const MAX_RAW_PIXELS: u64 = 120_000_000;
#[cfg(all(libraw_available, target_os = "android"))]
const MAX_RAW_FILE_BYTES: u64 = 2_000_000_000;
#[cfg(all(libraw_available, not(target_os = "android")))]
const MAX_RAW_FILE_BYTES: u64 = 8_000_000_000;
#[cfg(all(libraw_available, target_os = "android"))]
const MAX_SENSOR_PIXELS: u64 = 70_000_000;
#[cfg(all(libraw_available, not(target_os = "android")))]
const MAX_SENSOR_PIXELS: u64 = 160_000_000;
#[cfg(libraw_available)]
const MAX_SENSOR_EDGE: u32 = 40_000;

pub fn validate_raw_dimensions(width: u32, height: u32) -> Result<usize> {
    anyhow::ensure!(width > 0 && height > 0, "RAW dimensions must be non-zero");
    anyhow::ensure!(
        width <= MAX_RAW_EDGE && height <= MAX_RAW_EDGE,
        "RAW dimensions {width}x{height} exceed the {MAX_RAW_EDGE}-pixel edge limit"
    );
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .context("RAW pixel count overflow")?;
    anyhow::ensure!(
        pixels <= MAX_RAW_PIXELS,
        "RAW dimensions {width}x{height} contain {pixels} pixels; the limit is {MAX_RAW_PIXELS}"
    );
    usize::try_from(pixels).context("RAW pixel count does not fit this platform")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CfaKind {
    #[default]
    Bayer,
    XTrans,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DngColorEndpoint {
    pub cct: Option<f32>,
    pub color_matrix: [[f32; 3]; 4],
    pub calibration: [[f32; 4]; 4],
    pub forward_matrix: Option<[[f32; 4]; 3]>,
}

#[derive(Clone, Debug)]
pub(crate) enum CameraColorModel {
    Dng {
        endpoints: Box<[DngColorEndpoint; 2]>,
        analog_balance: [[f32; 4]; 4],
    },
    Matrix {
        xyz_to_camera: [[f32; 3]; 4],
    },
}

#[derive(Clone, Debug)]
pub(crate) struct CameraWhiteBalanceModel {
    pub base_wb: [f32; 4],
    pub cdesc: [u8; 4],
    pub base_cct: f32,
    pub color: CameraColorModel,
}

#[derive(Clone, Debug)]
pub struct CompactPixelMap<T> {
    width: u32,
    height: u32,
    storage_width: u32,
    storage_height: u32,
    values: Vec<T>,
}

impl<T> CompactPixelMap<T> {
    pub fn dense(width: u32, height: u32, values: Vec<T>) -> Self {
        debug_assert_eq!(
            values.len(),
            (width as usize).saturating_mul(height as usize)
        );
        Self {
            width,
            height,
            storage_width: width,
            storage_height: height,
            values,
        }
    }

    pub fn repeating(
        width: u32,
        height: u32,
        storage_width: u32,
        storage_height: u32,
        values: Vec<T>,
    ) -> Self {
        debug_assert!(storage_width > 0 && storage_height > 0);
        debug_assert_eq!(
            values.len(),
            (storage_width as usize).saturating_mul(storage_height as usize)
        );
        Self {
            width,
            height,
            storage_width,
            storage_height,
            values,
        }
    }

    pub fn len(&self) -> usize {
        (self.width as usize).saturating_mul(self.height as usize)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn storage_width(&self) -> u32 {
        self.storage_width
    }
    pub fn storage_height(&self) -> u32 {
        self.storage_height
    }
    pub fn storage_slice(&self) -> &[T] {
        &self.values
    }

    fn storage_index(&self, index: usize) -> usize {
        let width = self.width.max(1) as usize;
        let x = index % width;
        let y = index / width;
        (y % self.storage_height.max(1) as usize) * self.storage_width.max(1) as usize
            + (x % self.storage_width.max(1) as usize)
    }

    pub fn get(&self, index: usize) -> Option<&T> {
        (index < self.len()).then(|| &self.values[self.storage_index(index)])
    }

    pub fn iter(&self) -> CompactPixelMapIter<'_, T> {
        CompactPixelMapIter { map: self, next: 0 }
    }

    /// Compact backing storage used when fingerprinting runtime-derived data.
    /// Including the stored shape distinguishes a dense map from a repeating
    /// map without materializing either map at full image resolution.
    pub(crate) fn storage_parts(&self) -> (u32, u32, &[T]) {
        (self.storage_width, self.storage_height, &self.values)
    }
}

impl<T: Copy> CompactPixelMap<T> {
    /// Appends one logical row without materializing the whole logical map.
    /// Repeating maps are copied a pattern-row slice at a time, avoiding a
    /// modulo/division pair for every pixel in hot GPU-upload paths.
    pub fn append_row_to(&self, y: u32, output: &mut Vec<T>) {
        if y >= self.height || self.width == 0 || self.values.is_empty() {
            return;
        }
        let storage_width = self.storage_width.max(1) as usize;
        let storage_y = (y % self.storage_height.max(1)) as usize;
        let start = storage_y * storage_width;
        let pattern = &self.values[start..start + storage_width];
        let mut remaining = self.width as usize;
        while remaining >= pattern.len() {
            output.extend_from_slice(pattern);
            remaining -= pattern.len();
        }
        if remaining > 0 {
            output.extend_from_slice(&pattern[..remaining]);
        }
    }
}

impl<T: Copy + PartialEq> CompactPixelMap<T> {
    pub fn compact_from_dense(width: u32, height: u32, values: Vec<T>, max_period: u32) -> Self {
        if width == 0 || height == 0 || values.is_empty() {
            return Self::dense(width, height, values);
        }
        // Dense full-resolution correction maps can contain tens of millions of
        // values. Avoid an expensive period search there; the LibRaw loader
        // constructs compact maps directly for the normal periodic case.
        if values.len() > 4_000_000 {
            return Self::dense(width, height, values);
        }
        let candidates = [1u32, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64];
        for ph in candidates
            .into_iter()
            .filter(|p| *p <= height && *p <= max_period.max(1))
        {
            for pw in candidates
                .into_iter()
                .filter(|p| *p <= width && *p <= max_period.max(1))
            {
                let mut matches = true;
                'outer: for y in 0..height {
                    for x in 0..width {
                        let a = values[(y * width + x) as usize];
                        let b = values[((y % ph) * width + (x % pw)) as usize];
                        if a != b {
                            matches = false;
                            break 'outer;
                        }
                    }
                }
                if matches {
                    let mut pattern = Vec::with_capacity((pw * ph) as usize);
                    for y in 0..ph {
                        pattern.extend_from_slice(
                            &values[(y * width) as usize..(y * width + pw) as usize],
                        );
                    }
                    return Self::repeating(width, height, pw, ph, pattern);
                }
            }
        }
        Self::dense(width, height, values)
    }

    pub fn subregion_clamped(&self, origin_x: i64, origin_y: i64, width: u32, height: u32) -> Self {
        let source_width = self.width.max(1) as i64;
        let source_height = self.height.max(1) as i64;
        let fully_inside = origin_x >= 0
            && origin_y >= 0
            && origin_x + i64::from(width) <= source_width
            && origin_y + i64::from(height) <= source_height;
        let repeating = self.storage_width < self.width || self.storage_height < self.height;

        if fully_inside && repeating {
            let pattern_width = self.storage_width.min(width.max(1));
            let pattern_height = self.storage_height.min(height.max(1));
            let mut pattern = Vec::with_capacity((pattern_width * pattern_height) as usize);
            for y in 0..pattern_height {
                for x in 0..pattern_width {
                    let source_x = (origin_x + i64::from(x)) as u32;
                    let source_y = (origin_y + i64::from(y)) as u32;
                    pattern.push(self[(source_y * self.width + source_x) as usize]);
                }
            }
            return Self::repeating(width, height, pattern_width, pattern_height, pattern);
        }

        let mut values = Vec::with_capacity((width as usize).saturating_mul(height as usize));
        for y in 0..height {
            let source_y = (origin_y + i64::from(y)).clamp(0, source_height - 1) as u32;
            for x in 0..width {
                let source_x = (origin_x + i64::from(x)).clamp(0, source_width - 1) as u32;
                values.push(self[(source_y * self.width + source_x) as usize]);
            }
        }
        // Clamped border tiles are not strictly periodic at the duplicated
        // edges. Keep the exact dense values rather than spending time trying
        // many full-tile period candidates that cannot match.
        Self::dense(width, height, values)
    }
}

impl<T> Index<usize> for CompactPixelMap<T> {
    type Output = T;
    fn index(&self, index: usize) -> &Self::Output {
        assert!(index < self.len(), "compact pixel-map index out of bounds");
        &self.values[self.storage_index(index)]
    }
}

pub struct CompactPixelMapIter<'a, T> {
    map: &'a CompactPixelMap<T>,
    next: usize,
}

impl<'a, T> Iterator for CompactPixelMapIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        let index = self.next;
        if index >= self.map.len() {
            return None;
        }
        self.next += 1;
        Some(&self.map[index])
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.map.len().saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}

impl<'a, T> ExactSizeIterator for CompactPixelMapIter<'a, T> {}

impl<'a, T> IntoIterator for &'a CompactPixelMap<T> {
    type Item = &'a T;
    type IntoIter = CompactPixelMapIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CaptureMetadata {
    /// ISO sensitivity. Zero means unavailable.
    pub iso_speed: f32,
    /// Exposure time in seconds. Zero means unavailable.
    pub shutter_seconds: f32,
    /// Original image description supplied by the camera or photographer.
    pub description: String,
    /// Original artist/creator string supplied by the camera or photographer.
    pub artist: String,
}

/// Cached RawNIND output. Bayer models are retained as a denoised CFA mosaic
/// in the original sensor code-value domain, so highlight reconstruction and
/// demosaic remain full-frame, edit-dependent pipeline stages. The linear
/// model used for X-Trans remains interleaved camera-RGB IEEE-754 half floats.
/// Exactly one payload is populated. Sidecars persist only the model toggle;
/// this derived cache is always rebuildable from the original sensor mosaic.
#[derive(Clone, Debug)]
pub struct AiDenoisedImage {
    pub width: u32,
    pub height: u32,
    pub rgb16f: Arc<[u16]>,
    pub raw_cfa16: Arc<[u16]>,
}

impl AiDenoisedImage {
    /// Constructs a linear camera-RGB result (currently the X-Trans path).
    pub(crate) fn new(width: u32, height: u32, rgb16f: Vec<u16>) -> Result<Self> {
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(3))
            .and_then(|elements| usize::try_from(elements).ok())
            .context("AI-denoise image dimensions overflow")?;
        anyhow::ensure!(
            width > 0 && height > 0 && rgb16f.len() == expected,
            "AI-denoise image has {} values, expected {expected} for {width}x{height}",
            rgb16f.len()
        );
        Ok(Self {
            width,
            height,
            rgb16f: rgb16f.into(),
            raw_cfa16: Arc::from([]),
        })
    }

    /// Constructs a Bayer result in the source RAW's code-value domain.
    pub(crate) fn new_bayer_cfa(width: u32, height: u32, raw_cfa16: Vec<u16>) -> Result<Self> {
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| usize::try_from(pixels).ok())
            .context("AI-denoise Bayer dimensions overflow")?;
        anyhow::ensure!(
            width > 0 && height > 0 && raw_cfa16.len() == expected,
            "AI-denoise Bayer image has {} values, expected {expected} for {width}x{height}",
            raw_cfa16.len()
        );
        Ok(Self {
            width,
            height,
            rgb16f: Arc::from([]),
            raw_cfa16: raw_cfa16.into(),
        })
    }

    pub(crate) fn bayer_cfa(&self) -> Option<&[u16]> {
        (!self.raw_cfa16.is_empty()).then_some(self.raw_cfa16.as_ref())
    }

    pub(crate) fn camera_rgb16f(&self) -> Option<&[u16]> {
        (!self.rgb16f.is_empty()).then_some(self.rgb16f.as_ref())
    }

    pub(crate) fn payload(&self) -> &[u16] {
        if let Some(raw_cfa) = self.bayer_cfa() {
            raw_cfa
        } else {
            self.rgb16f.as_ref()
        }
    }

    pub(crate) fn is_valid_for(&self, width: u32, height: u32) -> bool {
        if self.width != width || self.height != height {
            return false;
        }
        let Some(pixels) = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| usize::try_from(pixels).ok())
        else {
            return false;
        };
        (self.raw_cfa16.len() == pixels && self.rgb16f.is_empty())
            || (self.rgb16f.len() == pixels.saturating_mul(3) && self.raw_cfa16.is_empty())
    }
}

#[derive(Clone, Debug)]
pub struct LoadedRaw {
    pub width: u32,
    pub height: u32,
    pub camera_make: String,
    pub camera_model: String,
    /// Lens manufacturer reported by the RAW metadata, when available.
    pub lens_make: String,
    /// Lens model reported by the RAW metadata, when available.
    pub lens_model: String,
    /// Capture focal length in millimetres. Zero means unavailable.
    pub focal_length: f32,
    /// Capture aperture (f-number). Zero means unavailable.
    pub aperture: f32,
    /// Subject distance in metres. Zero means unavailable.
    pub focus_distance: f32,
    /// Capture details retained from LibRaw for export metadata.
    pub(crate) capture_metadata: CaptureMetadata,
    pub cfa_kind: CfaKind,
    pub raw_pixels: Vec<u16>,
    pub color_indices: CompactPixelMap<u8>,
    pub wb_coeffs: [f32; 4],
    pub cam_to_srgb: [[f32; 4]; 3],
    pub black_levels: [f32; 4],
    /// Effective LibRaw black level for every oriented active-area photosite.
    /// This includes the shared level, per-CFA-plane offsets, and an optional
    /// repeating row/column pattern from `cblack[4..]`.
    pub black_levels_per_pixel: CompactPixelMap<f32>,
    pub white_levels: [f32; 4],
    /// Per-capture signal-dependent sensor noise estimate in normalized RAW units.
    pub noise_profile: NoiseProfile,
    /// DCP creative profile stages and retained embedded camera ICC data.
    pub camera_profile: CameraProfile,
    /// External DCP actually applied to this RAW, when one was selected.
    pub camera_profile_source: Option<PathBuf>,
    /// All external DCPs in the configured root that match this camera.
    pub available_camera_profiles: Vec<CameraProfileCandidate>,
    /// Camera/DCP calibration data retained so global white-balance edits can
    /// rebuild the camera transform instead of applying generic RGB gains.
    pub(crate) white_balance_model: Option<CameraWhiteBalanceModel>,
    /// Smooth corrected-image -> native-source distortion map. Lens shading
    /// and TCA are already applied to the CFA, while this common geometric
    /// component is deferred until the float RGB geometry pass.
    pub lens_geometry: Option<Arc<LensGeometryMap>>,
    /// Runtime-only derived output. Interior mutability lets a background
    /// worker publish it without cloning the much larger decoded RAW buffers.
    pub(crate) ai_denoised: Arc<RwLock<Option<AiDenoisedImage>>>,
    /// Full-frame chrominance offsets used by darktable-compatible inpaint
    /// opposed reconstruction, keyed by black point, clip threshold, and
    /// whether the derived Bayer CFA is active. Export tiles share this cache
    /// with their source so every tile uses one full-image measurement.
    pub(crate) opposed_chroma_cache: OpposedChromaCache,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct OpposedChromaCacheKey {
    black_point_bits: u32,
    clip_threshold_bits: u32,
    use_ai_cfa: bool,
}

pub(crate) type OpposedChromaCache = Arc<RwLock<HashMap<OpposedChromaCacheKey, [f32; 3]>>>;

impl LoadedRaw {
    fn opposed_sensor_value(&self, index: usize, black_point: f32, pixels: &[u16]) -> f32 {
        let channel = usize::from(self.color_indices[index].min(3));
        let raw = f32::from(pixels[index]);
        let metadata_black = self.black_levels_per_pixel[index];
        let white = self.white_levels[channel].max(metadata_black + 1.0);
        let sensor_range = (white - metadata_black).max(1.0);
        let black_offset = black_point.clamp(-0.25, 0.25) * sensor_range;
        let calibrated_black = (metadata_black + black_offset).clamp(0.0, white - 1.0);
        ((raw - calibrated_black) / (white - calibrated_black)).clamp(0.0, 4.0)
    }

    fn opposed_logical_color(&self, index: usize) -> usize {
        match self.color_indices[index].min(3) {
            0 => 0,
            2 => 2,
            _ => 1,
        }
    }

    fn opposed_refavg(&self, row: usize, col: usize, black_point: f32, pixels: &[u16]) -> f32 {
        let width = self.width as usize;
        let height = self.height as usize;
        let center = row * width + col;
        let center_color = self.opposed_logical_color(center);
        let mut means = [0.0f32; 3];
        let mut counts = [0u32; 3];

        // These bounds intentionally follow darktable's implementation,
        // including its last-row/last-column exclusion at the sensor edge.
        let row_end = (row + 2).min(height.saturating_sub(1));
        let col_end = (col + 2).min(width.saturating_sub(1));
        for sample_row in row.saturating_sub(1)..row_end {
            for sample_col in col.saturating_sub(1)..col_end {
                let index = sample_row * width + sample_col;
                let physical = usize::from(self.color_indices[index].min(3));
                let color = self.opposed_logical_color(index);
                let value = self.opposed_sensor_value(index, black_point, pixels)
                    * self.wb_coeffs[physical];
                means[color] += value.max(0.0);
                counts[color] += 1;
            }
        }
        for color in 0..3 {
            means[color] = if counts[color] == 0 {
                0.0
            } else {
                (means[color] / counts[color] as f32).cbrt()
            };
        }
        let opposed_root = match center_color {
            0 => 0.5 * (means[1] + means[2]),
            1 => 0.5 * (means[0] + means[2]),
            _ => 0.5 * (means[0] + means[1]),
        };
        opposed_root * opposed_root * opposed_root
    }

    fn calculate_opposed_chroma(
        &self,
        black_point: f32,
        clip_threshold: f32,
        pixels: &[u16],
    ) -> [f32; 3] {
        let width = self.width as usize;
        let height = self.height as usize;
        if width == 0 || height == 0 || pixels.len() != width.saturating_mul(height) {
            return [0.0; 3];
        }

        // Match darktable's mask storage exactly: the logical dimensions use
        // integer division by three, while each channel plane is padded to an
        // eight-element boundary. Partial sensor-edge superpixels consequently
        // address the zero-filled padding in the same way as opposed.c.
        let mask_width = width / 3;
        let mask_height = height / 3;
        if mask_width == 0 || mask_height == 0 {
            return [0.0; 3];
        }
        let aligned_mask_width = mask_width.div_ceil(8) * 8;
        let aligned_mask_height = mask_height.div_ceil(8) * 8;
        let mask_size = aligned_mask_width.saturating_mul(aligned_mask_height);
        let mut clipped_mask = vec![false; 3 * mask_size];
        let clip = 0.987 * clip_threshold.max(0.01);

        // darktable leaves the last complete superpixel row and column clear.
        for mask_row in 0..mask_height.saturating_sub(1) {
            for mask_col in 0..mask_width.saturating_sub(1) {
                let mask_index = mask_row * mask_width + mask_col;
                for offset_y in 0..3 {
                    let row = mask_row * 3 + offset_y;
                    for offset_x in 0..3 {
                        let col = mask_col * 3 + offset_x;
                        let index = row * width + col;
                        let physical = usize::from(self.color_indices[index].min(3));
                        let color = self.opposed_logical_color(index);
                        let value = self.opposed_sensor_value(index, black_point, pixels)
                            * self.wb_coeffs[physical];
                        let channel_clip = clip * self.wb_coeffs[physical];
                        clipped_mask[color * mask_size + mask_index] |= value >= channel_clip;
                    }
                }
            }
        }

        let mut nearby_mask = vec![false; 3 * mask_size];
        for row in 0..mask_height {
            for col in 0..mask_width {
                let index = row * mask_width + col;
                for color in 0..3 {
                    let plane = color * mask_size;
                    let safe = col >= 3
                        && row >= 3
                        && col < mask_width.saturating_sub(4)
                        && row < mask_height.saturating_sub(4);
                    let nearby = if safe {
                        let mut dilated = false;
                        'neighbours: for offset_y in -3isize..=3 {
                            for offset_x in -3isize..=3 {
                                if offset_x.abs() == 3 && offset_y.abs() == 3 {
                                    continue;
                                }
                                let sample = (row as isize + offset_y) as usize * mask_width
                                    + (col as isize + offset_x) as usize;
                                if clipped_mask[plane + sample] {
                                    dilated = true;
                                    break 'neighbours;
                                }
                            }
                        }
                        dilated
                    } else {
                        clipped_mask[plane + index]
                    };
                    nearby_mask[plane + index] = nearby;
                }
            }
        }

        let mut sums = [0.0f32; 3];
        let mut counts = [0.0f32; 3];
        for row in 0..height {
            for col in 0..width {
                let index = row * width + col;
                let physical = usize::from(self.color_indices[index].min(3));
                let color = self.opposed_logical_color(index);
                let value = self.opposed_sensor_value(index, black_point, pixels)
                    * self.wb_coeffs[physical];
                let channel_clip = clip * self.wb_coeffs[physical];
                let mask_index = (row / 3) * mask_width + col / 3;
                if nearby_mask[color * mask_size + mask_index]
                    && value > 0.2 * channel_clip
                    && value < channel_clip
                {
                    sums[color] += value - self.opposed_refavg(row, col, black_point, pixels);
                    counts[color] += 1.0;
                }
            }
        }

        std::array::from_fn(|color| {
            if counts[color] > 100.0 {
                sums[color] / counts[color]
            } else {
                0.0
            }
        })
    }

    pub(crate) fn inpaint_opposed_chroma(
        &self,
        black_point: f32,
        clip_threshold: f32,
        use_ai_cfa: bool,
    ) -> [f32; 3] {
        let key = OpposedChromaCacheKey {
            black_point_bits: black_point.clamp(-0.25, 0.25).to_bits(),
            clip_threshold_bits: clip_threshold.max(0.01).to_bits(),
            use_ai_cfa,
        };
        if let Ok(cache) = self.opposed_chroma_cache.read() {
            if let Some(chroma) = cache.get(&key) {
                return *chroma;
            }
        }
        let ai_image = use_ai_cfa.then(|| self.ai_denoised_image()).flatten();
        let pixels = ai_image
            .as_ref()
            .and_then(AiDenoisedImage::bayer_cfa)
            .unwrap_or(self.raw_pixels.as_slice());
        let chroma = self.calculate_opposed_chroma(black_point, clip_threshold, pixels);
        if let Ok(mut cache) = self.opposed_chroma_cache.write() {
            cache.insert(key, chroma);
        }
        chroma
    }

    pub(crate) fn ai_denoised_image(&self) -> Option<AiDenoisedImage> {
        self.ai_denoised
            .read()
            .ok()
            .and_then(|image| image.as_ref().cloned())
            .filter(|image| image.is_valid_for(self.width, self.height))
    }

    pub(crate) fn set_ai_denoised_image(&self, image: AiDenoisedImage) -> Result<()> {
        anyhow::ensure!(
            image.is_valid_for(self.width, self.height),
            "AI-denoise result {}x{} does not match RAW {}x{}",
            image.width,
            image.height,
            self.width,
            self.height
        );
        anyhow::ensure!(
            matches!(self.cfa_kind, CfaKind::Bayer) == image.bayer_cfa().is_some(),
            "AI-denoise payload type does not match the RAW CFA"
        );
        let mut cached = self
            .ai_denoised
            .write()
            .map_err(|_| anyhow::anyhow!("AI-denoise cache lock was poisoned"))?;
        *cached = Some(image);
        if let Ok(mut chroma) = self.opposed_chroma_cache.write() {
            chroma.retain(|key, _| !key.use_ai_cfa);
        }
        Ok(())
    }

    pub(crate) fn clear_ai_denoised_image(&self) {
        if let Ok(mut cached) = self.ai_denoised.write() {
            *cached = None;
        }
        if let Ok(mut chroma) = self.opposed_chroma_cache.write() {
            chroma.retain(|key, _| !key.use_ai_cfa);
        }
    }

    /// ISO sensitivity retained from the RAW metadata. Zero means unavailable.
    pub fn iso_speed(&self) -> f32 {
        self.capture_metadata.iso_speed
    }

    /// Apply sensor-specific Detail-panel starting values without touching
    /// creative tone/color controls or the user's demosaic preferences.
    pub fn apply_adaptive_detail_defaults(&self, exposure: &mut ExposureParams) {
        let defaults = self.noise_profile.adaptive_detail_defaults(
            self.capture_metadata.iso_speed,
            self.white_levels,
            self.wb_coeffs,
        );
        exposure.luminance_denoise = defaults.luminance_denoise;
        exposure.chroma_denoise = defaults.chroma_denoise;
        exposure.denoise_detail = defaults.denoise_detail;
        exposure.denoise_quality = defaults.denoise_quality;
    }

    /// Estimated as-shot scene illuminant temperature used as the neutral point
    /// for the user-facing Kelvin control.
    pub fn as_shot_temperature_kelvin(&self) -> Option<f32> {
        self.white_balance_model
            .as_ref()
            .map(|model| model.base_cct)
            .filter(|temperature| temperature.is_finite() && *temperature > 0.0)
    }

    /// RawNIND's published Bayer weights were trained with a D65/daylight
    /// white balance. Return camera-channel multipliers normalized to green,
    /// falling back to the as-shot multipliers when a colour matrix is absent.
    pub(crate) fn rawnind_daylight_white_balance(&self) -> [f32; 3] {
        #[cfg(libraw_available)]
        if let Some(model) = &self.white_balance_model {
            if let Some(daylight) = libraw_loader::daylight_white_balance(model) {
                return daylight;
            }
        }

        let green = [self.wb_coeffs[1], self.wb_coeffs[3]]
            .into_iter()
            .filter(|value| value.is_finite() && *value > 0.0)
            .fold((0.0, 0u32), |(sum, count), value| (sum + value, count + 1));
        let green = if green.1 > 0 {
            green.0 / green.1 as f32
        } else {
            1.0
        };
        let normalize = |value: f32| {
            let value = value / green.max(1e-8);
            if value.is_finite() && value > 0.0 {
                value
            } else {
                1.0
            }
        };
        [
            normalize(self.wb_coeffs[0]),
            1.0,
            normalize(self.wb_coeffs[2]),
        ]
    }

    /// Returns the camera-to-working transform and DCP blend for a relative
    /// global white-balance edit. Temperature is expressed as a reciprocal-
    /// temperature (mired) displacement; tint is a Planckian-locus-normal Duv
    /// displacement. Both are converted through the selected camera matrices.
    pub(crate) fn adjusted_camera_transform(
        &self,
        temperature: f32,
        tint: f32,
    ) -> ([[f32; 4]; 3], f32) {
        if temperature.abs() < 1e-6 && tint.abs() < 1e-6 {
            return (self.cam_to_srgb, self.camera_profile.interpolation_weight);
        }
        #[cfg(libraw_available)]
        if let Some(model) = &self.white_balance_model {
            if let Some(adjusted) = libraw_loader::adjusted_camera_transform(
                model,
                temperature.clamp(-GLOBAL_TEMPERATURE_LIMIT, GLOBAL_TEMPERATURE_LIMIT),
                tint.clamp(-100.0, 100.0),
            ) {
                return adjusted;
            }
        }
        (self.cam_to_srgb, self.camera_profile.interpolation_weight)
    }
}

#[cfg(not(libraw_available))]
pub fn load_raw_file(_path: &Path) -> Result<LoadedRaw> {
    Err(anyhow!(
        "this build was compiled without LibRaw. Install LibRaw and make libraw.pc visible through PKG_CONFIG_PATH, then rebuild AuRaw."
    ))
}

#[cfg(not(libraw_available))]
pub fn load_raw_embedded_thumbnail(_path: &Path, _maximum_edge: u32) -> Result<RawThumbnail> {
    Err(anyhow!(
        "this build was compiled without LibRaw, so embedded RAW thumbnails are unavailable"
    ))
}

#[cfg(not(libraw_available))]
pub fn load_raw_thumbnail(_path: &Path, _maximum_edge: u32) -> Result<RawThumbnail> {
    Err(anyhow!(
        "this build was compiled without LibRaw, so RAW thumbnails are unavailable"
    ))
}

#[cfg(not(libraw_available))]
pub fn load_raw_display_dimensions(_path: &Path) -> Result<[u32; 2]> {
    Err(anyhow!(
        "this build was compiled without LibRaw, so RAW dimensions are unavailable"
    ))
}

#[cfg(not(libraw_available))]
pub fn load_raw_file_with_dcp(_path: &Path, _profile_path: &Path) -> Result<LoadedRaw> {
    Err(anyhow!(
        "this build was compiled without LibRaw. Install LibRaw and make libraw.pc visible through PKG_CONFIG_PATH, then rebuild AuRaw."
    ))
}

#[cfg(not(libraw_available))]
pub fn load_raw_file_with_profile_config(
    _path: &Path,
    _mode: CameraProfileMode,
    _profile_folder: Option<&Path>,
) -> Result<LoadedRaw> {
    Err(anyhow!(
        "this build was compiled without LibRaw. Install LibRaw and make libraw.pc visible through PKG_CONFIG_PATH, then rebuild AuRaw."
    ))
}

#[cfg(not(libraw_available))]
pub fn load_raw_file_with_profile_selection(
    _path: &Path,
    _mode: CameraProfileMode,
    _profile_folder: Option<&Path>,
    _selected_profile: Option<&Path>,
) -> Result<LoadedRaw> {
    Err(anyhow!(
        "this build was compiled without LibRaw. Install LibRaw and make libraw.pc visible through PKG_CONFIG_PATH, then rebuild AuRaw."
    ))
}

#[cfg(libraw_available)]
pub fn load_raw_file(path: &Path) -> Result<LoadedRaw> {
    libraw_loader::load_raw_file(path)
}

#[cfg(libraw_available)]
pub fn load_raw_file_with_profile_config(
    path: &Path,
    mode: CameraProfileMode,
    profile_folder: Option<&Path>,
) -> Result<LoadedRaw> {
    libraw_loader::load_raw_file_with_profile_config(path, mode, profile_folder)
}

#[cfg(libraw_available)]
pub fn load_raw_file_with_profile_selection(
    path: &Path,
    mode: CameraProfileMode,
    profile_folder: Option<&Path>,
    selected_profile: Option<&Path>,
) -> Result<LoadedRaw> {
    libraw_loader::load_raw_file_with_profile_selection(
        path,
        mode,
        profile_folder,
        selected_profile,
    )
}

#[cfg(libraw_available)]
pub fn load_raw_file_with_dcp(path: &Path, profile_path: &Path) -> Result<LoadedRaw> {
    libraw_loader::load_raw_file_with_dcp(path, profile_path)
}

#[cfg(libraw_available)]
pub fn load_raw_embedded_thumbnail(path: &Path, maximum_edge: u32) -> Result<RawThumbnail> {
    libraw_loader::load_raw_embedded_thumbnail(path, maximum_edge)
}

#[cfg(libraw_available)]
pub fn load_raw_thumbnail(path: &Path, maximum_edge: u32) -> Result<RawThumbnail> {
    libraw_loader::load_raw_thumbnail(path, maximum_edge)
}

#[cfg(libraw_available)]
pub fn load_raw_display_dimensions(path: &Path) -> Result<[u32; 2]> {
    libraw_loader::load_raw_display_dimensions(path)
}

#[cfg(libraw_available)]
pub(crate) fn invalidate_dcp_profile_index() {
    libraw_loader::invalidate_dcp_profile_index();
}

#[cfg(not(libraw_available))]
pub(crate) fn invalidate_dcp_profile_index() {}

#[cfg(libraw_available)]
pub(crate) fn prewarm_dcp_profile_index(folder: &Path) {
    libraw_loader::prewarm_dcp_profile_index(folder);
}

#[cfg(not(libraw_available))]
pub(crate) fn prewarm_dcp_profile_index(_folder: &Path) {}

#[cfg(libraw_available)]
mod libraw_loader;

#[cfg(all(test, libraw_available))]
mod tests {
    use super::{
        CameraColorModel, CameraProfile, CameraProfileMode, CameraWhiteBalanceModel, CfaKind,
        CompactPixelMap, LoadedRaw, GLOBAL_TEMPERATURE_LIMIT,
    };

    #[test]
    fn automatic_profile_mode_defaults_to_the_embedded_matrix() {
        assert!(!CameraProfileMode::Automatic.prefers_external_dcp());
        assert!(!CameraProfileMode::MatrixOnly.prefers_external_dcp());
        assert!(CameraProfileMode::DcpProfiles.prefers_external_dcp());
    }

    fn raw_with_white_balance_model() -> LoadedRaw {
        LoadedRaw {
            width: 1,
            height: 1,
            camera_make: "Test".to_owned(),
            camera_model: "Matrix".to_owned(),
            lens_make: String::new(),
            lens_model: String::new(),
            focal_length: 0.0,
            aperture: 0.0,
            focus_distance: 0.0,
            capture_metadata: Default::default(),
            cfa_kind: CfaKind::Bayer,
            raw_pixels: vec![0],
            color_indices: CompactPixelMap::dense(1, 1, vec![0]),
            wb_coeffs: [2.0, 1.0, 1.5, 1.0],
            cam_to_srgb: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            black_levels: [0.0; 4],
            black_levels_per_pixel: CompactPixelMap::dense(1, 1, vec![0.0]),
            white_levels: [1.0; 4],
            noise_profile: crate::pipeline::NoiseProfile::default(),
            camera_profile: CameraProfile::default(),
            camera_profile_source: None,
            available_camera_profiles: Vec::new(),
            white_balance_model: Some(CameraWhiteBalanceModel {
                base_wb: [2.0, 1.0, 1.5, 1.0],
                cdesc: *b"RGBG",
                base_cct: 5_000.0,
                color: CameraColorModel::Matrix {
                    xyz_to_camera: [
                        [1.0, 0.0, 0.0],
                        [0.0, 1.0, 0.0],
                        [0.0, 0.0, 1.0],
                        [0.0, 1.0, 0.0],
                    ],
                },
            }),
            lens_geometry: None,
            ai_denoised: std::sync::Arc::new(std::sync::RwLock::new(None)),
            opposed_chroma_cache: Default::default(),
        }
    }

    #[test]
    fn extended_temperature_range_reaches_beyond_the_old_hundred_mired_clamp() {
        let raw = raw_with_white_balance_model();
        let positive_hundred = raw.adjusted_camera_transform(100.0, 0.0).0;
        let positive_limit = raw
            .adjusted_camera_transform(GLOBAL_TEMPERATURE_LIMIT, 0.0)
            .0;
        let negative_hundred = raw.adjusted_camera_transform(-100.0, 0.0).0;
        let negative_limit = raw
            .adjusted_camera_transform(-GLOBAL_TEMPERATURE_LIMIT, 0.0)
            .0;

        assert_ne!(positive_hundred, positive_limit);
        assert_ne!(negative_hundred, negative_limit);
        assert_eq!(
            positive_limit,
            raw.adjusted_camera_transform(GLOBAL_TEMPERATURE_LIMIT + 50.0, 0.0)
                .0
        );
    }

    #[test]
    fn inpaint_opposed_chrominance_uses_darktable_cube_root_reference() {
        const WIDTH: u32 = 96;
        const HEIGHT: u32 = 96;
        const WHITE: f32 = 10_000.0;
        let mut raw = raw_with_white_balance_model();
        raw.width = WIDTH;
        raw.height = HEIGHT;
        raw.wb_coeffs = [1.0; 4];
        raw.white_levels = [WHITE; 4];
        raw.white_balance_model = None;

        let mut colors = Vec::with_capacity((WIDTH * HEIGHT) as usize);
        let mut pixels = Vec::with_capacity((WIDTH * HEIGHT) as usize);
        for row in 0..HEIGHT {
            for col in 0..WIDTH {
                let physical = match (col % 2, row % 2) {
                    (0, 0) => 0,
                    (1, 0) => 1,
                    (0, 1) => 3,
                    _ => 2,
                };
                colors.push(physical);
                let logical = if physical == 3 { 1 } else { physical };
                let mut value = [0.8, 0.6, 0.4][logical as usize];
                if logical == 0 && (42..54).contains(&col) && (42..54).contains(&row) {
                    value = 1.0;
                }
                pixels.push((value * WHITE).round() as u16);
            }
        }
        raw.raw_pixels = pixels;
        raw.color_indices = CompactPixelMap::dense(WIDTH, HEIGHT, colors);
        raw.black_levels_per_pixel = CompactPixelMap::repeating(WIDTH, HEIGHT, 1, 1, vec![0.0]);
        raw.opposed_chroma_cache = Default::default();

        let chroma = raw.inpaint_opposed_chroma(0.0, 1.0, false);
        let opposed_root = 0.5 * (0.6f32.cbrt() + 0.4f32.cbrt());
        let expected_red = 0.8 - opposed_root * opposed_root * opposed_root;
        assert!((chroma[0] - expected_red).abs() < 0.005, "{chroma:?}");
        assert!(chroma.iter().all(|value| value.is_finite()));
    }
}
