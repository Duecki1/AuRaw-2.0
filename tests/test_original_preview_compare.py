from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
LIFECYCLE = (ROOT / "src/app/lifecycle.rs").read_text(encoding="utf-8")
PROCESSING = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
PREVIEW = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
TOP_BAR = (ROOT / "src/ui/top_bar.rs").read_text(encoding="utf-8")


def test_desktop_toolbar_toggles_original_and_edited_preview() -> None:
    assert '#[cfg(not(target_os = "android"))]' in TOP_BAR
    assert '"Show Original"' in TOP_BAR
    assert '"Show Edited"' in TOP_BAR
    assert "app.show_original_preview = !app.show_original_preview" in TOP_BAR


def test_android_hold_shows_original_only_while_stationary() -> None:
    assert 'cfg!(target_os = "android")' in PREVIEW
    assert "HOLD_DURATION" in PREVIEW
    assert "Duration::from_secs(1)" in PREVIEW
    assert "MOVE_TOLERANCE_POINTS" in PREVIEW
    assert "original_preview_hold_cancelled" in APP
    assert "input.pointer.primary_down()" in PREVIEW
    assert "app.show_original_preview = false" in PREVIEW
    assert "Original · release for edited" in PREVIEW


def test_original_preview_is_a_separate_cached_texture() -> None:
    assert "original_preview_texture: Option<egui::TextureHandle>" in APP
    assert "OriginalPreviewImage" in APP
    assert "GpuParams::new(&initial_exposure, &original_masks, &preview_raw)" in LIFECYCLE
    assert "new_headless_reusing_programs_with_mask_edge" in LIFECYCLE
    assert 'self.egui_ctx.load_texture(' in LIFECYCLE
    assert "rebuild_original_preview_texture" in PROCESSING
    assert "if !show_original" in PREVIEW
