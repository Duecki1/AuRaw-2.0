#!/usr/bin/env python3
"""Compare isolated AuRaw Develop exports with a Lightroom endpoint set.

The two renderers do not have identical baseline profiles: Lightroom's Adobe
Color adds a proprietary look table on top of Adobe Standard. To avoid scoring
that baseline difference as an adjustment error, this tool compares the
per-pixel change from each renderer's own baseline. Lightroom exports are
treated as Display P3 and AuRaw exports as sRGB.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from PIL import Image

SRGB_LUMA = np.array([0.2126, 0.7152, 0.0722], dtype=np.float32)
DISPLAY_P3_LUMA = np.array([0.2289746, 0.6917385, 0.0792869], dtype=np.float32)
LUMA_QUANTILES = np.array(
    [0.0, 0.01, 0.05, 0.10, 0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80, 0.90, 0.95, 0.99, 1.0],
    dtype=np.float32,
)


@dataclass(frozen=True)
class Endpoint:
    name: str
    auraw_file: str
    lightroom_file: str


ENDPOINTS = (
    Endpoint("Exposure +1.25", "exposure_plus1_25.png", "Exposure+1-25.jpg"),
    Endpoint("Exposure -1.25", "exposure_minus1_25.png", "Exposure-1-25.jpg"),
    Endpoint("Contrast +100", "contrast_plus100.png", "Contrast+100.jpg"),
    Endpoint("Contrast -100", "contrast_minus100.png", "Contrast-100.jpg"),
    Endpoint("Highlights +100", "highlights_plus100.png", "Highlights+100.jpg"),
    Endpoint("Highlights -100", "highlights_minus100.png", "Highlights-100.jpg"),
    Endpoint("Shadows +100", "shadows_plus100.png", "Shadows+100.jpg"),
    Endpoint("Shadows -100", "shadows_minus100.png", "Shadows-100.jpg"),
    Endpoint("Whites +100", "whites_plus100.png", "Whites+100.jpg"),
    Endpoint("Whites -100", "whites_minus100.png", "Whites-100.jpg"),
    Endpoint("Blacks +100", "blacks_plus100.png", "Blacks+100.jpg"),
    Endpoint("Blacks -100", "blacks_minus100.png", "Blacks-100.jpg"),
    Endpoint("Texture +100", "texture_plus100.png", "Texture +100.jpg"),
    Endpoint("Texture -100", "texture_minus100.png", "Texture -100.jpg"),
    Endpoint("Clarity +100", "clarity_plus100.png", "Clarity +100.jpg"),
    Endpoint("Clarity -100", "clarity_minus100.png", "Clarity -100.jpg"),
    Endpoint("Dehaze +100", "dehaze_plus100.png", "Dehaze +100.jpg"),
    Endpoint("Dehaze -100", "dehaze_minus100.png", "Dehaze -100.jpg"),
    Endpoint("Vibrance +100", "vibrance_plus100.png", "Vibrance +100.jpg"),
    Endpoint("Vibrance -100", "vibrance_minus100.png", "Vibrance -100.jpg"),
    Endpoint("Saturation +100", "saturation_plus100.png", "Saturation +100.jpg"),
    Endpoint("Saturation -100", "saturation_minus100.png", "Saturation -100.jpg"),
)


def parse_crop(value: str) -> tuple[int, int, int, int]:
    try:
        crop = tuple(int(part) for part in value.split(","))
    except ValueError as error:
        raise argparse.ArgumentTypeError("crop must contain integers: X,Y,WIDTH,HEIGHT") from error
    if len(crop) != 4 or min(crop) < 0 or crop[2] < 1 or crop[3] < 1:
        raise argparse.ArgumentTypeError("crop must be X,Y,WIDTH,HEIGHT with a positive size")
    return crop  # type: ignore[return-value]


def linear_rgb(
    path: Path,
    *,
    crop: tuple[int, int, int, int] | None,
    sample_step: int,
) -> np.ndarray:
    image = Image.open(path).convert("RGB")
    if crop is not None:
        x, y, width, height = crop
        image = image.crop((x, y, x + width, y + height))
    encoded = np.asarray(image, dtype=np.float32)[::sample_step, ::sample_step] / 255.0
    return np.where(
        encoded <= 0.04045,
        encoded / 12.92,
        ((encoded + 0.055) / 1.055) ** 2.4,
    )


def luma_delta_ev(base: np.ndarray, edit: np.ndarray, weights: np.ndarray) -> np.ndarray:
    base_luma = base @ weights
    edit_luma = edit @ weights
    return np.log2(np.maximum(edit_luma, 1e-6) / np.maximum(base_luma, 1e-6))


def quantile_curve(base: np.ndarray, delta: np.ndarray, weights: np.ndarray) -> np.ndarray:
    luma = base @ weights
    boundaries = np.quantile(luma, LUMA_QUANTILES)
    response = []
    for index, (lower, upper) in enumerate(zip(boundaries[:-1], boundaries[1:])):
        if index + 1 == len(boundaries) - 1:
            selected = (luma >= lower) & (luma <= upper)
        else:
            selected = (luma >= lower) & (luma < upper)
        response.append(float(np.median(delta[selected])))
    return np.asarray(response, dtype=np.float32)


def chroma_response(base: np.ndarray, edit: np.ndarray) -> float:
    base_max = np.max(base, axis=2)
    edit_max = np.max(edit, axis=2)
    base_chroma = (base_max - np.min(base, axis=2)) / np.maximum(base_max, 1e-4)
    edit_chroma = (edit_max - np.min(edit, axis=2)) / np.maximum(edit_max, 1e-4)
    selected = base_chroma > 0.05
    return float(np.median(edit_chroma[selected] / np.maximum(base_chroma[selected], 1e-4)))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lightroom-dir", type=Path, required=True)
    parser.add_argument("--auraw-dir", type=Path, required=True)
    parser.add_argument(
        "--auraw-crop",
        type=parse_crop,
        default=None,
        help="AuRaw crop as X,Y,WIDTH,HEIGHT; this RAW uses 12,8,7008,4672",
    )
    parser.add_argument(
        "--sample-step",
        type=int,
        default=2,
        help="sample every Nth row and column (default: 2)",
    )
    args = parser.parse_args()
    if args.sample_step < 1:
        parser.error("--sample-step must be positive")

    auraw_base = linear_rgb(
        args.auraw_dir / "baseline.png",
        crop=args.auraw_crop,
        sample_step=args.sample_step,
    )
    lightroom_base = linear_rgb(
        args.lightroom_dir / "Adobe Color.jpg",
        crop=None,
        sample_step=args.sample_step,
    )
    if auraw_base.shape != lightroom_base.shape:
        parser.error(
            f"baseline dimensions differ after crop/sample: "
            f"AuRaw {auraw_base.shape[:2]}, Lightroom {lightroom_base.shape[:2]}"
        )

    print(
        f"{'endpoint':<21} {'curve MAE':>9} {'pixel MAE':>9} "
        f"{'Au chroma':>10} {'LR chroma':>10}"
    )
    print("-" * 65)
    missing = 0
    for endpoint in ENDPOINTS:
        auraw_path = args.auraw_dir / endpoint.auraw_file
        lightroom_path = args.lightroom_dir / endpoint.lightroom_file
        if not auraw_path.is_file() or not lightroom_path.is_file():
            print(f"{endpoint.name:<21} {'missing':>9}")
            missing += 1
            continue
        auraw_edit = linear_rgb(
            auraw_path,
            crop=args.auraw_crop,
            sample_step=args.sample_step,
        )
        lightroom_edit = linear_rgb(
            lightroom_path,
            crop=None,
            sample_step=args.sample_step,
        )
        auraw_delta = luma_delta_ev(auraw_base, auraw_edit, SRGB_LUMA)
        lightroom_delta = luma_delta_ev(lightroom_base, lightroom_edit, DISPLAY_P3_LUMA)
        auraw_curve = quantile_curve(auraw_base, auraw_delta, SRGB_LUMA)
        lightroom_curve = quantile_curve(lightroom_base, lightroom_delta, DISPLAY_P3_LUMA)
        curve_mae = float(np.mean(np.abs(auraw_curve - lightroom_curve)))
        pixel_mae = float(np.median(np.abs(auraw_delta - lightroom_delta)))
        print(
            f"{endpoint.name:<21} {curve_mae:9.3f} {pixel_mae:9.3f} "
            f"{chroma_response(auraw_base, auraw_edit):10.3f} "
            f"{chroma_response(lightroom_base, lightroom_edit):10.3f}"
        )

    return 1 if missing else 0


if __name__ == "__main__":
    raise SystemExit(main())
