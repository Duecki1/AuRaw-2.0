from tests.source_helpers import read_source_tree
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MASKS = (ROOT / "src/pipeline/masks.rs").read_text(encoding="utf-8")
APP = read_source_tree(ROOT / "src/app.rs")
PREVIEW = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
SIDEBAR = read_source_tree(ROOT / "src/ui/sidebar.rs")
GPU = read_source_tree(ROOT / "src/pipeline/gpu.rs")
COMMON = (ROOT / "src/shaders/common.wgsl").read_text(encoding="utf-8")
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")


def test_mask_types_and_placeholders_are_present() -> None:
    for kind in (
        "Brush", "Radial", "Linear", "Subject", "Background", "Object",
        "Landscape", "LuminanceRange", "ColorRange", "DepthRange",
    ):
        assert kind in MASKS
        assert f"MaskKind::{kind}" in SIDEBAR
    assert "· soon" in SIDEBAR


def test_brush_radial_and_linear_interactions_are_cross_platform_egui() -> None:
    assert "Sense::drag()" in PREVIEW
    assert "response.is_pointer_button_down_on()" in PREVIEW
    assert "BrushMode::Paint" in PREVIEW
    assert "BrushMode::Erase" in PREVIEW
    assert "MaskGeometry::Radial" in PREVIEW
    assert "MaskGeometry::Linear" in PREVIEW
    # Mask editing itself stays on the shared egui path. Android-only code in
    # this module is limited to the press-and-hold original-preview gesture.
    mask_interaction = PREVIEW[PREVIEW.index("fn handle_mask_interaction"):]
    assert "cfg(target_os = \"android\")" not in mask_interaction
    assert "distance_px" in PREVIEW


def test_submasks_support_lightroom_style_boolean_composition() -> None:
    assert "MaskCombineMode::Add" in MASKS
    assert "MaskCombineMode::Subtract" in MASKS
    assert "MaskCombineMode::Intersect" in MASKS
    assert "*dst = dst.max(src)" in MASKS
    assert "*dst *= 1.0 - src" in MASKS
    assert "*dst *= src" in MASKS


def test_submasks_have_user_renameable_names() -> None:
    assert "pub name: String" in MASKS
    assert "name: kind.label().to_owned()" in MASKS
    assert "component.name" in SIDEBAR


def test_mask_atlas_is_shared_by_preview_and_export() -> None:
    assert "R8Unorm" in GPU
    assert "TextureViewDimension::D2Array" in GPU
    assert "update_mask_layer" in GPU
    assert "mark_mask_geometry_dirty" in APP
    assert "upload_mask_atlas" in EXPORT
    assert "rasterize_layer" in EXPORT


def test_local_adjustments_are_scene_linear_and_mask_weighted() -> None:
    assert "mask_adjust_0: array<vec4<f32>, 8>" in COMMON
    assert "mask_adjust_1: array<vec4<f32>, 8>" in COMMON
    assert "mask_adjust_2: array<vec4<f32>, 8>" in COMMON
    assert "local_adjustment_mix" in ADJUSTMENTS
    assert "apply_local_basic_tone_values" in ADJUSTMENTS
    assert "apply_temperature_tint_values" in ADJUSTMENTS
    assert "apply_texture_and_clarity_values" in ADJUSTMENTS
    assert "apply_dehaze_value" in ADJUSTMENTS
    assert "apply_saturation_value" in ADJUSTMENTS


def test_ai_and_range_masks_use_a_stable_unedited_raw_reference() -> None:
    capture = APP[APP.index("pub(crate) fn capture_mask_source"):APP.index("pub(crate) fn request_subject_mask")]
    assert "loaded_raw" in capture
    assert "ExposureParams::scene_referred_default()" in capture
    assert "MaskStack::default()" in capture
    assert "RawGpuPipeline::new_headless_reusing_programs" in capture
    assert "reference_pipeline.read_output_region_blocking" not in capture  # formatted as a chained call
    assert "let rgba = reference_pipeline" in capture
    assert "live edited output texture" in capture
    assert "source_edge" in capture and "2048" in capture


def test_brush_input_and_rasterization_avoid_progressive_slowdown() -> None:
    assert "if first_dab" in PREVIEW
    assert "distance_px >= spacing_px * 0.80" in PREVIEW
    assert "if changed" in PREVIEW and "last emitted point" in PREVIEW
    assert "use rayon::prelude::*" in MASKS
    assert "par_chunks_mut" in MASKS
    assert "ROW_BAND_HEIGHT" in MASKS
    assert "into_par_iter" in MASKS
    assert "take ownership" in MASKS


def test_ai_subject_feather_is_resolution_relative_and_background_is_exact_complement() -> None:
    assert "fn chamfer_distance" in MASKS
    assert "width.min(height) as f32 * 0.045" in MASKS
    assert "distance_to_outside[index] - distance_to_inside[index]" in MASKS
    assert "feather_probability_mask(&mut coverage" in MASKS
    assert "component.kind == MaskKind::Background" in MASKS
    assert "*value = 1.0 - *value" in MASKS
    assert "fixed 32-texel box blur" in MASKS


def test_ai_mask_resampling_is_bilinear_before_feathering() -> None:
    assert "let top = sample(x0, y0)" in MASKS
    assert "let bottom = sample(x0, y1)" in MASKS
    assert "*value = top + (bottom - top) * fy" in MASKS


def test_mask_input_and_cursor_are_clipped_to_the_visible_preview() -> None:
    assert "Self::handle_mask_interaction(ui, app, image_rect, visible_screen, &response)" in PREVIEW
    assert ".filter(|position| preview_rect.contains(*position))" in PREVIEW
    assert "let primary_down = pointer.is_some()" in PREVIEW
    assert "Self::paint_mask_overlay(ui, app, image_rect, visible_screen)" in PREVIEW
    assert "let painter = ui.painter_at(preview_rect);" in PREVIEW
    assert "painter_image_clipped" in PREVIEW
