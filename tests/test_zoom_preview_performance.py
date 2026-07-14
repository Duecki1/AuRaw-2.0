from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
PROCESSING_EXPORT = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
PROCESSING = (ROOT / "src/pipeline/processing.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
PREVIEW = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")


def test_zoom_detail_reuses_compiled_gpu_programs_and_existing_crop_pipeline() -> None:
    assert "new_headless_reusing_programs" in GPU
    assert "template.passes[program_index].pipeline.clone()" in GPU
    assert ".upload_raw_tile(&render_state.queue, &detail_raw)" in PROCESSING_EXPORT


def test_zoom_detail_is_viewport_sized_and_skips_no_gain_renders() -> None:
    assert "preview_viewport_pixels" in APP
    assert "pixels_per_point" in PREVIEW
    assert "requested_detail_edge" in PROCESSING_EXPORT
    assert "requested_visible_width <= visible_proxy_width * 1.05" in PROCESSING_EXPORT


def test_zoom_detail_builds_only_the_padded_visible_raw_region() -> None:
    assert "pub fn build_region_proxy" in PROCESSING
    assert "build_region_proxy(" in PROCESSING_EXPORT
    assert "crop_raw(&full_raw" not in PROCESSING_EXPORT


def test_zoom_detail_hides_processing_halo_and_aligns_cfa_phase() -> None:
    assert ".max(EXPORT_TILE_HALO)" in PROCESSING_EXPORT
    assert "aligned_detail_axis" in PROCESSING_EXPORT
    assert "texture_uv_rect" in APP
    assert "detail.texture_uv_rect" in PREVIEW
    assert "Anchor the synthetic proxy mosaic to the source region's real CFA" in PROCESSING


def test_zoomed_adjustments_dispatch_only_the_visible_detail_pipeline() -> None:
    assert "queue_preview_processing" in PROCESSING_EXPORT
    assert "if self.preview_zoom > 1.01 {\n            self.advance_zoomed_processing(frame);\n            return;" in PROCESSING_EXPORT
    assert ".dispatch_stage(&render_state.queue, &render_state.device, &params, stage)" in PROCESSING_EXPORT
    assert "The full-frame work is deliberately deferred until" in PROCESSING_EXPORT


def test_zoomed_adjustments_reuse_the_existing_crop_without_rebuilding_raw() -> None:
    assert "raw: Arc<LoadedRaw>" in APP
    assert "source_origin: [u32; 2]" in APP
    assert "let detail_raw = Arc::clone(&detail.raw);" in PROCESSING_EXPORT
    assert "Parameter edits are dispatched directly into this current crop" in PROCESSING_EXPORT


def test_deferred_full_preview_does_not_force_continuous_zoomed_repaints() -> None:
    eframe_impl = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
    assert "self.preview_detail_pending_stage.is_some()" in eframe_impl
    assert "self.preview_zoom <= 1.01 && self.pending_stage.is_some()" in eframe_impl
