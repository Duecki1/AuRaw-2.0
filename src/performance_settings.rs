use crate::pipeline::CameraProfileMode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SETTINGS_VERSION: u32 = 6;
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
    pub(crate) preview_quality: crate::app::PreviewQuality,
    #[serde(default)]
    pub(crate) camera_profile_mode: CameraProfileMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) camera_profile_folder: Option<PathBuf>,
    /// Human-readable source shown in Settings. Android stores a private mirror
    /// path in `camera_profile_folder`, while this keeps the selected SAF tree name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) camera_profile_folder_label: Option<String>,
    /// When enabled and no explicit folder is configured, desktop builds probe
    /// Adobe Camera Raw's standard CameraProfiles installation locations.
    #[serde(default = "default_camera_profile_auto_detect")]
    pub(crate) camera_profile_auto_detect: bool,
    /// Last manually chosen external DCP, stored relative to the configured
    /// profile root. New RAWs without a sidecar may inherit this choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_camera_profile: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    #[serde(default = "default_display_color_management")]
    pub(crate) display_color_management: bool,
    #[cfg(not(target_os = "android"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) display_profile_override: Option<PathBuf>,
    #[serde(default)]
    pub(crate) adjustment_copy_settings: crate::sidecar::AdjustmentCopySettings,
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

const fn default_camera_profile_auto_detect() -> bool {
    !cfg!(target_os = "android")
}

#[cfg(not(target_os = "android"))]
const fn default_display_color_management() -> bool {
    true
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            raw_cache_files: default_raw_cache_files(),
            thumbnail_workers: default_thumbnail_workers(),
            preview_quality: crate::app::PreviewQuality::default(),
            camera_profile_mode: CameraProfileMode::default(),
            camera_profile_folder: None,
            camera_profile_folder_label: None,
            camera_profile_auto_detect: default_camera_profile_auto_detect(),
            last_camera_profile: None,
            #[cfg(not(target_os = "android"))]
            display_color_management: default_display_color_management(),
            #[cfg(not(target_os = "android"))]
            display_profile_override: None,
            adjustment_copy_settings: crate::sidecar::AdjustmentCopySettings::default(),
            #[cfg(not(target_os = "android"))]
            last_library_folder: None,
        }
    }
}

impl PerformanceSettings {
    pub(crate) fn sanitized(mut self) -> Self {
        let loaded_version = self.version;
        self.version = SETTINGS_VERSION;
        // Copy/paste categories were introduced with version 3. Version 4
        // made inpainting and lens correction opt-in. Version 6 separates
        // geometry, camera profiles, and AI masks from their former combined
        // categories. Preserve the user's old Masks choice for the new AI mask
        // category while applying the requested geometry/profile defaults.
        if loaded_version < 4 {
            self.adjustment_copy_settings.inpainting = false;
            self.adjustment_copy_settings.lens_correction = false;
        }
        if loaded_version < 6 {
            self.adjustment_copy_settings.geometry = false;
            self.adjustment_copy_settings.camera_profile = true;
            self.adjustment_copy_settings.ai_masks = self.adjustment_copy_settings.masks;
        }
        self.raw_cache_files = self
            .raw_cache_files
            .min(crate::app::maximum_raw_cache_limit());
        self.thumbnail_workers = self
            .thumbnail_workers
            .clamp(1, crate::ui::library::maximum_thumbnail_worker_count());
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
            log::warn!(
                "could not inspect performance settings {}: {error}",
                path.display()
            );
            return PerformanceSettings::default();
        }
    }
    match std::fs::read(path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            serde_json::from_slice::<PerformanceSettings>(&bytes).map_err(|error| error.to_string())
        }) {
        Ok(settings) => settings.sanitized(),
        Err(error) => {
            log::warn!(
                "could not load performance settings {}: {error}",
                path.display()
            );
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

/// Adobe's documented Camera Raw camera-profile install roots. AuRaw only
/// auto-selects an existing directory; recursive DCP discovery remains in the
/// RAW loader so manually installed camera subfolders work too.
#[cfg(not(target_os = "android"))]
pub(crate) fn detected_adobe_camera_profile_folder() -> Option<PathBuf> {
    adobe_camera_profile_candidates()
        .into_iter()
        .find(|path| path.is_dir())
}

#[cfg(not(target_os = "android"))]
pub(crate) fn adobe_camera_profile_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let mut candidates = Vec::new();
        if let Some(program_data) =
            std::env::var_os("ProgramData").or_else(|| std::env::var_os("ALLUSERSPROFILE"))
        {
            candidates.push(
                PathBuf::from(program_data)
                    .join("Adobe")
                    .join("CameraRaw")
                    .join("CameraProfiles"),
            );
        }
        return candidates;
    }

    #[cfg(target_os = "macos")]
    {
        let mut candidates = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("Adobe")
                    .join("CameraRaw")
                    .join("CameraProfiles"),
            );
        }
        candidates.push(PathBuf::from(
            "/Library/Application Support/Adobe/CameraRaw/CameraProfiles",
        ));
        return candidates;
    }

    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    {
        Vec::new()
    }

    #[cfg(not(any(windows, unix, target_os = "macos")))]
    {
        Vec::new()
    }
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
            preview_quality: crate::app::PreviewQuality::High,
            camera_profile_mode: CameraProfileMode::DcpProfiles,
            camera_profile_folder: Some(PathBuf::from("profiles")),
            camera_profile_folder_label: Some("CameraProfiles".to_owned()),
            camera_profile_auto_detect: false,
            last_camera_profile: Some(PathBuf::from("Sony/Camera ST.dcp")),
            #[cfg(not(target_os = "android"))]
            display_color_management: true,
            #[cfg(not(target_os = "android"))]
            display_profile_override: None,
            adjustment_copy_settings: crate::sidecar::AdjustmentCopySettings {
                adjustments: true,
                geometry: true,
                camera_profile: false,
                masks: false,
                ai_masks: true,
                inpainting: true,
                lens_correction: false,
            },
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
        assert_eq!(settings.preview_quality, crate::app::PreviewQuality::High);
        assert_eq!(settings.camera_profile_mode, CameraProfileMode::DcpProfiles);
        assert_eq!(
            settings.camera_profile_folder,
            Some(PathBuf::from("profiles"))
        );
        assert_eq!(
            settings.camera_profile_folder_label.as_deref(),
            Some("CameraProfiles")
        );
        assert!(!settings.camera_profile_auto_detect);
        assert_eq!(
            settings.last_camera_profile,
            Some(PathBuf::from("Sony/Camera ST.dcp"))
        );
        assert!(settings.adjustment_copy_settings.geometry);
        assert!(!settings.adjustment_copy_settings.camera_profile);
        assert!(!settings.adjustment_copy_settings.masks);
        assert!(settings.adjustment_copy_settings.ai_masks);
        assert!(!settings.adjustment_copy_settings.lens_correction);
    }

    #[test]
    fn version_three_copy_settings_migrate_geometry_dependent_categories_to_opt_in() {
        let settings: PerformanceSettings = serde_json::from_str(
            r#"{"version":3,"raw_cache_files":1,"thumbnail_workers":1,"adjustment_copy_settings":{"adjustments":true,"masks":true,"inpainting":true,"lens_correction":true}}"#,
        )
        .expect("version 3 settings should remain readable")
        .sanitized();

        assert!(settings.adjustment_copy_settings.adjustments);
        assert!(!settings.adjustment_copy_settings.geometry);
        assert!(settings.adjustment_copy_settings.camera_profile);
        assert!(settings.adjustment_copy_settings.masks);
        assert!(settings.adjustment_copy_settings.ai_masks);
        assert!(!settings.adjustment_copy_settings.inpainting);
        assert!(!settings.adjustment_copy_settings.lens_correction);
    }

    #[test]
    fn version_five_masks_choice_is_reused_for_ai_masks() {
        let settings: PerformanceSettings = serde_json::from_str(
            r#"{"version":5,"raw_cache_files":1,"thumbnail_workers":1,"adjustment_copy_settings":{"adjustments":true,"masks":false,"inpainting":false,"lens_correction":false}}"#,
        )
        .expect("version 5 settings should remain readable")
        .sanitized();

        assert!(!settings.adjustment_copy_settings.geometry);
        assert!(settings.adjustment_copy_settings.camera_profile);
        assert!(!settings.adjustment_copy_settings.masks);
        assert!(!settings.adjustment_copy_settings.ai_masks);
    }

    #[test]
    fn older_settings_default_preview_quality_to_balanced() {
        let settings: PerformanceSettings =
            serde_json::from_str(r#"{"version":2,"raw_cache_files":1,"thumbnail_workers":1}"#)
                .expect("legacy settings should remain readable");

        assert_eq!(
            settings.preview_quality,
            crate::app::PreviewQuality::Balanced
        );
        assert_eq!(
            settings.adjustment_copy_settings,
            crate::sidecar::AdjustmentCopySettings::default()
        );
    }
}
