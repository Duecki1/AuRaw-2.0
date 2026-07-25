use crate::file_ops::{replace_file, sync_parent_directory};
use crate::pipeline::RawThumbnail;
use image::ImageFormat;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Write};
use std::path::Path;
#[cfg(not(target_os = "android"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(target_os = "android"))]
use std::time::UNIX_EPOCH;

const RAW_THUMBNAIL_SUFFIX: &str = ".auraw-raw-thumb.png";
const RAW_THUMBNAIL_FINGERPRINT_SUFFIX: &str = ".auraw-raw-thumb.fingerprint";
pub(crate) const DESKTOP_THUMBNAIL_CACHE_DIR: &str = "library-thumbnails";
const LEGACY_THUMBNAIL_CACHE_DIR: &str = ".auraw-cache";
const MAX_CACHED_THUMBNAIL_EDGE: u32 = 8192;
const MAX_CACHED_THUMBNAIL_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CACHED_THUMBNAIL_DECODE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_CACHED_THUMBNAIL_PIXELS: u64 = MAX_CACHED_THUMBNAIL_DECODE_BYTES / 4;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn downscale_to_fit(
    image: image::DynamicImage,
    maximum_edge: u32,
) -> image::DynamicImage {
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
    if width > MAX_CACHED_THUMBNAIL_EDGE || height > MAX_CACHED_THUMBNAIL_EDGE {
        return Err(format!(
            "thumbnail dimensions {width}x{height} are outside the cache safety limit"
        ));
    }
    if decoded_bytes > MAX_CACHED_THUMBNAIL_DECODE_BYTES {
        return Err(format!(
            "thumbnail {width}x{height} requires {decoded_bytes} decoded bytes, above the {} byte cache limit",
            MAX_CACHED_THUMBNAIL_DECODE_BYTES
        ));
    }
    Ok((pixels, row_bytes, decoded_bytes))
}

pub(crate) fn load_png(path: &Path, maximum_edge: u32) -> Result<Option<RawThumbnail>, String> {
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

    // Inspect the PNG header before any pixel allocation. Cache files, including
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
        ImageFormat::Png,
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
        image::ImageReader::with_format(std::io::BufReader::new(file), ImageFormat::Png);
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

pub(crate) fn save_png(path: &Path, thumbnail: &RawThumbnail) -> Result<(), String> {
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
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, ImageFormat::Png)
        .map_err(|error| format!("could not encode thumbnail cache: {error}"))?;
    if u64::try_from(encoded.get_ref().len()).unwrap_or(u64::MAX) > MAX_CACHED_THUMBNAIL_BYTES {
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
    write_bytes_atomic(path, encoded.get_ref()).map_err(|error| {
        format!(
            "could not write thumbnail cache {}: {error}",
            path.display()
        )
    })
}

#[cfg(target_os = "android")]
pub(crate) fn fingerprint_file(path: &Path, maximum_bytes: u64) -> Result<u64, String> {
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

pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(not(target_os = "android"))]
pub(crate) fn load_desktop_raw_thumbnail(
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
    load_png(&cache_path, maximum_edge)
}

#[cfg(not(target_os = "android"))]
pub(crate) fn save_desktop_raw_thumbnail(
    raw_path: &Path,
    thumbnail: &RawThumbnail,
) -> Result<(), String> {
    let expected = desktop_raw_stamp(raw_path)?;
    let cache_path = desktop_raw_thumbnail_path(raw_path);
    let fingerprint_path = desktop_raw_thumbnail_fingerprint_path(raw_path);
    save_png(&cache_path, thumbnail)?;
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
    let legacy_cache =
        legacy_sibling_cache_path_for_raw(raw_path, RAW_THUMBNAIL_SUFFIX);
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

    let thumbnail = match load_png(&legacy_cache, MAX_CACHED_THUMBNAIL_EDGE) {
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
pub(crate) fn desktop_thumbnail_cache_root() -> PathBuf {
    desktop_platform_cache_root()
        .join("auraw")
        .join(DESKTOP_THUMBNAIL_CACHE_DIR)
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
pub(crate) fn desktop_cache_path_for_raw(raw_path: &Path, suffix: &str) -> PathBuf {
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
pub(crate) fn legacy_sibling_cache_path_for_raw(raw_path: &Path, suffix: &str) -> PathBuf {
    let parent = raw_path.parent().unwrap_or_else(|| Path::new("."));
    let mut file_name = raw_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("raw"));
    file_name.push(suffix);
    parent.join(LEGACY_THUMBNAIL_CACHE_DIR).join(file_name)
}

#[cfg(not(target_os = "android"))]
pub(crate) fn remove_legacy_cache_file(path: &Path) {
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

pub(crate) fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
    fn oversized_png_dimensions_are_rejected_before_decode() {
        const OVERSIZED_PNG: &[u8] = &[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 1, 134,
            160, 0, 1, 134, 160, 8, 6, 0, 0, 0, 168, 82, 11, 200, 0, 0, 0, 8, 73, 68,
            65, 84, 120, 156, 3, 0, 0, 0, 0, 1, 72, 6, 137, 210, 0, 0, 0, 0, 73,
            69, 78, 68, 174, 66, 96, 130,
        ];
        let path = temporary_test_path("oversized.png");
        fs::write(&path, OVERSIZED_PNG).unwrap();
        assert!(load_png(&path, 512).unwrap().is_none());
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
            &b"not a png"[..],
            &b"\x89PNG\r\n\x1a\n"[..],
            &b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR"[..],
        ]
        .into_iter()
        .enumerate()
        {
            let path = temporary_test_path(&format!("malformed-{index}.png"));
            fs::write(&path, bytes).unwrap();
            assert!(load_png(&path, 512).unwrap().is_none());
            assert!(!path.exists());
        }
    }

    #[test]
    fn normal_cache_entry_still_round_trips() {
        let path = temporary_test_path("normal.png");
        let thumbnail = RawThumbnail {
            width: 2,
            height: 2,
            rgba: vec![
                255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 127, 127, 127, 255,
            ],
        };
        save_png(&path, &thumbnail).unwrap();
        let loaded = load_png(&path, 512).unwrap().expect("normal cache should load");
        assert_eq!(loaded.width, thumbnail.width);
        assert_eq!(loaded.height, thumbnail.height);
        assert_eq!(loaded.rgba, thumbnail.rgba);
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
        fs::write(&legacy_cache, b"malformed png").unwrap();
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
        assert!(cache.starts_with(desktop_thumbnail_cache_root()));
        assert!(cache.to_string_lossy().ends_with(RAW_THUMBNAIL_SUFFIX));
        assert_ne!(cache.parent(), raw.parent());
    }
}
