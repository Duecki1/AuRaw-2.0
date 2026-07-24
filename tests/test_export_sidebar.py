from __future__ import annotations

from tests.source_helpers import read_source_tree
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
APP = read_source_tree(ROOT / "src/app.rs")
SIDEBAR = read_source_tree(ROOT / "src/ui/sidebar.rs")
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
    assert "fn show_inpainting" in SIDEBAR
    assert '"Paint over unwanted content"' not in SIDEBAR
    assert '"Scene-referred RAW controls"' not in SIDEBAR
    assert 'ui.heading("Adjustments")' not in SIDEBAR
    assert 'ui.heading("Masks")' not in SIDEBAR
    assert 'ui.heading("Inpainting")' not in SIDEBAR
    assert 'ui.heading("Export")' not in SIDEBAR
    assert 'ui.small_button("Reset all")' in SIDEBAR
    assert '"Drag on the image. Releasing each stroke runs the local LaMa eraser."' in SIDEBAR


def test_export_button_only_lives_in_export_sidebar_tab() -> None:
    assert "Export PNG…" not in TOP_BAR
    button = 'egui::Button::new("Export PNG…")'
    assert SIDEBAR.count(button) == 1
    assert "fn show_export" in SIDEBAR
    assert SIDEBAR.index("fn show_export") < SIDEBAR.index(button)
    assert "app.export_png(frame)" in SIDEBAR
    assert 'egui::Button::new("Export JPEG…")' in SIDEBAR
    assert "app.export_jpeg(frame)" in SIDEBAR
    assert "jpeg_quality" in SIDEBAR


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
    assert "begin_display_linear_region_readback" in EXPORT
    assert ".finish(device)" in EXPORT
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
    assert "begin_display_linear_region_readback" in EXPORT
    assert ".finish(device)" in EXPORT


def test_export_settings_are_defaulted_and_passed_to_worker() -> None:
    assert APP.count("export_settings: ExportSettings::default()") == 2
    assert "self.export_settings," in APP
    assert "ExportMetadata::from_raw" in APP
    assert "settings.keep_metadata" in EXPORT
    assert "settings.jpeg_quality" in EXPORT
    assert "spawn_tiled_jpeg_export" in EXPORT
    assert "JpegEncoder::new_with_quality" in EXPORT
    assert 'write_all(b"Exif\\0\\0")' in EXPORT


def test_export_halo_covers_cumulative_spatial_support() -> None:
    shader = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
    highlight = (ROOT / "src/shaders/highlight_lch_pass.wgsl").read_text(encoding="utf-8")
    # Glow's five B3 stages accumulate steps 3+3+6+12+24 at maximum
    # resolution scale, with +/-2*step support in every stage.
    assert "2 * (3 + 3 + 6 + 12 + 24)" in shader
    assert "fn glow_diffuse_at" in shader
    assert "for (var ky = -2; ky <= 2; ky = ky + 1)" in shader
    assert "for (var kx = -2; kx <= 2; kx = kx + 1)" in shader
    assert "2 * (16 + 8 + 4 + 2 + 1 + 4 + 2 + 1 + 2 + 1 + 1)" in PROCESSING
    for radius in (16, 8, 4, 2, 1):
        assert f"run_highlight_guided_pass(gid, {radius}," in highlight
    assert "const EXPORT_CUMULATIVE_SUPPORT" in PROCESSING
    assert "EXPORT_CUMULATIVE_SUPPORT.div_ceil(8) * 8" in PROCESSING
    assert 'TONE_GUIDE_SUPPORT: u32 = if cfg!(target_os = "android") { 32 } else { 24 }' in PROCESSING
    assert "LOCAL_EFFECTS_SUPPORT: u32 = 24" in PROCESSING
    assert "GLOW_SUPPORT: u32 = 96" in PROCESSING
    assert "(MIN_EXPORT_TILE_HALO..=512).contains(&spec.halo)" in EXPORT
    assert "required_export_tile_halo(exposure, masks)" in EXPORT
    assert "tile_spec.halo.max(required_halo)" in EXPORT


def test_export_tone_statistics_cover_native_resolution_tile_cores() -> None:
    gpu = read_source_tree(ROOT / "src/pipeline/gpu.rs")
    tone = (ROOT / "src/shaders/tone_analysis.wgsl").read_text(encoding="utf-8")
    assert "preview_raw" not in EXPORT
    assert "begin_export_tone_analysis" in EXPORT
    assert "accumulate_export_tone_tile" in EXPORT
    assert "finish_export_tone_analysis" in EXPORT
    assert ".with_tone_histogram_bounds(" in EXPORT
    assert "tone_histogram_bounds: [u32; 4]" in gpu
    assert "params.tone_histogram_bounds" in tone


def test_export_sidebar_shows_live_progress_bar() -> None:
    assert "export_progress_state" in SIDEBAR
    assert "egui::ProgressBar::new" in SIDEBAR
    assert "Preparing export…" in SIDEBAR
    assert "tiles" in SIDEBAR

