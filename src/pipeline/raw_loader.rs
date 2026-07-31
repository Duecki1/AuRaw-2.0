use super::basicadj::{ExposureParams, GLOBAL_TEMPERATURE_LIMIT};
use super::color_profile::CameraProfile;
use super::geometry::LensGeometryMap;
use super::noise::NoiseProfile;
#[cfg(not(libraw_available))]
use anyhow::anyhow;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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
    /// Prefer a matching external DCP, then an embedded DNG/DCP profile, then
    /// fall back to the camera matrix.
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

/// Cached RawNIND output in the same white-balanced camera-RGB domain as the
/// ordinary demosaic stage. Samples are interleaved RGB IEEE-754 half floats.
/// Sidecars persist only the model toggle; this derived cache is always
/// rebuildable from the original sensor mosaic.
#[derive(Clone, Debug)]
pub struct AiDenoisedImage {
    pub width: u32,
    pub height: u32,
    pub rgb16f: Arc<[u16]>,
}

impl AiDenoisedImage {
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
        })
    }

    pub(crate) fn is_valid_for(&self, width: u32, height: u32) -> bool {
        self.width == width
            && self.height == height
            && u64::from(width)
                .checked_mul(u64::from(height))
                .and_then(|pixels| pixels.checked_mul(3))
                .and_then(|elements| usize::try_from(elements).ok())
                .is_some_and(|expected| self.rgb16f.len() == expected)
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
}

impl LoadedRaw {
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
        let mut cached = self
            .ai_denoised
            .write()
            .map_err(|_| anyhow::anyhow!("AI-denoise cache lock was poisoned"))?;
        *cached = Some(image);
        Ok(())
    }

    pub(crate) fn clear_ai_denoised_image(&self) {
        if let Ok(mut cached) = self.ai_denoised.write() {
            *cached = None;
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
        CameraColorModel, CameraProfile, CameraWhiteBalanceModel, CfaKind, CompactPixelMap,
        LoadedRaw, GLOBAL_TEMPERATURE_LIMIT,
    };

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
}
