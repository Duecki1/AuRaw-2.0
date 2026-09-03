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

pub fn sidecar_path_for_raw(raw_path: &Path) -> PathBuf {
    let mut path: OsString = raw_path.as_os_str().to_owned();
    path.push(SIDECAR_SUFFIX);
    PathBuf::from(path)
}

#[cfg(not(target_os = "android"))]
pub fn developed_thumbnail_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::desktop_cache_path_for_raw(raw_path, DEVELOPED_THUMBNAIL_SUFFIX)
}

#[cfg(not(target_os = "android"))]
pub(super) fn developed_thumbnail_fingerprint_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::desktop_cache_path_for_raw(
        raw_path,
        DEVELOPED_THUMBNAIL_FINGERPRINT_SUFFIX,
    )
}

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

    let mut fingerprint = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        fingerprint ^= u64::from(byte);
        fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(Some(fingerprint))
}

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
        return Ok(false);
    }

    let cached_fingerprint = match fs::read_to_string(&fingerprint_path) {
        Ok(value) => match u64::from_str_radix(value.trim(), 16) {
            Ok(value) => value,
            Err(_) => {
                return Ok(false);
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
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
    Ok(fresh)
}

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
    Ok(cache_path)
}

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
pub fn invalidate_developed_thumbnail_cache(raw_path: &Path) -> Result<(), String> {
    for path in [
        developed_thumbnail_path_for_raw(raw_path),
        developed_thumbnail_fingerprint_path_for_raw(raw_path),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not remove {}: {error}", path.display())),
        }
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn remove_desktop_edits(raw_path: &Path) -> Result<bool, String> {
    let paths = [
        sidecar_path_for_raw(raw_path),
        developed_thumbnail_path_for_raw(raw_path),
        developed_thumbnail_fingerprint_path_for_raw(raw_path),
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
    Ok(removed_any)
}
