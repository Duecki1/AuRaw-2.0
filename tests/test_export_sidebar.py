from __future__ import annotations

from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
SIDEBAR = (ROOT / "src/ui/sidebar.rs").read_text(encoding="utf-8")
TOP_BAR = (ROOT / "src/ui/top_bar.rs").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")
PROCESSING = (ROOT / "src/pipeline/processing.rs").read_text(encoding="utf-8")


def test_sidebar_has_exactly_four_requested_tabs() -> None:
    enum = re.search(r"pub enum SidebarTab \{(.+?)\n\}", APP, re.DOTALL)
    assert enum is not None
    body = enum.group(1)
    for name in ("Adjustments", "Masks", "Inpainting", "Export"):
        assert name in body
        assert f'SidebarTab::{name}, "{name}"' in SIDEBAR
    assert "fn show_masks" in SIDEBAR
    for kind in ("Brush", "Radial", "Linear", "Subject", "Background", "Object", "Landscape", "LuminanceRange", "ColorRange", "DepthRange"):
        assert f"MaskKind::{kind}" in SIDEBAR
    assert "generative inpainting controls are coming later" in SIDEBAR


def test_export_button_only_lives_in_export_sidebar_tab() -> None:
    assert "Export PNG…" not in TOP_BAR
    assert 'egui::Button::new("Export PNG…")' in SIDEBAR
    export_match = re.search(
        r"fn show_export\(.+?\n    \}\n\n    fn show_basic", SIDEBAR, re.DOTALL
    )
    assert export_match is not None
    assert "app.export_png(frame)" in export_match.group(0)


def test_export_resize_modes_and_metadata_controls_are_wired() -> None:
    for mode in (
        "Original",
        "LongEdge",
        "ShortEdge",
        "Width",
        "Height",
        "Percentage",
    ):
        assert mode in EXPORT
        assert f"ExportResizeMode::{mode}" in SIDEBAR
    assert "checked_output_dimensions" in EXPORT
    assert "resample_raw" not in EXPORT
    assert "read_display_linear_region_blocking" in EXPORT
    assert "LinearLightResizer" in EXPORT
    assert "vertical_by_source" in EXPORT
    assert "pending_rows" in EXPORT
    assert "cached_rows" not in EXPORT
    assert "IccOutputTransform::srgb" in EXPORT
    assert "temporary_export_path" in EXPORT
    assert "publish_completed_export" in EXPORT
    assert "MAX_EXPORT_PIXELS" in EXPORT
    assert "edge_or_dimension" in SIDEBAR
    assert "percentage" in SIDEBAR
    assert "allow_upscale" in SIDEBAR
    assert "keep_metadata" in SIDEBAR
    assert "build_exif_payload" in EXPORT
    assert "exif_metadata" in EXPORT
    assert 'add_itxt_chunk("Camera"' in EXPORT
    assert 'add_itxt_chunk("Source"' in EXPORT


def test_raw_mosaic_has_no_final_output_resizer() -> None:
    assert "pub fn resample_raw" not in PROCESSING
    assert "build_proxy" in PROCESSING
    assert "nearest_cfa_sample" in PROCESSING
    assert "read_display_linear_region_blocking" in EXPORT


def test_export_settings_are_defaulted_and_passed_to_worker() -> None:
    assert APP.count("export_settings: ExportSettings::default()") == 2
    assert "self.export_settings," in APP
    assert "ExportMetadata::from_raw" in APP
    assert "settings.keep_metadata" in EXPORT


def test_export_halo_covers_the_widest_glow_filter_support() -> None:
    shader = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
    match = re.search(r"pub const EXPORT_TILE_HALO: u32 = (\d+);", PROCESSING)
    assert match is not None
    halo = int(match.group(1))
    # glow_blur_at spans +/-2 * step_far and glow_source_at reaches one
    # additional pixel, so the current shader needs a 97-pixel radius.
    assert "let step_far = min(step_near * 2, 48);" in shader
    assert "for (var ky = -2; ky <= 2; ky = ky + 1)" in shader
    assert "for (var kx = -2; kx <= 2; kx = kx + 1)" in shader
    assert halo >= 2 * 48 + 1
    assert halo % 8 == 0
    assert "(EXPORT_TILE_HALO..=512).contains(&spec.halo)" in EXPORT
