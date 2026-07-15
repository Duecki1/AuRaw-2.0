use crate::pipeline::{
    ExposureParams, MaskGeometry, MaskKind, MaskStack, CURRENT_PROCESS_VERSION, MAX_LOCAL_MASKS,
};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const SIDECAR_SCHEMA_VERSION: u32 = 1;
pub const SIDECAR_SUFFIX: &str = ".auraw";
pub const MAX_SIDECAR_BYTES: u64 = if cfg!(target_os = "android") {
    32 * 1024 * 1024
} else {
    64 * 1024 * 1024
};

const SIDECAR_FORMAT: &str = "AuRaw edit sidecar";
const MAX_MASK_COMPONENTS: usize = 64;
const MAX_BRUSH_DABS: usize = 1_000_000;
const MAX_MASK_IMAGE_EDGE: u32 = 8192;
const MAX_EDIT_NAME_BYTES: usize = 4096;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

/// Stable location used by a background save worker. Android targets retain
/// the MediaStore URI because the native decode path itself uses a disposable
/// cache file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidecarTarget {
    Desktop {
        raw_path: PathBuf,
    },
    #[cfg(target_os = "android")]
    Android {
        raw_uri: String,
        display_name: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct LensEditState {
    pub enabled: bool,
    pub maker: String,
    pub model: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct EditState {
    pub exposure: ExposureParams,
    pub masks: Arc<MaskStack>,
    pub lens: LensEditState,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
struct SidecarDocument {
    format: String,
    schema_version: u32,
    process_version: u32,
    edits: EditState,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedSidecar {
    pub edits: EditState,
    /// True when an older supported schema or processing version was upgraded
    /// in memory. The next normal save rewrites it in the current format.
    pub migrated: bool,
}

#[derive(Debug)]
pub enum SidecarError {
    Io(std::io::Error),
    Invalid(String),
    Unsupported(String),
    Platform(String),
    TooLarge(u64),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Invalid(message) | Self::Unsupported(message) | Self::Platform(message) => {
                formatter.write_str(message)
            }
            Self::TooLarge(bytes) => write!(
                formatter,
                "sidecar is {bytes} bytes; the safety limit is {MAX_SIDECAR_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for SidecarError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Invalid(_) | Self::Unsupported(_) | Self::Platform(_) | Self::TooLarge(_) => None,
        }
    }
}

impl From<std::io::Error> for SidecarError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Appends rather than replaces the RAW extension: `photo.CR3` becomes
/// `photo.CR3.auraw`. Building from `OsString` preserves non-UTF-8 paths.
pub fn sidecar_path_for_raw(raw_path: &Path) -> PathBuf {
    let mut path: OsString = raw_path.as_os_str().to_owned();
    path.push(SIDECAR_SUFFIX);
    PathBuf::from(path)
}

pub fn encode(edits: EditState) -> Result<Vec<u8>, SidecarError> {
    validate_edit_state(&edits)?;
    preflight_encoded_images(&edits)?;
    if edits.exposure.process_version > CURRENT_PROCESS_VERSION {
        return Err(SidecarError::Unsupported(format!(
            "edit uses future processing version {} (this build supports {})",
            edits.exposure.process_version, CURRENT_PROCESS_VERSION
        )));
    }
    let document = SidecarDocument {
        format: SIDECAR_FORMAT.to_owned(),
        schema_version: SIDECAR_SCHEMA_VERSION,
        process_version: edits.exposure.process_version,
        edits,
    };
    let mut writer = CappedVec::new(MAX_SIDECAR_BYTES);
    serde_json::to_writer(&mut writer, &document).map_err(|error| {
        if writer.limit_reached {
            SidecarError::TooLarge(MAX_SIDECAR_BYTES + 1)
        } else {
            SidecarError::Invalid(format!("could not serialize edit: {error}"))
        }
    })?;
    Ok(writer.bytes)
}

pub fn decode(bytes: &[u8]) -> Result<LoadedSidecar, SidecarError> {
    if bytes.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(SidecarError::TooLarge(bytes.len() as u64));
    }
    let mut document: SidecarDocument = serde_json::from_slice(bytes)
        .map_err(|error| SidecarError::Invalid(format!("invalid sidecar JSON: {error}")))?;
    if document.format != SIDECAR_FORMAT {
        return Err(SidecarError::Invalid(
            "not an AuRaw edit sidecar".to_owned(),
        ));
    }
    if document.schema_version > SIDECAR_SCHEMA_VERSION {
        return Err(SidecarError::Unsupported(format!(
            "sidecar schema {} is newer than supported schema {}",
            document.schema_version, SIDECAR_SCHEMA_VERSION
        )));
    }
    if document.process_version != document.edits.exposure.process_version {
        return Err(SidecarError::Invalid(
            "sidecar processing versions do not agree".to_owned(),
        ));
    }
    if document.process_version > CURRENT_PROCESS_VERSION {
        return Err(SidecarError::Unsupported(format!(
            "sidecar uses future processing version {} (this build supports {})",
            document.process_version, CURRENT_PROCESS_VERSION
        )));
    }

    validate_edit_state(&document.edits)?;
    let original_schema = document.schema_version;
    let original_process = document.process_version;
    document.edits.exposure.migrate_to_current_process();
    document.edits.exposure.sanitize_tone_curves();
    for mask in &mut Arc::make_mut(&mut document.edits.masks).masks {
        mask.adjustments.sanitize_tone_curves();
    }
    validate_edit_state(&document.edits)?;

    Ok(LoadedSidecar {
        edits: document.edits,
        migrated: original_schema != SIDECAR_SCHEMA_VERSION
            || original_process != CURRENT_PROCESS_VERSION,
    })
}

pub fn load_desktop(raw_path: &Path) -> Result<Option<LoadedSidecar>, SidecarError> {
    let path = sidecar_path_for_raw(raw_path);
    match read_bounded(&path) {
        Ok(bytes) => decode(&bytes).map(Some),
        Err(SidecarError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn save_desktop(raw_path: &Path, edits: EditState) -> Result<PathBuf, SidecarError> {
    let path = sidecar_path_for_raw(raw_path);
    let bytes = encode(edits)?;
    atomic_write(&path, &bytes)?;
    Ok(path)
}

/// Synchronous worker API. Call from the existing RAW decode worker, never the
/// Android UI thread; the Java bridge materializes MediaStore data into a
/// bounded private cache file before JSON parsing.
#[cfg(target_os = "android")]
pub fn load_android(
    app: &android_activity::AndroidApp,
    raw_uri: &str,
    display_name: &str,
) -> Result<Option<LoadedSidecar>, SidecarError> {
    let Some(path) = crate::android::materialize_raw_sidecar(app, raw_uri, display_name)
        .map_err(SidecarError::Platform)?
    else {
        return Ok(None);
    };
    let result = read_bounded(&path).and_then(|bytes| decode(&bytes));
    if let Err(error) = fs::remove_file(&path) {
        log::warn!(
            "could not remove Android sidecar cache {}: {error}",
            path.display()
        );
    }
    result.map(Some)
}

/// Synchronous worker API. Serialization, fsync, and MediaStore publication
/// all happen on the caller's background thread.
#[cfg(target_os = "android")]
pub fn save_android(
    app: &android_activity::AndroidApp,
    raw_uri: &str,
    display_name: &str,
    edits: EditState,
) -> Result<String, SidecarError> {
    let bytes = encode(edits)?;
    let path = crate::android::create_raw_sidecar_cache(app).map_err(SidecarError::Platform)?;
    let result = write_synced(&path, &bytes).and_then(|()| {
        crate::android::publish_raw_sidecar(app, &path, raw_uri, display_name)
            .map_err(SidecarError::Platform)
    });
    if let Err(error) = fs::remove_file(&path) {
        log::warn!(
            "could not remove Android sidecar cache {}: {error}",
            path.display()
        );
    }
    result
}

pub(crate) fn read_bounded(path: &Path) -> Result<Vec<u8>, SidecarError> {
    let file = File::open(path)?;
    let declared = file.metadata()?.len();
    if declared > MAX_SIDECAR_BYTES {
        return Err(SidecarError::TooLarge(declared));
    }
    let mut bytes = Vec::with_capacity(declared as usize);
    file.take(MAX_SIDECAR_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(SidecarError::TooLarge(bytes.len() as u64));
    }
    Ok(bytes)
}

#[cfg(target_os = "android")]
pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), SidecarError> {
    if bytes.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(SidecarError::TooLarge(bytes.len() as u64));
    }
    let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SidecarError> {
    // A single relative component has `Some("")` as its parent. Opening that
    // empty path for the durability sync fails even though the rename already
    // succeeded, so normalize it to the current directory up front.
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| SidecarError::Invalid("sidecar path has no file name".to_owned()))?;
    let mut temporary_name = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = parent.join(temporary_name);

    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        sync_parent_directory(parent)?;
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(SidecarError::Io)
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
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

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> std::io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> std::io::Result<()> {
    Ok(())
}

fn validate_edit_state(edits: &EditState) -> Result<(), SidecarError> {
    validate_exposure(&edits.exposure)?;
    let stack = &edits.masks;
    if stack.masks.len() > MAX_LOCAL_MASKS {
        return invalid("sidecar contains too many local masks");
    }
    if stack
        .selected_mask
        .is_some_and(|index| index >= stack.masks.len())
    {
        return invalid("selected mask index is out of range");
    }
    if edits.lens.maker.len() > MAX_EDIT_NAME_BYTES || edits.lens.model.len() > MAX_EDIT_NAME_BYTES
    {
        return invalid("lens name is unreasonably long");
    }

    for (mask_index, mask) in stack.masks.iter().enumerate() {
        finite("mask opacity", &[mask.opacity])?;
        if !(0.0..=1.0).contains(&mask.opacity) {
            return invalid("mask opacity is outside 0..1");
        }
        validate_local_adjustments(&mask.adjustments)?;
        if mask.name.len() > MAX_EDIT_NAME_BYTES {
            return invalid("mask name is unreasonably long");
        }
        if mask.components.is_empty() || mask.components.len() > MAX_MASK_COMPONENTS {
            return invalid("mask has an invalid component count");
        }
        if stack.selected_mask == Some(mask_index)
            && stack
                .selected_component
                .is_some_and(|index| index >= mask.components.len())
        {
            return invalid("selected mask component index is out of range");
        }
        for component in &mask.components {
            if component.name.len() > MAX_EDIT_NAME_BYTES {
                return invalid("mask component name is unreasonably long");
            }
            if !geometry_matches_kind(component.kind, &component.geometry) {
                return invalid("mask component kind and geometry do not agree");
            }
            match &component.geometry {
                MaskGeometry::Brush {
                    size,
                    feather,
                    dabs,
                } => {
                    finite("brush geometry", &[*size, *feather])?;
                    bounded("brush size", *size, 0.0, 16.0)?;
                    bounded("brush feather", *feather, 0.0, 1.0)?;
                    if dabs.len() > MAX_BRUSH_DABS {
                        return invalid("brush mask contains too many dabs");
                    }
                    for dab in dabs {
                        finite(
                            "brush dab",
                            &[
                                dab.center[0],
                                dab.center[1],
                                dab.opacity,
                                dab.size,
                                dab.feather,
                            ],
                        )?;
                        bounded("brush dab x", dab.center[0], -16.0, 16.0)?;
                        bounded("brush dab y", dab.center[1], -16.0, 16.0)?;
                        bounded("brush dab opacity", dab.opacity, -1.0, 1.0)?;
                        bounded("brush dab size", dab.size, 0.0, 16.0)?;
                        bounded("brush dab feather", dab.feather, 0.0, 1.0)?;
                    }
                }
                MaskGeometry::Radial {
                    center,
                    radius,
                    rotation,
                    feather,
                    ..
                } => {
                    finite(
                        "radial geometry",
                        &[
                            center[0], center[1], radius[0], radius[1], *rotation, *feather,
                        ],
                    )?;
                    for value in center {
                        bounded("radial center", *value, -16.0, 16.0)?;
                    }
                    for value in radius {
                        bounded("radial radius", *value, 0.0, 16.0)?;
                    }
                    bounded("radial rotation", *rotation, -1_000_000.0, 1_000_000.0)?;
                    bounded("radial feather", *feather, 0.0, 1.0)?;
                }
                MaskGeometry::Linear {
                    start,
                    end,
                    feather,
                    ..
                } => {
                    finite(
                        "linear geometry",
                        &[start[0], start[1], end[0], end[1], *feather],
                    )?;
                    for value in start.iter().chain(end.iter()) {
                        bounded("linear point", *value, -16.0, 16.0)?;
                    }
                    bounded("linear feather", *feather, 0.0, 16.0)?;
                }
                MaskGeometry::Ai { mask, feather } => {
                    finite("AI mask feather", &[*feather])?;
                    bounded("AI mask feather", *feather, 0.0, 1.0)?;
                    if let Some(image) = mask {
                        validate_image(image.width, image.height, image.pixels.len(), 1)?;
                    }
                }
                MaskGeometry::LuminanceRange {
                    source,
                    low,
                    high,
                    feather,
                } => {
                    finite("luminance range mask", &[*low, *high, *feather])?;
                    bounded("luminance low", *low, -16.0, 16.0)?;
                    bounded("luminance high", *high, -16.0, 16.0)?;
                    bounded("luminance feather", *feather, 0.0, 16.0)?;
                    if let Some(image) = source {
                        validate_image(image.width, image.height, image.rgba.len(), 4)?;
                    }
                }
                MaskGeometry::ColorRange {
                    source,
                    sample,
                    tolerance,
                    feather,
                    ..
                } => {
                    finite(
                        "color range mask",
                        &[sample[0], sample[1], sample[2], *tolerance, *feather],
                    )?;
                    for value in sample {
                        bounded("color sample", *value, -16.0, 16.0)?;
                    }
                    bounded("color tolerance", *tolerance, 0.0, 16.0)?;
                    bounded("color feather", *feather, 0.0, 16.0)?;
                    if let Some(image) = source {
                        validate_image(image.width, image.height, image.rgba.len(), 4)?;
                    }
                }
                _ => {}
            }
        }
    }
    if stack.selected_mask.is_none() && stack.selected_component.is_some() {
        return invalid("a component is selected without a selected mask");
    }
    Ok(())
}

struct CappedVec {
    bytes: Vec<u8>,
    limit: u64,
    limit_reached: bool,
}

impl CappedVec {
    fn new(limit: u64) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            limit_reached: false,
        }
    }
}

impl Write for CappedVec {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next = self.bytes.len() as u64 + buffer.len() as u64;
        if next > self.limit {
            self.limit_reached = true;
            return Err(std::io::Error::other("sidecar size limit reached"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn preflight_encoded_images(edits: &EditState) -> Result<(), SidecarError> {
    // Leave room for brush dabs, curve points, names, and JSON structure. The
    // capped streaming writer remains authoritative for unusually large
    // non-image edit state.
    const STRUCTURE_HEADROOM: u64 = 1024 * 1024;
    let mut encoded_bytes = 0u64;
    for mask in &edits.masks.masks {
        for component in &mask.components {
            let image_bytes = match &component.geometry {
                MaskGeometry::Ai {
                    mask: Some(image), ..
                } => Some(image.pixels.len()),
                _ => None,
            };
            if let Some(image_bytes) = image_bytes {
                let base64_bytes = (image_bytes as u64)
                    .div_ceil(3)
                    .checked_mul(4)
                    .and_then(|bytes| bytes.checked_add(2))
                    .ok_or(SidecarError::TooLarge(u64::MAX))?;
                encoded_bytes = encoded_bytes
                    .checked_add(base64_bytes)
                    .ok_or(SidecarError::TooLarge(u64::MAX))?;
            }
        }
    }
    let estimated = encoded_bytes.saturating_add(STRUCTURE_HEADROOM);
    if estimated > MAX_SIDECAR_BYTES {
        Err(SidecarError::TooLarge(estimated))
    } else {
        Ok(())
    }
}

fn geometry_matches_kind(kind: MaskKind, geometry: &MaskGeometry) -> bool {
    matches!(
        (kind, geometry),
        (MaskKind::Brush, MaskGeometry::Brush { .. })
            | (MaskKind::Radial, MaskGeometry::Radial { .. })
            | (MaskKind::Linear, MaskGeometry::Linear { .. })
            | (
                MaskKind::Subject | MaskKind::Background,
                MaskGeometry::Ai { .. }
            )
            | (
                MaskKind::LuminanceRange,
                MaskGeometry::LuminanceRange { .. }
            )
            | (MaskKind::ColorRange, MaskGeometry::ColorRange { .. })
            | (
                MaskKind::Object | MaskKind::Landscape | MaskKind::DepthRange,
                MaskGeometry::Placeholder
            )
    )
}

fn validate_exposure(exposure: &ExposureParams) -> Result<(), SidecarError> {
    finite(
        "global adjustment",
        &[
            exposure.black_point,
            exposure.exposure,
            exposure.contrast,
            exposure.temperature,
            exposure.tint,
            exposure.saturation,
            exposure.vibrance,
            exposure.chroma_denoise,
            exposure.dual_threshold,
            exposure.frequency_chroma,
            exposure.ca_red,
            exposure.ca_blue,
            exposure.highlight_clip,
            exposure.highlight_reconstruction,
            exposure.highlight_color_adaptation,
            exposure.highlights,
            exposure.shadows,
            exposure.whites,
            exposure.blacks,
            exposure.texture,
            exposure.clarity,
            exposure.dehaze,
            exposure.glow_amount,
            exposure.glow_radius,
            exposure.glow_threshold,
            exposure.vignette_amount,
            exposure.vignette_midpoint,
            exposure.vignette_roundness,
            exposure.vignette_feather,
            exposure.vignette_highlights,
            exposure.sigmoid.contrast,
            exposure.sigmoid.skew,
            exposure.sigmoid.display_white_target,
            exposure.sigmoid.display_black_target,
            exposure.sigmoid.hue_preservation,
        ],
    )?;
    finite("global HSL hue", &exposure.hsl_hue)?;
    finite("global HSL saturation", &exposure.hsl_saturation)?;
    finite("global HSL luminance", &exposure.hsl_luminance)?;
    validate_curves(
        &[
            &exposure.tone_curve,
            &exposure.tone_curve_red,
            &exposure.tone_curve_green,
            &exposure.tone_curve_blue,
        ],
        "global tone curve",
    )?;
    validate_grading(&exposure.color_grading, "global color grading")
}

fn validate_local_adjustments(
    adjustments: &crate::pipeline::LocalAdjustments,
) -> Result<(), SidecarError> {
    finite(
        "local adjustment",
        &[
            adjustments.exposure,
            adjustments.contrast,
            adjustments.highlights,
            adjustments.shadows,
            adjustments.whites,
            adjustments.blacks,
            adjustments.temperature,
            adjustments.tint,
            adjustments.saturation,
            adjustments.texture,
            adjustments.clarity,
            adjustments.dehaze,
        ],
    )?;
    finite("local HSL hue", &adjustments.hsl_hue)?;
    finite("local HSL saturation", &adjustments.hsl_saturation)?;
    finite("local HSL luminance", &adjustments.hsl_luminance)?;
    validate_curves(
        &[
            &adjustments.tone_curve,
            &adjustments.tone_curve_red,
            &adjustments.tone_curve_green,
            &adjustments.tone_curve_blue,
        ],
        "local tone curve",
    )?;
    validate_grading(&adjustments.color_grading, "local color grading")
}

fn validate_curves(
    curves: &[&crate::pipeline::PointCurve],
    label: &str,
) -> Result<(), SidecarError> {
    for curve in curves {
        if !(2..=crate::pipeline::MAX_POINT_CURVE_POINTS as u32).contains(&curve.len) {
            return invalid("tone curve point count is invalid");
        }
        for point in curve.points {
            finite(label, &point)?;
        }
    }
    Ok(())
}

fn validate_grading(
    grading: &crate::pipeline::ColorGrading,
    label: &str,
) -> Result<(), SidecarError> {
    finite(
        label,
        &[
            grading.shadows.hue,
            grading.shadows.saturation,
            grading.shadows.luminance,
            grading.midtones.hue,
            grading.midtones.saturation,
            grading.midtones.luminance,
            grading.highlights.hue,
            grading.highlights.saturation,
            grading.highlights.luminance,
            grading.global.hue,
            grading.global.saturation,
            grading.global.luminance,
            grading.blending,
            grading.balance,
        ],
    )
}

fn finite(label: &str, values: &[f32]) -> Result<(), SidecarError> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        invalid(&format!("{label} contains a non-finite value"))
    }
}

fn bounded(label: &str, value: f32, minimum: f32, maximum: f32) -> Result<(), SidecarError> {
    if (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        invalid(&format!("{label} is outside the safe range"))
    }
}

fn validate_image(
    width: u32,
    height: u32,
    bytes: usize,
    channels: usize,
) -> Result<(), SidecarError> {
    if width == 0 || height == 0 || width > MAX_MASK_IMAGE_EDGE || height > MAX_MASK_IMAGE_EDGE {
        return invalid("mask image dimensions are invalid");
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| SidecarError::Invalid("mask image dimensions overflow".to_owned()))?;
    if bytes != expected {
        return invalid("mask image byte count does not match its dimensions");
    }
    Ok(())
}

fn invalid<T>(message: &str) -> Result<T, SidecarError> {
    Err(SidecarError::Invalid(message.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{MaskKind, CURRENT_PROCESS_VERSION};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_edits() -> EditState {
        let mut exposure = ExposureParams::scene_referred_default();
        exposure.dehaze = 27.0;
        let mut masks = MaskStack::default();
        masks.add_mask(MaskKind::Radial);
        EditState {
            exposure,
            masks: Arc::new(masks),
            lens: LensEditState {
                enabled: true,
                maker: "Test Optics".to_owned(),
                model: "35 mm f/2".to_owned(),
            },
        }
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "auraw-sidecar-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn sidecar_round_trip_preserves_edit_state() {
        let edits = sample_edits();
        let encoded = encode(edits.clone()).unwrap();
        let loaded = decode(&encoded).unwrap();
        assert_eq!(loaded.edits, edits);
        assert!(!loaded.migrated);
    }

    #[test]
    fn old_processing_state_is_migrated_deliberately() {
        let mut edits = sample_edits();
        edits.exposure.process_version = 4;
        let value = serde_json::to_vec(&SidecarDocument {
            format: SIDECAR_FORMAT.to_owned(),
            schema_version: 0,
            process_version: 4,
            edits,
        })
        .unwrap();
        let loaded = decode(&value).unwrap();
        assert_eq!(
            loaded.edits.exposure.process_version,
            CURRENT_PROCESS_VERSION
        );
        assert!(loaded.migrated);
    }

    #[test]
    fn corrupt_and_future_sidecars_are_rejected() {
        assert!(matches!(
            decode(br#"{"schema_version":1,"#),
            Err(SidecarError::Invalid(_))
        ));

        let edits = sample_edits();
        let future = SidecarDocument {
            format: SIDECAR_FORMAT.to_owned(),
            schema_version: SIDECAR_SCHEMA_VERSION + 1,
            process_version: CURRENT_PROCESS_VERSION,
            edits,
        };
        assert!(matches!(
            decode(&serde_json::to_vec(&future).unwrap()),
            Err(SidecarError::Unsupported(_))
        ));

        let mut non_finite = sample_edits();
        non_finite.exposure.exposure = f32::NAN;
        assert!(matches!(
            encode(non_finite),
            Err(SidecarError::Invalid(message)) if message.contains("non-finite")
        ));

        let mut unsafe_geometry = sample_edits();
        if let MaskGeometry::Radial { radius, .. } =
            &mut Arc::make_mut(&mut unsafe_geometry.masks).masks[0].components[0].geometry
        {
            radius[0] = 1.0e30;
        }
        assert!(matches!(
            encode(unsafe_geometry),
            Err(SidecarError::Invalid(message)) if message.contains("safe range")
        ));
    }

    #[test]
    fn desktop_save_is_atomic_and_uses_appended_suffix() {
        let directory = temporary_directory("atomic");
        let raw = directory.join("photo.CR3");
        fs::write(&raw, b"raw").unwrap();
        let edits = sample_edits();
        let path = save_desktop(&raw, edits.clone()).unwrap();
        assert_eq!(path.file_name().unwrap(), "photo.CR3.auraw");
        assert_eq!(load_desktop(&raw).unwrap().unwrap().edits, edits);
        assert_eq!(
            fs::read_dir(&directory)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
                .count(),
            0
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reconstructible_range_source_is_not_persisted() {
        use crate::pipeline::{MaskCombineMode, MaskComponent, MaskRgbImage};

        let mut edits = sample_edits();
        let width = 2048;
        let height = 2048;
        let source = MaskRgbImage::new(
            width,
            height,
            vec![127; width as usize * height as usize * 4],
        )
        .unwrap();
        Arc::make_mut(&mut edits.masks).masks[0].components[0] = MaskComponent {
            name: "Luminance Range".to_owned(),
            kind: MaskKind::LuminanceRange,
            combine: MaskCombineMode::Add,
            enabled: true,
            invert: false,
            geometry: MaskGeometry::LuminanceRange {
                source: Some(source),
                low: 0.2,
                high: 0.8,
                feather: 0.15,
            },
        };
        let encoded = encode(edits).unwrap();
        assert!(encoded.len() < 64 * 1024);
        let loaded = decode(&encoded).unwrap();
        assert!(matches!(
            &loaded.edits.masks.masks[0].components[0].geometry,
            MaskGeometry::LuminanceRange { source: None, .. }
        ));
    }

    #[test]
    fn repeated_shared_range_sources_stay_small() {
        use crate::pipeline::{MaskCombineMode, MaskComponent, MaskRgbImage};

        let mut edits = sample_edits();
        let width = 2048;
        let height = 2048;
        let source = MaskRgbImage::new(
            width,
            height,
            vec![63; width as usize * height as usize * 4],
        )
        .unwrap();
        let component = MaskComponent {
            name: "Range".to_owned(),
            kind: MaskKind::LuminanceRange,
            combine: MaskCombineMode::Add,
            enabled: true,
            invert: false,
            geometry: MaskGeometry::LuminanceRange {
                source: Some(source),
                low: 0.2,
                high: 0.8,
                feather: 0.15,
            },
        };
        Arc::make_mut(&mut edits.masks).masks[0].components = vec![component; 3];
        assert!(encode(edits).unwrap().len() < 64 * 1024);
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_raw_paths_keep_their_exact_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let raw = PathBuf::from(OsString::from_vec(b"photo-\xff.NEF".to_vec()));
        assert_eq!(
            sidecar_path_for_raw(&raw).as_os_str().as_bytes(),
            b"photo-\xff.NEF.auraw"
        );
    }

    #[test]
    fn relative_sidecar_parent_is_the_current_directory() {
        let path = Path::new("photo.NEF.auraw");
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        assert_eq!(parent, Path::new("."));
    }
}
