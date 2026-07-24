from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXPORT = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")
RAW = (ROOT / "src/pipeline/raw_loader.rs").read_text(encoding="utf-8")
LIBRAW = (ROOT / "src/pipeline/raw_loader/libraw_loader.rs").read_text(encoding="utf-8")
SIDEBAR = (ROOT / "src/ui/sidebar/export.rs").read_text(encoding="utf-8")


def test_raw_capture_metadata_is_retained_for_export() -> None:
    assert "struct CaptureMetadata" in RAW
    for field in ("iso_speed", "shutter_seconds", "description", "artist"):
        assert field in RAW
        assert field in EXPORT
    assert "other.iso_speed" in LIBRAW
    assert "other.shutter" in LIBRAW
    assert "other.desc" in LIBRAW
    assert "other.artist" in LIBRAW
    assert "raw.capture_metadata.iso_speed" in EXPORT
    assert "raw.capture_metadata.shutter_seconds" in EXPORT


def test_keep_metadata_controls_both_png_and_jpeg_metadata() -> None:
    assert "if request.keep_metadata" in EXPORT
    assert "info.exif_metadata = Some" in EXPORT
    assert "add_png_text_metadata" in EXPORT
    assert "write_final_jpeg(" in EXPORT
    assert "request.keep_metadata" in EXPORT
    assert "if !keep_metadata" in EXPORT


def test_exported_metadata_contains_common_viewer_fields() -> None:
    for png_key in (
        '"Source"',
        '"Camera"',
        '"Lens"',
        '"Focal length"',
        '"Aperture"',
        '"ISO speed"',
        '"Exposure time"',
        '"Artist"',
        '"Image description"',
        '"Original dimensions"',
        '"Export dimensions"',
        '"Orientation"',
    ):
        assert f"add_itxt_chunk({png_key}" in EXPORT or png_key in EXPORT

    # Standard TIFF/EXIF tags: ImageDescription, Artist, ExifIFD pointer,
    # ExposureTime, FNumber, ISO, FocalLength, UserComment and lens identity.
    for tag in (
        "0x010e",
        "0x013b",
        "0x8769",
        "0x829a",
        "0x829d",
        "0x8827",
        "0x920a",
        "0x9286",
        "0xa433",
        "0xa434",
    ):
        assert f"tag: {tag}" in EXPORT

    assert "source_file_name" in EXPORT
    assert "source_width" in EXPORT
    assert "source_height" in EXPORT
    assert "normalized-orientation metadata" in SIDEBAR
