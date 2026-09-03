use crate::pipeline::CameraProfileMode;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// First public settings layout. Bump when a public settings change needs migration.
const SETTINGS_VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PerformanceSettings {
    #[serde(default = "settings_version")]
    pub version: u32,
    #[serde(default = "default_raw_cache_files")]
    pub raw_cache_files: usize,
    #[serde(default = "default_thumbnail_workers")]
    pub thumbnail_workers: usize,
    #[serde(default)]
    pub render_edited_thumbnails_during_indexing: bool,
    #[serde(default)]
    pub library_thumbnail_size: crate::ui::library::LibraryThumbnailSize,
    #[serde(default)]
    pub library_sort_order: crate::ui::library::LibrarySortOrder,
    #[serde(default)]
    pub preview_quality: crate::app::PreviewQuality,
    #[serde(default)]
    pub image_relative_brush_size: bool,
    #[serde(default)]
    pub show_develop_navigation_labels: bool,
    #[serde(default = "default_export_name_template")]
    pub export_name_template: String,
    #[serde(default)]
    pub ui_design: crate::ui::theme::UiDesign,
    #[serde(default)]
    pub preview_backdrop: crate::ui::theme::PreviewBackdrop,
    #[serde(default)]
    pub onboarding_completed: bool,
    #[serde(default = "default_true")]
    pub auto_check_updates: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored_update_version: Option<String>,
    #[serde(default)]
    pub birefnet_quality: crate::ai_masks::BiRefNetQuality,
    #[cfg(not(target_os = "android"))]
    #[serde(default = "default_true")]
    pub subject_crop_refinement: bool,
    #[cfg(not(target_os = "android"))]
    #[serde(default = "default_true")]
    pub ai_gpu_acceleration: bool,
    #[cfg(not(target_os = "android"))]
    #[serde(default)]
    pub onnx_runtime_mode: crate::app::OnnxRuntimeMode,
    #[cfg(not(target_os = "android"))]
    #[serde(default)]
    pub discord_rich_presence: bool,
    #[serde(default)]
    pub camera_profile_mode: CameraProfileMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_profile_folder: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_profile_folder_label: Option<String>,
    #[serde(default = "default_camera_profile_auto_detect")]
    pub camera_profile_auto_detect: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_camera_profile: Option<PathBuf>,
    #[serde(default)]
    pub adjustment_copy_settings: crate::sidecar::AdjustmentCopySettings,
    #[cfg(target_os = "android")]
    #[serde(default)]
    pub(crate) last_android_library_folder: String,
    #[cfg(not(target_os = "android"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_library_folder: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_library_selected_folder: Option<PathBuf>,
    #[cfg(not(target_os = "android"))]
    #[serde(default = "default_true")]
    pub library_folder_sidebar_open: bool,
    #[cfg(not(target_os = "android"))]
    #[serde(default = "default_true")]
    pub develop_filmstrip_open: bool,
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

fn default_export_name_template() -> String {
    crate::export_naming::DEFAULT_EXPORT_NAME_TEMPLATE.to_owned()
}

const fn default_camera_profile_auto_detect() -> bool {
    !cfg!(target_os = "android")
}

const fn subject_quality_for_platform(
    configured: crate::ai_masks::BiRefNetQuality,
    android: bool,
) -> crate::ai_masks::BiRefNetQuality {
    if android {
        crate::ai_masks::BiRefNetQuality::Low
    } else {
        configured
    }
}

const fn default_true() -> bool {
    true
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            raw_cache_files: default_raw_cache_files(),
            thumbnail_workers: default_thumbnail_workers(),
            render_edited_thumbnails_during_indexing: false,
            library_thumbnail_size: crate::ui::library::LibraryThumbnailSize::default(),
            library_sort_order: crate::ui::library::LibrarySortOrder::default(),
            preview_quality: crate::app::PreviewQuality::default(),
            image_relative_brush_size: false,
            show_develop_navigation_labels: false,
            export_name_template: default_export_name_template(),
            ui_design: crate::ui::theme::UiDesign::default(),
            preview_backdrop: crate::ui::theme::PreviewBackdrop::default(),
            onboarding_completed: false,
            auto_check_updates: true,
            ignored_update_version: None,
            birefnet_quality: crate::ai_masks::BiRefNetQuality::default(),
            #[cfg(not(target_os = "android"))]
            subject_crop_refinement: true,
            #[cfg(not(target_os = "android"))]
            ai_gpu_acceleration: true,
            #[cfg(not(target_os = "android"))]
            onnx_runtime_mode: crate::app::OnnxRuntimeMode::default(),
            #[cfg(not(target_os = "android"))]
            discord_rich_presence: false,
            camera_profile_mode: CameraProfileMode::default(),
            camera_profile_folder: None,
            camera_profile_folder_label: None,
            camera_profile_auto_detect: default_camera_profile_auto_detect(),
            last_camera_profile: None,
            adjustment_copy_settings: crate::sidecar::AdjustmentCopySettings::default(),
            #[cfg(target_os = "android")]
            last_android_library_folder: String::new(),
            #[cfg(not(target_os = "android"))]
            last_library_folder: None,
            #[cfg(not(target_os = "android"))]
            last_library_selected_folder: None,
            #[cfg(not(target_os = "android"))]
            library_folder_sidebar_open: true,
            #[cfg(not(target_os = "android"))]
            develop_filmstrip_open: true,
        }
    }
}

impl PerformanceSettings {
    pub(crate) fn sanitized(mut self) -> Self {
        // Version 1 is the public baseline. Apply future public-version migrations here before
        // updating the stored version, then keep the value sanitization below version-agnostic.
        self.version = SETTINGS_VERSION;
        self.raw_cache_files = self
            .raw_cache_files
            .min(crate::app::maximum_raw_cache_limit());
        self.thumbnail_workers = self
            .thumbnail_workers
            .clamp(1, crate::ui::library::maximum_thumbnail_worker_count());
        self.birefnet_quality =
            subject_quality_for_platform(self.birefnet_quality, cfg!(target_os = "android"));
        self.export_name_template =
            crate::export_naming::sanitize_template_setting(&self.export_name_template);
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

    base.map(|base| base.join("calibraw").join("performance.json"))
}

#[cfg(not(target_os = "android"))]
pub(crate) fn detected_camera_profile_folder() -> Option<PathBuf> {
    camera_profile_candidates()
        .into_iter()
        .find(|path| path.is_dir())
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn camera_profile_candidates_under(root: &Path) -> Vec<PathBuf> {
    let mut candidates = vec![root.join("CameraRaw").join("CameraProfiles")];
    if let Ok(entries) = std::fs::read_dir(root) {
        candidates.extend(
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path().join("CameraRaw").join("CameraProfiles")),
        );
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

#[cfg(not(target_os = "android"))]
pub(crate) fn camera_profile_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        return if let Some(program_data) =
            std::env::var_os("ProgramData").or_else(|| std::env::var_os("ALLUSERSPROFILE"))
        {
            camera_profile_candidates_under(&PathBuf::from(program_data))
        } else {
            Vec::new()
        };
    }

    #[cfg(target_os = "macos")]
    {
        let mut candidates = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            candidates.extend(camera_profile_candidates_under(
                &PathBuf::from(home)
                    .join("Library")
                    .join("Application Support"),
            ));
        }
        candidates.extend(camera_profile_candidates_under(&PathBuf::from(
            "/Library/Application Support",
        )));
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
            render_edited_thumbnails_during_indexing: true,
            library_thumbnail_size: crate::ui::library::LibraryThumbnailSize::Large,
            library_sort_order: crate::ui::library::LibrarySortOrder::NameAscending,
            preview_quality: crate::app::PreviewQuality::High,
            image_relative_brush_size: true,
            show_develop_navigation_labels: true,
            export_name_template: "{OriginalName}-{ISO}".to_owned(),
            ui_design: crate::ui::theme::UiDesign::DaylightBlue,
            preview_backdrop: crate::ui::theme::PreviewBackdrop::White,
            onboarding_completed: true,
            auto_check_updates: false,
            ignored_update_version: Some("v98.0.0".to_owned()),
            birefnet_quality: crate::ai_masks::BiRefNetQuality::High,
            #[cfg(not(target_os = "android"))]
            subject_crop_refinement: false,
            #[cfg(not(target_os = "android"))]
            ai_gpu_acceleration: false,
            #[cfg(not(target_os = "android"))]
            onnx_runtime_mode: crate::app::OnnxRuntimeMode::Manual,
            #[cfg(not(target_os = "android"))]
            discord_rich_presence: true,
            camera_profile_mode: CameraProfileMode::DcpProfiles,
            camera_profile_folder: Some(PathBuf::from("profiles")),
            camera_profile_folder_label: Some("CameraProfiles".to_owned()),
            camera_profile_auto_detect: false,
            last_camera_profile: Some(PathBuf::from("Sony/Camera ST.dcp")),
            adjustment_copy_settings: crate::sidecar::AdjustmentCopySettings {
                adjustments: true,
                geometry: true,
                camera_profile: false,
                masks: false,
                ai_masks: true,
                lens_correction: false,
            },
            #[cfg(not(target_os = "android"))]
            last_library_folder: None,
            #[cfg(not(target_os = "android"))]
            last_library_selected_folder: None,
            #[cfg(not(target_os = "android"))]
            library_folder_sidebar_open: false,
            #[cfg(not(target_os = "android"))]
            develop_filmstrip_open: false,
        }
        .sanitized();
        assert_eq!(settings.version, SETTINGS_VERSION);
        assert_eq!(
            settings.raw_cache_files,
            crate::app::maximum_raw_cache_limit()
        );
        assert_eq!(settings.thumbnail_workers, 1);
        assert!(settings.render_edited_thumbnails_during_indexing);
        assert_eq!(
            settings.library_thumbnail_size,
            crate::ui::library::LibraryThumbnailSize::Large
        );
        assert_eq!(
            settings.library_sort_order,
            crate::ui::library::LibrarySortOrder::NameAscending
        );
        assert_eq!(settings.preview_quality, crate::app::PreviewQuality::High);
        assert!(settings.image_relative_brush_size);
        assert!(settings.show_develop_navigation_labels);
        assert_eq!(settings.export_name_template, "{OriginalName}-{ISO}");
        assert_eq!(settings.ui_design, crate::ui::theme::UiDesign::DaylightBlue);
        assert_eq!(
            settings.preview_backdrop,
            crate::ui::theme::PreviewBackdrop::White
        );
        assert!(settings.onboarding_completed);
        assert!(!settings.auto_check_updates);
        assert_eq!(settings.ignored_update_version.as_deref(), Some("v98.0.0"));
        assert_eq!(
            settings.birefnet_quality,
            crate::ai_masks::BiRefNetQuality::High
        );
        #[cfg(not(target_os = "android"))]
        {
            assert!(!settings.subject_crop_refinement);
            assert!(!settings.ai_gpu_acceleration);
            assert!(settings.discord_rich_presence);
        }
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
    fn omitted_settings_fields_use_current_defaults() {
        let settings: PerformanceSettings =
            serde_json::from_str(r#"{"version":1,"raw_cache_files":1,"thumbnail_workers":1}"#)
                .expect("baseline settings should remain readable");

        assert_eq!(SETTINGS_VERSION, 1);
        assert_eq!(settings.preview_quality, crate::app::PreviewQuality::Medium);
        assert!(!settings.image_relative_brush_size);
        assert!(!settings.show_develop_navigation_labels);
        assert_eq!(
            settings.export_name_template,
            crate::export_naming::DEFAULT_EXPORT_NAME_TEMPLATE
        );
        assert_eq!(settings.ui_design, crate::ui::theme::UiDesign::ObsidianBlue);
        assert_eq!(
            settings.preview_backdrop,
            crate::ui::theme::PreviewBackdrop::DarkGrey
        );
        assert!(!settings.render_edited_thumbnails_during_indexing);
        assert!(settings.auto_check_updates);
        assert!(settings.ignored_update_version.is_none());
        assert_eq!(
            settings.birefnet_quality,
            crate::ai_masks::BiRefNetQuality::Low
        );
        assert_eq!(
            settings.library_thumbnail_size,
            crate::ui::library::LibraryThumbnailSize::Medium
        );
        assert_eq!(
            settings.library_sort_order,
            crate::ui::library::LibrarySortOrder::NewestFirst
        );
        #[cfg(not(target_os = "android"))]
        {
            assert!(settings.subject_crop_refinement);
            assert!(settings.ai_gpu_acceleration);
            assert_eq!(
                settings.onnx_runtime_mode,
                crate::app::OnnxRuntimeMode::Automatic
            );
            assert!(!settings.discord_rich_presence);
            assert!(settings.library_folder_sidebar_open);
            assert!(settings.develop_filmstrip_open);
        }
        assert_eq!(
            settings.adjustment_copy_settings,
            crate::sidecar::AdjustmentCopySettings::default()
        );
        assert!(!settings.onboarding_completed);
    }

    #[test]
    fn library_preferences_round_trip() {
        let mut settings = PerformanceSettings {
            library_thumbnail_size: crate::ui::library::LibraryThumbnailSize::Enormous,
            library_sort_order: crate::ui::library::LibrarySortOrder::SmallestFirst,
            birefnet_quality: crate::ai_masks::BiRefNetQuality::High,
            image_relative_brush_size: true,
            show_develop_navigation_labels: true,
            export_name_template: "{OriginalName}-{CurrentDate}".to_owned(),
            ui_design: crate::ui::theme::UiDesign::Porcelain,
            preview_backdrop: crate::ui::theme::PreviewBackdrop::MatchPhoto,
            render_edited_thumbnails_during_indexing: true,
            ..Default::default()
        };
        #[cfg(not(target_os = "android"))]
        {
            settings.subject_crop_refinement = false;
            settings.ai_gpu_acceleration = false;
            settings.onnx_runtime_mode = crate::app::OnnxRuntimeMode::Manual;
            settings.discord_rich_presence = true;
            settings.last_library_folder = Some(PathBuf::from("photos"));
            settings.last_library_selected_folder = Some(PathBuf::from("photos/2026/trip"));
            settings.library_folder_sidebar_open = false;
            settings.develop_filmstrip_open = false;
        }

        let json = serde_json::to_string(&settings).unwrap();
        let restored: PerformanceSettings = serde_json::from_str(&json).unwrap();

        assert_eq!(
            restored.library_thumbnail_size,
            crate::ui::library::LibraryThumbnailSize::Enormous
        );
        assert_eq!(
            restored.library_sort_order,
            crate::ui::library::LibrarySortOrder::SmallestFirst
        );
        assert_eq!(
            restored.birefnet_quality,
            crate::ai_masks::BiRefNetQuality::High
        );
        assert!(restored.image_relative_brush_size);
        assert!(restored.show_develop_navigation_labels);
        assert_eq!(
            restored.export_name_template,
            "{OriginalName}-{CurrentDate}"
        );
        assert_eq!(restored.ui_design, crate::ui::theme::UiDesign::Porcelain);
        assert_eq!(
            restored.preview_backdrop,
            crate::ui::theme::PreviewBackdrop::MatchPhoto
        );
        assert!(restored.render_edited_thumbnails_during_indexing);
        #[cfg(not(target_os = "android"))]
        {
            assert!(!restored.subject_crop_refinement);
            assert!(!restored.ai_gpu_acceleration);
            assert_eq!(
                restored.onnx_runtime_mode,
                crate::app::OnnxRuntimeMode::Manual
            );
            assert!(restored.discord_rich_presence);
            assert_eq!(restored.last_library_folder, Some(PathBuf::from("photos")));
            assert_eq!(
                restored.last_library_selected_folder,
                Some(PathBuf::from("photos/2026/trip"))
            );
            assert!(!restored.library_folder_sidebar_open);
            assert!(!restored.develop_filmstrip_open);
        }
    }

    #[test]
    fn android_always_sanitizes_subject_quality_to_low() {
        assert_eq!(
            subject_quality_for_platform(crate::ai_masks::BiRefNetQuality::High, true),
            crate::ai_masks::BiRefNetQuality::Low
        );
        assert_eq!(
            subject_quality_for_platform(crate::ai_masks::BiRefNetQuality::High, false),
            crate::ai_masks::BiRefNetQuality::High
        );
    }
}
