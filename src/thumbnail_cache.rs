use crate::pipeline::RawThumbnail;
use image::ImageFormat;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Write};
use std::path::Path;
#[cfg(not(target_os = "android"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(target_os = "android"))]
use std::time::UNIX_EPOCH;

const RAW_THUMBNAIL_SUFFIX: &str = ".auraw-raw-thumb.png";
const RAW_THUMBNAIL_FINGERPRINT_SUFFIX: &str = ".auraw-raw-thumb.fingerprint";
const THUMBNAIL_CACHE_DIR: &str = ".auraw-cache";
const MAX_CACHED_THUMBNAIL_EDGE: u32 = 8192;
const MAX_CACHED_THUMBNAIL_BYTES: u64 = 128 * 1024 * 1024;
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
        let _ = fs::remove_file(path);
        return Ok(None);
    }

    let image = match image::open(path) {
        Ok(image) => image,
        Err(error) => {
            let _ = fs::remove_file(path);
            return Err(format!(
                "could not decode thumbnail cache {}: {error}",
                path.display()
            ));
        }
    };
    let image = downscale_to_fit(image, maximum_edge).to_rgba8();
    let (width, height) = image.dimensions();
    if width == 0
        || height == 0
        || width > MAX_CACHED_THUMBNAIL_EDGE
        || height > MAX_CACHED_THUMBNAIL_EDGE
    {
        let _ = fs::remove_file(path);
        return Ok(None);
    }
    Ok(Some(RawThumbnail {
        width,
        height,
        rgba: image.into_raw(),
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

#[cfg(any(target_os = "android", test))]
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
        return Ok(None);
    }
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
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn desktop_raw_thumbnail_path(raw_path: &Path) -> PathBuf {
    cache_sibling_path(raw_path, RAW_THUMBNAIL_SUFFIX)
}

#[cfg(not(target_os = "android"))]
fn desktop_raw_thumbnail_fingerprint_path(raw_path: &Path) -> PathBuf {
    cache_sibling_path(raw_path, RAW_THUMBNAIL_FINGERPRINT_SUFFIX)
}

#[cfg(not(target_os = "android"))]
fn cache_sibling_path(raw_path: &Path, suffix: &str) -> PathBuf {
    let parent = raw_path.parent().unwrap_or_else(|| Path::new("."));
    let mut file_name = raw_path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("raw"));
    file_name.push(suffix);
    parent.join(THUMBNAIL_CACHE_DIR).join(file_name)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv_fingerprint_is_stable() {
        assert_eq!(fnv1a64(b"auraw"), 0x2dfe708c8441d3cb);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_cache_path_is_hidden_and_sibling_scoped() {
        assert_eq!(
            desktop_raw_thumbnail_path(Path::new("photos/a.CR3")),
            Path::new("photos/.auraw-cache/a.CR3.auraw-raw-thumb.png")
        );
    }
}
