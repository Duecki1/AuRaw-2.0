#!/usr/bin/env python3
"""Regenerate the small CC0 Bayer and X-Trans DNG regression fixtures."""
from __future__ import annotations

import hashlib
from pathlib import Path

import numpy as np
import tifffile

ROOT = Path(__file__).resolve().parent
RAW_ROOT = ROOT / "raw"
WIDTH = HEIGHT = 256
BLACK = 512
WHITE = 16383

BAYER = np.asarray([[0, 1], [1, 2]], dtype=np.uint8)
XTRANS = np.asarray(
    [
        [1, 2, 1, 1, 0, 1],
        [0, 1, 0, 2, 1, 2],
        [1, 2, 1, 1, 0, 1],
        [1, 0, 1, 1, 2, 1],
        [2, 1, 2, 0, 1, 0],
        [1, 0, 1, 1, 2, 1],
    ],
    dtype=np.uint8,
)


def build_scene() -> np.ndarray:
    yy, xx = np.indices((HEIGHT, WIDTH), dtype=np.float32)
    rgb = np.empty((HEIGHT, WIDTH, 3), dtype=np.float32)
    rgb[..., 0] = 0.07 + 0.20 * xx / (WIDTH - 1)
    rgb[..., 1] = 0.08 + 0.18 * yy / (HEIGHT - 1)
    rgb[..., 2] = 0.10 + 0.12 * (xx + yy) / (WIDTH + HEIGHT - 2)

    # Neutral slanted edge: edge spread and direction response.
    edge = xx[24:120, 24:112] > 64.0 + 0.37 * (yy[24:120, 24:112] - 72.0)
    neutral = np.where(edge, 0.68, 0.065).astype(np.float32)
    rgb[24:120, 24:112, :] = neutral[..., None]

    # Difficult-frequency target: 1-3 px textiles plus coloured crossings.
    fy = yy[24:120, 136:240]
    fx = xx[24:120, 136:240]
    weave = 0.32 + 0.16 * np.sign(np.sin(fx * np.pi / 2.0) * np.sin(fy * np.pi / 3.0))
    diagonal = 0.055 * np.sin((fx + 1.7 * fy) * np.pi / 2.5)
    rgb[24:120, 136:240, 0] = weave + diagonal
    rgb[24:120, 136:240, 1] = weave
    rgb[24:120, 136:240, 2] = weave - diagonal

    # Flat, underexposed, high-ISO-like patch with deterministic chroma noise.
    rng = np.random.default_rng(0xA0_52)
    shadow = np.full((88, 88, 3), 0.025, dtype=np.float32)
    common = rng.normal(0.0, 0.0045, (88, 88, 1)).astype(np.float32)
    chroma = rng.normal(0.0, 0.0020, shadow.shape).astype(np.float32)
    rgb[144:232, 24:112, :] = shadow + common + chroma

    # Clipped neutral and coloured highlights with smooth shoulders.
    hy = yy[136:240, 128:240]
    hx = xx[136:240, 128:240]
    highlight = np.full((104, 112, 3), 0.12, dtype=np.float32)
    spots = [
        (158.0, 166.0, np.array([1.35, 0.22, 0.08], dtype=np.float32)),
        (205.0, 165.0, np.array([0.10, 1.30, 0.25], dtype=np.float32)),
        (161.0, 213.0, np.array([0.12, 0.28, 1.40], dtype=np.float32)),
        (211.0, 212.0, np.array([1.35, 1.35, 1.35], dtype=np.float32)),
    ]
    for cx, cy, color in spots:
        radius = np.sqrt((hx - cx) ** 2 + (hy - cy) ** 2)
        weight = np.clip(1.0 - radius / 23.0, 0.0, 1.0) ** 1.7
        highlight = np.maximum(highlight, weight[..., None] * color)
    rgb[136:240, 128:240, :] = highlight
    return np.clip(rgb, 0.0, 1.2)


def mosaic(rgb: np.ndarray, pattern: np.ndarray) -> np.ndarray:
    ph, pw = pattern.shape
    yy, xx = np.indices(rgb.shape[:2])
    channels = pattern[yy % ph, xx % pw]
    sampled = np.take_along_axis(rgb, channels[..., None], axis=2)[..., 0]
    normalized = np.clip(sampled, 0.0, 1.0)
    return np.rint(BLACK + normalized * (WHITE - BLACK)).astype("<u2")


def rational(values: list[float], scale: int = 1_000_000) -> tuple[int, ...]:
    output: list[int] = []
    for value in values:
        output.extend((int(round(value * scale)), scale))
    return tuple(output)


def write_dng(path: Path, pattern: np.ndarray, make: str, model: str) -> None:
    raw = mosaic(build_scene(), pattern)
    ph, pw = pattern.shape
    # XYZ D65 -> synthetic camera RGB. This is the inverse direction required
    # by DNG ColorMatrix1; the sensor itself is deliberately idealized.
    xyz_to_camera = [
        3.2404542, -1.5371385, -0.4985314,
        -0.9692660, 1.8760108, 0.0415560,
        0.0556434, -0.2040259, 1.0572252,
    ]
    tags = [
        (271, "s", 0, make, False),
        (272, "s", 0, model, False),
        (274, "H", 1, 1, False),
        (33421, "H", 2, (ph, pw), False),
        (33422, "B", ph * pw, tuple(int(v) for v in pattern.flat), False),
        (50706, "B", 4, (1, 4, 0, 0), False),
        (50707, "B", 4, (1, 3, 0, 0), False),
        (50708, "s", 0, model, False),
        (50710, "B", 3, (0, 1, 2), False),
        (50711, "H", 1, 1, False),
        (50714, "H", 1, BLACK, False),
        (50717, "I", 1, WHITE, False),
        (50718, "2I", 2, (1, 1, 1, 1), False),
        (50719, "I", 2, (0, 0), False),
        (50720, "I", 2, (WIDTH, HEIGHT), False),
        (50721, "2i", 9, rational(xyz_to_camera), False),
        (50728, "2I", 3, (1, 1, 1, 1, 1, 1), False),
        (50730, "2i", 1, (0, 1), False),
        (50778, "H", 1, 21, False),
        (50829, "I", 4, (0, 0, HEIGHT, WIDTH), False),
    ]
    tifffile.imwrite(
        path,
        raw,
        dtype=np.uint16,
        photometric=32803,
        metadata=None,
        compression=None,
        rowsperstrip=HEIGHT,
        extratags=tags,
        byteorder="<",
    )


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    RAW_ROOT.mkdir(parents=True, exist_ok=True)
    fixtures = [
        ("synthetic-bayer.dng", BAYER, "AuRaw", "AuRaw Synthetic Bayer"),
        ("synthetic-xtrans.dng", XTRANS, "FUJIFILM", "AuRaw Synthetic X-Trans"),
    ]
    for name, pattern, make, model in fixtures:
        path = RAW_ROOT / name
        write_dng(path, pattern, make, model)
        print(f"{digest(path)}  {path.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
