from __future__ import annotations

from pathlib import Path
import struct


ROOT = Path(__file__).resolve().parents[1]
ICON_DIR = ROOT / "packaging" / "icons"
APP_ID = "de.duecki.auraw"


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


def test_android_declares_standard_and_round_launcher_icons() -> None:
    manifest = (ROOT / "android/app/src/main/AndroidManifest.xml").read_text(
        encoding="utf-8"
    )
    standard = ROOT / "android/app/src/main/res/mipmap-anydpi-v26/ic_launcher.xml"
    rounded = ROOT / "android/app/src/main/res/mipmap-anydpi-v26/ic_launcher_round.xml"

    assert 'android:icon="@mipmap/ic_launcher"' in manifest
    assert 'android:roundIcon="@mipmap/ic_launcher_round"' in manifest
    assert standard.is_file()
    assert rounded.is_file()
    assert "@drawable/ic_launcher_foreground" in rounded.read_text(encoding="utf-8")




def test_gitea_appimage_packages_and_verifies_the_shared_icon() -> None:
    workflow = (ROOT / ".gitea/workflows/build.yml").read_text(encoding="utf-8")

    assert f"APP_ID={APP_ID}" in workflow
    assert 'DESKTOP_FILE="$PWD/packaging/linux/$APP_ID.desktop"' in workflow
    assert 'APPIMAGE_ICON="$PWD/appimage-packaging/$APP_ID.png"' in workflow
    assert 'ln -s "$APP_ID.png" AppDir/.DirIcon' in workflow
    assert 'test -L squashfs-root/.DirIcon' in workflow
    assert 'cmp packaging/icons/auraw-256.png "squashfs-root/$APP_ID.png"' in workflow
    assert "usr/share/icons/hicolor/256x256/apps/$APP_ID.png" in workflow
    assert "usr/share/applications/$APP_ID.desktop" in workflow
    assert "r, g, b, a = 232, 126, 42, 255" not in workflow


