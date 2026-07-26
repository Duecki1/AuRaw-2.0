from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
LIFECYCLE = (ROOT / "src/app/lifecycle.rs").read_text(encoding="utf-8")
PROCESSING = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
PREVIEW = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
TOP_BAR = (ROOT / "src/ui/top_bar.rs").read_text(encoding="utf-8")


def test_desktop_toolbar_toggles_original_and_edited_preview() -> None:
    assert '#[cfg(not(target_os = "android"))]' in TOP_BAR
    assert '"Show original preview"' in TOP_BAR
    assert '"Show edited preview"' in TOP_BAR
    assert "egui_phosphor::regular::EYE" in TOP_BAR
    assert "egui_phosphor::regular::EYE_SLASH" in TOP_BAR
    assert "app.original_preview_visible()" in TOP_BAR
    assert "app.toggle_original_preview();" in TOP_BAR


def test_android_hold_shows_original_only_while_stationary() -> None:
    assert '#[cfg(target_os = "android")]' in PREVIEW
    assert "const HOLD_TIME" in PREVIEW
    assert "Duration::from_millis(350)" in PREVIEW
    assert "MAX_STATIONARY_DISTANCE" in PREVIEW
    assert "android_original_hold" in APP
    assert "input.pointer.primary_down()" in PREVIEW
    assert "app.set_original_preview_requested(false)" in PREVIEW
    assert "app.set_original_preview_requested(true)" in PREVIEW
    assert "position.distance(hold.start) > MAX_STATIONARY_DISTANCE" in PREVIEW


def test_original_preview_reuses_gpu_pipelines_without_texture_swapping() -> None:
    assert "original_preview_exposure: ExposureParams" in APP
    assert "original_preview_requested: bool" in APP
    assert "original_preview_rendered_state: Option<(bool, u64)>" in APP
    assert "self.original_preview_exposure = initial_exposure" in LIFECYCLE
    assert "pub(crate) fn sync_original_preview" in PROCESSING
    assert "pipeline.recompute(&render_state.queue, &render_state.device, &params)" in PROCESSING
    assert "requested_state = (self.original_preview_requested, self.preview_revision)" in PROCESSING
    assert "app.original_preview_visible()" in PREVIEW
