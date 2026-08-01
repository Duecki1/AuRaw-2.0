from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
PROCESSING_EXPORT = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
PROCESSING = (ROOT / "src/pipeline/processing.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
PREVIEW = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")


def test_zoom_detail_reuses_compiled_gpu_programs_and_existing_crop_pipeline() -> None:
    assert "new_headless_reusing_programs" in GPU
    assert "template.pipelines[program_index].clone()" in GPU
    assert ".upload_raw_tile(&render_state.queue, &detail_raw)" in PROCESSING_EXPORT


def test_zoom_detail_is_viewport_sized_and_starts_immediately_above_fit() -> None:
    assert "preview_viewport_pixels" in APP
    assert "pixels_per_point" in PREVIEW
    assert "requested_detail_edge" in PROCESSING_EXPORT
    assert "const DETAIL_ZOOM_START: f32 = 1.0005" in PROCESSING_EXPORT
    assert "requested_visible_width <= visible_proxy_width * 1.05" not in PROCESSING_EXPORT
    assert 'Duration::from_millis(if cfg!(target_os = "android") { 220 } else { 140 })' in PROCESSING_EXPORT


def test_zoom_detail_builds_only_the_padded_visible_raw_region() -> None:
    assert "pub fn build_region_proxy" in PROCESSING
    assert "build_region_proxy(" in PROCESSING_EXPORT
    assert "crop_raw(&full_raw" not in PROCESSING_EXPORT


def test_zoom_detail_hides_processing_halo_and_aligns_cfa_phase() -> None:
    assert ".max(EXPORT_TILE_HALO)" in PROCESSING_EXPORT
    assert "aligned_detail_axis" in PROCESSING_EXPORT
    assert "texture_uv_rect" in APP
    assert "detail.texture_uv_rect" in PREVIEW
    assert "let phase_x = (x + output_phase_x).min(raw.width - 1);" in PROCESSING
    assert "let phase_y = (y + output_phase_y).min(raw.height - 1);" in PROCESSING


def test_zoomed_adjustments_dispatch_detail_with_live_main_fallback() -> None:
    assert "queue_preview_processing" in PROCESSING_EXPORT
    assert "self.advance_zoomed_processing(frame);" in PROCESSING_EXPORT
    assert "if self.preview_detail_is_current()" in PROCESSING_EXPORT
    assert ".dispatch_stage(&render_state.queue, &render_state.device, &params, stage)" in PROCESSING_EXPORT
    assert "navigation_pending_stage" in APP
    assert "advance_navigation_preview" in PROCESSING_EXPORT
    assert "navigation_proxy_edge()" in PROCESSING_EXPORT
    assert "new_headless_reusing_programs_with_mask_edge" in PROCESSING_EXPORT
    assert "preview_base_pipeline" in PROCESSING_EXPORT
    assert ".preview_base_pipeline()" in PREVIEW
    preview_base = PROCESSING_EXPORT[
        PROCESSING_EXPORT.index("pub(crate) fn preview_base_pipeline"):
        PROCESSING_EXPORT.index("pub(crate) fn preview_is_preparing")
    ]
    assert "self.gpu_pipeline.as_ref()" in preview_base
    assert "preview_navigation" not in preview_base


def test_zoomed_adjustments_reuse_the_existing_crop_without_rebuilding_raw() -> None:
    assert "raw: Arc<LoadedRaw>" in APP
    assert "source_origin: [u32; 2]" in APP
    assert "let detail_raw = Arc::clone(&detail.raw);" in PROCESSING_EXPORT
    assert "if detail_is_current {" in PROCESSING_EXPORT
    detail_guard_start = PROCESSING_EXPORT.index("if detail_is_current {")
    detail_guard_end = PROCESSING_EXPORT.index("let urgent =", detail_guard_start)
    assert "return;" in PROCESSING_EXPORT[detail_guard_start:detail_guard_end]


def test_zoomed_fallback_processing_repaints_until_complete() -> None:
    eframe_impl = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
    assert "self.preview_processing_pending()" in eframe_impl
    pending = PROCESSING_EXPORT[
        PROCESSING_EXPORT.index("pub(crate) fn preview_processing_pending"):
        PROCESSING_EXPORT.index("pub(crate) fn note_preview_motion")
    ]
    assert "self.preview_detail_pending_stage.is_some()" in pending
    assert "self.navigation_pending_stage.is_some()" in pending
    assert "!self.preview_detail_is_current()" in pending


def test_navigation_proxy_is_full_frame_but_intentionally_very_low_resolution() -> None:
    assert 'if cfg!(target_os = "android") { 384 } else { 512 }' in PROCESSING_EXPORT
    assert 'if cfg!(target_os = "android") { 256 } else { 384 }' in PROCESSING_EXPORT
    assert "Arc::new(build_proxy(" in PROCESSING_EXPORT
    assert "navigation_dirty_mask_layers" in APP


def test_plain_zoom_does_not_eagerly_build_a_second_full_frame_pipeline() -> None:
    navigation = PROCESSING_EXPORT[
        PROCESSING_EXPORT.index("fn advance_navigation_preview"):
        PROCESSING_EXPORT.index("pub(crate) fn mark_pipeline_dirty")
    ]
    assert "let zoomed = self.preview_zoom > DETAIL_ZOOM_START;" in navigation
    assert "zoomed && (self.preview_navigation.is_some() || should_update)" in navigation
    assert "Eager creation here caused a visible hitch" in navigation


def test_android_detail_crop_waits_until_pinch_navigation_ends() -> None:
    detail = PROCESSING_EXPORT[
        PROCESSING_EXPORT.index("fn advance_preview_detail"):
        PROCESSING_EXPORT.index("fn advance_navigation_preview")
    ]
    assert "if self.preview_touch_navigation_active" in detail
    assert "both fingers are lifted" in detail


def test_no_zoom_phase_can_flash_the_tiny_navigation_proxy() -> None:
    preview_base = PROCESSING_EXPORT[
        PROCESSING_EXPORT.index("pub(crate) fn preview_base_pipeline"):
        PROCESSING_EXPORT.index("pub(crate) fn preview_is_preparing")
    ]
    assert "self.gpu_pipeline.as_ref()" in preview_base
    assert "preview_navigation" not in preview_base
    assert "if !should_exist && !should_update {" in PROCESSING_EXPORT
    assert "self.preview_navigation.take()" in PROCESSING_EXPORT
    assert "if self.preview_zoom > DETAIL_ZOOM_START {" in PROCESSING_EXPORT
    assert "self.preview_zoom > DETAIL_ZOOM_START || self.preview_navigation.is_some()" not in PROCESSING_EXPORT


def test_all_preview_qualities_follow_physical_screen_density() -> None:
    for variant in ("Low", "Medium", "High", "Max"):
        assert f"Self::{variant}" in APP
    for density in ("0.50", "0.67", "0.84", "1.00"):
        assert density in APP
    assert "edge_for_scale(viewport_pixels, 1.0)" in APP
    assert "edge_for_scale(viewport_pixels, 1.35)" in APP
    assert "available.x * pixels_per_point" in PREVIEW
    assert "available.y * pixels_per_point" in PREVIEW


def test_preview_geometry_does_not_change_when_backing_proxy_switches() -> None:
    assert "geometry_width" in PREVIEW
    assert "geometry_height" in PREVIEW
    assert ".loaded_raw" in PREVIEW
    assert "let source_dimensions = app" in PREVIEW
    assert ".loaded_raw" in PREVIEW
    assert "geometry_width as f32 / geometry_height.max(1) as f32" in PREVIEW


def test_zoom_detail_masks_remain_in_full_image_coordinate_space() -> None:
    # adjustments.wgsl samples local_mask_tex using tile_origin/full_size UVs.
    # A crop-remapped atlas would therefore be transformed twice and drift as
    # the detail crop pans around the image.
    assert "let global_pos = vec2<f32>(pos + tile_origin())" in ADJUSTMENTS
    assert "let uv = clamp(global_pos / full_size" in ADJUSTMENTS
    assert "let detail_masks = self.masks.cropped_for_region(" not in PROCESSING_EXPORT
    assert "&self.masks,\n            &detail_raw," in PROCESSING_EXPORT
    assert "&self.masks,\n            &full_raw," in PROCESSING_EXPORT
    assert "full_raw.width,\n                    full_raw.height," in PROCESSING_EXPORT
    assert ".upload_raw_tile(&render_state.queue, &detail_raw)" in PROCESSING_EXPORT
    assert "self.detail_dirty_mask_layers.iter().any" in PROCESSING_EXPORT
