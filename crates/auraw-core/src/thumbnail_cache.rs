use crate::file_ops::{replace_file, sync_parent_directory};
use crate::pipeline::RawThumbnail;
use image::codecs::jpeg::JpegEncoder;
use image::ImageFormat;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
#[cfg(not(target_os = "android"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
#[cfg(not(target_os = "android"))]
use std::time::UNIX_EPOCH;

#[cfg(any(not(target_os = "android"), test))]
const RAW_THUMBNAIL_SUFFIX: &str = ".auraw-raw-thumb.jpg";
#[cfg(any(not(target_os = "android"), test))]
const RAW_THUMBNAIL_FINGERPRINT_SUFFIX: &str = ".auraw-raw-thumb.fingerprint";
#[cfg(any(not(target_os = "android"), test))]
pub const DESKTOP_THUMBNAIL_CACHE_DIR: &str = "library-thumbnails";
#[cfg(any(not(target_os = "android"), test))]
const LEGACY_THUMBNAIL_CACHE_DIR: &str = ".auraw-cache";
pub const THUMBNAIL_JPEG_QUALITY: u8 = 88;
const MAX_CACHED_THUMBNAIL_EDGE: u32 = 8192;
const MAX_CACHED_THUMBNAIL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CACHED_THUMBNAIL_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CACHED_THUMBNAIL_PIXELS: u64 = MAX_CACHED_THUMBNAIL_DECODE_BYTES / 4;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

struct RenderedThumbnailLimiter {
    state: Mutex<RenderedThumbnailLimiterState>,
    available: Condvar,
}

struct RenderedThumbnailLimiterState {
    limit: usize,
    active: usize,
}

pub struct RenderedThumbnailPermit {
    limiter: &'static RenderedThumbnailLimiter,
}

static RENDERED_THUMBNAIL_LIMITER: OnceLock<RenderedThumbnailLimiter> = OnceLock::new();

fn rendered_thumbnail_limiter() -> &'static RenderedThumbnailLimiter {
    RENDERED_THUMBNAIL_LIMITER.get_or_init(|| RenderedThumbnailLimiter {
        state: Mutex::new(RenderedThumbnailLimiterState {
            limit: 1,
            active: 0,
        }),
        available: Condvar::new(),
    })
}

pub fn set_rendered_thumbnail_worker_limit(limit: usize) {
    let limiter = rendered_thumbnail_limiter();
    let mut state = limiter
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.limit = limit.max(1);
    limiter.available.notify_all();
}

pub fn acquire_rendered_thumbnail_worker() -> RenderedThumbnailPermit {
    let limiter = rendered_thumbnail_limiter();
    let mut state = limiter
        .state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while state.active >= state.limit {
        state = limiter
            .available
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    state.active += 1;
    RenderedThumbnailPermit { limiter }
}

impl Drop for RenderedThumbnailPermit {
    fn drop(&mut self) {
        let mut state = self
            .limiter
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = state.active.saturating_sub(1);
        self.limiter.available.notify_one();
    }
}

pub fn downscale_to_fit(image: image::DynamicImage, maximum_edge: u32) -> image::DynamicImage {
    if image.width() > maximum_edge || image.height() > maximum_edge {
        image.thumbnail(maximum_edge, maximum_edge)
    } else {
        image
    }
}

fn discard_invalid_cache_file(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => log::warn!(
            "could not remove invalid thumbnail cache {}: {error}",
            path.display()
        ),
    }
}

fn accepted_thumbnail_layout(width: u32, height: u32) -> Result<(u64, u64, u64), String> {
    if width == 0 || height == 0 {
        return Err("thumbnail dimensions must be non-zero".to_owned());
    }
    if width > MAX_CACHED_THUMBNAIL_EDGE || height > MAX_CACHED_THUMBNAIL_EDGE {
        return Err(format!(
            "thumbnail dimensions {width}x{height} are outside the cache safety limit"
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| "thumbnail pixel count overflow".to_owned())?;
    if pixels > MAX_CACHED_THUMBNAIL_PIXELS {
        return Err(format!(
            "thumbnail {width}x{height} contains {pixels} pixels, above the {MAX_CACHED_THUMBNAIL_PIXELS} pixel cache limit"
        ));
    }
    let row_bytes = u64::from(width)
        .checked_mul(4)
        .ok_or_else(|| "thumbnail RGBA row byte count overflow".to_owned())?;
    let decoded_bytes = row_bytes
        .checked_mul(u64::from(height))
        .ok_or_else(|| "thumbnail decoded byte count overflow".to_owned())?;
    if decoded_bytes > MAX_CACHED_THUMBNAIL_DECODE_BYTES {
        return Err(format!(
            "thumbnail {width}x{height} requires {decoded_bytes} decoded bytes, above the {} byte cache limit",
            MAX_CACHED_THUMBNAIL_DECODE_BYTES
        ));
    }
    Ok((pixels, row_bytes, decoded_bytes))
}

pub fn load_jpeg(path: &Path, maximum_edge: u32) -> Result<Option<RawThumbnail>, String> {
    if maximum_edge == 0 {
        return Err("thumbnail edge must be non-zero".to_owned());
    }
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not inspect thumbnail cache {}: {error}",
                path.display()
            ))
        }
    };
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CACHED_THUMBNAIL_BYTES {
        discard_invalid_cache_file(path);
        return Ok(None);
    }

    // Inspect the JPEG header before any pixel allocation. Cache files, including
    // legacy sibling caches, are untrusted and may contain a tiny compressed
    // payload advertising hostile dimensions.
    let dimensions = image::ImageReader::with_format(
        match fs::File::open(path) {
            Ok(file) => std::io::BufReader::new(file),
            Err(error) => {
                return Err(format!(
                    "could not open thumbnail cache {}: {error}",
                    path.display()
                ))
            }
        },
        ImageFormat::Jpeg,
    )
    .into_dimensions();
    let (source_width, source_height) = match dimensions {
        Ok(dimensions) => dimensions,
        Err(_) => {
            discard_invalid_cache_file(path);
            return Ok(None);
        }
    };
    if accepted_thumbnail_layout(source_width, source_height).is_err() {
        discard_invalid_cache_file(path);
        return Ok(None);
    }

    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) => {
            return Err(format!(
                "could not reopen thumbnail cache {}: {error}",
                path.display()
            ))
        }
    };
    let mut reader =
        image::ImageReader::with_format(std::io::BufReader::new(file), ImageFormat::Jpeg);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_CACHED_THUMBNAIL_EDGE);
    limits.max_image_height = Some(MAX_CACHED_THUMBNAIL_EDGE);
    limits.max_alloc = Some(MAX_CACHED_THUMBNAIL_DECODE_BYTES);
    reader.limits(limits);
    let image = match reader.decode() {
        Ok(image) => image,
        Err(_) => {
            discard_invalid_cache_file(path);
            return Ok(None);
        }
    };
    let image = downscale_to_fit(image, maximum_edge).to_rgba8();
    let (width, height) = image.dimensions();
    let (_, _, decoded_bytes) = match accepted_thumbnail_layout(width, height) {
        Ok(layout) => layout,
        Err(_) => {
            discard_invalid_cache_file(path);
            return Ok(None);
        }
    };
    let expected = match usize::try_from(decoded_bytes) {
        Ok(expected) => expected,
        Err(_) => {
            discard_invalid_cache_file(path);
            return Ok(None);
        }
    };
    let rgba = image.into_raw();
    if rgba.len() != expected {
        discard_invalid_cache_file(path);
        return Ok(None);
    }
    Ok(Some(RawThumbnail {
        width,
        height,
        rgba,
    }))
}

pub fn save_jpeg(path: &Path, thumbnail: &RawThumbnail) -> Result<(), String> {
    if thumbnail.width == 0
        || thumbnail.height == 0
        || thumbnail.width > MAX_CACHED_THUMBNAIL_EDGE
        || thumbnail.height > MAX_CACHED_THUMBNAIL_EDGE
    {
        return Err("thumbnail dimensions are outside the cache safety limit".to_owned());
    }
    let expected = usize::try_from(thumbnail.width)
        .ok()
        .and_then(|width| {
            usize::try_from(thumbnail.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "thumbnail byte count overflow".to_owned())?;
    if thumbnail.rgba.len() != expected {
        return Err("thumbnail has an invalid RGBA byte count".to_owned());
    }

    let image =
        image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.rgba.clone())
            .ok_or_else(|| "thumbnail has an invalid RGBA layout".to_owned())?;
    let image = image::DynamicImage::ImageRgba8(image).to_rgb8();
    let image = image::DynamicImage::ImageRgb8(image);
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, THUMBNAIL_JPEG_QUALITY)
        .encode_image(&image)
        .map_err(|error| format!("could not encode thumbnail cache: {error}"))?;
    if u64::try_from(encoded.len()).unwrap_or(u64::MAX) > MAX_CACHED_THUMBNAIL_BYTES {
        return Err("encoded thumbnail exceeds the cache size limit".to_owned());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "could not create thumbnail cache directory {}: {error}",
                parent.display()
            )
        })?;
    }
    write_bytes_atomic(path, &encoded).map_err(|error| {
        format!(
            "could not write thumbnail cache {}: {error}",
            path.display()
        )
    })
}

#[cfg(target_os = "android")]
pub fn fingerprint_file(path: &Path, maximum_bytes: u64) -> Result<u64, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > maximum_bytes {
        return Err(format!(
            "{} is {} bytes; the fingerprint limit is {maximum_bytes}",
            path.display(),
            metadata.len()
        ));
    }
    let bytes =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    Ok(fnv1a64(&bytes))
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(not(target_os = "android"))]
pub fn load_desktop_raw_thumbnail(
    raw_path: &Path,
    maximum_edge: u32,
) -> Result<Option<RawThumbnail>, String> {
    let cache_path = desktop_raw_thumbnail_path(raw_path);
    let fingerprint_path = desktop_raw_thumbnail_fingerprint_path(raw_path);
    if !cache_path.is_file() || !fingerprint_path.is_file() {
        migrate_legacy_desktop_raw_thumbnail(raw_path)?;
    }
    if !cache_path.is_file() || !fingerprint_path.is_file() {
        return Ok(None);
    }
    let expected = desktop_raw_stamp(raw_path)?;
    let cached = fs::read_to_string(&fingerprint_path).map_err(|error| {
        format!(
            "could not read RAW thumbnail fingerprint {}: {error}",
            fingerprint_path.display()
        )
    })?;
    if cached.trim() != expected {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(&fingerprint_path);
        migrate_legacy_desktop_raw_thumbnail(raw_path)?;
        if !cache_path.is_file() || !fingerprint_path.is_file() {
            return Ok(None);
        }
    }
    remove_legacy_raw_thumbnail_cache(raw_path);
    load_jpeg(&cache_path, maximum_edge)
}

#[cfg(not(target_os = "android"))]
pub fn save_desktop_raw_thumbnail(raw_path: &Path, thumbnail: &RawThumbnail) -> Result<(), String> {
    let expected = desktop_raw_stamp(raw_path)?;
    let cache_path = desktop_raw_thumbnail_path(raw_path);
    let fingerprint_path = desktop_raw_thumbnail_fingerprint_path(raw_path);
    save_jpeg(&cache_path, thumbnail)?;
    write_bytes_atomic(&fingerprint_path, format!("{expected}\n").as_bytes()).map_err(|error| {
        format!(
            "could not write RAW thumbnail fingerprint {}: {error}",
            fingerprint_path.display()
        )
    })?;
    if desktop_raw_stamp(raw_path)? != expected {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(&fingerprint_path);
        return Err("RAW changed while its thumbnail was being cached".to_owned());
    }
    remove_legacy_raw_thumbnail_cache(raw_path);
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn migrate_legacy_desktop_raw_thumbnail(raw_path: &Path) -> Result<(), String> {
    let legacy_cache = legacy_sibling_cache_path_for_raw(raw_path, RAW_THUMBNAIL_SUFFIX);
    let legacy_fingerprint =
        legacy_sibling_cache_path_for_raw(raw_path, RAW_THUMBNAIL_FINGERPRINT_SUFFIX);
    if !legacy_cache.is_file() || !legacy_fingerprint.is_file() {
        return Ok(());
    }

    let expected = desktop_raw_stamp(raw_path)?;
    let cached = match fs::read_to_string(&legacy_fingerprint) {
        Ok(cached) => cached,
        Err(_) => {
            remove_legacy_raw_thumbnail_cache(raw_path);
            return Ok(());
        }
    };
    if cached.trim() != expected {
        remove_legacy_raw_thumbnail_cache(raw_path);
        return Ok(());
    }

    let thumbnail = match load_jpeg(&legacy_cache, MAX_CACHED_THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => thumbnail,
        Ok(None) | Err(_) => {
            remove_legacy_raw_thumbnail_cache(raw_path);
            return Ok(());
        }
    };
    save_desktop_raw_thumbnail(raw_path, &thumbnail)
}

#[cfg(not(target_os = "android"))]
fn remove_legacy_raw_thumbnail_cache(raw_path: &Path) {
    remove_legacy_cache_file(&legacy_sibling_cache_path_for_raw(
        raw_path,
        RAW_THUMBNAIL_SUFFIX,
    ));
    remove_legacy_cache_file(&legacy_sibling_cache_path_for_raw(
        raw_path,
        RAW_THUMBNAIL_FINGERPRINT_SUFFIX,
    ));
}

#[cfg(not(target_os = "android"))]
fn desktop_raw_thumbnail_path(raw_path: &Path) -> PathBuf {
    desktop_cache_path_for_raw(raw_path, RAW_THUMBNAIL_SUFFIX)
}

#[cfg(not(target_os = "android"))]
fn desktop_raw_thumbnail_fingerprint_path(raw_path: &Path) -> PathBuf {
    desktop_cache_path_for_raw(raw_path, RAW_THUMBNAIL_FINGERPRINT_SUFFIX)
}

/// Returns AuRaw's private per-user thumbnail-cache root. Library previews are
/// deliberately never written beside a user's photos.
#[cfg(not(target_os = "android"))]
pub fn desktop_app_cache_root() -> PathBuf {
    desktop_platform_cache_root().join("auraw")
}

#[cfg(not(target_os = "android"))]
pub fn desktop_thumbnail_cache_root() -> PathBuf {
    desktop_app_cache_root().join(DESKTOP_THUMBNAIL_CACHE_DIR)
}

/// Removes every generated library preview. Both unedited RAW thumbnails and
/// edited renditions live below this private, app-owned directory and can be
/// rebuilt from the RAW plus its sidecar.
#[cfg(not(target_os = "android"))]
pub fn clear_desktop_thumbnail_cache() -> Result<(), String> {
    let root = desktop_thumbnail_cache_root();
    match fs::remove_dir_all(&root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not clear thumbnail cache {}: {error}",
            root.display()
        )),
    }
}

#[cfg(not(target_os = "android"))]
pub fn desktop_thumbnail_cache_size_bytes() -> Result<u64, String> {
    directory_size_bytes(&desktop_thumbnail_cache_root())
}

#[cfg(not(target_os = "android"))]
fn directory_size_bytes(root: &Path) -> Result<u64, String> {
    let mut directories = vec![root.to_path_buf()];
    let mut total = 0_u64;
    while let Some(directory) = directories.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "could not inspect thumbnail cache {}: {error}",
                    directory.display()
                ))
            }
        };
        for entry in entries {
            let entry = entry.map_err(|error| {
                format!(
                    "could not read thumbnail cache entry in {}: {error}",
                    directory.display()
                )
            })?;
            let file_type = entry.file_type().map_err(|error| {
                format!(
                    "could not inspect thumbnail cache entry {}: {error}",
                    entry.path().display()
                )
            })?;
            if file_type.is_dir() {
                directories.push(entry.path());
            } else if file_type.is_file() {
                let bytes = entry
                    .metadata()
                    .map_err(|error| {
                        format!(
                            "could not inspect thumbnail cache entry {}: {error}",
                            entry.path().display()
                        )
                    })?
                    .len();
                total = total.saturating_add(bytes);
            }
        }
    }
    Ok(total)
}

#[cfg(all(not(target_os = "android"), windows))]
fn desktop_platform_cache_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .map(|home| home.join("AppData").join("Local"))
        })
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(all(not(target_os = "android"), target_os = "macos"))]
fn desktop_platform_cache_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library").join("Caches"))
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(all(not(target_os = "android"), unix, not(target_os = "macos")))]
fn desktop_platform_cache_root() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(all(not(target_os = "android"), not(any(unix, windows))))]
fn desktop_platform_cache_root() -> PathBuf {
    std::env::temp_dir()
}

/// Maps a RAW path to an opaque file inside AuRaw's private cache. Hashing the
/// complete absolute/canonical path prevents equal filenames in different
/// libraries from colliding without exposing the user's folder structure.
#[cfg(not(target_os = "android"))]
pub fn desktop_cache_path_for_raw(raw_path: &Path, suffix: &str) -> PathBuf {
    let identity = fs::canonicalize(raw_path).unwrap_or_else(|_| {
        if raw_path.is_absolute() {
            raw_path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(raw_path)
        }
    });
    let key = desktop_path_fingerprint(&identity);
    desktop_thumbnail_cache_root()
        .join(format!("{:02x}", key >> 56))
        .join(format!("{key:016x}{suffix}"))
}

#[cfg(all(not(target_os = "android"), unix))]
fn desktop_path_fingerprint(path: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    fnv1a64(path.as_os_str().as_bytes())
}

#[cfg(all(not(target_os = "android"), windows))]
fn desktop_path_fingerprint(path: &Path) -> u64 {
    use std::os::windows::ffi::OsStrExt;

    let mut hash = 0xcbf29ce484222325u64;
    for word in path.as_os_str().encode_wide() {
        for byte in word.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

#[cfg(all(not(target_os = "android"), not(any(unix, windows))))]
fn desktop_path_fingerprint(path: &Path) -> u64 {
    fnv1a64(path.to_string_lossy().as_bytes())
}

#[cfg(not(target_os = "android"))]
pub fn legacy_sibling_cache_path_for_raw(raw_path: &Path, suffix: &str) -> PathBuf {
    let parent = raw_path.parent().unwrap_or_else(|| Path::new("."));
    let mut file_name = raw_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("raw"));
    file_name.push(suffix);
    parent.join(LEGACY_THUMBNAIL_CACHE_DIR).join(file_name)
}

#[cfg(not(target_os = "android"))]
pub fn remove_legacy_cache_file(path: &Path) {
    let parent = path.parent().map(Path::to_path_buf);
    let _ = fs::remove_file(path);
    if let Some(parent) = parent {
        let _ = fs::remove_dir(parent);
    }
}

#[cfg(not(target_os = "android"))]
fn desktop_raw_stamp(raw_path: &Path) -> Result<String, String> {
    let metadata = fs::metadata(raw_path)
        .map_err(|error| format!("could not inspect RAW {}: {error}", raw_path.display()))?;
    if !metadata.is_file() {
        return Err(format!("RAW {} is not a regular file", raw_path.display()));
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(format!("v1:{}:{modified}", metadata.len()))
}

pub fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("thumbnail"));
    let temporary_id = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
    let mut temporary_name = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(".{}.{}.tmp", std::process::id(), temporary_id));
    let temporary = parent.join(temporary_name);

    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        sync_parent_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_fingerprint_is_stable() {
        assert_eq!(fnv1a64(b"auraw"), 0x2dfe708c8441d3cb);
    }

    fn temporary_test_path(label: &str) -> PathBuf {
        let id = NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "auraw-thumbnail-cache-test-{}-{id}-{label}",
            std::process::id()
        ))
    }

    #[test]
    fn oversized_jpeg_dimensions_are_rejected_before_decode() {
        const OVERSIZED_JPEG: &[u8] = &[
            0xff, 0xd8, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x27, 0x10, 0x27, 0x10, 0x03, 0x01, 0x11,
            0x00, 0x02, 0x11, 0x00, 0x03, 0x11, 0x00, 0xff, 0xd9,
        ];
        let path = temporary_test_path("oversized.jpg");
        fs::write(&path, OVERSIZED_JPEG).unwrap();
        assert!(load_jpeg(&path, 512).unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn overflowing_dimension_math_is_rejected() {
        let error = accepted_thumbnail_layout(u32::MAX, u32::MAX).unwrap_err();
        assert!(error.contains("overflow") || error.contains("safety limit"));
    }

    #[test]
    fn truncated_and_malformed_cache_entries_are_recoverable_misses() {
        for (index, bytes) in [
            &b"not a jpeg"[..],
            &b"\xff\xd8"[..],
            &b"\xff\xd8\xff\xc0\x00\x11"[..],
        ]
        .into_iter()
        .enumerate()
        {
            let path = temporary_test_path(&format!("malformed-{index}.jpg"));
            fs::write(&path, bytes).unwrap();
            assert!(load_jpeg(&path, 512).unwrap().is_none());
            assert!(!path.exists());
        }
    }

    #[test]
    fn png_cache_entry_is_not_loaded_as_jpeg() {
        let path = temporary_test_path("legacy-png.jpg");
        let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]));
        image
            .save_with_format(&path, ImageFormat::Png)
            .expect("test PNG should encode");

        assert!(load_jpeg(&path, 512).unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn normal_jpeg_cache_entry_round_trips_with_bounded_loss() {
        let path = temporary_test_path("normal.jpg");
        let thumbnail = RawThumbnail {
            width: 16,
            height: 16,
            rgba: [40, 80, 120, 255].repeat(16 * 16),
        };
        save_jpeg(&path, &thumbnail).unwrap();
        assert!(fs::read(&path).unwrap().starts_with(&[0xff, 0xd8]));
        let loaded = load_jpeg(&path, 512)
            .unwrap()
            .expect("normal cache should load");
        assert_eq!(loaded.width, thumbnail.width);
        assert_eq!(loaded.height, thumbnail.height);
        for (actual, expected) in loaded
            .rgba
            .chunks_exact(4)
            .zip(thumbnail.rgba.chunks_exact(4))
        {
            for channel in 0..3 {
                assert!(actual[channel].abs_diff(expected[channel]) <= 3);
            }
            assert_eq!(actual[3], 255);
        }
        let _ = fs::remove_file(path);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn invalid_legacy_cache_is_removed_and_ignored() {
        let root = temporary_test_path("legacy-root");
        fs::create_dir_all(&root).unwrap();
        let raw = root.join("sample.raw");
        fs::write(&raw, b"raw").unwrap();
        let legacy_cache = legacy_sibling_cache_path_for_raw(&raw, RAW_THUMBNAIL_SUFFIX);
        let legacy_fingerprint =
            legacy_sibling_cache_path_for_raw(&raw, RAW_THUMBNAIL_FINGERPRINT_SUFFIX);
        fs::create_dir_all(legacy_cache.parent().unwrap()).unwrap();
        fs::write(&legacy_cache, b"malformed jpeg").unwrap();
        fs::write(&legacy_fingerprint, desktop_raw_stamp(&raw).unwrap()).unwrap();

        assert!(load_desktop_raw_thumbnail(&raw, 512).unwrap().is_none());
        assert!(!legacy_cache.exists());
        assert!(!legacy_fingerprint.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_cache_path_is_private_and_not_sibling_scoped() {
        let raw = Path::new("photos/a.CR3");
        let cache = desktop_raw_thumbnail_path(raw);
        assert!(desktop_thumbnail_cache_root().starts_with(desktop_app_cache_root()));
        assert!(cache.starts_with(desktop_thumbnail_cache_root()));
        assert!(cache.to_string_lossy().ends_with(RAW_THUMBNAIL_SUFFIX));
        assert_ne!(cache.parent(), raw.parent());
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn thumbnail_cache_size_counts_nested_cache_files() {
        let root = temporary_test_path("size");
        let nested = root.join("ab");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("one.jpg"), b"123").unwrap();
        fs::write(nested.join("two.fingerprint"), b"12345").unwrap();

        assert_eq!(directory_size_bytes(&root).unwrap(), 8);
        fs::remove_dir_all(root).unwrap();
    }
}
