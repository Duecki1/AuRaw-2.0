#!/usr/bin/env python3
"""Compare isolated AuRaw renders with high-quality Lightroom endpoints.

The comparison is relative to each renderer's own baseline so a proprietary
camera-profile look is not mistaken for an adjustment error. Lightroom input
is decoded at its native 16-bit precision and interpreted as Adobe RGB (1998);
AuRaw's 16-bit PNG suite is interpreted as sRGB. ImageMagick 7 is required
because Pillow currently reduces 16-bit RGB TIFF/PNG data to eight bits.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import shutil
import subprocess

import numpy as np
from PIL import Image

SRGB_LUMA = np.array([0.2126729, 0.7151522, 0.0721750], dtype=np.float32)
ADOBE_RGB_LUMA = np.array([0.29734498, 0.62736357, 0.07529146], dtype=np.float32)
ADOBE_RGB_GAMMA = 2.0 + 51.0 / 256.0
LUMA_QUANTILES = np.array(
    [0.0, 0.01, 0.05, 0.10, 0.20, 0.30, 0.40, 0.50,
     0.60, 0.70, 0.80, 0.90, 0.95, 0.99, 1.0],
    dtype=np.float64,
)


@dataclass(frozen=True)
class Endpoint:
    name: str
    auraw_file: str
    lightroom_file: str
    detail_control: bool = False


ENDPOINTS = (
    Endpoint("Exposure +1.25", "exposure_plus1_25.png", "Exposure +1.25.tif"),
    Endpoint("Exposure -1.25", "exposure_minus1_25.png", "Exposure -1.25.tif"),
    Endpoint("Exposure +5", "exposure_plus5.png", "Exposure +5.tif"),
    Endpoint("Exposure -5", "exposure_minus5.png", "Exposure -5.tif"),
    Endpoint("Contrast +100", "contrast_plus100.png", "Contrast +100.tif"),
    Endpoint("Contrast -100", "contrast_minus100.png", "Contrast -100.tif"),
    Endpoint("Highlights +100", "highlights_plus100.png", "Highlights +100.tif"),
    Endpoint("Highlights -100", "highlights_minus100.png", "Highlights -100.tif"),
    Endpoint("Shadows +100", "shadows_plus100.png", "Shadows +100.tif"),
    Endpoint("Shadows -100", "shadows_minus100.png", "Shadows -100.tif"),
    Endpoint("Whites +100", "whites_plus100.png", "Whites +100.tif"),
    Endpoint("Whites -100", "whites_minus100.png", "Whites -100.tif"),
    Endpoint("Blacks +100", "blacks_plus100.png", "Blacks +100.tif"),
    Endpoint("Blacks -100", "blacks_minus100.png", "Blacks -100.tif"),
    Endpoint("Texture +100", "texture_plus100.png", "Texture +100.tif", True),
    Endpoint("Texture -100", "texture_minus100.png", "Texture -100.tif", True),
    Endpoint("Clarity +100", "clarity_plus100.png", "Clarity +100.tif", True),
    Endpoint("Clarity -100", "clarity_minus100.png", "Clarity -100.tif", True),
    Endpoint("Dehaze +100", "dehaze_plus100.png", "Dehaze +100.tif"),
    Endpoint("Dehaze -100", "dehaze_minus100.png", "Dehaze -100.tif"),
    Endpoint("Vibrance +100", "vibrance_plus100.png", "Vibrance +100.tif"),
    # The supplied Lightroom filename contains this typo; the endpoint is Vibrance.
    Endpoint("Vibrance -100", "vibrance_minus100.png", "Vibration -100.tif"),
    Endpoint("Saturation +100", "saturation_plus100.png", "Saturation +100.tif"),
    Endpoint("Saturation -100", "saturation_minus100.png", "Saturation -100.tif"),
)


def parse_crop(value: str) -> tuple[int, int, int, int]:
    try:
        crop = tuple(int(part) for part in value.split(","))
    except ValueError as error:
        raise argparse.ArgumentTypeError("crop must contain integers: X,Y,WIDTH,HEIGHT") from error
    if len(crop) != 4 or min(crop) < 0 or crop[2] < 1 or crop[3] < 1:
        raise argparse.ArgumentTypeError("crop must be X,Y,WIDTH,HEIGHT with a positive size")
    return crop  # type: ignore[return-value]


def image_region_size(
    path: Path, crop: tuple[int, int, int, int] | None
) -> tuple[int, int]:
    with Image.open(path) as image:
        source_width, source_height = image.size
    if crop is None:
        return source_width, source_height
    x, y, width, height = crop
    if x + width > source_width or y + height > source_height:
        raise ValueError(
            f"crop {crop} is outside {path.name} ({source_width}x{source_height})"
        )
    return width, height


def encoded_rgb16(
    path: Path,
    *,
    crop: tuple[int, int, int, int] | None,
    sample_step: int,
) -> np.ndarray:
    width, height = image_region_size(path, crop)
    sampled_width = (width + sample_step - 1) // sample_step
    sampled_height = (height + sample_step - 1) // sample_step
    command = ["magick", str(path)]
    if crop is not None:
        x, y, crop_width, crop_height = crop
        command += ["-crop", f"{crop_width}x{crop_height}+{x}+{y}", "+repage"]
    if sample_step > 1:
        # Point sampling avoids inventing spatial detail while reducing memory.
        command += ["-filter", "point", "-sample", f"{sampled_width}x{sampled_height}!"]
    command += ["-alpha", "off", "-depth", "16", "-endian", "LSB", "rgb:-"]
    try:
        result = subprocess.run(command, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except subprocess.CalledProcessError as error:
        message = error.stderr.decode("utf-8", "replace").strip()
        raise RuntimeError(f"ImageMagick could not decode {path}: {message}") from error
    expected_values = sampled_width * sampled_height * 3
    encoded = np.frombuffer(result.stdout, dtype="<u2")
    if encoded.size != expected_values:
        raise RuntimeError(
            f"ImageMagick returned {encoded.size} values for {path}; expected {expected_values}"
        )
    return encoded.astype(np.float32).reshape(sampled_height, sampled_width, 3) / 65535.0


def linear_rgb(
    path: Path,
    *,
    crop: tuple[int, int, int, int] | None,
    sample_step: int,
    color_space: str,
) -> np.ndarray:
    encoded = encoded_rgb16(path, crop=crop, sample_step=sample_step)
    if color_space == "adobe-rgb":
        return np.power(np.maximum(encoded, 0.0), ADOBE_RGB_GAMMA)
    return np.where(
        encoded <= 0.04045,
        encoded / 12.92,
        ((encoded + 0.055) / 1.055) ** 2.4,
    )


def luma_delta_ev(base: np.ndarray, edit: np.ndarray, weights: np.ndarray) -> np.ndarray:
    base_luma = base @ weights
    edit_luma = edit @ weights
    return np.log2(np.maximum(edit_luma, 1e-7) / np.maximum(base_luma, 1e-7))


def baseline_luma_quantiles(rgb: np.ndarray, weights: np.ndarray) -> np.ndarray:
    return np.quantile(rgb @ weights, [0.05, 0.50, 0.95])


def quantile_curve(base: np.ndarray, delta: np.ndarray, weights: np.ndarray) -> np.ndarray:
    # Rank bins stay populated even when a clipped endpoint contains many equal
    # black/white samples; threshold masks can otherwise create empty bins.
    order = np.argsort((base @ weights).reshape(-1), kind="stable")
    ranked_delta = delta.reshape(-1)[order]
    count = ranked_delta.size
    boundaries = np.rint(LUMA_QUANTILES * count).astype(np.int64)
    boundaries[0] = 0
    boundaries[-1] = count
    response = []
    for lower, upper in zip(boundaries[:-1], boundaries[1:]):
        upper = max(upper, lower + 1)
        response.append(float(np.median(ranked_delta[lower:min(upper, count)])))
    return np.asarray(response, dtype=np.float32)


def chroma_response(base: np.ndarray, edit: np.ndarray) -> float:
    base_max = np.max(base, axis=2)
    edit_max = np.max(edit, axis=2)
    base_chroma = (base_max - np.min(base, axis=2)) / np.maximum(base_max, 1e-5)
    edit_chroma = (edit_max - np.min(edit, axis=2)) / np.maximum(edit_max, 1e-5)
    selected = (base_chroma > 0.03) & (base_max > 0.002) & (base_max < 0.98)
    if not np.any(selected):
        return 1.0
    return float(np.median(edit_chroma[selected] / np.maximum(base_chroma[selected], 1e-5)))


def box_blur(image: np.ndarray, radius: int = 2) -> np.ndarray:
    padded = np.pad(image, radius, mode="reflect")
    total = np.zeros_like(image, dtype=np.float64)
    diameter = 2 * radius + 1
    for y in range(diameter):
        for x in range(diameter):
            total += padded[y : y + image.shape[0], x : x + image.shape[1]]
    return (total / (diameter * diameter)).astype(np.float32)


def detail_response(base: np.ndarray, edit: np.ndarray, weights: np.ndarray) -> float:
    base_ev = np.log2(np.maximum(base @ weights, 1e-5))
    edit_ev = np.log2(np.maximum(edit @ weights, 1e-5))
    base_residual = base_ev - box_blur(base_ev)
    edit_residual = edit_ev - box_blur(edit_ev)
    denominator = float(np.quantile(np.abs(base_residual), 0.90))
    return float(np.quantile(np.abs(edit_residual), 0.90)) / max(denominator, 1e-6)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lightroom-dir", type=Path, required=True)
    parser.add_argument("--auraw-dir", type=Path, required=True)
    parser.add_argument("--lightroom-baseline", default="Camera NT.tif")
    parser.add_argument("--auraw-baseline", default="baseline.png")
    parser.add_argument("--lightroom-crop", type=parse_crop, default=None)
    parser.add_argument("--auraw-crop", type=parse_crop, default=None)
    parser.add_argument(
        "--sample-step", type=int, default=4,
        help="sample approximately every Nth row and column (default: 4)",
    )
    args = parser.parse_args()
    if args.sample_step < 1:
        parser.error("--sample-step must be positive")
    if shutil.which("magick") is None:
        parser.error("ImageMagick 7 (`magick`) is required for native 16-bit RGB decoding")

    try:
        auraw_base = linear_rgb(
            args.auraw_dir / args.auraw_baseline,
            crop=args.auraw_crop,
            sample_step=args.sample_step,
            color_space="srgb",
        )
        lightroom_base = linear_rgb(
            args.lightroom_dir / args.lightroom_baseline,
            crop=args.lightroom_crop,
            sample_step=args.sample_step,
            color_space="adobe-rgb",
        )
    except (OSError, ValueError, RuntimeError) as error:
        parser.error(str(error))
    if auraw_base.shape != lightroom_base.shape:
        parser.error(
            "baseline dimensions differ after crop/sample: "
            f"AuRaw {auraw_base.shape[:2]}, Lightroom {lightroom_base.shape[:2]}"
        )

    auraw_baseline_luma = baseline_luma_quantiles(auraw_base, SRGB_LUMA)
    lightroom_baseline_luma = baseline_luma_quantiles(lightroom_base, ADOBE_RGB_LUMA)
    print("baseline linear-luma quantiles       p05       p50       p95")
    print(
        "AuRaw                              "
        + " ".join(f"{value:9.4f}" for value in auraw_baseline_luma)
    )
    print(
        "Lightroom                          "
        + " ".join(f"{value:9.4f}" for value in lightroom_baseline_luma)
    )
    print()
    print(
        f"{'endpoint':<21} {'curve MAE':>9} {'Au chroma':>10} {'LR chroma':>10} "
        f"{'Au detail':>10} {'LR detail':>10}"
    )
    print("-" * 76)
    missing = 0
    for endpoint in ENDPOINTS:
        auraw_path = args.auraw_dir / endpoint.auraw_file
        lightroom_path = args.lightroom_dir / endpoint.lightroom_file
        if not auraw_path.is_file() or not lightroom_path.is_file():
            print(f"{endpoint.name:<21} {'missing':>9}")
            missing += 1
            continue
        auraw_edit = linear_rgb(
            auraw_path, crop=args.auraw_crop, sample_step=args.sample_step, color_space="srgb"
        )
        lightroom_edit = linear_rgb(
            lightroom_path,
            crop=args.lightroom_crop,
            sample_step=args.sample_step,
            color_space="adobe-rgb",
        )
        auraw_curve = quantile_curve(
            auraw_base, luma_delta_ev(auraw_base, auraw_edit, SRGB_LUMA), SRGB_LUMA
        )
        lightroom_curve = quantile_curve(
            lightroom_base,
            luma_delta_ev(lightroom_base, lightroom_edit, ADOBE_RGB_LUMA),
            ADOBE_RGB_LUMA,
        )
        curve_mae = float(np.mean(np.abs(auraw_curve - lightroom_curve)))
        if endpoint.detail_control:
            au_detail = f"{detail_response(auraw_base, auraw_edit, SRGB_LUMA):.3f}"
            lr_detail = f"{detail_response(lightroom_base, lightroom_edit, ADOBE_RGB_LUMA):.3f}"
        else:
            au_detail = lr_detail = "-"
        print(
            f"{endpoint.name:<21} {curve_mae:9.3f} "
            f"{chroma_response(auraw_base, auraw_edit):10.3f} "
            f"{chroma_response(lightroom_base, lightroom_edit):10.3f} "
            f"{au_detail:>10} {lr_detail:>10}"
        )

    return 1 if missing else 0


if __name__ == "__main__":
    raise SystemExit(main())
