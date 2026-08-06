#!/usr/bin/env python3
"""Generate an analytical Shadows/Blacks color-ramp report.

This is a policy/reference model, not executed WGSL. It mirrors the scalar
low-tone transfer functions and uses a luminance-preserving approximation for
the default sigmoid solely to expose RGB-ratio, hue, and normalized-chroma
changes. Naga validation and rendered GPU comparisons remain required.
"""
from __future__ import annotations

import argparse
import csv
import math
import sys
from pathlib import Path
from typing import Iterable

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


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--csv", type=Path)
    args = parser.parse_args()
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


if __name__ == "__main__":
    main()
