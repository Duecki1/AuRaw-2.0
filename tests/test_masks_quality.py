from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MASKS = (ROOT / "src/pipeline/masks.rs").read_text(encoding="utf-8")
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
PREVIEW = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
SIDEBAR = (ROOT / "src/ui/sidebar.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
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
    assert "cfg(target_os = \"android\")" not in PREVIEW
    assert "distance_px" in PREVIEW


def test_submasks_support_lightroom_style_boolean_composition() -> None:
    assert "MaskCombineMode::Add" in MASKS
    assert "MaskCombineMode::Subtract" in MASKS
    assert "MaskCombineMode::Intersect" in MASKS
    assert "*dst = dst.max(src)" in MASKS
    assert "*dst *= 1.0 - src" in MASKS
    assert "*dst *= src" in MASKS


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
