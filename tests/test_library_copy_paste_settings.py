from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SIDECAR = (ROOT / "src/sidecar.rs").read_text(encoding="utf-8")
SETTINGS = (ROOT / "src/ui/settings.rs").read_text(encoding="utf-8")
PERFORMANCE = (ROOT / "src/performance_settings.rs").read_text(encoding="utf-8")
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")


def test_copy_settings_separate_geometry_profile_and_mask_categories() -> None:
    assert "pub geometry: bool" in SIDECAR
    assert "pub camera_profile: bool" in SIDECAR
    assert "pub masks: bool" in SIDECAR
    assert "pub ai_masks: bool" in SIDECAR
    assert 'checkbox(&mut settings.geometry, "Geometry")' in SETTINGS
    assert 'checkbox(&mut settings.camera_profile, "Camera profile")' in SETTINGS
    assert 'checkbox(&mut settings.masks, "Normal masks")' in SETTINGS
    assert 'checkbox(&mut settings.ai_masks, "AI masks")' in SETTINGS


def test_copy_setting_defaults_and_migration_are_explicit() -> None:
    default_block = SIDECAR[SIDECAR.index("impl Default for AdjustmentCopySettings") :]
    default_block = default_block[: default_block.index("#[derive(Clone, Debug, PartialEq")]
    assert "geometry: false" in default_block
    assert "camera_profile: true" in default_block
    assert "masks: true" in default_block
    assert "ai_masks: true" in default_block
    assert "const SETTINGS_VERSION: u32 = 6" in PERFORMANCE
    assert "self.adjustment_copy_settings.ai_masks = self.adjustment_copy_settings.masks" in PERFORMANCE


def test_ai_mask_refresh_progress_is_visible_for_one_or_many_images() -> None:
    progress = LIBRARY[LIBRARY.index('egui::Window::new("Regenerating AI masks")') - 350 :]
    progress = progress[: progress.index('#[cfg(not(target_os = "android"))]')]
    assert "if total > 1" not in progress
    assert "egui::ProgressBar::new(fraction)" in progress
    assert 'format!("{completed} / {total} processed")' in progress
