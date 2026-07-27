from __future__ import annotations

from pathlib import Path
import struct


ROOT = Path(__file__).resolve().parents[1]
ICON_DIR = ROOT / "packaging" / "icons"


def png_dimensions(path: Path) -> tuple[int, int]:
    data = path.read_bytes()
    assert data.startswith(b"\x89PNG\r\n\x1a\n")
    return struct.unpack(">II", data[16:24])


def test_desktop_icons_share_the_android_mark_and_colors() -> None:
    svg = (ICON_DIR / "auraw.svg").read_text(encoding="utf-8")
    android_foreground = (
        ROOT / "android/app/src/main/res/drawable/ic_launcher_foreground.xml"
    ).read_text(encoding="utf-8")
    android_colors = (
        ROOT / "android/app/src/main/res/values/colors.xml"
    ).read_text(encoding="utf-8")

    assert "#111827" in svg and "#111827" in android_colors
    assert "M54 18 84 88H69L62 70H46L39 88H24" in svg
    assert "M54,18 L84,88 L69,88 L62,70 L46,70 L39,88 L24,88" in android_foreground


def test_desktop_raster_assets_have_release_icon_dimensions() -> None:
    assert png_dimensions(ICON_DIR / "auraw-256.png") == (256, 256)
    assert png_dimensions(ICON_DIR / "auraw-1024.png") == (1024, 1024)

    ico = (ICON_DIR / "auraw.ico").read_bytes()
    reserved, image_type, image_count = struct.unpack("<HHH", ico[:6])
    assert (reserved, image_type) == (0, 1)
    assert image_count >= 6
