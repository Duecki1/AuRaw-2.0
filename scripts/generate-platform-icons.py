#!/usr/bin/env python3
"""Generate release icon rasters from the shared AuRaw mark."""

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "packaging" / "icons"
BACKGROUND = (17, 24, 39, 255)
FOREGROUND = (255, 255, 255, 255)
OUTER_A = [(54, 18), (84, 88), (69, 88), (62, 70), (46, 70), (39, 88), (24, 88)]
INNER_A = [(51, 57), (57, 57), (54, 44)]


def scaled(points: list[tuple[int, int]], scale: float) -> list[tuple[float, float]]:
    return [(x * scale, y * scale) for x, y in points]


def render_icon(edge: int) -> Image.Image:
    supersampling = 4
    render_edge = edge * supersampling
    scale = render_edge / 108
    image = Image.new("RGBA", (render_edge, render_edge), BACKGROUND)
    draw = ImageDraw.Draw(image)
    draw.polygon(scaled(OUTER_A, scale), fill=FOREGROUND)
    draw.polygon(scaled(INNER_A, scale), fill=BACKGROUND)
    return image.resize((edge, edge), Image.Resampling.LANCZOS)


def main() -> None:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    icon_1024 = render_icon(1024)
    icon_1024.save(OUTPUT / "auraw-1024.png", optimize=True)
    render_icon(256).save(OUTPUT / "auraw-256.png", optimize=True)
    icon_1024.save(
        OUTPUT / "auraw.ico",
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )


if __name__ == "__main__":
    main()
