from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SIDECAR = (ROOT / "src/sidecar.rs").read_text(encoding="utf-8")
SETTINGS = (ROOT / "src/ui/settings.rs").read_text(encoding="utf-8")
PERFORMANCE = (ROOT / "src/performance_settings.rs").read_text(encoding="utf-8")
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
LIBRARY_ADJUSTMENTS = (ROOT / "src/app/library_adjustments.rs").read_text(encoding="utf-8")
MASKS_AI = (ROOT / "src/app/masks_ai.rs").read_text(encoding="utf-8")


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
    assert 'format!("{completed} / {total} AI masks updated")' in progress


def test_batch_paste_finishes_sidecar_destinations_before_reloading_current_image() -> None:
    assert "let ordered_paths = raw_paths" in LIBRARY_ADJUSTMENTS
    assert "let ordered_targets = targets" in LIBRARY_ADJUSTMENTS
    assert "Process every sidecar-only destination" in LIBRARY_ADJUSTMENTS


def test_ai_mask_refresh_waits_for_pasted_and_regenerated_sidecar_saves() -> None:
    assert "Saving," in APP[APP.index("enum LibraryAiMaskRefreshPhase") :]
    can_start = LIBRARY_ADJUSTMENTS[
        LIBRARY_ADJUSTMENTS.index("pub(crate) fn can_start_library_ai_mask_refresh") :
    ]
    can_start = can_start[: can_start.index("pub(crate) fn library_ai_mask_refresh_status")]
    assert "!self.sidecar_save_in_progress()" in can_start
    poll = LIBRARY_ADJUSTMENTS[
        LIBRARY_ADJUSTMENTS.index("pub(crate) fn poll_library_ai_mask_refresh") :
    ]
    poll = poll[: poll.index("fn finish_library_ai_mask_refresh")]
    assert "LibraryAiMaskRefreshPhase::Saving" in poll
    assert "self.queue_explicit_sidecar_save()" in poll
    assert "if self.sidecar_save_in_progress()" in poll


def test_ai_mask_refresh_discovers_stale_components_without_cached_bitmaps() -> None:
    targets = MASKS_AI[
        MASKS_AI.index("fn generated_ai_mask_targets") :
    ]
    targets = targets[: targets.index("fn has_range_mask_targets")]
    assert "MaskGeometry::Ai { .. }" in targets
    assert "MaskGeometry::Object { strokes, .. }" in targets
    assert "mask: Some(_)" not in targets

    update_loop = MASKS_AI[MASKS_AI.index("fn continue_ai_mask_update") :]
    update_loop = update_loop[: update_loop.index("fn finish_ai_mask_update")]
    assert "MaskGeometry::Object { strokes, .. }" in update_loop
    assert "mask: Some(_)" not in update_loop

    content_aware = SIDECAR[
        SIDECAR.index("fn masks_contain_content_aware_components") :
    ]
    content_aware = content_aware[: content_aware.index("pub fn edit_state_has_adjustments")]
    assert "MaskGeometry::Ai { .. }" in content_aware
    assert "MaskGeometry::Object { strokes, .. }" in content_aware
    assert "mask: Some(_)" not in content_aware


def test_paste_recovers_from_invalid_destination_sidecar_json() -> None:
    helper = LIBRARY_ADJUSTMENTS[
        LIBRARY_ADJUSTMENTS.index("fn desktop_library_sidecar_edits") :
    ]
    helper = helper[: helper.index("impl AurawApp")]
    assert "SidecarError::Invalid(error)" in helper
    assert "Ok(None)" in helper
    assert "desktop_library_sidecar_edits(raw_path)?" in LIBRARY_ADJUSTMENTS


def test_library_ai_refresh_keeps_its_progress_window_visible() -> None:
    worker_dialogs = MASKS_AI[MASKS_AI.index("fn show_subject_dialogs") :]
    assert "let library_batch_refreshing = self.library_ai_mask_refresh.is_some();" in worker_dialogs
    assert "self.subject_receiver.is_some() && !library_batch_refreshing" in worker_dialogs
    assert "self.object_receiver.is_some() && !library_batch_refreshing" in worker_dialogs


def test_library_ai_refresh_reports_inflight_mask_progress() -> None:
    status = LIBRARY_ADJUSTMENTS[
        LIBRARY_ADJUSTMENTS.index("pub(crate) fn library_ai_mask_refresh_status") :
    ]
    status = status[: status.index("pub(crate) fn start_library_ai_mask_refresh_paths")]
    assert "ai_mask_update_remaining_target_count" in status
    assert "state.mask_completed + current_mask_progress" in status
