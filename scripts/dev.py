#!/usr/bin/env python3
"""NumPy-backed image regression and synthetic-corpus commands for AuRaw."""

from __future__ import annotations

import argparse
from collections.abc import Iterable, Sequence
import csv
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from typing import NoReturn

try:
    import numpy as np
except ModuleNotFoundError:  # Permit --help without NumPy.
    np = None  # type: ignore[assignment]

ROOT = Path(__file__).resolve().parents[1]

SCENE_MIDDLE_GREY = 0.1845
LUMA = (0.2627002, 0.6779981, 0.0593017)
SCENE_INPUTS = [
    0.0,
    1e-12,
    1e-10,
    1e-8,
    5e-8,
    1e-7,
    5e-7,
    1e-6,
    1e-5,
    3e-5,
    1e-4,
    3e-4,
    1e-3,
    3e-3,
    1e-2,
    3e-2,
    0.1,
    0.18,
    0.5,
    1.0,
    4.0,
]
DISPLAY_INPUTS = [
    0.0,
    1e-12,
    1e-10,
    1e-8,
    5e-8,
    1e-7,
    5e-7,
    1e-6,
    1e-5,
    1e-4,
    1e-3,
    1e-2,
    0.05,
    0.10,
    0.149999,
    0.15,
    0.150001,
    0.20,
    0.50,
    1.0,
]
SETTINGS = [-100, -75, -50, -25, 0, 25, 50, 75, 100]
DEFAULT_PERCENTILES = (-5.0, 0.0)
COLOR_RATIOS = {
    "neutral": (1.0, 1.0, 1.0),
    "red": (1.0, 0.0, 0.0),
    "orange": (1.0, 0.5, 0.0),
    "yellow": (1.0, 1.0, 0.0),
    "green": (0.0, 1.0, 0.0),
    "cyan": (0.0, 1.0, 1.0),
    "blue": (0.0, 0.0, 1.0),
    "magenta": (1.0, 0.0, 1.0),
}

# AuRaw/darktable-default sigmoid coefficients in src/pipeline/sigmoid.rs.
SIGMOID_WHITE = 1.0
SIGMOID_LOG2_PAPER_EXPOSURE = -1.4751521
SIGMOID_FILM_FOG = 0.0013843221
SIGMOID_FILM_POWER = 1.4909091
SIGMOID_PAPER_POWER = 1.0

Rgb = tuple[float, float, float]


def clamp(x: float, lo: float, hi: float) -> float:
    return max(lo, min(hi, x))


def dot(a: Rgb, b: Rgb) -> float:
    return sum(x * y for x, y in zip(a, b))


def scale(rgb: Rgb, factor: float) -> Rgb:
    return tuple(channel * factor for channel in rgb)  # type: ignore[return-value]


def rgb_for_luminance(ratio: Rgb, luminance: float) -> Rgb:
    basis_luminance = dot(ratio, LUMA)
    if luminance <= 0.0 or basis_luminance <= 0.0:
        return (0.0, 0.0, 0.0)
    return scale(ratio, luminance / basis_luminance)


def remap_luminance(rgb: Rgb, target_luminance: float) -> Rgb:
    source_luminance = dot(rgb, LUMA)
    if source_luminance <= 0.0:
        return rgb
    return scale(rgb, target_luminance / source_luminance)


def smoothstep(a: float, b: float, x: float) -> float:
    t = clamp((x - a) / max(b - a, 1e-6), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


def shaped(v: float) -> float:
    n = clamp(v / 100.0, -1.0, 1.0)
    magnitude = abs(n)
    return math.copysign(magnitude * (1.45 - 0.45 * magnitude), n) if magnitude else 0.0


def shadow_range(p05: float, p50: float) -> tuple[float, float]:
    return p05 - 0.90, p50 + 1.35


def shadow_mask(ev: float, bounds: tuple[float, float]) -> float:
    lower, upper = bounds
    return 1.0 - smoothstep(lower, upper, ev)


def shadows_scene(y: float, setting: float, percentiles=DEFAULT_PERCENTILES) -> tuple[float, float, float]:
    if y <= 0.0:
        return y, 0.0, 0.0
    ev = math.log2(y / SCENE_MIDDLE_GREY)
    bounds = shadow_range(*percentiles)
    weight = shadow_mask(ev, bounds)
    amount = shaped(setting)
    lower, upper = bounds
    limit = 0.64 * max(upper - lower, 0.25)
    strength = math.copysign(min(abs(amount) * 2.20, limit), amount) if amount else 0.0
    delta_ev = strength * weight
    return y * 2.0**delta_ev, delta_ev, weight


def sigmoid(y: float) -> float:
    base = SIGMOID_FILM_FOG + max(y, 0.0)
    if base <= 0.0:
        return 0.0
    log2_film = SIGMOID_FILM_POWER * math.log2(base)
    log_ratio = log2_film - SIGMOID_LOG2_PAPER_EXPOSURE
    if log_ratio >= 0.0:
        ratio = 1.0 / (1.0 + 2.0 ** (-log_ratio))
    else:
        z = 2.0**log_ratio
        ratio = z / (1.0 + z)
    return SIGMOID_WHITE * clamp(ratio, 0.0, 1.0) ** SIGMOID_PAPER_POWER


def blacks_display(y: float, setting: float) -> tuple[float, float, float]:
    if y <= 0.0 or setting == 0.0:
        return y, 0.0, 0.0
    amount = shaped(setting)
    hdr_guard = 1.0 - smoothstep(0.35, 1.0, y)
    if amount >= 0.0:
        weight = (0.08 + 0.92 * 2.0 ** (-y / 0.035)) * hdr_guard
        delta_ev = amount * 1.75 * weight
    else:
        deep = 1.0 - smoothstep(0.012, 0.030, y)
        tail = 0.10 + 2.35 * 2.0 ** (-y / 0.070)
        weight = (10.50 * deep + tail) * hdr_guard
        delta_ev = -(-amount) * weight
    return y * 2.0**delta_ev, delta_ev, weight


def signed_cuberoot(value: float) -> float:
    return math.copysign(abs(value) ** (1.0 / 3.0), value)


def rec2020_to_oklab(rgb: Rgb) -> Rgb:
    r, g, b = rgb
    x = 0.6369580 * r + 0.1446169 * g + 0.1688809 * b
    y = 0.2627002 * r + 0.6779981 * g + 0.0593017 * b
    z = 0.0000000 * r + 0.0280727 * g + 1.0609851 * b
    sr = 3.24096994 * x - 1.53738318 * y - 0.49861076 * z
    sg = -0.96924364 * x + 1.87596750 * y + 0.04155506 * z
    sb = 0.05563008 * x - 0.20397696 * y + 1.05697151 * z
    l = signed_cuberoot(0.4122214708 * sr + 0.5363325363 * sg + 0.0514459929 * sb)
    m = signed_cuberoot(0.2119034982 * sr + 0.6806995451 * sg + 0.1073969566 * sb)
    s = signed_cuberoot(0.0883024619 * sr + 0.2817188376 * sg + 0.6299787005 * sb)
    return (
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    )


def color_metrics(input_rgb: Rgb, output_rgb: Rgb) -> dict[str, float]:
    input_luma = dot(input_rgb, LUMA)
    output_luma = dot(output_rgb, LUMA)
    if input_luma > 0.0 and output_luma > 0.0:
        ratio_residual = max(
            abs(output_rgb[index] / output_luma - input_rgb[index] / input_luma)
            for index in range(3)
        )
    else:
        ratio_residual = 0.0

    input_lab = rec2020_to_oklab(input_rgb)
    output_lab = rec2020_to_oklab(output_rgb)
    input_chroma = math.hypot(input_lab[1], input_lab[2])
    output_chroma = math.hypot(output_lab[1], output_lab[2])
    input_hue = math.degrees(math.atan2(input_lab[2], input_lab[1])) if input_chroma > 1e-12 else 0.0
    output_hue = math.degrees(math.atan2(output_lab[2], output_lab[1])) if output_chroma > 1e-12 else 0.0
    hue_shift = (output_hue - input_hue + 180.0) % 360.0 - 180.0
    input_normalized_chroma = input_chroma / max(abs(input_lab[0]), 1e-12)
    output_normalized_chroma = output_chroma / max(abs(output_lab[0]), 1e-12)
    return {
        "rgb_ratio_residual": ratio_residual,
        "oklab_hue_input_degrees": input_hue,
        "oklab_hue_output_degrees": output_hue,
        "oklab_hue_shift_degrees": hue_shift,
        "normalized_chroma_input": input_normalized_chroma,
        "normalized_chroma_output": output_normalized_chroma,
        "normalized_chroma_change": output_normalized_chroma - input_normalized_chroma,
    }


def row(
    *,
    control: str,
    setting: float,
    color: str,
    domain: str,
    input_luminance: float,
    input_rgb: Rgb,
    operation_rgb: Rgb,
    display_rgb: Rgb,
    delta_ev: float,
    weight: float,
) -> dict[str, object]:
    metrics = color_metrics(input_rgb, operation_rgb)
    return {
        "control": control,
        "setting": setting,
        "color": color,
        "operation_domain": domain,
        "input_luminance": input_luminance,
        "input_r": input_rgb[0],
        "input_g": input_rgb[1],
        "input_b": input_rgb[2],
        "operation_output_luminance": dot(operation_rgb, LUMA),
        "operation_output_r": operation_rgb[0],
        "operation_output_g": operation_rgb[1],
        "operation_output_b": operation_rgb[2],
        "effective_ev_change": delta_ev,
        "effective_mask_weight": weight,
        "display_output_luminance": dot(display_rgb, LUMA),
        "display_output_r": display_rgb[0],
        "display_output_g": display_rgb[1],
        "display_output_b": display_rgb[2],
        **metrics,
    }


def rows() -> Iterable[dict[str, object]]:
    for setting in SETTINGS:
        for luminance in SCENE_INPUTS:
            scene_out, delta_ev, weight = shadows_scene(luminance, setting)
            for color, ratio in COLOR_RATIOS.items():
                input_rgb = rgb_for_luminance(ratio, luminance)
                operation_rgb = remap_luminance(input_rgb, scene_out)
                display_rgb = remap_luminance(operation_rgb, sigmoid(scene_out))
                yield row(
                    control="Shadows",
                    setting=setting,
                    color=color,
                    domain="scene-linear",
                    input_luminance=luminance,
                    input_rgb=input_rgb,
                    operation_rgb=operation_rgb,
                    display_rgb=display_rgb,
                    delta_ev=delta_ev,
                    weight=weight,
                )

    for setting in SETTINGS:
        for luminance in DISPLAY_INPUTS:
            display_out, delta_ev, weight = blacks_display(luminance, setting)
            for color, ratio in COLOR_RATIOS.items():
                input_rgb = rgb_for_luminance(ratio, luminance)
                operation_rgb = remap_luminance(input_rgb, display_out)
                yield row(
                    control="Blacks",
                    setting=setting,
                    color=color,
                    domain="display-linear",
                    input_luminance=luminance,
                    input_rgb=input_rgb,
                    operation_rgb=operation_rgb,
                    display_rgb=operation_rgb,
                    delta_ev=delta_ev,
                    weight=weight,
                )

def command_analyze_low_tone(args: argparse.Namespace) -> int:
    """Write the analytical Shadows/Blacks response table."""
    data = list(rows())
    fields = list(data[0])
    if args.csv:
        args.csv.parent.mkdir(parents=True, exist_ok=True)
        stream = args.csv.open("w", newline="", encoding="utf-8")
    else:
        stream = sys.stdout
    try:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        writer.writerows(data)
    finally:
        if args.csv:
            stream.close()
    return 0

if np is not None:
    SRGB_LUMA = np.array([0.2126729, 0.7151522, 0.0721750], dtype=np.float32)
    ADOBE_RGB_LUMA = np.array([0.29734498, 0.62736357, 0.07529146], dtype=np.float32)
else:
    SRGB_LUMA = (0.2126729, 0.7151522, 0.0721750)
    ADOBE_RGB_LUMA = (0.29734498, 0.62736357, 0.07529146)
ADOBE_RGB_GAMMA = 2.0 + 51.0 / 256.0
LUMA_QUANTILES = (
    np.array(
        [0.0, 0.01, 0.05, 0.10, 0.20, 0.30, 0.40, 0.50,
         0.60, 0.70, 0.80, 0.90, 0.95, 0.99, 1.0],
        dtype=np.float64,
    )
    if np is not None
    else (0.0, 0.01, 0.05, 0.10, 0.20, 0.30, 0.40, 0.50,
          0.60, 0.70, 0.80, 0.90, 0.95, 0.99, 1.0)
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
    from PIL import Image

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

def command_compare_lightroom(args: argparse.Namespace) -> int:
    """Compare isolated AuRaw and Lightroom adjustment responses."""
    if np is None:
        print("error: numpy is required for compare-lightroom", file=sys.stderr)
        return 2
    if args.sample_step < 1:
        print("error: --sample-step must be positive", file=sys.stderr)
        return 2
    if shutil.which("magick") is None:
        print("error: ImageMagick 7 (`magick`) is required for native 16-bit RGB decoding", file=sys.stderr)
        return 2

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
        print(f"error: {error}", file=sys.stderr)
        return 2
    if auraw_base.shape != lightroom_base.shape:
        print(
            "error: baseline dimensions differ after crop/sample: "
            f"AuRaw {auraw_base.shape[:2]}, Lightroom {lightroom_base.shape[:2]}",
            file=sys.stderr,
        )
        return 2

    auraw_baseline_luma = baseline_luma_quantiles(auraw_base, SRGB_LUMA)
    lightroom_baseline_luma = baseline_luma_quantiles(lightroom_base, ADOBE_RGB_LUMA)
    print("baseline linear-luma quantiles       p05       p50       p95")
    print("AuRaw                              " + " ".join(f"{value:9.4f}" for value in auraw_baseline_luma))
    print("Lightroom                          " + " ".join(f"{value:9.4f}" for value in lightroom_baseline_luma))
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

CORPUS_WIDTH = CORPUS_HEIGHT = 256
CORPUS_BLACK = 512
CORPUS_WHITE = 16383

if np is not None:
    CORPUS_BAYER = np.asarray([[0, 1], [1, 2]], dtype=np.uint8)
    CORPUS_XTRANS = np.asarray(
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
else:
    CORPUS_BAYER = None
    CORPUS_XTRANS = None


def build_scene() -> np.ndarray:
    yy, xx = np.indices((CORPUS_HEIGHT, CORPUS_WIDTH), dtype=np.float32)
    rgb = np.empty((CORPUS_HEIGHT, CORPUS_WIDTH, 3), dtype=np.float32)
    rgb[..., 0] = 0.07 + 0.20 * xx / (CORPUS_WIDTH - 1)
    rgb[..., 1] = 0.08 + 0.18 * yy / (CORPUS_HEIGHT - 1)
    rgb[..., 2] = 0.10 + 0.12 * (xx + yy) / (CORPUS_WIDTH + CORPUS_HEIGHT - 2)

    # Neutral slanted edge: edge spread and direction response.
    edge = xx[24:120, 24:112] > 64.0 + 0.37 * (yy[24:120, 24:112] - 72.0)
    neutral = np.where(edge, 0.68, 0.065).astype(np.float32)
    rgb[24:120, 24:112, :] = neutral[..., None]

    # CFA alias suite: four deterministic near-Nyquist targets covering the
    # common failure modes that motivate dual demosaic. Keeping them in one
    # compact block lets Bayer and X-Trans fixtures share identical scene data.
    # 1) woven fabric with chromatic diagonal modulation.
    fy = yy[24:72, 136:188]
    fx = xx[24:72, 136:188]
    weave = 0.32 + 0.16 * np.sign(np.sin(fx * np.pi / 2.0) * np.sin(fy * np.pi / 3.0))
    diagonal = 0.055 * np.sin((fx + 1.7 * fy) * np.pi / 2.5)
    rgb[24:72, 136:188, 0] = weave + diagonal
    rgb[24:72, 136:188, 1] = weave
    rgb[24:72, 136:188, 2] = weave - diagonal

    # 2) neutral radial zone plate: orientation-independent alias stress.
    fy = yy[24:72, 188:240] - 48.0
    fx = xx[24:72, 188:240] - 214.0
    radius2 = fx * fx + fy * fy
    zone = 0.34 + 0.20 * np.sin(0.095 * radius2)
    rgb[24:72, 188:240, :] = zone[..., None]

    # 3) fine diagonal foliage-like luminance with green-biased microcontrast.
    fy = yy[72:120, 136:188]
    fx = xx[72:120, 136:188]
    leaf = 0.30 + 0.11 * np.sin((1.8 * fx + fy) * np.pi / 2.2)
    leaf += 0.07 * np.sin((fx - 1.4 * fy) * np.pi / 3.1)
    rgb[72:120, 136:188, 0] = leaf * 0.82
    rgb[72:120, 136:188, 1] = leaf * 1.08
    rgb[72:120, 136:188, 2] = leaf * 0.76

    # 4) one/two-pixel chromatic stripe crossings: false-colour stress.
    fy = yy[72:120, 188:240]
    fx = xx[72:120, 188:240]
    carrier = np.sign(np.sin(fx * np.pi / 1.5))
    cross = np.sign(np.sin((fx + fy) * np.pi / 2.0))
    base = 0.34 + 0.08 * np.sign(np.sin(fy * np.pi / 2.5))
    rgb[72:120, 188:240, 0] = base + 0.09 * carrier
    rgb[72:120, 188:240, 1] = base + 0.03 * cross
    rgb[72:120, 188:240, 2] = base - 0.09 * carrier

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
    return np.rint(CORPUS_BLACK + normalized * (CORPUS_WHITE - CORPUS_BLACK)).astype("<u2")


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
        (50714, "H", 1, CORPUS_BLACK, False),
        (50717, "I", 1, CORPUS_WHITE, False),
        (50718, "2I", 2, (1, 1, 1, 1), False),
        (50719, "I", 2, (0, 0), False),
        (50720, "I", 2, (CORPUS_WIDTH, CORPUS_HEIGHT), False),
        (50721, "2i", 9, rational(xyz_to_camera), False),
        (50728, "2I", 3, (1, 1, 1, 1, 1, 1), False),
        (50730, "2i", 1, (0, 1), False),
        (50778, "H", 1, 21, False),
        (50829, "I", 4, (0, 0, CORPUS_HEIGHT, CORPUS_WIDTH), False),
    ]
    import tifffile

    tifffile.imwrite(
        path,
        raw,
        dtype=np.uint16,
        photometric=32803,
        metadata=None,
        compression=None,
        rowsperstrip=CORPUS_HEIGHT,
        extratags=tags,
        byteorder="<",
    )


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def command_corpus(_args: argparse.Namespace) -> int:
    """Regenerate the checked-in CC0 Bayer and X-Trans DNG fixtures."""
    if np is None:
        print("error: numpy is required for corpus generation", file=sys.stderr)
        return 2
    raw_root = ROOT / "regression/raw"
    raw_root.mkdir(parents=True, exist_ok=True)
    fixtures = [
        ("synthetic-bayer.dng", CORPUS_BAYER, "AuRaw", "AuRaw Synthetic Bayer"),
        ("synthetic-xtrans.dng", CORPUS_XTRANS, "FUJIFILM", "AuRaw Synthetic X-Trans"),
    ]
    for name, pattern, make, model in fixtures:
        path = raw_root / name
        write_dng(path, pattern, make, model)
        print(f"{digest(path)}  {path.relative_to(ROOT / 'regression')}")
    return 0


class DevCommandError(RuntimeError):
    """An actionable command failure with a stable process exit code."""

    def __init__(self, message: str, exit_code: int = 1) -> None:
        super().__init__(message)
        self.exit_code = exit_code


def fail(message: str, exit_code: int = 1) -> NoReturn:
    """Raise a command failure that ``main`` can report consistently."""
    raise DevCommandError(message, exit_code)


def command_list(parts: Sequence[str | os.PathLike[str]]) -> list[str]:
    """Return a subprocess-safe argument list."""
    return [os.fspath(part) for part in parts]


def run_process(
    parts: Sequence[str | os.PathLike[str]],
    *,
    cwd: Path = ROOT,
    env: dict[str, str] | None = None,
    check: bool = True,
    capture_output: bool = False,
    text: bool = False,
    stdout: int | None = None,
    stderr: int | None = None,
) -> subprocess.CompletedProcess[str] | subprocess.CompletedProcess[bytes]:
    """Run one explicit subprocess command and translate launch failures."""
    command = command_list(parts)
    try:
        return subprocess.run(
            command,
            cwd=cwd,
            env=env,
            check=check,
            capture_output=capture_output,
            text=text,
            stdout=stdout,
            stderr=stderr,
        )
    except OSError as error:
        fail(f"unable to execute {command[0]}: {error}")


def rooted_path(path: str | os.PathLike[str]) -> Path:
    """Resolve a path relative to the repository."""
    candidate = Path(path).expanduser()
    return candidate if candidate.is_absolute() else ROOT / candidate


def run_regression_command(arguments: Sequence[str]) -> None:
    """Run one isolated regression subcommand and stop on failure."""
    run_process(
        [sys.executable, ROOT / "scripts/dev.py", "regression", *arguments],
        cwd=ROOT,
    )


def required_environment(name: str, message: str) -> str:
    """Read one required non-empty environment variable."""
    value = os.environ.get(name)
    if not value:
        fail(message)
    return value


def command_regression_suite(_args: argparse.Namespace) -> int:
    """Run the full CPU/GPU image-regression workflow."""
    manifest = rooted_path(
        os.environ.get("AURAW_REGRESSION_MANIFEST", ROOT / "regression/corpus.yaml")
    )
    thresholds = rooted_path(
        os.environ.get("AURAW_REGRESSION_THRESHOLDS", ROOT / "regression/thresholds.yaml")
    )
    reference_engines = rooted_path(
        os.environ.get(
            "AURAW_REFERENCE_ENGINES", ROOT / "regression/reference-engines.yaml"
        )
    )
    reference_engine = os.environ.get("AURAW_REFERENCE_ENGINE", "darktable")
    reference_root = rooted_path(
        os.environ.get(
            "AURAW_REFERENCE_ROOT", ROOT / f"regression/references/{reference_engine}"
        )
    )
    output_root = rooted_path(
        os.environ.get("AURAW_REGRESSION_OUTPUT_ROOT", ROOT / "regression/candidates")
    )
    report_root = rooted_path(
        os.environ.get("AURAW_REGRESSION_REPORT_ROOT", ROOT / "regression/reports")
    )
    cpu_command = required_environment(
        "AURAW_CPU_RENDER_COMMAND",
        "Set AURAW_CPU_RENDER_COMMAND with {raw} and {output} placeholders",
    )
    gpu_command = required_environment(
        "AURAW_GPU_RENDER_COMMAND",
        "Set AURAW_GPU_RENDER_COMMAND with {raw} and {output} placeholders",
    )

    run_regression_command(["validate-corpus", "--manifest", os.fspath(manifest), "--verify-files"])
    run_regression_command(
        ["validate-reference-engines", "--config", os.fspath(reference_engines)]
    )
    for backend, template in (("cpu", cpu_command), ("gpu", gpu_command)):
        run_regression_command(
            [
                "render",
                "--manifest",
                os.fspath(manifest),
                "--backend",
                backend,
                "--command-template",
                template,
                "--output-root",
                os.fspath(output_root / backend),
                "--repeat",
                "2",
            ]
        )

    for backend, maximum in (
        ("cpu", os.environ.get("AURAW_CPU_DETERMINISM_MAX_ABS", "0")),
        ("gpu", os.environ.get("AURAW_GPU_DETERMINISM_MAX_ABS", "0")),
    ):
        run_regression_command(
            [
                "determinism",
                "--manifest",
                os.fspath(manifest),
                "--backend",
                backend,
                "--run-a",
                os.fspath(output_root / backend / "run-1"),
                "--run-b",
                os.fspath(output_root / backend / "run-2"),
                "--max-abs",
                maximum,
                "--report",
                os.fspath(report_root / f"{backend}-determinism.json"),
            ]
        )

    for backend in ("cpu", "gpu"):
        run_regression_command(
            [
                "compare",
                "--manifest",
                os.fspath(manifest),
                "--thresholds",
                os.fspath(thresholds),
                "--reference-root",
                os.fspath(reference_root),
                "--candidate-root",
                os.fspath(output_root / backend / "run-1"),
                "--backend",
                backend,
                "--reference-engine",
                reference_engine,
                "--reference-engines",
                os.fspath(reference_engines),
                "--report-dir",
                os.fspath(report_root / f"{backend}-vs-{reference_engine}"),
            ]
        )

    run_regression_command(
        [
            "cpu-gpu",
            "--manifest",
            os.fspath(manifest),
            "--thresholds",
            os.fspath(thresholds),
            "--cpu-root",
            os.fspath(output_root / "cpu/run-1"),
            "--gpu-root",
            os.fspath(output_root / "gpu/run-1"),
            "--report-dir",
            os.fspath(report_root / "cpu-gpu"),
        ]
    )
    return 0


def command_smoke_regression(_args: argparse.Namespace) -> int:
    """Run the deterministic regression-renderer smoke gate."""
    renderer = rooted_path(
        os.environ.get(
            "AURAW_REGRESSION_RENDERER", ROOT / "target/debug/auraw-regression-render"
        )
    )
    output_root = rooted_path(
        os.environ.get("AURAW_REGRESSION_SMOKE_DIR", ROOT / "target/regression-smoke")
    )
    for run_number in (1, 2):
        (output_root / f"run-{run_number}").mkdir(parents=True, exist_ok=True)

    run_regression_command(
        [
            "validate-corpus",
            "--manifest",
            os.fspath(ROOT / "regression/corpus.yaml"),
            "--verify-files",
        ]
    )
    run_regression_command(
        [
            "validate-reference-engines",
            "--config",
            os.fspath(ROOT / "regression/reference-engines.yaml"),
        ]
    )

    scenes = ("synthetic-bayer-multitarget", "synthetic-xtrans-multitarget")
    for run_number in (1, 2):
        for scene in scenes:
            raw_name = scene.removesuffix("-multitarget") + ".dng"
            run_process(
                [
                    renderer,
                    "--backend",
                    "gpu",
                    "--input",
                    ROOT / "regression/raw" / raw_name,
                    "--output",
                    output_root / f"run-{run_number}/{scene}.npz",
                ]
            )

    run_regression_command(
        [
            "determinism",
            "--manifest",
            os.fspath(ROOT / "regression/corpus.yaml"),
            "--backend",
            "gpu",
            "--run-a",
            os.fspath(output_root / "run-1"),
            "--run-b",
            os.fspath(output_root / "run-2"),
            "--max-abs",
            "0",
            "--report",
            os.fspath(output_root / "determinism.json"),
        ]
    )

    regression_root = ROOT / "regression"
    sys.path.insert(0, os.fspath(regression_root))
    try:
        from iqr.io import load_linear_image

        for path in sorted((output_root / "run-1").glob("*.npz")):
            image = load_linear_image(path, color_space="linear-rec2020-d65")
            if image.rgb.shape != (256, 256, 3):
                fail(f"unexpected shape for {path}: {image.rgb.shape}")
            if image.metadata.get("renderer") != "auraw-regression-render":
                fail(f"missing renderer metadata in {path}")
            print(f"validated {path.name}: {image.rgb.shape}, {image.rgb.dtype}")
    finally:
        try:
            sys.path.remove(os.fspath(regression_root))
        except ValueError:
            pass
    return 0


def percentile(values: Sequence[float], quantile: float) -> float:
    """Return a linearly interpolated percentile for a non-empty sample."""
    if not values:
        fail("cannot calculate a percentile from an empty sample")
    ordered = sorted(float(value) for value in values)
    position = (len(ordered) - 1) * quantile
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    weight = position - lower
    return ordered[lower] * (1.0 - weight) + ordered[upper] * weight


def parse_benchmark_workgroup(value: str) -> tuple[int, int]:
    """Parse a WIDTHxHEIGHT workgroup value used by the benchmark runner."""
    match = re.fullmatch(r"([1-9][0-9]*)[xX]([1-9][0-9]*)", value.strip())
    if match is None:
        raise argparse.ArgumentTypeError("workgroup size must use WIDTHxHEIGHT")
    return int(match.group(1)), int(match.group(2))


def command_bench(args: argparse.Namespace) -> int:
    """Benchmark GPU workgroup configurations and emit a comparable JSON report."""
    budget_path = rooted_path(args.budget)
    try:
        budget = json.loads(budget_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"unable to read GPU benchmark budget {budget_path}: {error}")

    if budget.get("schema") != 2:
        fail(f"unsupported GPU benchmark budget schema in {budget_path}")
    scenes = budget.get("scenes")
    configured_workgroups = budget.get("workgroup_sizes")
    limits = budget.get("budgets")
    if not isinstance(scenes, list) or not scenes:
        fail("GPU benchmark budget must contain at least one scene")
    if not isinstance(configured_workgroups, list) or not configured_workgroups:
        fail("GPU benchmark budget must contain workgroup_sizes")
    if not isinstance(limits, dict):
        fail("GPU benchmark budget must contain budgets")
    required_limits = {
        "pipeline_create_p95_ms",
        "render_p95_ms",
        "export_mp_per_second_min",
    }
    missing_limits = required_limits.difference(limits)
    if missing_limits:
        fail(
            "GPU benchmark budget is missing: "
            + ", ".join(sorted(missing_limits))
        )

    if not all(isinstance(value, str) for value in configured_workgroups):
        fail("GPU benchmark workgroup_sizes must be strings")
    workgroups = args.workgroup_size or [
        parse_benchmark_workgroup(value) for value in configured_workgroups
    ]
    try:
        warmup_runs = int(budget.get("warmup_runs", 0))
        measured_runs = int(budget.get("measured_runs", 1))
    except (TypeError, ValueError):
        fail("GPU benchmark run counts must be integers")
    if warmup_runs < 0 or measured_runs < 1:
        fail("GPU benchmark run counts are invalid")

    executable_suffix = ".exe" if os.name == "nt" else ""
    renderer = rooted_path(
        args.renderer
        or ROOT / f"target/release/auraw-regression-render{executable_suffix}"
    )
    if args.dry_run:
        for workgroup_x, workgroup_y in workgroups:
            label = f"{workgroup_x}x{workgroup_y}"
            for scene in scenes:
                if not isinstance(scene, str):
                    fail("GPU benchmark scene names must be strings")
                raw_name = scene.removesuffix("-multitarget") + ".dng"
                raw_path = ROOT / "regression/raw" / raw_name
                if not raw_path.is_file():
                    fail(f"GPU benchmark RAW does not exist: {raw_path}")
                output_path = ROOT / "target/benchmarks" / f"{label}-{scene}-1.npz"
                metrics_path = ROOT / "target/benchmarks" / f"{label}-{scene}-1.json"
                command = command_list(
                    [
                        renderer,
                        "--backend",
                        "gpu",
                        "--input",
                        raw_path,
                        "--output",
                        output_path,
                        "--workgroup-size",
                        label,
                        "--benchmark-json",
                        metrics_path,
                    ]
                )
                print(shlex.join(command))
        return 0

    if not args.no_build:
        run_process(
            [
                "cargo",
                "build",
                "--release",
                "-p",
                "auraw-cli",
                "--bin",
                "auraw-regression-render",
            ]
        )
    if not renderer.is_file():
        fail(f"GPU benchmark renderer does not exist: {renderer}")

    report_path = rooted_path(args.report)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    all_results: list[dict[str, object]] = []
    adapter: dict[str, object] | None = None
    failed = False

    with tempfile.TemporaryDirectory(prefix="auraw-gpu-bench-") as temporary:
        temporary_root = Path(temporary)
        for workgroup_x, workgroup_y in workgroups:
            label = f"{workgroup_x}x{workgroup_y}"
            measured_samples: list[dict[str, object]] = []
            print(f"benchmarking workgroup {label}")
            for scene in scenes:
                if not isinstance(scene, str):
                    fail("GPU benchmark scene names must be strings")
                raw_name = scene.removesuffix("-multitarget") + ".dng"
                raw_path = ROOT / "regression/raw" / raw_name
                if not raw_path.is_file():
                    fail(f"GPU benchmark RAW does not exist: {raw_path}")

                for run_number in range(warmup_runs + measured_runs):
                    measured = run_number >= warmup_runs
                    phase = "measured" if measured else "warmup"
                    phase_run = run_number - warmup_runs + 1 if measured else run_number + 1
                    stem = f"{label}-{scene}-{phase}-{phase_run}"
                    output_path = temporary_root / f"{stem}.npz"
                    metrics_path = temporary_root / f"{stem}.json"
                    started = time.perf_counter()
                    run_process(
                        [
                            renderer,
                            "--backend",
                            "gpu",
                            "--input",
                            raw_path,
                            "--output",
                            output_path,
                            "--workgroup-size",
                            label,
                            "--benchmark-json",
                            metrics_path,
                        ],
                        stdout=subprocess.DEVNULL,
                    )
                    process_ms = (time.perf_counter() - started) * 1_000.0
                    try:
                        sample = json.loads(metrics_path.read_text(encoding="utf-8"))
                    except (OSError, json.JSONDecodeError) as error:
                        fail(f"unable to read renderer metrics {metrics_path}: {error}")
                    if sample.get("workgroup_size") != [workgroup_x, workgroup_y, 1]:
                        fail(f"renderer reported the wrong workgroup for {stem}")
                    required_metrics = {
                        "pipeline_create_ms",
                        "render_ms",
                        "export_mp_per_second",
                    }
                    missing_metrics = required_metrics.difference(sample)
                    if missing_metrics:
                        fail(
                            f"renderer metrics for {stem} are missing: "
                            + ", ".join(sorted(missing_metrics))
                        )
                    sample_adapter = sample.get("adapter")
                    if not isinstance(sample_adapter, dict):
                        fail(f"renderer omitted adapter metadata for {stem}")
                    if adapter is None:
                        adapter = sample_adapter
                    elif adapter != sample_adapter:
                        fail("GPU adapter changed during the benchmark run")
                    sample.update(
                        {
                            "scene": scene,
                            "run": phase_run,
                            "process_ms": process_ms,
                        }
                    )
                    if measured:
                        measured_samples.append(sample)

            pipeline_times = [float(sample["pipeline_create_ms"]) for sample in measured_samples]
            render_times = [float(sample["render_ms"]) for sample in measured_samples]
            throughputs = [float(sample["export_mp_per_second"]) for sample in measured_samples]
            process_times = [float(sample["process_ms"]) for sample in measured_samples]
            aggregate = {
                "pipeline_create_p95_ms": percentile(pipeline_times, 0.95),
                "render_p50_ms": percentile(render_times, 0.50),
                "render_p95_ms": percentile(render_times, 0.95),
                "export_mp_per_second_min": min(throughputs),
                "export_mp_per_second_median": percentile(throughputs, 0.50),
                "process_p95_ms": percentile(process_times, 0.95),
            }
            checks = {
                "pipeline_create_p95_ms": aggregate["pipeline_create_p95_ms"]
                <= float(limits["pipeline_create_p95_ms"]),
                "render_p95_ms": aggregate["render_p95_ms"]
                <= float(limits["render_p95_ms"]),
                "export_mp_per_second_min": aggregate["export_mp_per_second_min"]
                >= float(limits["export_mp_per_second_min"]),
            }
            workgroup_passed = all(checks.values())
            failed = failed or not workgroup_passed
            all_results.append(
                {
                    "workgroup_size": [workgroup_x, workgroup_y, 1],
                    "aggregate": aggregate,
                    "budget_checks": checks,
                    "passed": workgroup_passed,
                    "samples": measured_samples,
                }
            )
            status = "PASS" if workgroup_passed else "FAIL"
            print(
                f"  {status} compile p95 {aggregate['pipeline_create_p95_ms']:.2f} ms, "
                f"render p95 {aggregate['render_p95_ms']:.2f} ms, "
                f"minimum {aggregate['export_mp_per_second_min']:.3f} MP/s"
            )

    report = {
        "schema": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "budget_file": os.fspath(budget_path.relative_to(ROOT) if budget_path.is_relative_to(ROOT) else budget_path),
        "renderer": os.fspath(renderer),
        "adapter": adapter,
        "scenes": scenes,
        "warmup_runs": warmup_runs,
        "measured_runs": measured_runs,
        "budgets": limits,
        "workgroups": all_results,
        "passed": not failed,
    }
    report_path.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {report_path}")
    return 1 if failed else 0


def command_regression(args: argparse.Namespace) -> int:
    """Delegate to the image-quality regression framework."""
    regression_root = ROOT / "regression"
    sys.path.insert(0, str(regression_root))
    try:
        from iqr.cli import main as regression_main
        return int(regression_main(args.regression_args))
    finally:
        try:
            sys.path.remove(str(regression_root))
        except ValueError:
            pass

def build_parser() -> argparse.ArgumentParser:
    """Build the command-line parser."""
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    corpus_parser = subparsers.add_parser("corpus", help="regenerate the synthetic RAW corpus")
    corpus_parser.set_defaults(handler=command_corpus)

    low_tone = subparsers.add_parser("analyze-low-tone", help="emit the Shadows/Blacks analytical response table")
    low_tone.add_argument("--csv", type=Path)
    low_tone.set_defaults(handler=command_analyze_low_tone)

    compare = subparsers.add_parser("compare-lightroom", help="compare AuRaw controls with Lightroom endpoints")
    compare.add_argument("--lightroom-dir", type=Path, required=True)
    compare.add_argument("--auraw-dir", type=Path, required=True)
    compare.add_argument("--lightroom-baseline", default="Camera NT.tif")
    compare.add_argument("--auraw-baseline", default="baseline.png")
    compare.add_argument("--lightroom-crop", type=parse_crop, default=None)
    compare.add_argument("--auraw-crop", type=parse_crop, default=None)
    compare.add_argument("--sample-step", type=int, default=4)
    compare.set_defaults(handler=command_compare_lightroom)

    regression = subparsers.add_parser("regression", help="run an image-regression framework command")
    regression.add_argument("regression_args", nargs=argparse.REMAINDER)
    regression.set_defaults(handler=command_regression)

    regression_suite = subparsers.add_parser("regression-suite", help="run the full CPU/GPU image-regression workflow")
    regression_suite.set_defaults(handler=command_regression_suite)

    smoke = subparsers.add_parser("smoke-regression", help="run the deterministic regression-renderer smoke gate")
    smoke.set_defaults(handler=command_smoke_regression)

    bench = subparsers.add_parser(
        "bench", help="benchmark GPU compute workgroup configurations"
    )
    bench.add_argument(
        "--budget", type=Path, default=Path("benchmarks/gpu-budget.json")
    )
    bench.add_argument("--renderer", type=Path)
    bench.add_argument(
        "--report", type=Path, default=Path("target/benchmarks/gpu-workgroups.json")
    )
    bench.add_argument("--no-build", action="store_true")
    bench.add_argument(
        "--dry-run",
        action="store_true",
        help="print every renderer invocation without building or running it",
    )
    bench.add_argument(
        "--workgroup-size",
        action="append",
        type=parse_benchmark_workgroup,
        help="benchmark only WIDTHxHEIGHT; may be repeated",
    )
    bench.set_defaults(handler=command_bench)

    return parser


def main(argv: Sequence[str] | None = None) -> int:
    """Parse arguments and dispatch a development command."""
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return int(args.handler(args))
    except KeyboardInterrupt:
        return 130
    except subprocess.CalledProcessError as error:
        return int(error.returncode or 1)
    except DevCommandError as error:
        if str(error):
            print(f"error: {error}", file=sys.stderr)
        return error.exit_code
    except OSError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
