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
    assert "Local adjustment masks will appear here" in SIDEBAR
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
    assert "output_dimensions" in EXPORT
    assert "resample_raw" in EXPORT
    assert "edge_or_dimension" in SIDEBAR
    assert "percentage" in SIDEBAR
    assert "allow_upscale" in SIDEBAR
    assert "keep_metadata" in SIDEBAR
    assert "build_exif_payload" in EXPORT
    assert "exif_metadata" in EXPORT
    assert 'add_itxt_chunk("Camera"' in EXPORT
    assert 'add_itxt_chunk("Source"' in EXPORT


def test_export_resampler_preserves_cfa_planes() -> None:
    assert "pub fn resample_raw" in PROCESSING
    assert "raw.color_indices[index] != cfa" in PROCESSING
    assert "x_weight * y_weight" in PROCESSING
    assert "nearest_cfa_sample" in PROCESSING


def test_export_settings_are_defaulted_and_passed_to_worker() -> None:
    assert APP.count("export_settings: ExportSettings::default()") == 2
    assert "self.export_settings," in APP
    assert "ExportMetadata::from_raw" in APP
    assert "settings.keep_metadata" in EXPORT
