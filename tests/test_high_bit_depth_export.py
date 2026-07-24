from __future__ import annotations

from pathlib import Path

from tests.source_helpers import read_source_tree

ROOT = Path(__file__).resolve().parents[1]
EXPORT = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")
PIPELINE = (ROOT / "src/pipeline/mod.rs").read_text(encoding="utf-8")
SIDEBAR = read_source_tree(ROOT / "src/ui/sidebar.rs")
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")
APP = read_source_tree(ROOT / "src/app.rs")


def test_export_model_exposes_tiff_high_bit_depth_and_float_master() -> None:
    assert "pub enum ExportFormat" in EXPORT
    assert "Tiff" in EXPORT
    assert 'Self::Tiff => "image/tiff"' in EXPORT
    assert "pub enum ExportBitDepth" in EXPORT
    assert "Sixteen" in EXPORT
    assert "Float32Linear" in EXPORT
    assert "pub enum ExportColorProfile" in EXPORT
    assert "CustomIcc" in EXPORT
    assert "spawn_tiled_tiff_export" in PIPELINE
    assert "spawn_tiled_tiff_export" in APP


def test_png_supports_16_bit_and_embeds_selected_icc() -> None:
    assert "png::BitDepth::Sixteen" in EXPORT
    assert "ExportRowFormat::Rgba16Be" in EXPORT
    assert "info.icc_profile = Some(Cow::Owned(profile.clone()))" in EXPORT
    assert "IccOutputTransform::from_icc" in EXPORT
    assert "RenderingIntent::RelativeColorimetric" in EXPORT


def test_tiff_supports_integer_and_ieee_float_samples_with_icc() -> None:
    assert "ExportRowFormat::Rgb16Le" in EXPORT
    assert "ExportRowFormat::RgbF32Le" in EXPORT
    assert "let sample_format = if row_format == ExportRowFormat::RgbF32Le" in EXPORT
    assert "tag: 258" in EXPORT  # BitsPerSample
    assert "tag: 339" in EXPORT  # SampleFormat
    assert "tag: 34675" in EXPORT  # ICCProfile
    assert "build_matrix_shaper_icc" in EXPORT
    assert '"Linear Rec.2020"' in EXPORT
    assert "with_passthrough(request.bit_depth == ExportBitDepth::Float32Linear)" in EXPORT


def test_profile_selectable_export_ui_and_format_rules_are_wired() -> None:
    assert 'ui.strong("Precision")' in SIDEBAR
    assert 'ui.strong("Color profile")' in SIDEBAR
    assert '"Choose ICC profile…"' in SIDEBAR
    assert 'egui::Button::new("Export TIFF…")' in SIDEBAR
    assert "app.export_tiff(frame)" in SIDEBAR
    assert "bit_depth != ExportBitDepth::Float32Linear" in SIDEBAR
    assert 'ui.selectable_value(&mut dialog.format, ExportFormat::Tiff, "TIFF")' in LIBRARY
    assert "ExportFormat::Jpeg =>" in LIBRARY
    assert "ExportBitDepth::Eight" in LIBRARY
    assert "ExportFormat::Png" in LIBRARY
    assert "ExportBitDepth::Sixteen" in LIBRARY


def test_custom_icc_is_embedded_in_jpeg_delivery_files() -> None:
    assert "write_jpeg_icc_segments" in EXPORT
    assert 'b"ICC_PROFILE\\0"' in EXPORT
    assert "request.color.embedded_icc.as_deref()" in EXPORT
