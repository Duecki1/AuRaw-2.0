use super::basicadj::GLOBAL_TEMPERATURE_LIMIT;
use super::color_profile::CameraProfile;
#[cfg(not(libraw_available))]
use anyhow::anyhow;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

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
    pub cfa_kind: CfaKind,
    pub raw_pixels: Vec<u16>,
    pub color_indices: Vec<u8>,
    pub wb_coeffs: [f32; 4],
    pub cam_to_srgb: [[f32; 4]; 3],
    pub black_levels: [f32; 4],
    /// Effective LibRaw black level for every oriented active-area photosite.
    /// This includes the shared level, per-CFA-plane offsets, and an optional
    /// repeating row/column pattern from `cblack[4..]`.
    pub black_levels_per_pixel: Vec<f32>,
    pub white_levels: [f32; 4],
    /// DCP creative profile stages and retained embedded camera ICC data.
    pub camera_profile: CameraProfile,
    /// External DCP actually applied to this RAW, when one was selected.
    pub camera_profile_source: Option<PathBuf>,
    /// All external DCPs in the configured root that match this camera.
    pub available_camera_profiles: Vec<CameraProfileCandidate>,
    /// Camera/DCP calibration data retained so global white-balance edits can
    /// rebuild the camera transform instead of applying generic RGB gains.
    pub(crate) white_balance_model: Option<CameraWhiteBalanceModel>,
}

impl LoadedRaw {
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
pub fn load_raw_thumbnail(_path: &Path, _maximum_edge: u32) -> Result<RawThumbnail> {
    Err(anyhow!(
        "this build was compiled without LibRaw, so RAW thumbnails are unavailable"
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
pub fn load_raw_thumbnail(path: &Path, maximum_edge: u32) -> Result<RawThumbnail> {
    libraw_loader::load_raw_thumbnail(path, maximum_edge)
}

#[cfg(libraw_available)]
mod libraw_loader;

#[cfg(all(test, libraw_available))]
mod tests {
    use super::{
        CameraColorModel, CameraProfile, CameraWhiteBalanceModel, CfaKind, LoadedRaw,
        GLOBAL_TEMPERATURE_LIMIT,
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
            cfa_kind: CfaKind::Bayer,
            raw_pixels: vec![0],
            color_indices: vec![0],
            wb_coeffs: [2.0, 1.0, 1.5, 1.0],
            cam_to_srgb: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
            ],
            black_levels: [0.0; 4],
            black_levels_per_pixel: vec![0.0],
            white_levels: [1.0; 4],
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
