use crate::pipeline::CameraProfileMode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SETTINGS_VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PerformanceSettings {
    #[serde(default = "settings_version")]
    pub(crate) version: u32,
    #[serde(default = "default_raw_cache_files")]
    pub(crate) raw_cache_files: usize,
    #[serde(default = "default_thumbnail_workers")]
    pub(crate) thumbnail_workers: usize,
    #[serde(default)]
    pub(crate) camera_profile_mode: CameraProfileMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) camera_profile_folder: Option<PathBuf>,
    /// Last manually chosen external DCP, stored relative to the configured
    /// profile root. New RAWs without a sidecar may inherit this choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_camera_profile: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_library_folder: Option<PathBuf>,
}

const fn settings_version() -> u32 {
    SETTINGS_VERSION
}

fn default_raw_cache_files() -> usize {
    crate::app::default_raw_cache_limit()
}

fn default_thumbnail_workers() -> usize {
    crate::ui::library::default_thumbnail_worker_count()
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            raw_cache_files: default_raw_cache_files(),
            thumbnail_workers: default_thumbnail_workers(),
            camera_profile_mode: CameraProfileMode::default(),
            camera_profile_folder: None,
            last_camera_profile: None,
            #[cfg(not(target_os = "android"))]
            last_library_folder: None,
        }
    }
}

impl PerformanceSettings {
    pub(crate) fn sanitized(mut self) -> Self {
        self.version = SETTINGS_VERSION;
        self.raw_cache_files = self
            .raw_cache_files
            .min(crate::app::maximum_raw_cache_limit());
        self.thumbnail_workers = self.thumbnail_workers.clamp(
            1,
            crate::ui::library::maximum_thumbnail_worker_count(),
        );
        self
    }
}

pub(crate) fn load(path: Option<&Path>) -> PerformanceSettings {
    let Some(path) = path else {
        return PerformanceSettings::default();
    };
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_SETTINGS_BYTES => {}
        Ok(_) => return PerformanceSettings::default(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return PerformanceSettings::default()
        }
        Err(error) => {
            log::warn!("could not inspect performance settings {}: {error}", path.display());
            return PerformanceSettings::default();
        }
    }
    match std::fs::read(path).map_err(|error| error.to_string()).and_then(|bytes| {
        serde_json::from_slice::<PerformanceSettings>(&bytes).map_err(|error| error.to_string())
    })
    {
        Ok(settings) => settings.sanitized(),
        Err(error) => {
            log::warn!("could not load performance settings {}: {error}", path.display());
            PerformanceSettings::default()
        }
    }
}

pub(crate) fn save(path: Option<&Path>, settings: PerformanceSettings) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    let settings = settings.sanitized();
    let bytes = serde_json::to_vec_pretty(&settings)
        .map_err(|error| format!("could not encode performance settings: {error}"))?;
    crate::thumbnail_cache::write_bytes_atomic(path, &bytes).map_err(|error| {
        format!(
            "could not save performance settings {}: {error}",
            path.display()
        )
    })
}

#[cfg(not(target_os = "android"))]
pub(crate) fn desktop_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);

    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support"));

    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        });

    #[cfg(not(any(windows, unix, target_os = "macos")))]
    let base: Option<PathBuf> = None;

    base.map(|base| base.join("auraw").join("performance.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_values_are_clamped() {
        let settings = PerformanceSettings {
            version: 99,
            raw_cache_files: usize::MAX,
            thumbnail_workers: 0,
            camera_profile_mode: CameraProfileMode::DcpProfiles,
            camera_profile_folder: Some(PathBuf::from("profiles")),
            last_camera_profile: Some(PathBuf::from("Sony/Camera ST.dcp")),
            #[cfg(not(target_os = "android"))]
            last_library_folder: None,
        }
        .sanitized();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(
            settings.raw_cache_files,
            crate::app::maximum_raw_cache_limit()
        );
        assert_eq!(settings.thumbnail_workers, 1);
        assert_eq!(settings.camera_profile_mode, CameraProfileMode::DcpProfiles);
        assert_eq!(settings.camera_profile_folder, Some(PathBuf::from("profiles")));
        assert_eq!(
            settings.last_camera_profile,
            Some(PathBuf::from("Sony/Camera ST.dcp"))
        );
    }
}
