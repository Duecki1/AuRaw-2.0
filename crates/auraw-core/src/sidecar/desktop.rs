//! Desktop sidecar paths and developed-thumbnail cache lifecycle.

use super::SIDECAR_SUFFIX;
#[cfg(not(target_os = "android"))]
use super::{
    atomic_write, read_bounded, SidecarError, DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT,
    DEVELOPED_THUMBNAIL_FINGERPRINT_SUFFIX, DEVELOPED_THUMBNAIL_SUFFIX,
};
#[cfg(not(target_os = "android"))]
use crate::pipeline::RawThumbnail;
use std::ffi::OsString;
#[cfg(not(target_os = "android"))]
use std::fs;
use std::path::{Path, PathBuf};

/// Appends rather than replaces the RAW extension: `photo.CR3` becomes
/// `photo.CR3.auraw`. Building from `OsString` preserves non-UTF-8 paths.
pub fn sidecar_path_for_raw(raw_path: &Path) -> PathBuf {
    let mut path: OsString = raw_path.as_os_str().to_owned();
    path.push(SIDECAR_SUFFIX);
    PathBuf::from(path)
}

/// Places the developed preview in AuRaw's private per-user cache rather than
/// creating hidden files beside the user's RAW library.
#[cfg(not(target_os = "android"))]
pub fn developed_thumbnail_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::desktop_cache_path_for_raw(raw_path, DEVELOPED_THUMBNAIL_SUFFIX)
}

#[cfg(not(target_os = "android"))]
fn developed_thumbnail_fingerprint_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::desktop_cache_path_for_raw(
        raw_path,
        DEVELOPED_THUMBNAIL_FINGERPRINT_SUFFIX,
    )
}

#[cfg(not(target_os = "android"))]
fn legacy_developed_thumbnail_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::legacy_sibling_cache_path_for_raw(raw_path, DEVELOPED_THUMBNAIL_SUFFIX)
}

#[cfg(not(target_os = "android"))]
fn legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::legacy_sibling_cache_path_for_raw(
        raw_path,
        DEVELOPED_THUMBNAIL_FINGERPRINT_SUFFIX,
    )
}

/// Returns a stable fingerprint of the current edit sidecar. Thumbnail workers
/// compare this before and after GPU readback so an older render can never
/// overwrite the cache for a newer save.
#[cfg(not(target_os = "android"))]
pub fn desktop_sidecar_fingerprint(raw_path: &Path) -> Result<Option<u64>, String> {
    let path = sidecar_path_for_raw(raw_path);
    let bytes = match read_bounded(&path) {
        Ok(bytes) => bytes,
        Err(SidecarError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None)
        }
        Err(error) => {
            return Err(format!(
                "could not fingerprint edit sidecar {}: {error}",
                path.display()
            ))
        }
    };

    // FNV-1a is deliberately simple and deterministic. This is an invalidation
    // token, not a cryptographic integrity check.
    let mut fingerprint = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        fingerprint ^= u64::from(byte);
        fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(Some(fingerprint))
}

/// Loads a developed thumbnail only when it is newer than the RAW and its
/// stored sidecar fingerprint exactly matches the current edit file. Missing or
/// stale caches are regenerated from the RAW plus its sidecar by the library
/// thumbnail worker before any unedited embedded preview is considered.
#[cfg(not(target_os = "android"))]
pub fn load_developed_thumbnail_cache(
    raw_path: &Path,
    maximum_edge: u32,
) -> Result<Option<RawThumbnail>, String> {
    if maximum_edge == 0 {
        return Err("thumbnail edge must be non-zero".to_owned());
    }
    if !developed_thumbnail_cache_is_fresh(raw_path)? {
        return Ok(None);
    }
    let cache_path = developed_thumbnail_path_for_raw(raw_path);
    match crate::thumbnail_cache::load_jpeg(&cache_path, maximum_edge) {
        Ok(Some(thumbnail)) => Ok(Some(thumbnail)),
        Ok(None) => {
            let _ = fs::remove_file(developed_thumbnail_fingerprint_path_for_raw(raw_path));
            Ok(None)
        }
        Err(error) => {
            let _ = fs::remove_file(&cache_path);
            let _ = fs::remove_file(developed_thumbnail_fingerprint_path_for_raw(raw_path));
            Err(format!(
                "could not decode developed thumbnail {}: {error}",
                cache_path.display()
            ))
        }
    }
}

#[cfg(not(target_os = "android"))]
pub fn developed_thumbnail_cache_is_fresh(raw_path: &Path) -> Result<bool, String> {
    migrate_legacy_developed_thumbnail_cache(raw_path)?;
    let sidecar_path = sidecar_path_for_raw(raw_path);
    let cache_path = developed_thumbnail_path_for_raw(raw_path);
    let fingerprint_path = developed_thumbnail_fingerprint_path_for_raw(raw_path);
    let cache_metadata = match fs::metadata(&cache_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "could not inspect developed thumbnail {}: {error}",
                cache_path.display()
            ))
        }
    };
    let _sidecar_metadata = match fs::metadata(&sidecar_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "could not inspect edit sidecar {}: {error}",
                sidecar_path.display()
            ))
        }
    };
    let raw_metadata = fs::metadata(raw_path).map_err(|error| {
        format!(
            "could not inspect RAW while validating its thumbnail {}: {error}",
            raw_path.display()
        )
    })?;

    let Ok(cache_modified) = cache_metadata.modified() else {
        return Ok(false);
    };
    let Ok(raw_modified) = raw_metadata.modified() else {
        return Ok(false);
    };
    if cache_modified < raw_modified {
        remove_legacy_developed_thumbnail_cache(raw_path);
        return Ok(false);
    }

    // Hash the sidecar only after the cheap existence and timestamp checks.
    // Missing/stale caches therefore never pay to read a potentially large
    // sidecar containing raster masks.
    let cached_fingerprint = match fs::read_to_string(&fingerprint_path) {
        Ok(value) => match u64::from_str_radix(value.trim(), 16) {
            Ok(value) => value,
            Err(_) => {
                remove_legacy_developed_thumbnail_cache(raw_path);
                return Ok(false);
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_legacy_developed_thumbnail_cache(raw_path);
            return Ok(false);
        }
        Err(error) => {
            return Err(format!(
                "could not read developed thumbnail fingerprint {}: {error}",
                fingerprint_path.display()
            ))
        }
    };
    let fresh = desktop_sidecar_fingerprint(raw_path)?
        .map(|fingerprint| fingerprint ^ DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT)
        == Some(cached_fingerprint);
    remove_legacy_developed_thumbnail_cache(raw_path);
    Ok(fresh)
}

/// Atomically stores a GPU-rendered thumbnail, but only if the sidecar still
/// has the fingerprint that was current when the render began.
#[cfg(not(target_os = "android"))]
pub fn save_developed_thumbnail_cache(
    raw_path: &Path,
    thumbnail: &RawThumbnail,
    expected_sidecar_fingerprint: u64,
) -> Result<PathBuf, String> {
    if desktop_sidecar_fingerprint(raw_path)? != Some(expected_sidecar_fingerprint) {
        return Err("edit sidecar changed while its thumbnail was rendering".to_owned());
    }
    let cache_path = developed_thumbnail_path_for_raw(raw_path);
    let fingerprint_path = developed_thumbnail_fingerprint_path_for_raw(raw_path);
    crate::thumbnail_cache::save_jpeg(&cache_path, thumbnail).map_err(|error| {
        format!(
            "could not cache developed thumbnail {}: {error}",
            cache_path.display()
        )
    })?;
    atomic_write(
        &fingerprint_path,
        format!(
            "{:016x}\n",
            expected_sidecar_fingerprint ^ DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT
        )
        .as_bytes(),
    )
    .map_err(|error| {
        format!(
            "could not cache developed thumbnail fingerprint {}: {error}",
            fingerprint_path.display()
        )
    })?;

    if desktop_sidecar_fingerprint(raw_path)? != Some(expected_sidecar_fingerprint) {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(&fingerprint_path);
        return Err("edit sidecar changed while its thumbnail was being cached".to_owned());
    }
    remove_legacy_developed_thumbnail_cache(raw_path);
    Ok(cache_path)
}

/// Copies a valid developed preview to a RAW whose sidecar has already been
/// copied. The destination gets its own cache key and fingerprint, so later
/// edits to either RAW invalidate only that RAW's preview.
#[cfg(not(target_os = "android"))]
pub fn copy_developed_thumbnail_cache(
    source_raw: &Path,
    destination_raw: &Path,
) -> Result<bool, String> {
    let Some(thumbnail) = load_developed_thumbnail_cache(source_raw, 8192)? else {
        return Ok(false);
    };
    let Some(fingerprint) = desktop_sidecar_fingerprint(destination_raw)? else {
        return Ok(false);
    };
    save_developed_thumbnail_cache(destination_raw, &thumbnail, fingerprint)?;
    Ok(true)
}

#[cfg(not(target_os = "android"))]
fn migrate_legacy_developed_thumbnail_cache(raw_path: &Path) -> Result<(), String> {
    let cache_path = developed_thumbnail_path_for_raw(raw_path);
    let fingerprint_path = developed_thumbnail_fingerprint_path_for_raw(raw_path);
    if cache_path.is_file() && fingerprint_path.is_file() {
        return Ok(());
    }

    let legacy_cache = legacy_developed_thumbnail_path_for_raw(raw_path);
    let legacy_fingerprint = legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path);
    if !legacy_cache.is_file() || !legacy_fingerprint.is_file() {
        return Ok(());
    }

    let raw_metadata = match fs::metadata(raw_path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(()),
    };
    let cache_metadata = match fs::metadata(&legacy_cache) {
        Ok(metadata) => metadata,
        Err(_) => {
            remove_legacy_developed_thumbnail_cache(raw_path);
            return Ok(());
        }
    };
    let cache_is_new_enough = cache_metadata
        .modified()
        .ok()
        .zip(raw_metadata.modified().ok())
        .is_some_and(|(cache_modified, raw_modified)| cache_modified >= raw_modified);
    let Some(current_fingerprint) = desktop_sidecar_fingerprint(raw_path)? else {
        remove_legacy_developed_thumbnail_cache(raw_path);
        return Ok(());
    };
    let cached_fingerprint = fs::read_to_string(&legacy_fingerprint)
        .ok()
        .and_then(|value| u64::from_str_radix(value.trim(), 16).ok());
    if !cache_is_new_enough || cached_fingerprint != Some(current_fingerprint) {
        remove_legacy_developed_thumbnail_cache(raw_path);
        return Ok(());
    }

    let thumbnail = match crate::thumbnail_cache::load_jpeg(&legacy_cache, 8192) {
        Ok(Some(thumbnail)) => thumbnail,
        Ok(None) | Err(_) => {
            remove_legacy_developed_thumbnail_cache(raw_path);
            return Ok(());
        }
    };
    if save_developed_thumbnail_cache(raw_path, &thumbnail, current_fingerprint).is_err() {
        return Ok(());
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn invalidate_developed_thumbnail_cache(raw_path: &Path) -> Result<(), String> {
    for path in [
        developed_thumbnail_path_for_raw(raw_path),
        developed_thumbnail_fingerprint_path_for_raw(raw_path),
        legacy_developed_thumbnail_path_for_raw(raw_path),
        legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not remove {}: {error}", path.display())),
        }
    }
    remove_legacy_developed_thumbnail_cache(raw_path);
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn remove_legacy_developed_thumbnail_cache(raw_path: &Path) {
    crate::thumbnail_cache::remove_legacy_cache_file(&legacy_developed_thumbnail_path_for_raw(
        raw_path,
    ));
    crate::thumbnail_cache::remove_legacy_cache_file(
        &legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path),
    );
}

#[cfg(not(target_os = "android"))]
pub fn remove_desktop_edits(raw_path: &Path) -> Result<bool, String> {
    let paths = [
        sidecar_path_for_raw(raw_path),
        developed_thumbnail_path_for_raw(raw_path),
        developed_thumbnail_fingerprint_path_for_raw(raw_path),
        legacy_developed_thumbnail_path_for_raw(raw_path),
        legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path),
    ];
    let mut removed_any = false;
    for path in paths {
        match fs::remove_file(&path) {
            Ok(()) => removed_any = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("could not remove {}: {error}", path.display()));
            }
        }
    }
    remove_legacy_developed_thumbnail_cache(raw_path);
    Ok(removed_any)
}
