from __future__ import annotations

import configparser
import re
import struct
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ICON_DIR = ROOT / "packaging" / "icons"
APP_ID = "de.duecki.auraw"
ANDROID_NS = "http://schemas.android.com/apk/res/android"


def android(name: str) -> str:
    return f"{{{ANDROID_NS}}}{name}"


def png_dimensions(path: Path) -> tuple[int, int]:
    data = path.read_bytes()
    assert data.startswith(b"\x89PNG\r\n\x1a\n")
    return struct.unpack(">II", data[16:24])


def path_vertices(value: str) -> list[tuple[float, float]]:
    tokens = re.findall(r"[A-Za-z]|-?\d+(?:\.\d+)?", value)
    vertices: list[tuple[float, float]] = []
    index = 0
    command = ""
    x = y = 0.0
    start_x = start_y = 0.0
    while index < len(tokens):
        if tokens[index].isalpha():
            command = tokens[index]
            index += 1
            if command in "Zz":
                x, y = start_x, start_y
                continue
        relative = command.islower()
        upper = command.upper()
        if upper in {"M", "L"}:
            nx, ny = float(tokens[index]), float(tokens[index + 1])
            index += 2
            if relative:
                nx, ny = x + nx, y + ny
            x, y = nx, ny
            if upper == "M":
                start_x, start_y = x, y
                command = "l" if relative else "L"
            vertices.append((x, y))
        elif upper == "H":
            nx = float(tokens[index])
            index += 1
            x = x + nx if relative else nx
            vertices.append((x, y))
        elif upper == "V":
            ny = float(tokens[index])
            index += 1
            y = y + ny if relative else ny
            vertices.append((x, y))
        else:
            raise AssertionError(f"unsupported icon path command: {command}")
    return vertices


def test_desktop_icons_share_the_android_mark_and_colors() -> None:
    svg_root = ET.parse(ICON_DIR / "auraw.svg").getroot()
    vector_root = ET.parse(
        ROOT / "android/app/src/main/res/drawable/ic_launcher_foreground.xml"
    ).getroot()
    colors_root = ET.parse(ROOT / "android/app/src/main/res/values/colors.xml").getroot()

    svg_rect = next(element for element in svg_root if element.tag.endswith("rect"))
    svg_path = next(element for element in svg_root if element.tag.endswith("path"))
    android_path = next(element for element in vector_root if element.tag.endswith("path"))
    launcher_color = next(
        element.text for element in colors_root if element.attrib.get("name") == "ic_launcher_background"
    )

    assert svg_rect.attrib["fill"].upper() == launcher_color.upper()
    android_fill = android_path.attrib[android("fillColor")]
    if len(android_fill) == 9:  # Android AARRGGBB -> SVG RRGGBB
        android_fill = "#" + android_fill[-6:]
    assert svg_path.attrib["fill"].upper() == android_fill.upper()
    assert path_vertices(svg_path.attrib["d"]) == path_vertices(android_path.attrib[android("pathData")])


def test_desktop_raster_assets_have_release_icon_dimensions() -> None:
    assert png_dimensions(ICON_DIR / "auraw-256.png") == (256, 256)
    assert png_dimensions(ICON_DIR / "auraw-1024.png") == (1024, 1024)
    ico = (ICON_DIR / "auraw.ico").read_bytes()
    reserved, image_type, image_count = struct.unpack("<HHH", ico[:6])
    assert (reserved, image_type) == (0, 1)
    assert image_count >= 6


def test_android_declares_standard_and_round_launcher_icons() -> None:
    manifest = ET.parse(ROOT / "android/app/src/main/AndroidManifest.xml").getroot()
    application = manifest.find("application")
    assert application is not None
    assert application.attrib[android("icon")] == "@mipmap/ic_launcher"
    assert application.attrib[android("roundIcon")] == "@mipmap/ic_launcher_round"

    for name in ("ic_launcher.xml", "ic_launcher_round.xml"):
        root = ET.parse(ROOT / "android/app/src/main/res/mipmap-anydpi-v26" / name).getroot()
        foreground = next(element for element in root if element.tag.endswith("foreground"))
        assert foreground.attrib[android("drawable")] == "@drawable/ic_launcher_foreground"


def test_linux_desktop_metadata_uses_the_shared_application_identity() -> None:
    parser = configparser.ConfigParser(interpolation=None)
    parser.read(ROOT / "packaging/linux" / f"{APP_ID}.desktop", encoding="utf-8")
    entry = parser["Desktop Entry"]
    assert entry.get("Icon") == APP_ID
    assert entry.get("StartupWMClass") == APP_ID
    assert entry.get("Exec") == "auraw"
    assert "Graphics" in entry.get("Categories", "").split(";")
