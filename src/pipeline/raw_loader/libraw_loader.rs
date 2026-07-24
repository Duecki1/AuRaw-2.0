use super::{
    validate_raw_dimensions, CameraColorModel, CameraProfile, CameraProfileCandidate,
    CameraProfileMode, CameraWhiteBalanceModel, CfaKind, CompactPixelMap, DngColorEndpoint, LoadedRaw, RawThumbnail,
    MAX_RAW_FILE_BYTES, MAX_SENSOR_EDGE, MAX_SENSOR_PIXELS,
};
use crate::pipeline::color_profile::{DcpMatrixSet, DcpProfile};
use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::fs;
use std::io::Cursor;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

const MAX_DCP_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DCP_SCAN_FILES: usize = 10_000;
const MAX_DCP_SCAN_DEPTH: usize = 16;
/// Conservative default only for RAWs that provide no usable DNG
/// BaselineExposure. This is deliberately much smaller than the historical
/// universal +0.7 EV renderer lift and is applied exactly once, before any
/// user Exposure edit.
const MISSING_BASELINE_EXPOSURE_FALLBACK_EV: f32 = 0.25;

fn valid_baseline_exposure(value: f32) -> Option<f32> {
    // LibRaw initializes a missing BaselineExposure to a finite sentinel below
    // -999 EV. Reject that sentinel and corrupt/non-finite values.
    (value.is_finite() && value > -999.0).then_some(value)
}

fn resolve_default_exposure_ev(baseline_exposure: Option<f32>, profile_offset_ev: f32) -> f32 {
    let baseline = baseline_exposure.unwrap_or(MISSING_BASELINE_EXPOSURE_FALLBACK_EV);
    // DNG BaselineExposureOffset is a profile-specific delta to the image
    // baseline. Combine the two named metadata terms once; never add a second
    // renderer-only exposure constant. Keep pathological metadata bounded.
    (baseline + profile_offset_ev).clamp(-5.0, 5.0)
}
#[cfg(target_os = "android")]
const MAX_EMBEDDED_THUMBNAIL_BYTES: usize = 64 * 1024 * 1024;
#[cfg(not(target_os = "android"))]
const MAX_EMBEDDED_THUMBNAIL_BYTES: usize = 128 * 1024 * 1024;
#[cfg(target_os = "android")]
const MAX_THUMBNAIL_SOURCE_EDGE: u32 = 8_192;
#[cfg(not(target_os = "android"))]
const MAX_THUMBNAIL_SOURCE_EDGE: u32 = 65_535;
#[cfg(target_os = "android")]
const MAX_THUMBNAIL_DECODE_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(not(target_os = "android"))]
const MAX_THUMBNAIL_DECODE_BYTES: u64 = 256 * 1024 * 1024;
#[cfg(target_os = "android")]
static ANDROID_PROCESSED_THUMBNAIL_GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(target_os = "android")]
const MAX_ANDROID_THUMBNAIL_FALLBACK_SENSOR_PIXELS: u64 = MAX_SENSOR_PIXELS;

// Rec.2020 and the camera profiles used here are D65-referred. Normalizing
// XYZ -> camera rows against equal-energy XYZ (1, 1, 1) makes an otherwise
// neutral camera value warm. These coordinates make camera neutral map to
// the Rec.2020 neutral axis instead.
const D65_XYZ: [f32; 3] = [0.9504559, 1.0, 1.0890578];
const XYZ_TO_REC2020: [[f32; 3]; 3] = [
    [1.7166512, -0.3556708, -0.2533663],
    [-0.6666844, 1.6164812, 0.0157685],
    [0.0176399, -0.0427706, 0.9421031],
];

#[allow(
    clippy::upper_case_acronyms,
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals
)]
mod ffi {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

pub fn load_raw_file(path: &Path) -> Result<LoadedRaw> {
    load_raw_file_with_profile_config(path, CameraProfileMode::Automatic, None)
}

pub fn load_raw_file_with_profile_config(
    path: &Path,
    mode: CameraProfileMode,
    profile_folder: Option<&Path>,
) -> Result<LoadedRaw> {
    load_raw_file_with_profile_selection(path, mode, profile_folder, None)
}

pub fn load_raw_file_with_profile_selection(
    path: &Path,
    mode: CameraProfileMode,
    profile_folder: Option<&Path>,
    selected_profile: Option<&Path>,
) -> Result<LoadedRaw> {
    validate_input_file(path, MAX_RAW_FILE_BYTES, "RAW input")?;

    let c_path = path_to_libraw_cstring(path)?;
    let ctx = LibRawContext::new()?;
    let identify_started = Instant::now();
    check_libraw(
        // SAFETY: `ctx.raw` is a live LibRaw handle owned by `ctx`, and `c_path` remains alive for the call.
        unsafe { ffi::libraw_open_file(ctx.raw, c_path.as_ptr()) },
        "open RAW file",
    )?;
    crate::diagnostics::record(format!(
        "LibRaw identify/open_file finished in {:.3}s",
        identify_started.elapsed().as_secs_f64()
    ));
    // SAFETY: opening the RAW populates identity and geometry metadata.
    unsafe { validate_opened_raw_geometry(&ctx) }?;

    let profile_metadata_started = Instant::now();
    let embedded_profile = match mode {
        CameraProfileMode::MatrixOnly | CameraProfileMode::DcpProfiles => None,
        CameraProfileMode::Automatic => read_optional_profile(path),
    };
    let raw_camera_signature = match mode {
        CameraProfileMode::Automatic => embedded_profile
            .as_ref()
            .and_then(|profile| profile.camera_calibration_signature.clone()),
        CameraProfileMode::DcpProfiles => {
            read_optional_profile(path).and_then(|profile| profile.camera_calibration_signature)
        }
        CameraProfileMode::MatrixOnly => None,
    };
    crate::diagnostics::record(format!(
        "Embedded camera-profile metadata read in {:.3}s",
        profile_metadata_started.elapsed().as_secs_f64()
    ));

    // SAFETY: LibRaw has identified the file and iparams strings are initialized.
    let (camera_make, camera_model) = unsafe {
        let iparams = &(*ctx.raw).rawdata.iparams;
        (
            c_array_to_string(&iparams.make),
            c_array_to_string(&iparams.model),
        )
    };

    let external_profiles_started = Instant::now();
    let mut matches = match mode {
        CameraProfileMode::MatrixOnly => Vec::new(),
        CameraProfileMode::DcpProfiles | CameraProfileMode::Automatic => profile_folder
            .map(|folder| find_matching_dcp_profiles(folder, &camera_make, &camera_model))
            .transpose()
            .unwrap_or_else(|error| {
                if let Some(folder) = profile_folder {
                    log::warn!(
                        "could not search DCP profile folder {}: {error:#}",
                        folder.display()
                    );
                }
                None
            })
            .unwrap_or_default(),
    };
    crate::diagnostics::record(format!(
        "External camera-profile lookup finished in {:.3}s",
        external_profiles_started.elapsed().as_secs_f64()
    ));

    let available_camera_profiles = matches
        .iter()
        .map(|candidate| CameraProfileCandidate {
            path: candidate.path.clone(),
            name: candidate.name.clone(),
        })
        .collect::<Vec<_>>();

    let explicitly_selected = selected_profile.and_then(|requested| {
        matches
            .iter()
            .position(|candidate| candidate.path == requested)
            .map(|index| matches.remove(index))
    });
    if selected_profile.is_some() && explicitly_selected.is_none() {
        crate::diagnostics::record(format!(
            "Camera profile: requested profile is not a valid match for {} {}; falling back to automatic selection",
            camera_make, camera_model
        ));
    }
    let external_profile =
        explicitly_selected.or_else(|| (!matches.is_empty()).then(|| matches.remove(0)));

    let (selected_profile_path, selected_profile) = if let Some(mut candidate) = external_profile {
        candidate.profile.camera_calibration_signature = raw_camera_signature;
        crate::diagnostics::record(format!(
            "Camera profile: external DCP '{}' for {} {}",
            candidate.path.display(),
            camera_make,
            camera_model
        ));
        (Some(candidate.path), Some(candidate.profile))
    } else {
        let profile = match (mode, embedded_profile) {
            (CameraProfileMode::Automatic, Some(profile)) => {
                crate::diagnostics::record(format!(
                    "Camera profile: embedded DNG/DCP profile for {} {}",
                    camera_make, camera_model
                ));
                Some(profile)
            }
            (CameraProfileMode::DcpProfiles, _) => {
                crate::diagnostics::record(format!(
                    "Camera profile: no matching external DCP for {} {}; using camera matrix",
                    camera_make, camera_model
                ));
                None
            }
            (CameraProfileMode::Automatic, None) => {
                crate::diagnostics::record(format!(
                    "Camera profile: automatic fallback to camera matrix for {} {}",
                    camera_make, camera_model
                ));
                None
            }
            (CameraProfileMode::MatrixOnly, _) => {
                crate::diagnostics::record(format!(
                    "Camera profile: matrix-only for {} {}",
                    camera_make, camera_model
                ));
                None
            }
        };
        (None, profile)
    };

    // SAFETY: the context is valid and exclusively owned by this worker.
    let unpack_started = Instant::now();
    check_libraw(unsafe { ffi::libraw_unpack(ctx.raw) }, "unpack RAW file")?;
    crate::diagnostics::record(format!(
        "LibRaw sensor unpack finished in {:.3}s",
        unpack_started.elapsed().as_secs_f64()
    ));
    // SAFETY: unpack succeeded and the converter validates all exposed buffers.
    let materialize_started = Instant::now();
    let mut loaded = unsafe { loaded_raw_from_context(&ctx, selected_profile) }?;
    crate::diagnostics::record(format!(
        "Decoded mosaic materialization finished in {:.3}s",
        materialize_started.elapsed().as_secs_f64()
    ));
    loaded.camera_profile_source = selected_profile_path;
    loaded.available_camera_profiles = available_camera_profiles;
    Ok(loaded)
}

pub fn load_raw_file_with_dcp(path: &Path, profile_path: &Path) -> Result<LoadedRaw> {
    validate_input_file(path, MAX_RAW_FILE_BYTES, "RAW input")?;
    validate_input_file(profile_path, MAX_DCP_FILE_BYTES, "DCP profile")?;
    let mut selected = DcpProfile::from_path(profile_path)
        .with_context(|| format!("read DCP profile {}", profile_path.display()))?
        .ok_or_else(|| anyhow!("{} is not a DNG camera profile", profile_path.display()))?;

    // CameraCalibration belongs to the raw DNG, while the compatibility
    // signature belongs to the selected profile. Carry the camera-side
    // signature into an external profile before evaluating the matrix path.
    if let Some(raw_profile) = read_optional_profile(path) {
        selected.camera_calibration_signature = raw_profile.camera_calibration_signature;
    }
    let display_name = dcp_profile_display_name(selected.name.as_deref(), profile_path);
    let mut loaded = load_raw_file_with_selected_profile(path, Some(selected))?;
    loaded.camera_profile_source = Some(profile_path.to_path_buf());
    loaded.available_camera_profiles = vec![CameraProfileCandidate {
        path: profile_path.to_path_buf(),
        name: display_name,
    }];
    Ok(loaded)
}

/// Reads the display-oriented active image dimensions from the RAW header only.
/// This deliberately stops after LibRaw identify/open_file, so the library can
/// reserve stable thumbnail geometry without unpacking sensor pixels or decoding
/// the embedded preview.
pub fn load_raw_display_dimensions(path: &Path) -> Result<[u32; 2]> {
    validate_input_file(path, MAX_RAW_FILE_BYTES, "RAW dimension input")?;
    let ctx = open_libraw(path)?;

    // SAFETY: open_libraw completed identify and owns the context exclusively.
    let sizes = unsafe { &(*ctx.raw).rawdata.sizes };
    let width = u32::from(sizes.width);
    let height = u32::from(sizes.height);
    anyhow::ensure!(width > 0 && height > 0, "LibRaw header reports empty active dimensions");

    // LibRaw orientation 5/6 rotates the active image by 90/270 degrees. 180°
    // keeps the same dimensions. Unknown orientations are left unswapped here;
    // full RAW loading will still perform its stricter validation later.
    Ok(match sizes.flip {
        5 | 6 => [height, width],
        _ => [width, height],
    })
}

/// Loads a display-ready sRGB thumbnail without unpacking the sensor data when
/// the RAW contains an embedded preview. Files without a usable embedded
/// preview fall back to LibRaw's half-size preview processing on this worker
/// thread. Android only permits that sensor-unpack fallback for small RAWs;
/// large preview-less files keep a placeholder instead of risking an
/// out-of-memory termination while a Develop image is resident.
pub fn load_raw_thumbnail(path: &Path, maximum_edge: u32) -> Result<RawThumbnail> {
    validate_input_file(path, MAX_RAW_FILE_BYTES, "RAW thumbnail input")?;
    anyhow::ensure!(maximum_edge > 0, "thumbnail edge must be non-zero");

    match load_embedded_thumbnail(path, maximum_edge) {
        Ok(thumbnail) => Ok(thumbnail),
        Err(embedded_error) => load_processed_thumbnail(path, maximum_edge)
            .with_context(|| format!("embedded RAW preview was unavailable ({embedded_error:#})")),
    }
}

fn open_libraw(path: &Path) -> Result<LibRawContext> {
    let c_path = path_to_libraw_cstring(path)?;
    let ctx = LibRawContext::new()?;
    check_libraw(
        // SAFETY: `ctx.raw` is a live LibRaw handle and `c_path` remains alive for the call.
        unsafe { ffi::libraw_open_file(ctx.raw, c_path.as_ptr()) },
        "open RAW thumbnail",
    )?;
    // Embedded preview extraction does not allocate the full active sensor.
    // Enforce conservative header edge/overflow limits here, but leave the
    // stricter platform pixel budget to full RAW decode and the fallback path.
    // This lets Android show embedded previews from modern 60–100 MP cameras.
    unsafe { validate_opened_thumbnail_geometry(&ctx) }?;
    Ok(ctx)
}

fn load_embedded_thumbnail(path: &Path, maximum_edge: u32) -> Result<RawThumbnail> {
    let ctx = open_libraw(path)?;
    validate_embedded_thumbnail_header(&ctx)?;
    // SAFETY: the context is live and exclusively owned by this thread.
    check_libraw(
        unsafe { ffi::libraw_unpack_thumb(ctx.raw) },
        "unpack RAW thumbnail",
    )?;
    // `sizes.flip` describes the sensor image, but TIFF and CR3 files may store
    // a preview with a different orientation. Resolve the preview selected by
    // `libraw_unpack_thumb` back to its per-thumbnail metadata when available.
    let orientation = embedded_thumbnail_orientation(&ctx);

    let mut error = 0;
    // SAFETY: unpack_thumb succeeded and LibRaw returns an owned allocation or null.
    let image = unsafe { ffi::libraw_dcraw_make_mem_thumb(ctx.raw, &mut error) };
    let image = ProcessedImage::new(image, error, "make in-memory RAW thumbnail")?;
    // SAFETY: `image` owns a live LibRaw processed-image allocation.
    unsafe { thumbnail_from_processed(&image, maximum_edge, orientation) }
}

fn validate_embedded_thumbnail_header(ctx: &LibRawContext) -> Result<()> {
    // SAFETY: `open_libraw` completed identify and owns this context
    // exclusively. These header fields are populated before thumbnail unpack.
    let thumbnail = unsafe { &(*ctx.raw).thumbnail };
    validate_embedded_thumbnail_metadata(
        thumbnail.tformat,
        thumbnail.twidth,
        thumbnail.theight,
        thumbnail.tlength,
        thumbnail.tcolors,
    )
}

fn validate_embedded_thumbnail_metadata(
    format: ffi::LibRaw_thumbnail_formats,
    width: u16,
    height: u16,
    length: u32,
    colors: i32,
) -> Result<()> {
    let length = usize::try_from(length).context("embedded RAW preview length overflow")?;
    anyhow::ensure!(
        length > 0 && length <= MAX_EMBEDDED_THUMBNAIL_BYTES,
        "embedded RAW preview payload size {length} is outside the safe range"
    );

    match format {
        ffi::LibRaw_thumbnail_formats_LIBRAW_THUMBNAIL_JPEG => {
            // Some proprietary formats do not expose JPEG dimensions until
            // the payload is parsed. Validate dimensions here when present;
            // `thumbnail_from_processed` validates the JPEG header itself.
            if width != 0 || height != 0 {
                anyhow::ensure!(
                    width > 0
                        && height > 0
                        && u32::from(width) <= MAX_THUMBNAIL_SOURCE_EDGE
                        && u32::from(height) <= MAX_THUMBNAIL_SOURCE_EDGE,
                    "embedded JPEG preview {width}x{height} is outside the safe dimension range"
                );
            }
        }
        ffi::LibRaw_thumbnail_formats_LIBRAW_THUMBNAIL_BITMAP => {
            anyhow::ensure!(
                width > 0
                    && height > 0
                    && u32::from(width) <= MAX_THUMBNAIL_SOURCE_EDGE
                    && u32::from(height) <= MAX_THUMBNAIL_SOURCE_EDGE,
                "embedded bitmap preview {width}x{height} is outside the safe dimension range"
            );
            anyhow::ensure!(
                matches!(colors, 1 | 3),
                "unsupported {colors}-channel embedded bitmap preview"
            );
            let expected = usize::from(width)
                .checked_mul(usize::from(height))
                .and_then(|pixels| pixels.checked_mul(colors as usize))
                .context("embedded bitmap preview byte count overflow")?;
            anyhow::ensure!(
                expected <= length
                    && u64::try_from(expected).unwrap_or(u64::MAX) <= MAX_THUMBNAIL_DECODE_BYTES,
                "embedded bitmap preview metadata requires {expected} bytes but declares {length}"
            );
        }
        _ => {
            return Err(anyhow!("unsupported embedded RAW preview format {format}"));
        }
    }
    Ok(())
}

fn load_processed_thumbnail(path: &Path, maximum_edge: u32) -> Result<RawThumbnail> {
    #[cfg(target_os = "android")]
    let _android_memory_gate = ANDROID_PROCESSED_THUMBNAIL_GATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let ctx = open_libraw(path)?;
    #[cfg(target_os = "android")]
    {
        // Even half-size dcraw processing first unpacks the full sensor. The
        // memory gate above prevents multiple preview-less RAWs from taking
        // this path concurrently. Permit the same sensor safety ceiling as a
        // normal Android RAW decode so imported modern-camera files can still
        // receive a library preview when their embedded preview is unsupported.
        let sizes = unsafe { &(*ctx.raw).rawdata.sizes };
        let sensor_pixels = u64::from(sizes.raw_width)
            .checked_mul(u64::from(sizes.raw_height))
            .context("RAW thumbnail fallback sensor dimensions overflow")?;
        anyhow::ensure!(
            sensor_pixels <= MAX_ANDROID_THUMBNAIL_FALLBACK_SENSOR_PIXELS,
            "embedded preview is unavailable and the {sensor_pixels}-pixel sensor exceeds the Android sensor safety limit"
        );
    }
    // SAFETY: this context is exclusively owned and its params are initialized by libraw_init.
    unsafe {
        (*ctx.raw).params.half_size = 1;
        (*ctx.raw).params.use_camera_wb = 1;
        (*ctx.raw).params.output_color = 1; // sRGB
        (*ctx.raw).params.output_bps = 8;
        // Preserve LibRaw's metadata-driven orientation. A value of zero would
        // explicitly suppress it, while -1 asks LibRaw to use the camera value.
        (*ctx.raw).params.user_flip = -1;
    }
    // SAFETY: the live context is used serially on this worker thread.
    check_libraw(
        unsafe { ffi::libraw_unpack(ctx.raw) },
        "unpack RAW thumbnail fallback",
    )?;
    // SAFETY: unpack succeeded and processing uses the initialized output params above.
    check_libraw(
        unsafe { ffi::libraw_dcraw_process(ctx.raw) },
        "process RAW thumbnail fallback",
    )?;

    let mut error = 0;
    // SAFETY: dcraw_process succeeded and LibRaw returns an owned allocation or null.
    let image = unsafe { ffi::libraw_dcraw_make_mem_image(ctx.raw, &mut error) };
    let image = ProcessedImage::new(image, error, "make fallback RAW thumbnail")?;
    // dcraw_process already applies orientation.
    unsafe { thumbnail_from_processed(&image, maximum_edge, 0) }
}

fn embedded_thumbnail_orientation(ctx: &LibRawContext) -> i32 {
    // SAFETY: callers hold a live context after open_file/unpack_thumb and only
    // read the metadata arrays owned by that context.
    unsafe {
        let raw = &*ctx.raw;
        let selected = &raw.thumbnail;
        let thumbnail_list = &raw.thumbs_list;
        let count = usize::try_from(thumbnail_list.thumbcount)
            .unwrap_or(0)
            .min(thumbnail_list.thumblist.len());
        matching_thumbnail_orientation(
            (selected.twidth, selected.theight, selected.tlength),
            thumbnail_list.thumblist[..count].iter().map(|thumbnail| {
                (
                    thumbnail.twidth,
                    thumbnail.theight,
                    thumbnail.tlength,
                    thumbnail.tflip,
                )
            }),
        )
        .unwrap_or(raw.rawdata.sizes.flip)
    }
}

fn matching_thumbnail_orientation(
    selected: (u16, u16, u32),
    candidates: impl IntoIterator<Item = (u16, u16, u32, u16)>,
) -> Option<i32> {
    candidates.into_iter().find_map(|candidate| {
        let (width, height, length, flip) = candidate;
        (flip != u16::MAX && (width, height, length) == selected).then_some(i32::from(flip))
    })
}

struct ProcessedImage(*mut ffi::libraw_processed_image_t);

impl ProcessedImage {
    fn new(image: *mut ffi::libraw_processed_image_t, error: i32, action: &str) -> Result<Self> {
        if image.is_null() {
            check_libraw(error, action)?;
            return Err(anyhow!("LibRaw failed to {action}: no image was returned"));
        }
        Ok(Self(image))
    }
}

impl Drop for ProcessedImage {
    fn drop(&mut self) {
        // SAFETY: this is the allocation returned by a LibRaw make_mem call and
        // it is released exactly once here.
        unsafe { ffi::libraw_dcraw_clear_mem(self.0) };
    }
}

unsafe fn thumbnail_from_processed(
    image: &ProcessedImage,
    maximum_edge: u32,
    orientation: i32,
) -> Result<RawThumbnail> {
    let processed = &*image.0;
    let data_size = processed.data_size as usize;
    anyhow::ensure!(
        data_size > 0 && data_size <= MAX_EMBEDDED_THUMBNAIL_BYTES,
        "LibRaw thumbnail payload size {data_size} is outside the safe range"
    );
    let data = std::slice::from_raw_parts(processed.data.as_ptr(), data_size);

    let decoded = match processed.type_ {
        ffi::LibRaw_image_formats_LIBRAW_IMAGE_JPEG => {
            let dimensions_reader =
                image::ImageReader::with_format(Cursor::new(data), image::ImageFormat::Jpeg);
            let (width, height) = dimensions_reader
                .into_dimensions()
                .context("inspect embedded JPEG thumbnail")?;
            anyhow::ensure!(
                width > 0
                    && height > 0
                    && width <= MAX_THUMBNAIL_SOURCE_EDGE
                    && height <= MAX_THUMBNAIL_SOURCE_EDGE,
                "embedded JPEG thumbnail {width}x{height} is outside the safe dimension range"
            );
            let decoded_bytes = u64::from(width)
                .checked_mul(u64::from(height))
                .and_then(|pixels| pixels.checked_mul(3))
                .context("embedded JPEG thumbnail byte count overflow")?;
            anyhow::ensure!(
                decoded_bytes <= MAX_THUMBNAIL_DECODE_BYTES,
                "embedded JPEG thumbnail requires at least {decoded_bytes} decoded bytes, exceeding the safe allocation limit"
            );
            let mut reader =
                image::ImageReader::with_format(Cursor::new(data), image::ImageFormat::Jpeg);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(MAX_THUMBNAIL_SOURCE_EDGE);
            limits.max_image_height = Some(MAX_THUMBNAIL_SOURCE_EDGE);
            limits.max_alloc = Some(MAX_THUMBNAIL_DECODE_BYTES);
            reader.limits(limits);
            reader.decode().context("decode embedded JPEG thumbnail")?
        }
        ffi::LibRaw_image_formats_LIBRAW_IMAGE_BITMAP => {
            anyhow::ensure!(
                processed.bits == 8,
                "unsupported {}-bit RAW thumbnail",
                processed.bits
            );
            let width = u32::from(processed.width);
            let height = u32::from(processed.height);
            let colors = usize::from(processed.colors);
            anyhow::ensure!(
                width > 0 && height > 0,
                "LibRaw returned an empty bitmap thumbnail"
            );
            anyhow::ensure!(
                width <= MAX_THUMBNAIL_SOURCE_EDGE && height <= MAX_THUMBNAIL_SOURCE_EDGE,
                "LibRaw bitmap thumbnail {width}x{height} exceeds the safe edge limit"
            );
            anyhow::ensure!(
                matches!(colors, 1 | 3),
                "unsupported {colors}-channel RAW thumbnail"
            );
            let pixels = usize::try_from(width)
                .ok()
                .and_then(|width| {
                    usize::try_from(height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .context("RAW thumbnail dimensions overflow")?;
            let expected = pixels
                .checked_mul(colors)
                .context("RAW thumbnail byte count overflow")?;
            anyhow::ensure!(
                u64::try_from(expected).unwrap_or(u64::MAX) <= MAX_THUMBNAIL_DECODE_BYTES,
                "LibRaw bitmap thumbnail requires {expected} decoded bytes, exceeding the safe allocation limit"
            );
            anyhow::ensure!(data.len() >= expected, "truncated bitmap RAW thumbnail");
            if colors == 3 {
                let buffer = image::RgbImage::from_raw(width, height, data[..expected].to_vec())
                    .context("invalid RGB RAW thumbnail buffer")?;
                image::DynamicImage::ImageRgb8(buffer)
            } else {
                let buffer = image::GrayImage::from_raw(width, height, data[..expected].to_vec())
                    .context("invalid grayscale RAW thumbnail buffer")?;
                image::DynamicImage::ImageLuma8(buffer)
            }
        }
        format => return Err(anyhow!("unsupported LibRaw thumbnail format {format}")),
    };

    // Shrink before applying mirrored/rotated orientation. Applying it to the
    // full embedded preview creates another full-resolution allocation, which
    // is especially costly while Android still retains a Develop pipeline.
    let mut oriented = crate::thumbnail_cache::downscale_to_fit(decoded, maximum_edge);
    let transform = match orientation {
        0 => image::metadata::Orientation::NoTransforms,
        1 => image::metadata::Orientation::FlipHorizontal,
        2 => image::metadata::Orientation::FlipVertical,
        3 => image::metadata::Orientation::Rotate180,
        4 => image::metadata::Orientation::Rotate90FlipH,
        5 => image::metadata::Orientation::Rotate270,
        6 => image::metadata::Orientation::Rotate90,
        7 => image::metadata::Orientation::Rotate270FlipH,
        _ => image::metadata::Orientation::NoTransforms,
    };
    oriented.apply_orientation(transform);
    let thumbnail = oriented.to_rgba8();
    let (width, height) = thumbnail.dimensions();
    Ok(RawThumbnail {
        width,
        height,
        rgba: thumbnail.into_raw(),
    })
}

fn validate_input_file(path: &Path, maximum_bytes: u64, label: &str) -> Result<()> {
    let source =
        fs::metadata(path).with_context(|| format!("inspect {label} {}", path.display()))?;
    anyhow::ensure!(source.is_file(), "{label} is not a regular file");
    anyhow::ensure!(source.len() > 0, "{label} is empty");
    anyhow::ensure!(
        source.len() <= maximum_bytes,
        "{label} is {} bytes; the safe input limit is {maximum_bytes}",
        source.len()
    );
    Ok(())
}

struct MatchedDcpProfile {
    score: i32,
    path: PathBuf,
    name: String,
    profile: DcpProfile,
}

#[derive(Clone)]
struct IndexedDcpProfile {
    path: PathBuf,
    name: String,
    profile_name: Option<String>,
    camera_model: Option<String>,
}

#[derive(Clone)]
struct CachedDcpIndex {
    root_modified: Option<SystemTime>,
    profiles: Arc<Vec<IndexedDcpProfile>>,
}

static DCP_PROFILE_INDEX: OnceLock<Mutex<HashMap<PathBuf, CachedDcpIndex>>> = OnceLock::new();
static DCP_PROFILE_SCAN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn dcp_profile_index_cache() -> &'static Mutex<HashMap<PathBuf, CachedDcpIndex>> {
    DCP_PROFILE_INDEX.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn invalidate_dcp_profile_index() {
    if let Some(cache) = DCP_PROFILE_INDEX.get() {
        if let Ok(mut cache) = cache.lock() {
            cache.clear();
        }
    }
}

pub(super) fn prewarm_dcp_profile_index(folder: &Path) {
    if let Err(error) = indexed_dcp_profiles(folder) {
        log::warn!(
            "could not prewarm DCP profile index for {}: {error:#}",
            folder.display()
        );
    }
}

fn indexed_dcp_profiles(folder: &Path) -> Result<Arc<Vec<IndexedDcpProfile>>> {
    let metadata = fs::metadata(folder)
        .with_context(|| format!("inspect DCP profile folder {}", folder.display()))?;
    anyhow::ensure!(metadata.is_dir(), "configured DCP profile path is not a folder");
    let cache_key = fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    let root_modified = metadata.modified().ok();

    if let Ok(cache) = dcp_profile_index_cache().lock() {
        if let Some(index) = cache.get(&cache_key) {
            if index.root_modified == root_modified {
                return Ok(Arc::clone(&index.profiles));
            }
        }
    }

    // Startup prewarming and a very early RAW open can race. Serialize index
    // construction so they never scan and parse the same profile tree twice.
    let _scan_guard = DCP_PROFILE_SCAN_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .ok();
    if let Ok(cache) = dcp_profile_index_cache().lock() {
        if let Some(index) = cache.get(&cache_key) {
            if index.root_modified == root_modified {
                return Ok(Arc::clone(&index.profiles));
            }
        }
    }

    let mut stack = vec![(folder.to_path_buf(), 0usize)];
    let mut scanned = 0usize;
    let mut profiles = Vec::new();
    'scan: while let Some((directory, depth)) = stack.pop() {
        if depth > MAX_DCP_SCAN_DEPTH {
            continue;
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                log::warn!("could not read DCP directory {}: {error}", directory.display());
                continue;
            }
        };
        for entry in entries {
            if scanned >= MAX_DCP_SCAN_FILES {
                log::warn!("stopped DCP profile scan after {MAX_DCP_SCAN_FILES} filesystem entries");
                break 'scan;
            }
            scanned += 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    log::warn!("could not inspect DCP directory entry: {error}");
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() && !file_type.is_symlink() {
                stack.push((entry.path(), depth + 1));
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let path = entry.path();
            if !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dcp"))
            {
                continue;
            }
            match entry.metadata() {
                Ok(metadata) if metadata.len() > 0 && metadata.len() <= MAX_DCP_FILE_BYTES => {}
                _ => continue,
            }
            let identity = match DcpProfile::identity_from_path(&path) {
                Ok(Some(identity)) => identity,
                Ok(None) => continue,
                Err(error) => {
                    log::warn!("ignoring invalid DCP profile identity {}: {error:#}", path.display());
                    continue;
                }
            };
            let name = dcp_profile_display_name(identity.name.as_deref(), &path);
            profiles.push(IndexedDcpProfile {
                path,
                name,
                profile_name: identity.name,
                camera_model: identity.camera_model,
            });
        }
    }

    let profiles = Arc::new(profiles);
    if let Ok(mut cache) = dcp_profile_index_cache().lock() {
        cache.insert(
            cache_key,
            CachedDcpIndex {
                root_modified,
                profiles: Arc::clone(&profiles),
            },
        );
    }
    crate::diagnostics::record(format!(
        "Indexed {} DCP profiles after scanning {scanned} filesystem entries",
        profiles.len()
    ));
    Ok(profiles)
}

fn find_matching_dcp_profiles(
    folder: &Path,
    camera_make: &str,
    camera_model: &str,
) -> Result<Vec<MatchedDcpProfile>> {
    let make_key = normalize_camera_name(camera_make);
    let model_key = normalize_camera_name(camera_model);
    let combined_key = normalize_camera_name(&format!("{camera_make} {camera_model}"));
    if model_key.is_empty() {
        return Ok(Vec::new());
    }

    let index = indexed_dcp_profiles(folder)?;
    let mut matches = Vec::new();
    for indexed in index.iter() {
        let score = dcp_match_score(
            indexed.camera_model.as_deref(),
            indexed.profile_name.as_deref(),
            &indexed.path,
            &make_key,
            &model_key,
            &combined_key,
        );
        if score <= 0 {
            continue;
        }
        let profile = match DcpProfile::from_path(&indexed.path) {
            Ok(Some(profile)) => profile,
            Ok(None) => continue,
            Err(error) => {
                log::warn!(
                    "ignoring matched but invalid DCP profile {}: {error:#}",
                    indexed.path.display()
                );
                continue;
            }
        };
        matches.push(MatchedDcpProfile {
            score,
            path: indexed.path.clone(),
            name: indexed.name.clone(),
            profile,
        });
    }

    matches.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                left.name
                    .to_ascii_lowercase()
                    .cmp(&right.name.to_ascii_lowercase())
            })
            .then_with(|| {
                left.path
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .cmp(&right.path.to_string_lossy().to_ascii_lowercase())
            })
    });

    // ProfileName is not guaranteed unique. Keep the friendly name when it is
    // unique, otherwise append the filename so the editor dropdown can
    // distinguish every camera-matched profile deterministically.
    for index in 0..matches.len() {
        let duplicate = matches.iter().enumerate().any(|(other_index, other)| {
            other_index != index && other.name.eq_ignore_ascii_case(&matches[index].name)
        });
        if duplicate {
            let file_name = matches[index]
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned);
            if let Some(file_name) = file_name {
                let base_name = matches[index].name.clone();
                matches[index].name = format!("{base_name} — {file_name}");
            }
        }
    }

    Ok(matches)
}

fn dcp_profile_display_name(profile_name: Option<&str>, path: &Path) -> String {
    profile_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "Camera profile".to_owned())
}

fn dcp_match_score(
    camera_model: Option<&str>,
    profile_name: Option<&str>,
    path: &Path,
    make_key: &str,
    model_key: &str,
    combined_key: &str,
) -> i32 {
    let declared = camera_model
        .map(normalize_camera_name)
        .unwrap_or_default();
    let filename = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(normalize_camera_name)
        .unwrap_or_default();
    let profile_name = profile_name
        .map(normalize_camera_name)
        .unwrap_or_default();
    let path_key = normalize_camera_name(&path.to_string_lossy());

    let mut score = if !declared.is_empty() {
        if declared == model_key || declared == combined_key {
            1000
        } else if model_key.len() >= 4 && declared.contains(model_key) {
            900
        } else {
            return 0;
        }
    } else if filename == model_key || filename == combined_key {
        700
    } else if model_key.len() >= 4 && filename.contains(model_key) {
        600
    } else if model_key.len() >= 4 && path_key.contains(model_key) {
        // Some profile packs use generic filenames such as "Camera ST.dcp"
        // inside a camera-model directory. The configured root may therefore
        // contain many cameras while the parent folder provides the identity.
        550
    } else {
        return 0;
    };

    if !make_key.is_empty()
        && (declared.contains(make_key)
            || filename.contains(make_key)
            || path_key.contains(make_key))
    {
        score += 25;
    }
    // When a folder contains several creative variants for one camera, prefer
    // a neutral/default camera profile deterministically rather than a vivid or
    // monochrome look. Exact camera metadata matching still dominates.
    if profile_name.contains("adobestandard") || filename.contains("adobestandard") {
        score += 20;
    } else if profile_name.contains("camerastandard")
        || filename.contains("camerastandard")
        || profile_name.ends_with("camerast")
        || filename.ends_with("camerast")
    {
        score += 15;
    }
    if profile_name.contains("monochrome")
        || filename.contains("monochrome")
        || profile_name.ends_with("camerabw")
        || filename.ends_with("camerabw")
    {
        score -= 30;
    }
    score
}

fn normalize_camera_name(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn read_optional_profile(path: &Path) -> Option<DcpProfile> {
    // DCP tags can be embedded directly in a DNG. Treat malformed optional
    // creative-profile metadata as non-fatal while preserving a diagnostic.
    match DcpProfile::from_path(path) {
        Ok(profile) => profile,
        Err(error) => {
            log::warn!(
                "ignoring malformed embedded DCP profile in {}: {error:#}",
                path.display()
            );
            None
        }
    }
}

fn load_raw_file_with_selected_profile(
    path: &Path,
    dcp_profile: Option<DcpProfile>,
) -> Result<LoadedRaw> {
    validate_input_file(path, MAX_RAW_FILE_BYTES, "RAW input")?;

    let c_path = path_to_libraw_cstring(path)?;
    let ctx = LibRawContext::new()?;

    check_libraw(
        // SAFETY: `ctx.raw` is a live LibRaw handle owned by `ctx`, and `c_path` is a NUL-terminated CString that remains alive for the call.
        unsafe { ffi::libraw_open_file(ctx.raw, c_path.as_ptr()) },
        "open RAW file",
    )?;
    // LibRaw exposes dimensions after open_file. Reject hostile geometry
    // before unpack can allocate the full decoded sensor buffer.
    // SAFETY: the file is open and LibRaw has initialized its size metadata; the helper only reads fields from the live context.
    unsafe { validate_opened_raw_geometry(&ctx) }?;
    // SAFETY: `ctx.raw` is valid and exclusively used on this worker thread; LibRaw owns the decoded buffers until `ctx` is dropped.
    check_libraw(unsafe { ffi::libraw_unpack(ctx.raw) }, "unpack RAW file")?;

    // SAFETY: unpack succeeded, so LibRaw buffer pointers and dimensions are initialized; the converter validates all lengths before dereferencing.
    unsafe { loaded_raw_from_context(&ctx, dcp_profile) }
}

#[cfg(unix)]
fn path_to_libraw_cstring(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .with_context(|| format!("RAW path contains an interior NUL byte: {}", path.display()))
}

#[cfg(not(unix))]
fn path_to_libraw_cstring(path: &Path) -> Result<CString> {
    let utf8 = path.to_str().with_context(|| {
        format!(
            "LibRaw requires a Unicode path on this platform: {}",
            path.display()
        )
    })?;
    CString::new(utf8.as_bytes())
        .with_context(|| format!("RAW path contains an interior NUL byte: {}", path.display()))
}

struct LibRawContext {
    raw: *mut ffi::libraw_data_t,
}

impl LibRawContext {
    fn new() -> Result<Self> {
        // SAFETY: LibRaw accepts a zero flags value and returns either a new owned handle or null.
        let raw = unsafe { ffi::libraw_init(0) };
        if raw.is_null() {
            Err(anyhow!("libraw_init returned null"))
        } else {
            Ok(Self { raw })
        }
    }
}

impl Drop for LibRawContext {
    fn drop(&mut self) {
        // SAFETY: `self.raw` is the unique handle returned by `libraw_init` and is closed exactly once here.
        unsafe {
            ffi::libraw_close(self.raw);
        }
    }
}

unsafe fn validate_opened_thumbnail_geometry(ctx: &LibRawContext) -> Result<()> {
    let raw = &*ctx.raw;
    let sizes = &raw.rawdata.sizes;
    let active_width = u32::from(sizes.width);
    let active_height = u32::from(sizes.height);
    if active_width != 0 || active_height != 0 {
        anyhow::ensure!(
            active_width > 0
                && active_height > 0
                && active_width <= MAX_SENSOR_EDGE
                && active_height <= MAX_SENSOR_EDGE,
            "LibRaw header reports active dimensions {active_width}x{active_height} outside the thumbnail safety limit"
        );
        active_width
            .checked_mul(active_height)
            .context("RAW thumbnail active pixel count overflow")?;
    }

    // A few containers expose embedded-preview metadata before LibRaw has
    // populated raw sensor geometry. That is safe for `unpack_thumb`: payload
    // dimensions and bytes are independently bounded below. Validate sensor
    // geometry when present, but do not reject an otherwise valid preview just
    // because those unrelated fields are zero.
    let sensor_width = u32::from(sizes.raw_width);
    let sensor_height = u32::from(sizes.raw_height);
    if sensor_width != 0 || sensor_height != 0 {
        anyhow::ensure!(
            sensor_width > 0
                && sensor_height > 0
                && sensor_width <= MAX_SENSOR_EDGE
                && sensor_height <= MAX_SENSOR_EDGE,
            "LibRaw header reports sensor dimensions {sensor_width}x{sensor_height} outside the thumbnail safety limit"
        );
        sensor_width
            .checked_mul(sensor_height)
            .context("RAW thumbnail header pixel count overflow")?;
    }
    Ok(())
}

unsafe fn validate_opened_raw_geometry(ctx: &LibRawContext) -> Result<()> {
    let raw = &*ctx.raw;
    let sizes = &raw.rawdata.sizes;
    let active_width = sizes.width as u32;
    let active_height = sizes.height as u32;
    validate_raw_dimensions(active_width, active_height)
        .context("LibRaw header reports an image too large to unpack safely")?;

    let sensor_width = sizes.raw_width as u32;
    let sensor_height = sizes.raw_height as u32;
    anyhow::ensure!(
        sensor_width > 0 && sensor_height > 0,
        "LibRaw header reports empty sensor dimensions"
    );
    anyhow::ensure!(
            sensor_width <= MAX_SENSOR_EDGE && sensor_height <= MAX_SENSOR_EDGE,
            "LibRaw sensor dimensions {sensor_width}x{sensor_height} exceed the {MAX_SENSOR_EDGE}-pixel edge limit"
        );
    let sensor_pixels = u64::from(sensor_width)
        .checked_mul(u64::from(sensor_height))
        .context("LibRaw sensor pixel count overflow")?;
    anyhow::ensure!(
            sensor_pixels <= MAX_SENSOR_PIXELS,
            "LibRaw sensor dimensions {sensor_width}x{sensor_height} contain {sensor_pixels} pixels; the safe unpack limit is {MAX_SENSOR_PIXELS}"
        );
    let minimum_pitch = u64::from(sensor_width)
        .checked_mul(std::mem::size_of::<u16>() as u64)
        .context("LibRaw sensor pitch overflow")?;
    let raw_pitch = u64::from(sizes.raw_pitch);
    // Some LibRaw decoders leave raw_pitch at zero until unpack. The
    // sensor pixel cap still bounds that allocation; validate a declared
    // pitch only when the header actually supplies one.
    anyhow::ensure!(
        raw_pitch == 0 || (raw_pitch >= minimum_pitch && raw_pitch <= 1_073_741_824),
        "LibRaw header reports invalid raw pitch {raw_pitch} for width {sensor_width}"
    );
    Ok(())
}

unsafe fn loaded_raw_from_context(
    ctx: &LibRawContext,
    dcp_profile: Option<DcpProfile>,
) -> Result<LoadedRaw> {
    let raw = &*ctx.raw;
    let rawdata = &raw.rawdata;
    let sizes = &rawdata.sizes;
    let color = &rawdata.color;
    let iparams = &rawdata.iparams;
    let lens = &raw.lens;
    let other = &raw.other;

    if rawdata.raw_image.is_null() {
        return Err(anyhow!(
            "LibRaw did not expose a single-channel raw_image buffer"
        ));
    }

    let raw_width = sizes.raw_width as u32;
    let raw_height = sizes.raw_height as u32;
    let crop_x = sizes.left_margin as u32;
    let crop_y = sizes.top_margin as u32;
    let width = sizes.width as u32;
    let height = sizes.height as u32;
    validate_raw_dimensions(width, height)
        .context("LibRaw reported an image too large to process safely")?;
    if !sizes.pixel_aspect.is_finite() || sizes.pixel_aspect <= 0.0 {
        return Err(anyhow!(
            "LibRaw reported invalid pixel aspect ratio {}",
            sizes.pixel_aspect
        ));
    }
    if (sizes.pixel_aspect - 1.0).abs() > 1e-6 {
        return Err(anyhow!(
                "non-square RAW pixels (aspect {}) require a geometry-resampling stage that AuRaw does not implement yet",
                sizes.pixel_aspect
            ));
    }
    if !matches!(sizes.flip, 0 | 3 | 5 | 6) {
        return Err(anyhow!(
            "unsupported LibRaw orientation code {}; expected 0, 3, 5, or 6",
            sizes.flip
        ));
    }

    let crop_right = crop_x
        .checked_add(width)
        .ok_or_else(|| anyhow!("LibRaw horizontal crop overflow"))?;
    let crop_bottom = crop_y
        .checked_add(height)
        .ok_or_else(|| anyhow!("LibRaw vertical crop overflow"))?;
    if crop_right > raw_width || crop_bottom > raw_height {
        return Err(anyhow!(
            "LibRaw crop is outside RAW bounds: crop {}x{} at {},{} in {}x{}",
            width,
            height,
            crop_x,
            crop_y,
            raw_width,
            raw_height
        ));
    }

    let cfa_kind = cfa_kind_from_filters(iparams.filters)?;
    let cdesc = cdesc4(iparams);
    let cfa_map = canonical_cfa_map(cdesc)?;
    let physical_black_levels = black_levels(color.black, &color.cblack);
    let (width, height, raw_pixels, color_indices, black_levels_per_pixel) = copy_active_pixels(
        ctx.raw,
        rawdata.raw_image,
        raw_width,
        raw_height,
        crop_x,
        crop_y,
        width,
        height,
        sizes.raw_pitch as usize,
        sizes.flip,
        cfa_kind,
        cdesc,
        cfa_map,
        color.black,
        &color.cblack,
    )?;
    let physical_wb = white_balance(color.cam_mul, cdesc);
    let wb_coeffs = canonicalize_f32x4(physical_wb, cfa_map);
    let calibration_compatible = dcp_profile
        .as_ref()
        .is_none_or(DcpProfile::calibration_is_compatible);
    let (cam_to_srgb, profile_weight, white_balance_model) = camera_to_working_matrix(
        color,
        physical_wb,
        cdesc,
        dcp_profile.as_ref(),
        calibration_compatible,
    )?;
    let black_levels = canonicalize_f32x4(physical_black_levels, cfa_map);
    // LibRaw changed `linear_max` from `long[4]` in the 0.21 series
    // to `unsigned[4]` in newer releases. Bindgen therefore exposes it
    // as either `[i64; 4]` or `[u32; 4]`, depending on the installed
    // headers. Normalize both representations and reject negative or
    // otherwise out-of-range metadata values.
    let linear_max = color.linear_max.map(normalize_libraw_linear_max);
    let white_levels = canonicalize_f32x4(
        white_levels(color.maximum, linear_max, physical_black_levels),
        cfa_map,
    );

    let mut camera_profile = dcp_profile
        .map(|profile| CameraProfile::from_dcp(profile, profile_weight))
        .unwrap_or_default();
    let baseline_exposure = valid_baseline_exposure(color.dng_levels.baseline_exposure);
    camera_profile.default_exposure_ev = resolve_default_exposure_ev(
        baseline_exposure,
        camera_profile.profile_exposure_offset_ev,
    );
    if !color.profile.is_null() && color.profile_length > 0 {
        let length = usize::try_from(color.profile_length).unwrap_or(0);
        if length <= 16 * 1024 * 1024 {
            let source = std::slice::from_raw_parts(color.profile as *const u8, length);
            let mut profile = Vec::new();
            profile
                .try_reserve_exact(length)
                .context("reserve embedded camera ICC profile")?;
            profile.extend_from_slice(source);
            camera_profile.embedded_camera_icc = Some(profile);
        } else {
            log::warn!("ignoring embedded camera ICC profile larger than 16 MiB");
        }
    }

    Ok(LoadedRaw {
        width,
        height,
        camera_make: c_array_to_string(&iparams.make),
        camera_model: c_array_to_string(&iparams.model),
        lens_make: c_array_to_string(&lens.LensMake),
        lens_model: c_array_to_string(&lens.Lens),
        focal_length: finite_positive_or_zero(other.focal_len),
        aperture: finite_positive_or_zero(other.aperture),
        focus_distance: 0.0,
        capture_metadata: super::CaptureMetadata {
            iso_speed: finite_positive_or_zero(other.iso_speed),
            shutter_seconds: finite_positive_or_zero(other.shutter),
            description: c_array_to_string(&other.desc),
            artist: c_array_to_string(&other.artist),
        },
        cfa_kind,
        raw_pixels,
        color_indices,
        wb_coeffs,
        cam_to_srgb,
        black_levels,
        black_levels_per_pixel,
        white_levels,
        camera_profile,
        camera_profile_source: None,
        available_camera_profiles: Vec::new(),
        white_balance_model,
    })
}

fn normalize_libraw_linear_max<T>(value: T) -> u32
where
    u32: TryFrom<T>,
{
    u32::try_from(value).unwrap_or(0)
}

type ActivePixelData = (
    u32,
    u32,
    Vec<u16>,
    CompactPixelMap<u8>,
    CompactPixelMap<f32>,
);

#[allow(clippy::too_many_arguments)]
unsafe fn copy_active_pixels(
    raw: *mut ffi::libraw_data_t,
    raw_image: *const u16,
    raw_width: u32,
    raw_height: u32,
    crop_x: u32,
    crop_y: u32,
    width: u32,
    height: u32,
    raw_pitch: usize,
    flip: i32,
    cfa_kind: CfaKind,
    cdesc: [u8; 4],
    cfa_map: [u8; 4],
    shared_black: u32,
    cblack: &[u32],
) -> Result<ActivePixelData> {
    let raw_width = raw_width as usize;
    let raw_height = raw_height as usize;
    let crop_x = crop_x as usize;
    let crop_y = crop_y as usize;
    let width = width as usize;
    let height = height as usize;
    let row_bytes = raw_width
        .checked_mul(std::mem::size_of::<u16>())
        .ok_or_else(|| anyhow!("RAW row size overflow"))?;
    let pitch = if raw_pitch == 0 { row_bytes } else { raw_pitch };
    if pitch < row_bytes {
        return Err(anyhow!(
            "LibRaw raw_pitch ({pitch}) is smaller than one decoded row ({row_bytes})"
        ));
    }
    if pitch % std::mem::align_of::<u16>() != 0 {
        return Err(anyhow!("LibRaw raw_pitch ({pitch}) is not u16-aligned"));
    }

    let crop_right = crop_x
        .checked_add(width)
        .ok_or_else(|| anyhow!("active RAW horizontal crop overflow"))?;
    let crop_bottom = crop_y
        .checked_add(height)
        .ok_or_else(|| anyhow!("active RAW vertical crop overflow"))?;
    if crop_bottom > raw_height || crop_right > raw_width {
        return Err(anyhow!("active RAW crop exceeds decoded RAW buffer"));
    }

    let (out_width, out_height) = match flip {
        5 | 6 => (height, width),
        _ => (width, height),
    };
    let output_len = out_width
        .checked_mul(out_height)
        .ok_or_else(|| anyhow!("oriented RAW dimensions overflow"))?;
    validate_raw_dimensions(out_width as u32, out_height as u32)?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(output_len)
        .context("reserve oriented RAW pixel buffer")?;
    pixels.resize(output_len, 0);

    // Copy the actual mosaic first. The common unrotated case can move whole
    // rows at once instead of performing one pointer calculation per pixel.
    if flip == 0 {
        for y in 0..height {
            let raw_y = crop_y + y;
            let row_offset = raw_y
                .checked_mul(pitch)
                .ok_or_else(|| anyhow!("RAW row pointer offset overflow"))?;
            let row_ptr = (raw_image as *const u8).add(row_offset) as *const u16;
            let source = std::slice::from_raw_parts(row_ptr.add(crop_x), width);
            let destination = &mut pixels[y * out_width..(y + 1) * out_width];
            destination.copy_from_slice(source);
        }
    } else {
        // Rotated RAWs cannot use contiguous row copies, but every destination
        // row is independent. Spread those rows across Rayon workers instead of
        // walking the entire 30–60 MP mosaic on one CPU core. Capture the source
        // address as an integer so the immutable LibRaw buffer can be read from
        // worker threads without sharing a mutable raw pointer wrapper.
        let raw_image_addr = raw_image as usize;
        pixels
            .par_chunks_mut(out_width)
            .enumerate()
            .try_for_each(|(y, destination)| -> Result<()> {
                for (x, output) in destination.iter_mut().enumerate() {
                    let (src_x, src_y) = oriented_source_pos(x, y, width, height, flip);
                    let raw_x = crop_x + src_x;
                    let raw_y = crop_y + src_y;
                    let row_offset = raw_y
                        .checked_mul(pitch)
                        .ok_or_else(|| anyhow!("RAW row pointer offset overflow"))?;
                    // SAFETY: crop bounds were validated above and LibRaw keeps
                    // the decoded mosaic alive and immutable for this entire call.
                    let row_ptr = unsafe {
                        (raw_image_addr as *const u8).add(row_offset) as *const u16
                    };
                    *output = unsafe { *row_ptr.add(raw_x) };
                }
                Ok(())
            })?;
    }

    // CFA channels and metadata black levels are periodic for supported Bayer
    // and X-Trans mosaics. Store one exact repeating cell instead of allocating
    // 1 + 4 bytes for every photosite. The period combines the CFA cycle with
    // LibRaw's optional row/column black-level pattern; rotated images swap the
    // pattern axes. If metadata ever describes a period as large as the image,
    // this naturally becomes an exact dense map rather than approximating it.
    let cfa_period = match cfa_kind {
        CfaKind::Bayer => 2usize,
        CfaKind::XTrans => 6usize,
    };
    let (black_rows, black_cols) = black_pattern_dimensions(cblack).unwrap_or((1, 1));
    let source_period_x = lcm_usize(cfa_period, black_cols).max(1);
    let source_period_y = lcm_usize(cfa_period, black_rows).max(1);
    let (period_width, period_height) = if matches!(flip, 5 | 6) {
        (source_period_y.min(out_width), source_period_x.min(out_height))
    } else {
        (source_period_x.min(out_width), source_period_y.min(out_height))
    };
    let pattern_len = period_width
        .checked_mul(period_height)
        .ok_or_else(|| anyhow!("RAW metadata pattern dimensions overflow"))?;
    let mut colors = Vec::new();
    colors
        .try_reserve_exact(pattern_len)
        .context("reserve compact oriented CFA pattern")?;
    let mut black_map = Vec::new();
    black_map
        .try_reserve_exact(pattern_len)
        .context("reserve compact oriented black-level pattern")?;

    for y in 0..period_height {
        for x in 0..period_width {
            let (src_x, src_y) = oriented_source_pos(x, y, width, height, flip);
            let raw_x = crop_x + src_x;
            let raw_y = crop_y + src_y;
            let libraw_color = ffi::libraw_COLOR(raw, raw_y as i32, raw_x as i32);
            if !(0..=3).contains(&libraw_color) {
                return Err(anyhow!(
                    "LibRaw returned invalid CFA channel {libraw_color} at {raw_x},{raw_y}"
                ));
            }
            if cdesc[libraw_color as usize] == 0 {
                return Err(anyhow!(
                    "LibRaw used undescribed CFA channel {libraw_color} at {raw_x},{raw_y}"
                ));
            }
            colors.push(cfa_map[libraw_color as usize]);
            black_map.push(effective_black_level(
                shared_black,
                cblack,
                libraw_color as usize,
                src_x,
                src_y,
            ));
        }
    }

    let colors = CompactPixelMap::repeating(
        out_width as u32,
        out_height as u32,
        period_width as u32,
        period_height as u32,
        colors,
    );
    let black_map = CompactPixelMap::repeating(
        out_width as u32,
        out_height as u32,
        period_width as u32,
        period_height as u32,
        black_map,
    );

    Ok((
        out_width as u32,
        out_height as u32,
        pixels,
        colors,
        black_map,
    ))
}

fn oriented_source_pos(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    flip: i32,
) -> (usize, usize) {
    match flip {
        3 => (width - 1 - x, height - 1 - y),
        5 => (width - 1 - y, x),
        6 => (y, height - 1 - x),
        _ => (x, y),
    }
}

fn cdesc4(iparams: &ffi::libraw_iparams_t) -> [u8; 4] {
    [
        iparams.cdesc[0] as u8,
        iparams.cdesc[1] as u8,
        iparams.cdesc[2] as u8,
        iparams.cdesc[3] as u8,
    ]
}

fn cfa_kind_from_filters(filters: u32) -> Result<CfaKind> {
    match filters {
        // LibRaw reserves 9 for the Fuji 6x6 X-Trans matrix.
        9 => Ok(CfaKind::XTrans),
        // Ordinary Bayer masks use the packed 32-bit representation.
        value if value >= 1000 => Ok(CfaKind::Bayer),
        0 => Err(anyhow!(
            "full-colour/linear RAW input is not supported by the CFA GPU pipeline"
        )),
        1 => Err(anyhow!(
            "Leaf CatchLight 16x16 CFA is not supported by the current demosaic paths"
        )),
        value => Err(anyhow!(
            "unsupported LibRaw CFA filter code {value}; expected Bayer or Fuji X-Trans"
        )),
    }
}

fn canonical_cfa_map(cdesc: [u8; 4]) -> Result<[u8; 4]> {
    let mut map = [3u8; 4];
    let mut red_count = 0u8;
    let mut green_count = 0u8;
    let mut blue_count = 0u8;

    for index in 0..4 {
        map[index] = match cdesc[index] as char {
            'R' | 'r' => {
                red_count = red_count.saturating_add(1);
                0
            }
            'B' | 'b' => {
                blue_count = blue_count.saturating_add(1);
                2
            }
            'G' | 'g' => {
                let canonical = if green_count == 0 { 1 } else { 3 };
                green_count = green_count.saturating_add(1);
                canonical
            }
            '\0' => 3,
            other => {
                return Err(anyhow!(
                    "unsupported non-RGB CFA descriptor {other:?} in {:?}",
                    cdesc.map(char::from)
                ));
            }
        };
    }

    if red_count != 1 || blue_count != 1 || !(1..=2).contains(&green_count) {
        return Err(anyhow!(
                "unsupported RGB CFA descriptor {:?}; expected one red, one blue, and one or two green planes",
                cdesc.map(char::from)
            ));
    }

    Ok(map)
}

fn canonicalize_f32x4(values: [f32; 4], cfa_map: [u8; 4]) -> [f32; 4] {
    let mut out = [0.0; 4];
    for physical in 0..4 {
        out[cfa_map[physical] as usize] = values[physical];
    }
    out
}

fn logical_rgb_channel(cdesc: [u8; 4], cfa_channel: usize) -> Option<usize> {
    match cdesc[cfa_channel.min(3)] as char {
        'R' | 'r' => Some(0),
        'G' | 'g' => Some(1),
        'B' | 'b' => Some(2),
        // A NUL descriptor marks an unused physical profile row. Do not
        // fold it into a real RGB channel, even if malformed metadata left
        // non-zero coefficients there.
        _ => None,
    }
}

fn white_balance(mut wb: [f32; 4], cdesc: [u8; 4]) -> [f32; 4] {
    let mut green_sum = 0.0;
    let mut green_count = 0.0;

    for index in 0..4 {
        let is_green = matches!(cdesc[index] as char, 'G' | 'g');
        if is_green && wb[index].is_finite() && wb[index] > 0.0 {
            green_sum += wb[index];
            green_count += 1.0;
        }
    }

    let green_reference = if green_count > 0.0 {
        green_sum / green_count
    } else if wb[1].is_finite() && wb[1] > 0.0 {
        wb[1]
    } else {
        1.0
    };

    for value in &mut wb {
        *value = if value.is_finite() && *value > 0.0 {
            *value / green_reference
        } else {
            1.0
        };
    }

    wb
}

fn black_levels(black: u32, cblack: &[u32]) -> [f32; 4] {
    let mut out = [black as f32; 4];
    for (index, value) in out.iter_mut().enumerate() {
        *value += cblack.get(index).copied().unwrap_or(0) as f32;
    }
    out
}

fn gcd_usize(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        let remainder = a % b;
        a = b;
        b = remainder;
    }
    a.max(1)
}

fn lcm_usize(a: usize, b: usize) -> usize {
    a.checked_div(gcd_usize(a.max(1), b.max(1)))
        .and_then(|value| value.checked_mul(b.max(1)))
        .unwrap_or(usize::MAX)
}

fn effective_black_level(
    black: u32,
    cblack: &[u32],
    channel: usize,
    active_x: usize,
    active_y: usize,
) -> f32 {
    let channel_offset = cblack.get(channel.min(3)).copied().unwrap_or(0);
    let pattern_offset = black_pattern_dimensions(cblack)
        .and_then(|(rows, cols)| {
            let pattern_index = (active_y % rows)
                .checked_mul(cols)?
                .checked_add(active_x % cols)?
                .checked_add(6)?;
            cblack.get(pattern_index).copied()
        })
        .unwrap_or(0);

    black
        .saturating_add(channel_offset)
        .saturating_add(pattern_offset) as f32
}

fn black_pattern_dimensions(cblack: &[u32]) -> Option<(usize, usize)> {
    let rows = usize::try_from(*cblack.get(4)?).ok()?;
    let cols = usize::try_from(*cblack.get(5)?).ok()?;
    if rows == 0 || cols == 0 {
        return None;
    }
    let values = rows.checked_mul(cols)?;
    let end = 6usize.checked_add(values)?;
    (end <= cblack.len()).then_some((rows, cols))
}

fn white_levels(maximum: u32, linear_max: [u32; 4], black_levels: [f32; 4]) -> [f32; 4] {
    // `maximum` is LibRaw's decoded white/saturation level. `linear_max`
    // is an optional per-plane vendor "specular white" / linearity limit
    // and is known to be invalid in some files. Use it only when it forms
    // a sane range and does not exceed a reported shared maximum.
    let shared_fallback = (maximum != 0)
        .then_some(maximum)
        .or_else(|| linear_max.iter().copied().find(|value| *value != 0))
        .unwrap_or(65535);

    let mut out = [shared_fallback as f32; 4];
    for index in 0..4 {
        let candidate = linear_max[index];
        let candidate_is_sane = candidate != 0
            && candidate as f32 > black_levels[index] + 1.0
            && (maximum == 0 || candidate <= maximum);
        if candidate_is_sane {
            out[index] = candidate as f32;
        }
    }
    out
}

fn cam_to_working(xyz_to_cam: [[f32; 3]; 4], cdesc: [u8; 4]) -> [[f32; 4]; 3] {
    let physical = camera_to_working_physical(xyz_to_cam);

    // The demosaic output is RGB, but camera profiles can contain four
    // physical planes (normally R, G1, B, G2). Fold profile columns by
    // cdesc only after each CFA plane has been normalized independently.
    let mut out = [[0.0; 4]; 3];
    for (physical_col, _) in cdesc.iter().enumerate() {
        let Some(rgb_col) = logical_rgb_channel(cdesc, physical_col) else {
            continue;
        };
        for row in 0..3 {
            out[row][rgb_col] += physical[row][physical_col];
        }
    }

    out
}

fn camera_to_working_physical(xyz_to_cam: [[f32; 3]; 4]) -> [[f32; 4]; 3] {
    let cam_to_xyz = normalized_pseudoinverse(xyz_to_cam);
    let mut physical = [[0.0; 4]; 3];
    for row in 0..3 {
        for col in 0..4 {
            physical[row][col] = XYZ_TO_REC2020[row][0] * cam_to_xyz[0][col]
                + XYZ_TO_REC2020[row][1] * cam_to_xyz[1][col]
                + XYZ_TO_REC2020[row][2] * cam_to_xyz[2][col];
        }
    }
    physical
}

#[derive(Clone, Copy)]
struct InterpolatedDngProfile {
    color_matrix: [[f32; 3]; 4],
    calibration: [[f32; 4]; 4],
    forward_matrix: Option<[[f32; 4]; 3]>,
    weight: f32,
}

fn camera_to_working_matrix(
    color: &ffi::libraw_colordata_t,
    wb_coeffs: [f32; 4],
    cdesc: [u8; 4],
    parsed_profile: Option<&DcpProfile>,
    calibration_compatible: bool,
) -> Result<([[f32; 4]; 3], f32, Option<CameraWhiteBalanceModel>)> {
    let analog_balance = analog_balance_matrix(color.dng_levels.analogbalance);
    // Prefer the profile records parsed directly from the selected DNG/DCP
    // IFD. LibRaw remains the fallback for proprietary RAW files and DNGs
    // whose optional profile IFD could not be read.
    let dng_profile = parsed_profile
        .and_then(|profile| {
            interpolated_parsed_dng_profile(
                profile,
                color,
                wb_coeffs,
                cdesc,
                analog_balance,
                calibration_compatible,
            )
        })
        .or_else(|| {
            interpolated_dng_profile(
                color,
                wb_coeffs,
                cdesc,
                analog_balance,
                calibration_compatible,
            )
        });
    let (matrix, weight) = if let Some(profile) = dng_profile {
        (
            dng_camera_to_working(profile, analog_balance, wb_coeffs, wb_coeffs, cdesc)?,
            profile.weight,
        )
    } else {
        // Proprietary RAW formats generally expose LibRaw's consolidated
        // XYZ->camera matrix rather than individual DNG tags.
        (cam_to_working(color.cam_xyz, cdesc), 0.0)
    };

    if matrix.iter().flatten().any(|value| !value.is_finite())
        || matrix.iter().flatten().all(|value| value.abs() <= 1e-12)
    {
        return Err(anyhow!(
                "LibRaw did not provide an invertible camera colour matrix; refusing to treat camera RGB as the working colour space"
            ));
    }
    let color_model = parsed_profile
        .and_then(|profile| {
            parsed_camera_color_model(profile, color, analog_balance, calibration_compatible)
        })
        .or_else(|| libraw_camera_color_model(color, analog_balance, calibration_compatible))
        .unwrap_or(CameraColorModel::Matrix {
            xyz_to_camera: color.cam_xyz,
        });
    let base_cct = cct_from_profile_weight(&color_model, weight)
        .or_else(|| estimate_scene_cct(color, wb_coeffs, cdesc))
        .or_else(|| estimate_cct_from_model(&color_model, wb_coeffs))
        .unwrap_or(6504.0)
        .clamp(1500.0, 50_000.0);
    let model = CameraWhiteBalanceModel {
        base_wb: wb_coeffs,
        cdesc,
        base_cct,
        color: color_model,
    };
    Ok((matrix, weight, Some(model)))
}

fn parsed_camera_color_model(
    profile: &DcpProfile,
    color: &ffi::libraw_colordata_t,
    analog_balance: [[f32; 4]; 4],
    calibration_compatible: bool,
) -> Option<CameraColorModel> {
    let endpoint = |index: usize| {
        let set = &profile.matrices[index];
        Some(DngColorEndpoint {
            cct: set.illuminant.and_then(calibration_illuminant_cct),
            color_matrix: set
                .color_matrix
                .filter(|matrix| matrix4x3_is_valid(*matrix))?,
            calibration: parsed_calibration(
                set,
                color.dng_color[index].calibration,
                calibration_compatible,
            ),
            forward_matrix: set
                .forward_matrix
                .filter(|matrix| matrix3x4_is_valid(*matrix)),
        })
    };
    paired_endpoints(endpoint(0), endpoint(1)).map(|endpoints| CameraColorModel::Dng {
        endpoints: Box::new(endpoints),
        analog_balance,
    })
}

fn libraw_camera_color_model(
    color: &ffi::libraw_colordata_t,
    analog_balance: [[f32; 4]; 4],
    calibration_compatible: bool,
) -> Option<CameraColorModel> {
    let endpoint = |index: usize| {
        let set = &color.dng_color[index];
        matrix4x3_is_valid(set.colormatrix).then(|| DngColorEndpoint {
            cct: calibration_illuminant_cct(set.illuminant),
            color_matrix: set.colormatrix,
            calibration: if calibration_compatible {
                identity_fallback_4x4(set.calibration)
            } else {
                identity_4x4()
            },
            forward_matrix: matrix3x4_is_valid(set.forwardmatrix).then_some(set.forwardmatrix),
        })
    };
    paired_endpoints(endpoint(0), endpoint(1)).map(|endpoints| CameraColorModel::Dng {
        endpoints: Box::new(endpoints),
        analog_balance,
    })
}

fn paired_endpoints(
    first: Option<DngColorEndpoint>,
    second: Option<DngColorEndpoint>,
) -> Option<[DngColorEndpoint; 2]> {
    match (first, second) {
        (Some(a), Some(b)) => Some([a, b]),
        (Some(a), None) => Some([a, a]),
        (None, Some(b)) => Some([b, b]),
        (None, None) => None,
    }
}

fn cct_from_profile_weight(color: &CameraColorModel, weight: f32) -> Option<f32> {
    let CameraColorModel::Dng { endpoints, .. } = color else {
        return None;
    };
    let first = 1_000_000.0 / endpoints[0].cct?.max(1.0);
    let second = 1_000_000.0 / endpoints[1].cct?.max(1.0);
    Some(1_000_000.0 / (first + (second - first) * weight.clamp(0.0, 1.0)))
}

fn estimate_cct_from_model(color: &CameraColorModel, wb: [f32; 4]) -> Option<f32> {
    let neutral = camera_neutral(wb);
    let xyz_to_camera = match color {
        CameraColorModel::Dng {
            endpoints,
            analog_balance,
        } => multiply_4x4_4x3(
            multiply_4x4(*analog_balance, endpoints[0].calibration),
            endpoints[0].color_matrix,
        ),
        CameraColorModel::Matrix { xyz_to_camera } => *xyz_to_camera,
    };
    xyz_to_cct(multiply_3x4_vector(pseudoinverse(xyz_to_camera), neutral))
}

pub(super) fn adjusted_camera_transform(
    model: &CameraWhiteBalanceModel,
    temperature: f32,
    tint: f32,
) -> Option<([[f32; 4]; 3], f32)> {
    // The global temperature control is a physical mired displacement:
    // positive values select a higher-CCT white and therefore render the
    // scene warmer. Tint is Duv normal to the Planckian locus. Neither
    // control contains camera-channel coefficients.
    let base_mired = 1_000_000.0 / model.base_cct.max(1.0);
    let target_mired = (base_mired - temperature).clamp(20.0, 666.666_7);
    let target_cct = 1_000_000.0 / target_mired;
    let base_white = planckian_white_xyz(model.base_cct, 0.0)?;
    // Positive UI tint moves toward magenta; negative moves toward green.
    let target_white = planckian_white_xyz(target_cct, tint * 0.000_5)?;
    let base_neutral = camera_neutral(model.base_wb);

    match &model.color {
        CameraColorModel::Dng {
            endpoints,
            analog_balance,
        } => {
            let base_weight = endpoint_weight(endpoints, model.base_cct);
            let target_weight = endpoint_weight(endpoints, target_cct);
            let base_profile = interpolate_endpoints(endpoints, base_weight);
            let target_profile = interpolate_endpoints(endpoints, target_weight);
            let base_xyz_to_camera = multiply_4x4_4x3(
                multiply_4x4(*analog_balance, base_profile.calibration),
                base_profile.color_matrix,
            );
            let target_xyz_to_camera = multiply_4x4_4x3(
                multiply_4x4(*analog_balance, target_profile.calibration),
                target_profile.color_matrix,
            );
            let predicted_base = multiply_4x3_vector(base_xyz_to_camera, base_white);
            let predicted_target = multiply_4x3_vector(target_xyz_to_camera, target_white);
            let target_neutral = neutral_from_camera_ratio(
                base_neutral,
                predicted_base,
                predicted_target,
                model.cdesc,
            )?;
            let target_wb = target_neutral.map(|value| 1.0 / value.max(1e-8));
            let transform = dng_camera_to_working(
                target_profile,
                *analog_balance,
                target_wb,
                model.base_wb,
                model.cdesc,
            )
            .ok()?;
            Some((transform, target_weight))
        }
        CameraColorModel::Matrix { xyz_to_camera } => {
            let predicted_base = multiply_4x3_vector(*xyz_to_camera, base_white);
            let predicted_target = multiply_4x3_vector(*xyz_to_camera, target_white);
            let target_neutral = neutral_from_camera_ratio(
                base_neutral,
                predicted_base,
                predicted_target,
                model.cdesc,
            )?;
            let target_wb = target_neutral.map(|value| 1.0 / value.max(1e-8));
            let mut physical = camera_to_working_physical(*xyz_to_camera);
            for column in 0..4 {
                let relative_gain = target_wb[column] / model.base_wb[column].max(1e-8);
                for row in &mut physical {
                    row[column] *= relative_gain;
                }
            }
            Some((fold_physical_camera_planes(physical, model.cdesc), 0.0))
        }
    }
}

fn neutral_from_camera_ratio(
    base: [f32; 4],
    predicted_base: [f32; 4],
    predicted_target: [f32; 4],
    cdesc: [u8; 4],
) -> Option<[f32; 4]> {
    let mut out = [0.0; 4];
    for index in 0..4 {
        if logical_rgb_channel(cdesc, index).is_none() {
            out[index] = base[index];
            continue;
        }
        // Three-channel DNG matrices commonly leave the fourth physical
        // row at zero even when LibRaw reports RGBG in cdesc. It is an
        // inactive coordinate, not a failed WB calculation.
        if predicted_base[index].abs() < 1e-8 && predicted_target[index].abs() < 1e-8 {
            out[index] = base[index];
            continue;
        }
        if predicted_base[index].abs() < 1e-8
            || !predicted_base[index].is_finite()
            || !predicted_target[index].is_finite()
        {
            return None;
        }
        out[index] = base[index] * predicted_target[index] / predicted_base[index];
        if !out[index].is_finite() || out[index] <= 1e-8 {
            return None;
        }
    }
    Some(out)
}

fn endpoint_weight(endpoints: &[DngColorEndpoint; 2], cct: f32) -> f32 {
    match (endpoints[0].cct, endpoints[1].cct) {
        (Some(first), Some(second)) => mired_interpolation_weight(cct, first, second),
        _ => 0.0,
    }
}

fn interpolate_endpoints(endpoints: &[DngColorEndpoint; 2], weight: f32) -> InterpolatedDngProfile {
    InterpolatedDngProfile {
        color_matrix: lerp_4x3(endpoints[0].color_matrix, endpoints[1].color_matrix, weight),
        calibration: lerp_4x4(endpoints[0].calibration, endpoints[1].calibration, weight),
        forward_matrix: interpolate_optional_forward_matrix(
            endpoints[0].forward_matrix,
            endpoints[1].forward_matrix,
            weight,
        ),
        weight,
    }
}

fn planckian_white_xyz(cct: f32, duv: f32) -> Option<[f32; 3]> {
    let t = cct.clamp(1667.0, 25_000.0);
    let [x, y] = planckian_xy(t)?;
    let uv = xy_to_uv([x, y])?;
    let low = locus_uv((t * 0.99).max(1667.0))?;
    let high = locus_uv((t * 1.01).min(25_000.0))?;
    let tangent = [high[0] - low[0], high[1] - low[1]];
    let length = (tangent[0] * tangent[0] + tangent[1] * tangent[1]).sqrt();
    if length <= 1e-10 {
        return None;
    }
    let mut normal = [-tangent[1] / length, tangent[0] / length];
    if normal[1] < 0.0 {
        normal = [-normal[0], -normal[1]];
    }
    uv_to_xyz([uv[0] + normal[0] * duv, uv[1] + normal[1] * duv])
}

fn locus_uv(cct: f32) -> Option<[f32; 2]> {
    let xyz = planckian_xy(cct)?;
    xy_to_uv(xyz)
}

fn planckian_xy(cct: f32) -> Option<[f32; 2]> {
    let t = cct.clamp(1667.0, 25_000.0);
    let x = if t <= 4000.0 {
        -0.266_123_9e9 / t.powi(3) - 0.234_358e6 / t.powi(2) + 0.877_695_6e3 / t + 0.179_91
    } else {
        -3.025_846_9e9 / t.powi(3) + 2.107_038e6 / t.powi(2) + 0.222_634_7e3 / t + 0.240_39
    };
    let y = if t <= 2222.0 {
        -1.106_381_4 * x.powi(3) - 1.348_110_2 * x.powi(2) + 2.185_558_3 * x - 0.202_196_8
    } else if t <= 4000.0 {
        -0.954_947_6 * x.powi(3) - 1.374_185_9 * x.powi(2) + 2.091_37 * x - 0.167_488_7
    } else {
        3.081_758 * x.powi(3) - 5.873_387 * x.powi(2) + 3.751_129_9 * x - 0.370_014_8
    };
    (x.is_finite() && y.is_finite() && x > 0.0 && y > 0.0).then_some([x, y])
}

fn xy_to_uv(xy: [f32; 2]) -> Option<[f32; 2]> {
    let denominator = -2.0 * xy[0] + 12.0 * xy[1] + 3.0;
    (denominator.abs() > 1e-10).then_some([4.0 * xy[0] / denominator, 6.0 * xy[1] / denominator])
}

fn uv_to_xyz(uv: [f32; 2]) -> Option<[f32; 3]> {
    let denominator = 2.0 * uv[0] - 8.0 * uv[1] + 4.0;
    if denominator.abs() <= 1e-10 {
        return None;
    }
    let x = 3.0 * uv[0] / denominator;
    let y = 2.0 * uv[1] / denominator;
    (x.is_finite() && y.is_finite() && y > 1e-10).then_some([x / y, 1.0, (1.0 - x - y) / y])
}

fn interpolated_parsed_dng_profile(
    profile: &DcpProfile,
    color: &ffi::libraw_colordata_t,
    wb_coeffs: [f32; 4],
    cdesc: [u8; 4],
    analog_balance: [[f32; 4]; 4],
    calibration_compatible: bool,
) -> Option<InterpolatedDngProfile> {
    let first = &profile.matrices[0];
    let second = &profile.matrices[1];
    let valid = [
        first.color_matrix.is_some_and(matrix4x3_is_valid),
        second.color_matrix.is_some_and(matrix4x3_is_valid),
    ];
    match valid {
        [false, false] => return None,
        [true, false] => {
            return parsed_single_dng_profile(
                first,
                color.dng_color[0].calibration,
                0.0,
                calibration_compatible,
            )
        }
        [false, true] => {
            return parsed_single_dng_profile(
                second,
                color.dng_color[1].calibration,
                1.0,
                calibration_compatible,
            )
        }
        [true, true] => {}
    }

    let cct0 = calibration_illuminant_cct(first.illuminant?)?;
    let cct1 = calibration_illuminant_cct(second.illuminant?)?;
    let mut scene_cct =
        estimate_scene_cct(color, wb_coeffs, cdesc).unwrap_or_else(|| (cct0 * cct1).sqrt());
    let neutral = camera_neutral(wb_coeffs);
    let first_color = first.color_matrix?;
    let second_color = second.color_matrix?;
    let first_calibration = parsed_calibration(
        first,
        color.dng_color[0].calibration,
        calibration_compatible,
    );
    let second_calibration = parsed_calibration(
        second,
        color.dng_color[1].calibration,
        calibration_compatible,
    );

    let mut weight = mired_interpolation_weight(scene_cct, cct0, cct1);
    for _ in 0..6 {
        let color_matrix = lerp_4x3(first_color, second_color, weight);
        let calibration = lerp_4x4(first_calibration, second_calibration, weight);
        let abcc = multiply_4x4(analog_balance, calibration);
        let xyz_to_camera = multiply_4x4_4x3(abcc, color_matrix);
        let camera_to_xyz = pseudoinverse(xyz_to_camera);
        let white_xyz = multiply_3x4_vector(camera_to_xyz, neutral);
        if let Some(refined) = xyz_to_cct(white_xyz) {
            scene_cct = refined.clamp(1500.0, 50_000.0);
            weight = mired_interpolation_weight(scene_cct, cct0, cct1);
        }
    }

    Some(InterpolatedDngProfile {
        color_matrix: lerp_4x3(first_color, second_color, weight),
        calibration: lerp_4x4(first_calibration, second_calibration, weight),
        forward_matrix: interpolate_optional_forward_matrix(
            first.forward_matrix,
            second.forward_matrix,
            weight,
        ),
        weight,
    })
}

fn parsed_single_dng_profile(
    set: &DcpMatrixSet,
    fallback_calibration: [[f32; 4]; 4],
    weight: f32,
    calibration_compatible: bool,
) -> Option<InterpolatedDngProfile> {
    Some(InterpolatedDngProfile {
        color_matrix: set.color_matrix?,
        calibration: parsed_calibration(set, fallback_calibration, calibration_compatible),
        forward_matrix: set
            .forward_matrix
            .filter(|matrix| matrix3x4_is_valid(*matrix)),
        weight,
    })
}

fn parsed_calibration(
    set: &DcpMatrixSet,
    fallback: [[f32; 4]; 4],
    calibration_compatible: bool,
) -> [[f32; 4]; 4] {
    if calibration_compatible {
        set.camera_calibration
            .filter(|matrix| matrix4x4_is_valid(*matrix))
            .unwrap_or_else(|| identity_fallback_4x4(fallback))
    } else {
        identity_4x4()
    }
}

fn interpolated_dng_profile(
    color: &ffi::libraw_colordata_t,
    wb_coeffs: [f32; 4],
    cdesc: [u8; 4],
    analog_balance: [[f32; 4]; 4],
    calibration_compatible: bool,
) -> Option<InterpolatedDngProfile> {
    let valid = [
        matrix4x3_is_valid(color.dng_color[0].colormatrix),
        matrix4x3_is_valid(color.dng_color[1].colormatrix),
    ];
    match valid {
        [false, false] => return None,
        [true, false] => {
            return Some(single_dng_profile(
                &color.dng_color[0],
                0.0,
                calibration_compatible,
            ));
        }
        [false, true] => {
            return Some(single_dng_profile(
                &color.dng_color[1],
                1.0,
                calibration_compatible,
            ));
        }
        [true, true] => {}
    }

    let cct0 = calibration_illuminant_cct(color.dng_color[0].illuminant)?;
    let cct1 = calibration_illuminant_cct(color.dng_color[1].illuminant)?;
    let mut scene_cct =
        estimate_scene_cct(color, wb_coeffs, cdesc).unwrap_or_else(|| (cct0 * cct1).sqrt());
    let neutral = camera_neutral(wb_coeffs);

    // DNG interpolation is linear in reciprocal correlated colour
    // temperature. Refine the initial metadata estimate from the actual
    // AsShotNeutral response so files without a WBCT table still select the
    // correct profile blend.
    let mut weight = mired_interpolation_weight(scene_cct, cct0, cct1);
    for _ in 0..6 {
        let color_matrix = lerp_4x3(
            color.dng_color[0].colormatrix,
            color.dng_color[1].colormatrix,
            weight,
        );
        let calibration = if calibration_compatible {
            lerp_4x4(
                identity_fallback_4x4(color.dng_color[0].calibration),
                identity_fallback_4x4(color.dng_color[1].calibration),
                weight,
            )
        } else {
            identity_4x4()
        };
        let abcc = multiply_4x4(analog_balance, calibration);
        let xyz_to_camera = multiply_4x4_4x3(abcc, color_matrix);
        let camera_to_xyz = pseudoinverse(xyz_to_camera);
        let white_xyz = multiply_3x4_vector(camera_to_xyz, neutral);
        if let Some(refined) = xyz_to_cct(white_xyz) {
            scene_cct = refined.clamp(1500.0, 50_000.0);
            weight = mired_interpolation_weight(scene_cct, cct0, cct1);
        }
    }

    let color_matrix = lerp_4x3(
        color.dng_color[0].colormatrix,
        color.dng_color[1].colormatrix,
        weight,
    );
    let calibration = if calibration_compatible {
        lerp_4x4(
            identity_fallback_4x4(color.dng_color[0].calibration),
            identity_fallback_4x4(color.dng_color[1].calibration),
            weight,
        )
    } else {
        identity_4x4()
    };
    let forward_matrix = interpolate_forward_matrix(
        color.dng_color[0].forwardmatrix,
        color.dng_color[1].forwardmatrix,
        weight,
    );
    Some(InterpolatedDngProfile {
        color_matrix,
        calibration,
        forward_matrix,
        weight,
    })
}

fn single_dng_profile(
    dng: &ffi::libraw_dng_color_t,
    weight: f32,
    calibration_compatible: bool,
) -> InterpolatedDngProfile {
    InterpolatedDngProfile {
        color_matrix: dng.colormatrix,
        calibration: if calibration_compatible {
            identity_fallback_4x4(dng.calibration)
        } else {
            identity_4x4()
        },
        forward_matrix: matrix3x4_is_valid(dng.forwardmatrix).then_some(dng.forwardmatrix),
        weight,
    }
}

fn dng_camera_to_working(
    profile: InterpolatedDngProfile,
    analog_balance: [[f32; 4]; 4],
    neutral_wb: [f32; 4],
    applied_wb: [f32; 4],
    cdesc: [u8; 4],
) -> Result<[[f32; 4]; 3]> {
    let abcc = multiply_4x4(analog_balance, profile.calibration);
    let neutral = camera_neutral(neutral_wb);

    let camera_to_xyz_d50 = if let Some(forward) = profile.forward_matrix {
        // DNG 1.7: FM * D * inverse(AB * CC), where D white-balances
        // reference-camera coordinates using ReferenceNeutral.
        let inverse_abcc = invert_4x4(abcc)
            .ok_or_else(|| anyhow!("DNG AnalogBalance * CameraCalibration is singular"))?;
        let reference_neutral = multiply_4x4_vector(inverse_abcc, neutral);
        let mut balanced_reference_to_xyz = forward;
        for column in 0..4 {
            let value = reference_neutral[column];
            if !value.is_finite() || value.abs() < 1e-10 {
                return Err(anyhow!("DNG ReferenceNeutral contains an invalid channel"));
            }
            for row in &mut balanced_reference_to_xyz {
                row[column] /= value;
            }
        }
        multiply_3x4_4x4(balanced_reference_to_xyz, inverse_abcc)
    } else {
        // Without ForwardMatrix, invert AB*CC*CM and chromatically adapt
        // the scene white represented by CameraNeutral to PCS D50.
        let xyz_to_camera = multiply_4x4_4x3(abcc, profile.color_matrix);
        let camera_to_xyz = pseudoinverse(xyz_to_camera);
        if camera_to_xyz
            .iter()
            .flatten()
            .all(|value| value.abs() <= 1e-12)
        {
            return Err(anyhow!("DNG XYZ-to-camera matrix is singular"));
        }
        let source_white = multiply_3x4_vector(camera_to_xyz, neutral);
        let adaptation = bradford_adaptation(source_white, [0.964_22, 1.0, 0.825_21])
            .ok_or_else(|| anyhow!("DNG CameraNeutral does not define a valid white point"))?;
        multiply_3x3_3x4(adaptation, camera_to_xyz)
    };

    // DNG's PCS is D50 while the scene working space is linear Rec.2020
    // D65. Adapt once, then factor out the white balance already applied to
    // CFA samples on the GPU.
    const D50_TO_D65: [[f32; 3]; 3] = [
        [0.955_473_4, -0.023_098_5, 0.063_259_3],
        [-0.028_369_7, 1.009_995_5, 0.021_041_4],
        [0.012_314, -0.020_507_7, 1.330_365_9],
    ];
    let xyz_d50_to_rec2020 = multiply_3x3(XYZ_TO_REC2020, D50_TO_D65);
    let mut physical = multiply_3x3_3x4(xyz_d50_to_rec2020, camera_to_xyz_d50);
    for column in 0..4 {
        let gain = applied_wb[column].max(1e-8);
        for row in &mut physical {
            row[column] /= gain;
        }
    }
    Ok(fold_physical_camera_planes(physical, cdesc))
}

fn fold_physical_camera_planes(physical: [[f32; 4]; 3], cdesc: [u8; 4]) -> [[f32; 4]; 3] {
    let mut out = [[0.0; 4]; 3];
    for (physical_col, _) in cdesc.iter().enumerate() {
        let Some(rgb_col) = logical_rgb_channel(cdesc, physical_col) else {
            continue;
        };
        for row in 0..3 {
            out[row][rgb_col] += physical[row][physical_col];
        }
    }
    out
}

fn analog_balance_matrix(values: [f32; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for index in 0..4 {
        out[index][index] = if values[index].is_finite() && values[index] > 1e-8 {
            values[index]
        } else {
            1.0
        };
    }
    out
}

fn camera_neutral(wb_coeffs: [f32; 4]) -> [f32; 4] {
    wb_coeffs.map(|gain| 1.0 / gain.max(1e-8))
}

fn interpolate_forward_matrix(
    first: [[f32; 4]; 3],
    second: [[f32; 4]; 3],
    weight: f32,
) -> Option<[[f32; 4]; 3]> {
    match (matrix3x4_is_valid(first), matrix3x4_is_valid(second)) {
        (true, true) => Some(lerp_3x4(first, second, weight)),
        (true, false) => Some(first),
        (false, true) => Some(second),
        (false, false) => None,
    }
}

fn interpolate_optional_forward_matrix(
    first: Option<[[f32; 4]; 3]>,
    second: Option<[[f32; 4]; 3]>,
    weight: f32,
) -> Option<[[f32; 4]; 3]> {
    match (
        first.filter(|matrix| matrix3x4_is_valid(*matrix)),
        second.filter(|matrix| matrix3x4_is_valid(*matrix)),
    ) {
        (Some(a), Some(b)) => Some(lerp_3x4(a, b, weight)),
        (Some(matrix), None) | (None, Some(matrix)) => Some(matrix),
        (None, None) => None,
    }
}

fn identity_fallback_4x4(matrix: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    if matrix
        .iter()
        .flatten()
        .any(|v| v.is_finite() && v.abs() > 1e-8)
    {
        matrix
    } else {
        identity_4x4()
    }
}

fn identity_4x4() -> [[f32; 4]; 4] {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn matrix4x3_is_valid(matrix: [[f32; 3]; 4]) -> bool {
    matrix.iter().flatten().all(|v| v.is_finite())
        && matrix.iter().flatten().any(|v| v.abs() > 1e-8)
}

fn matrix3x4_is_valid(matrix: [[f32; 4]; 3]) -> bool {
    matrix.iter().flatten().all(|v| v.is_finite())
        && matrix.iter().flatten().any(|v| v.abs() > 1e-8)
}

fn matrix4x4_is_valid(matrix: [[f32; 4]; 4]) -> bool {
    matrix.iter().flatten().all(|v| v.is_finite())
        && matrix.iter().flatten().any(|v| v.abs() > 1e-8)
}

fn estimate_scene_cct(
    color: &ffi::libraw_colordata_t,
    wb_coeffs: [f32; 4],
    cdesc: [u8; 4],
) -> Option<f32> {
    let mut best_cct = 0.0;
    let mut best_error = f32::INFINITY;

    for row in color.WBCT_Coeffs {
        let cct = row[0];
        if !cct.is_finite() || cct <= 0.0 {
            continue;
        }

        let candidate = white_balance([row[1], row[2], row[3], row[4]], cdesc);
        let error = (candidate[0].ln() - wb_coeffs[0].ln()).abs()
            + (candidate[2].ln() - wb_coeffs[2].ln()).abs();

        if error < best_error {
            best_error = error;
            best_cct = cct;
        }
    }

    if best_cct > 0.0 {
        Some(best_cct.clamp(1500.0, 50000.0))
    } else {
        None
    }
}

fn calibration_illuminant_cct(illuminant: u16) -> Option<f32> {
    match illuminant {
        1 => Some(5500.0),  // Daylight
        2 => Some(4000.0),  // Fluorescent
        3 => Some(2856.0),  // Tungsten
        4 => Some(5500.0),  // Flash
        9 => Some(5500.0),  // Fine weather
        10 => Some(6500.0), // Cloudy weather
        11 => Some(7500.0), // Shade
        12 => Some(6500.0), // Daylight fluorescent
        13 => Some(5000.0), // Day white fluorescent
        14 => Some(4150.0), // Cool white fluorescent
        15 => Some(3500.0), // White fluorescent
        16 => Some(3000.0), // Warm white fluorescent
        17 => Some(2856.0), // Standard light A
        18 => Some(4874.0), // Standard light B
        19 => Some(6774.0), // Standard light C
        20 => Some(5503.0), // D55
        21 => Some(6504.0), // D65
        22 => Some(7504.0), // D75
        23 => Some(5003.0), // D50
        24 => Some(3200.0), // ISO studio tungsten
        _ => None,
    }
}

fn mired_interpolation_weight(cct: f32, first_cct: f32, second_cct: f32) -> f32 {
    let first = 1_000_000.0 / first_cct.max(1.0);
    let second = 1_000_000.0 / second_cct.max(1.0);
    let scene = 1_000_000.0 / cct.max(1.0);
    let denominator = second - first;
    if denominator.abs() < 1e-8 {
        0.0
    } else {
        ((scene - first) / denominator).clamp(0.0, 1.0)
    }
}

fn xyz_to_cct(xyz: [f32; 3]) -> Option<f32> {
    let sum = xyz[0] + xyz[1] + xyz[2];
    if !sum.is_finite() || sum.abs() < 1e-10 {
        return None;
    }
    let x = xyz[0] / sum;
    let y = xyz[1] / sum;
    let denominator = y - 0.1858;
    if denominator.abs() < 1e-8 {
        return None;
    }
    let n = (x - 0.3320) / denominator;
    let cct = -449.0 * n * n * n + 3525.0 * n * n - 6823.3 * n + 5520.33;
    (cct.is_finite() && cct > 0.0).then_some(cct)
}

fn bradford_adaptation(source: [f32; 3], target: [f32; 3]) -> Option<[[f32; 3]; 3]> {
    const BRADFORD: [[f32; 3]; 3] = [
        [0.8951, 0.2664, -0.1614],
        [-0.7502, 1.7135, 0.0367],
        [0.0389, -0.0685, 1.0296],
    ];
    const BRADFORD_INV: [[f32; 3]; 3] = [
        [0.986_992_9, -0.147_054_3, 0.159_962_7],
        [0.432_305_3, 0.518_360_3, 0.049_291_2],
        [-0.008_528_7, 0.040_042_8, 0.968_486_7],
    ];
    if !source.iter().all(|v| v.is_finite()) || source[1].abs() < 1e-10 {
        return None;
    }
    let normalized_source = source.map(|v| v / source[1]);
    let source_lms = multiply_3x3_vector(BRADFORD, normalized_source);
    let target_lms = multiply_3x3_vector(BRADFORD, target);
    if source_lms.iter().any(|v| !v.is_finite() || v.abs() < 1e-10) {
        return None;
    }
    let diagonal = [
        [target_lms[0] / source_lms[0], 0.0, 0.0],
        [0.0, target_lms[1] / source_lms[1], 0.0],
        [0.0, 0.0, target_lms[2] / source_lms[2]],
    ];
    Some(multiply_3x3(BRADFORD_INV, multiply_3x3(diagonal, BRADFORD)))
}

fn lerp_4x3(a: [[f32; 3]; 4], b: [[f32; 3]; 4], t: f32) -> [[f32; 3]; 4] {
    let mut out = [[0.0; 3]; 4];
    for row in 0..4 {
        for col in 0..3 {
            out[row][col] = a[row][col] + (b[row][col] - a[row][col]) * t;
        }
    }
    out
}

fn lerp_3x4(a: [[f32; 4]; 3], b: [[f32; 4]; 3], t: f32) -> [[f32; 4]; 3] {
    let mut out = [[0.0; 4]; 3];
    for row in 0..3 {
        for col in 0..4 {
            out[row][col] = a[row][col] + (b[row][col] - a[row][col]) * t;
        }
    }
    out
}

fn lerp_4x4(a: [[f32; 4]; 4], b: [[f32; 4]; 4], t: f32) -> [[f32; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for row in 0..4 {
        for col in 0..4 {
            out[row][col] = a[row][col] + (b[row][col] - a[row][col]) * t;
        }
    }
    out
}

fn multiply_4x4(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0; 4]; 4];
    for row in 0..4 {
        for col in 0..4 {
            for k in 0..4 {
                out[row][col] += a[row][k] * b[k][col];
            }
        }
    }
    out
}

fn multiply_4x4_4x3(a: [[f32; 4]; 4], b: [[f32; 3]; 4]) -> [[f32; 3]; 4] {
    let mut out = [[0.0; 3]; 4];
    for row in 0..4 {
        for col in 0..3 {
            for k in 0..4 {
                out[row][col] += a[row][k] * b[k][col];
            }
        }
    }
    out
}

fn multiply_3x4_4x4(a: [[f32; 4]; 3], b: [[f32; 4]; 4]) -> [[f32; 4]; 3] {
    let mut out = [[0.0; 4]; 3];
    for row in 0..3 {
        for col in 0..4 {
            for k in 0..4 {
                out[row][col] += a[row][k] * b[k][col];
            }
        }
    }
    out
}

fn multiply_3x3(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for row in 0..3 {
        for col in 0..3 {
            for k in 0..3 {
                out[row][col] += a[row][k] * b[k][col];
            }
        }
    }
    out
}

fn multiply_3x3_3x4(a: [[f32; 3]; 3], b: [[f32; 4]; 3]) -> [[f32; 4]; 3] {
    let mut out = [[0.0; 4]; 3];
    for row in 0..3 {
        for col in 0..4 {
            for k in 0..3 {
                out[row][col] += a[row][k] * b[k][col];
            }
        }
    }
    out
}

fn multiply_4x4_vector(matrix: [[f32; 4]; 4], vector: [f32; 4]) -> [f32; 4] {
    matrix.map(|row| {
        row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2] + row[3] * vector[3]
    })
}

fn multiply_3x4_vector(matrix: [[f32; 4]; 3], vector: [f32; 4]) -> [f32; 3] {
    matrix.map(|row| {
        row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2] + row[3] * vector[3]
    })
}

fn multiply_4x3_vector(matrix: [[f32; 3]; 4], vector: [f32; 3]) -> [f32; 4] {
    matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

fn multiply_3x3_vector(matrix: [[f32; 3]; 3], vector: [f32; 3]) -> [f32; 3] {
    matrix.map(|row| row[0] * vector[0] + row[1] * vector[1] + row[2] * vector[2])
}

fn invert_4x4(matrix: [[f32; 4]; 4]) -> Option<[[f32; 4]; 4]> {
    let mut augmented = [[0.0f64; 8]; 4];
    for row in 0..4 {
        for col in 0..4 {
            augmented[row][col] = f64::from(matrix[row][col]);
        }
        augmented[row][row + 4] = 1.0;
    }
    for pivot in 0..4 {
        let mut best = pivot;
        for row in pivot + 1..4 {
            if augmented[row][pivot].abs() > augmented[best][pivot].abs() {
                best = row;
            }
        }
        if !augmented[best][pivot].is_finite() || augmented[best][pivot].abs() < 1e-14 {
            return None;
        }
        augmented.swap(pivot, best);
        let divisor = augmented[pivot][pivot];
        for value in &mut augmented[pivot] {
            *value /= divisor;
        }
        let pivot_values = augmented[pivot];
        for (row_index, row) in augmented.iter_mut().enumerate() {
            if row_index == pivot {
                continue;
            }
            let factor = row[pivot];
            for (value, pivot_value) in row.iter_mut().zip(pivot_values) {
                *value -= factor * pivot_value;
            }
        }
    }
    let mut out = [[0.0; 4]; 4];
    for row in 0..4 {
        for col in 0..4 {
            let value = augmented[row][col + 4];
            if !value.is_finite() {
                return None;
            }
            out[row][col] = value as f32;
        }
    }
    Some(out)
}

fn normalized_pseudoinverse(mut xyz_to_cam: [[f32; 3]; 4]) -> [[f32; 4]; 3] {
    for row in &mut xyz_to_cam {
        // Match Ansel/dcraw's normalization of XYZ -> camera after the
        // sRGB/XYZ D65 matrix has been applied: each camera row must
        // produce one for the D65 white point, not for equal-energy XYZ.
        let white_response = row[0] * D65_XYZ[0] + row[1] * D65_XYZ[1] + row[2] * D65_XYZ[2];
        if white_response.is_finite() && white_response.abs() > 1e-12 {
            for value in row {
                *value /= white_response;
            }
        }
    }

    pseudoinverse(xyz_to_cam)
}

fn pseudoinverse(input: [[f32; 3]; 4]) -> [[f32; 4]; 3] {
    // Form (A^T A | I) in f64. Camera matrices are small, but doing the
    // inversion in f32 makes near-dependent profile columns needlessly
    // fragile and can silently force the identity colour fallback.
    let mut temp = [[0.0f64; 6]; 3];

    for i in 0..3 {
        temp[i][i + 3] = 1.0;
        for j in 0..3 {
            for row in &input {
                temp[i][j] += f64::from(row[i]) * f64::from(row[j]);
            }
        }
    }

    for i in 0..3 {
        let mut pivot_row = i;
        let mut pivot_abs = temp[i][i].abs();
        for (row, values) in temp.iter().enumerate().skip(i + 1) {
            let candidate = values[i].abs();
            if candidate > pivot_abs {
                pivot_abs = candidate;
                pivot_row = row;
            }
        }
        if !pivot_abs.is_finite() || pivot_abs < 1e-14 {
            return [[0.0; 4]; 3];
        }
        if pivot_row != i {
            temp.swap(i, pivot_row);
        }

        let pivot = temp[i][i];
        for value in &mut temp[i] {
            *value /= pivot;
        }
        let pivot_values = temp[i];
        for (row_index, row) in temp.iter_mut().enumerate() {
            if row_index == i {
                continue;
            }
            let scale = row[i];
            for (value, pivot_value) in row.iter_mut().zip(pivot_values) {
                *value -= pivot_value * scale;
            }
        }
    }

    let mut out = [[0.0; 4]; 3];
    for col in 0..4 {
        for row in 0..3 {
            let value = (0..3)
                .map(|k| temp[row][k + 3] * f64::from(input[col][k]))
                .sum::<f64>();
            if !value.is_finite() {
                return [[0.0; 4]; 3];
            }
            out[row][col] = value as f32;
        }
    }
    out
}

fn c_array_to_string(value: &[c_char]) -> String {
    // Fixed-size LibRaw arrays are normally NUL terminated, but treating
    // them as an unbounded C string is undefined behaviour when malformed
    // metadata fills the entire array. Keep conversion inside the slice.
    let bytes: Vec<u8> = value
        .iter()
        .copied()
        .take_while(|value| *value != 0)
        .map(|value| value as u8)
        .collect();
    String::from_utf8_lossy(&bytes).trim().to_owned()
}

fn finite_positive_or_zero(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

fn check_libraw(err: i32, action: &str) -> Result<()> {
    if err == 0 {
        return Ok(());
    }

    // SAFETY: LibRaw returns a process-lifetime NUL-terminated error string for the supplied error code.
    let message = unsafe {
        let ptr = ffi::libraw_strerror(err);
        if ptr.is_null() {
            "unknown LibRaw error".into()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };

    Err(anyhow!("LibRaw failed to {action}: {message} ({err})"))
}

#[cfg(test)]
mod tests {
    use super::{
        adjusted_camera_transform, black_levels, cam_to_working, canonical_cfa_map,
        canonicalize_f32x4, cfa_kind_from_filters, effective_black_level, identity_4x4,
        load_raw_thumbnail, matching_thumbnail_orientation, oriented_source_pos,
        resolve_default_exposure_ev, valid_baseline_exposure,
        validate_embedded_thumbnail_metadata, white_balance, white_levels, CameraColorModel,
        CameraWhiteBalanceModel, CfaKind, DngColorEndpoint, MAX_EMBEDDED_THUMBNAIL_BYTES,
        MISSING_BASELINE_EXPOSURE_FALLBACK_EV,
    };

    const RGBG: [u8; 4] = *b"RGBG";

    #[test]
    fn default_render_exposure_prefers_dng_baseline_and_combines_profile_offset_once() {
        assert_eq!(valid_baseline_exposure(-1000.0), None);
        assert_eq!(valid_baseline_exposure(f32::NAN), None);
        assert_eq!(valid_baseline_exposure(-0.35), Some(-0.35));
        assert!((resolve_default_exposure_ev(Some(-0.35), 0.20) + 0.15).abs() < 1e-6);
        assert!(
            (resolve_default_exposure_ev(None, 0.0) - MISSING_BASELINE_EXPOSURE_FALLBACK_EV)
                .abs()
                < 1e-6
        );
        assert!(
            (resolve_default_exposure_ev(None, 0.10)
                - (MISSING_BASELINE_EXPOSURE_FALLBACK_EV + 0.10))
                .abs()
                < 1e-6
        );
    }

    #[test]
    fn synthetic_dng_produces_a_bounded_rgba_thumbnail() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("regression/raw/synthetic-bayer.dng");
        let thumbnail = load_raw_thumbnail(&path, 128).unwrap();
        assert!(thumbnail.width > 0 && thumbnail.height > 0);
        assert!(thumbnail.width <= 128 && thumbnail.height <= 128);
        assert_eq!(
            thumbnail.rgba.len(),
            thumbnail.width as usize * thumbnail.height as usize * 4
        );
    }

    #[test]
    fn embedded_thumbnail_orientation_uses_the_matching_preview_metadata() {
        let selected = (1600, 1067, 412_345);
        let candidates = [
            (160, 107, 12_345, 3),
            (1600, 1067, 412_345, u16::MAX),
            (1600, 1067, 412_344, 5),
            (1600, 1067, 412_345, 6),
        ];

        assert_eq!(
            matching_thumbnail_orientation(selected, candidates),
            Some(6)
        );
        assert_eq!(
            matching_thumbnail_orientation(selected, [(1600, 1067, 412_345, u16::MAX)]),
            None
        );
    }

    #[test]
    fn embedded_thumbnail_header_is_bounded_before_native_unpack() {
        assert!(validate_embedded_thumbnail_metadata(
            super::ffi::LibRaw_thumbnail_formats_LIBRAW_THUMBNAIL_JPEG,
            1600,
            1067,
            2_000_000,
            3,
        )
        .is_ok());
        assert!(validate_embedded_thumbnail_metadata(
            super::ffi::LibRaw_thumbnail_formats_LIBRAW_THUMBNAIL_JPEG,
            1600,
            1067,
            u32::try_from(MAX_EMBEDDED_THUMBNAIL_BYTES + 1).unwrap(),
            3,
        )
        .is_err());
        assert!(validate_embedded_thumbnail_metadata(
            super::ffi::LibRaw_thumbnail_formats_LIBRAW_THUMBNAIL_BITMAP,
            1000,
            1000,
            1_000_000,
            3,
        )
        .is_err());
    }

    #[test]
    fn global_wb_reinterpolates_dual_illuminant_camera_data() {
        let endpoint = |cct, red_scale, blue_scale| DngColorEndpoint {
            cct: Some(cct),
            color_matrix: [
                [red_scale, 0.0, 0.0],
                [0.0, 0.5, 0.0],
                [0.0, 0.0, blue_scale],
                [0.0, 0.5, 0.0],
            ],
            calibration: identity_4x4(),
            forward_matrix: None,
        };
        let model = CameraWhiteBalanceModel {
            base_wb: [2.0, 1.0, 1.5, 1.0],
            cdesc: RGBG,
            base_cct: 5000.0,
            color: CameraColorModel::Dng {
                endpoints: Box::new([endpoint(2856.0, 1.2, 0.8), endpoint(6504.0, 0.9, 1.1)]),
                analog_balance: identity_4x4(),
            },
        };

        let (cooler, cooler_weight) = adjusted_camera_transform(&model, -20.0, 0.0).unwrap();
        let (warmer, warmer_weight) = adjusted_camera_transform(&model, 20.0, 0.0).unwrap();
        assert!(warmer_weight > cooler_weight);
        assert!(warmer.iter().flatten().all(|value| value.is_finite()));
        assert_ne!(cooler, warmer);

        let (tinted, _) = adjusted_camera_transform(&model, 20.0, 20.0).unwrap();
        assert_ne!(warmer, tinted);
    }

    #[test]
    fn libraw_filter_codes_select_the_demosaic_family() {
        assert_eq!(cfa_kind_from_filters(9).unwrap(), CfaKind::XTrans);
        assert_eq!(cfa_kind_from_filters(0x9494_9494).unwrap(), CfaKind::Bayer);
        assert!(cfa_kind_from_filters(0).is_err());
        assert!(cfa_kind_from_filters(1).is_err());
    }

    #[test]
    fn documented_libraw_rotations_map_output_to_source_coordinates() {
        // Source is 3x2. A 90-degree output is 2x3.
        assert_eq!(oriented_source_pos(0, 0, 3, 2, 5), (2, 0));
        assert_eq!(oriented_source_pos(1, 2, 3, 2, 5), (0, 1));
        assert_eq!(oriented_source_pos(0, 0, 3, 2, 6), (0, 1));
        assert_eq!(oriented_source_pos(1, 2, 3, 2, 6), (2, 0));
    }

    #[test]
    fn camera_neutral_maps_to_rec2020_neutral() {
        // Identity is a useful synthetic XYZ -> camera profile: the old
        // row-sum normalization mapped camera (1, 1, 1) to equal-energy
        // XYZ and therefore to a visibly warm Rec.2020 value.
        let matrix = cam_to_working(
            [
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 0.0],
            ],
            RGBG,
        );

        for (channel, row) in matrix.iter().enumerate() {
            let mapped_neutral = row[0] + row[1] + row[2];
            assert!(
                (mapped_neutral - 1.0).abs() < 1e-5,
                "camera neutral mapped to {mapped_neutral} in working channel {channel}"
            );
        }
    }

    #[test]
    fn cfa_planes_are_canonicalized_without_merging_greens() {
        let map = canonical_cfa_map(*b"GRGB").unwrap();
        assert_eq!(map, [1, 0, 3, 2]);
        assert_eq!(
            canonicalize_f32x4([10.0, 20.0, 30.0, 40.0], map),
            [20.0, 10.0, 40.0, 30.0]
        );
    }

    #[test]
    fn non_rgb_cfa_is_rejected_instead_of_silently_miscolored() {
        assert!(canonical_cfa_map(*b"GMCY").is_err());
        assert!(canonical_cfa_map(*b"RGBG").is_ok());
    }

    #[test]
    fn calibration_keeps_both_green_planes_distinct() {
        assert_eq!(black_levels(64, &[1, 2, 3, 4]), [65.0, 66.0, 67.0, 68.0]);
        assert_eq!(
            white_levels(4095, [4000, 4010, 4020, 4030], [64.0; 4]),
            [4000.0, 4010.0, 4020.0, 4030.0]
        );
    }

    #[test]
    fn invalid_linear_max_falls_back_to_decoded_white_level() {
        assert_eq!(
            white_levels(4095, [10, 4000, 5000, 0], [64.0; 4]),
            [4095.0, 4000.0, 4095.0, 4095.0]
        );
    }

    #[test]
    fn repeating_black_pattern_uses_active_area_coordinates() {
        // Two rows by three columns, after the four per-plane offsets.
        let cblack = [1, 2, 3, 4, 2, 3, 10, 20, 30, 40, 50, 60];
        assert_eq!(effective_black_level(64, &cblack, 2, 0, 0), 77.0);
        assert_eq!(effective_black_level(64, &cblack, 2, 4, 3), 117.0);
    }

    #[test]
    fn malformed_black_pattern_is_ignored_without_out_of_bounds_access() {
        let cblack = [1, 2, 3, 4, 99, 99];
        assert_eq!(effective_black_level(64, &cblack, 1, 500, 500), 66.0);
    }

    #[test]
    fn white_balance_uses_the_average_green_reference() {
        let wb = white_balance([2.0, 1.0, 1.5, 1.2], RGBG);
        let green_mean = 0.5 * (wb[1] + wb[3]);
        assert!((green_mean - 1.0).abs() < 1e-6);
        assert!((wb[1] - wb[3]).abs() > 1e-3);
    }
}
