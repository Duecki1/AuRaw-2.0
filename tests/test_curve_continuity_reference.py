"""Functional invariants for the scene-curve reference math.

The Python model intentionally avoids copying the shader's exact shoulder
coordinate.  The WGSL source remains authoritative for that internal join;
these tests only verify the behavior the join must preserve.
"""
from __future__ import annotations

import math
import re
from pathlib import Path

import numpy as np

# Scene-domain boundaries used by callers, not fitted curve coefficients.
SCENE_MIDDLE_GREY = 0.1845
SCENE_CURVE_MAX = 32768.0
SCENE_CURVE_WORK_LIMIT = 60000.0
HALF_FLOAT_MAX = float(np.finfo(np.float16).max)

_TONEMAP_SHADER = Path(__file__).parents[1] / "crates" / "auraw-gpu" / "src" / "shaders" / "tonemap.wgsl"


def _shader_float_constant(name: str) -> np.float32:
    """Read an internal scalar from the shader without duplicating its digits."""
    source = _TONEMAP_SHADER.read_text(encoding="utf-8")
    pattern = rf"const\s+{re.escape(name)}\s*:\s*f32\s*=\s*([+\-0-9.eE]+)\s*;"
    match = re.search(pattern, source)
    if match is None:
        raise AssertionError(f"could not find scalar WGSL constant {name}")
    return np.float32(match.group(1))


_SHOULDER_ENCODE_START = _shader_float_constant(
    "SCENE_CURVE_SHOULDER_ENCODE_START"
)


def _curve_parameters() -> tuple[np.float32, ...]:
    middle = np.float32(SCENE_MIDDLE_GREY)
    maximum = np.float32(SCENE_CURVE_MAX)
    shoulder_width = np.float32(np.float32(1.0) - _SHOULDER_ENCODE_START)
    shoulder_start = np.float32(
        middle * _SHOULDER_ENCODE_START / shoulder_width
    )
    shoulder_tangent = np.float32(middle / shoulder_width)
    return middle, maximum, shoulder_width, shoulder_start, shoulder_tangent


def _shoulder_decode(t: np.float32 | float) -> np.float32:
    _, maximum, _, shoulder_start, shoulder_tangent = _curve_parameters()
    bounded = np.clip(np.float32(t), np.float32(0.0), np.float32(1.0))
    t2 = np.float32(bounded * bounded)
    t3 = np.float32(t2 * bounded)
    return np.float32(
        np.float32(np.float32(2.0) * t3 - np.float32(3.0) * t2 + np.float32(1.0))
        * shoulder_start
        + np.float32(t3 - np.float32(2.0) * t2 + bounded) * shoulder_tangent
        + np.float32(-np.float32(2.0) * t3 + np.float32(3.0) * t2)
        * maximum
    )


def _shoulder_derivative(t: np.float32 | float) -> np.float32:
    _, maximum, _, shoulder_start, shoulder_tangent = _curve_parameters()
    bounded = np.clip(np.float32(t), np.float32(0.0), np.float32(1.0))
    t2 = np.float32(bounded * bounded)
    return np.float32(
        np.float32(np.float32(6.0) * t2 - np.float32(6.0) * bounded)
        * shoulder_start
        + np.float32(
            np.float32(3.0) * t2
            - np.float32(4.0) * bounded
            + np.float32(1.0)
        )
        * shoulder_tangent
        + np.float32(-np.float32(6.0) * t2 + np.float32(6.0) * bounded)
        * maximum
    )


def scene_curve_decode(value: np.float32 | float) -> np.float32:
    middle, maximum, shoulder_width, _, _ = _curve_parameters()
    bounded = np.clip(np.float32(value), np.float32(0.0), np.float32(1.0))
    if bounded <= _SHOULDER_ENCODE_START:
        denominator = np.maximum(
            np.float32(1.0) - bounded, np.float32(1e-6)
        )
        return np.float32(middle * bounded / denominator)

    t = np.float32((bounded - _SHOULDER_ENCODE_START) / shoulder_width)
    return np.clip(_shoulder_decode(t), np.float32(0.0), maximum)


def scene_curve_encode(value: np.float32 | float) -> np.float32:
    middle, maximum, shoulder_width, shoulder_start, _ = _curve_parameters()
    positive = np.clip(np.float32(value), np.float32(0.0), maximum)
    if positive <= shoulder_start:
        return np.minimum(
            np.float32(positive / np.float32(positive + middle)),
            _SHOULDER_ENCODE_START,
        )

    low = np.float32(0.0)
    high = np.float32(1.0)
    for _ in range(8):
        midpoint = np.float32(np.float32(0.5) * np.float32(low + high))
        if _shoulder_decode(midpoint) < positive:
            low = midpoint
        else:
            high = midpoint

    low_encoded = np.float32(_SHOULDER_ENCODE_START + shoulder_width * low)
    high_encoded = np.float32(_SHOULDER_ENCODE_START + shoulder_width * high)
    low_error = abs(float(scene_curve_decode(low_encoded) - positive))
    high_error = abs(float(scene_curve_decode(high_encoded) - positive))
    return high_encoded if high_error < low_error else low_encoded


def limit_scene_curve_rgb_ratio_preserving(
    value: tuple[float, float, float],
) -> tuple[float, float, float]:
    peak = max(abs(channel) for channel in value)
    scale = min(1.0, SCENE_CURVE_WORK_LIMIT / max(peak, 1e-12))
    return tuple(
        min(
            max(channel * scale, -SCENE_CURVE_WORK_LIMIT),
            SCENE_CURVE_WORK_LIMIT,
        )
        for channel in value
    )


def _adjacent_f32_values(center: np.float32, each_side: int) -> np.ndarray:
    lower: list[np.float32] = []
    value = np.float32(center)
    for _ in range(each_side):
        value = np.nextafter(value, np.float32(-np.inf), dtype=np.float32)
        lower.append(value)

    upper: list[np.float32] = []
    value = np.float32(center)
    for _ in range(each_side):
        value = np.nextafter(value, np.float32(np.inf), dtype=np.float32)
        upper.append(value)

    return np.asarray(
        list(reversed(lower)) + [np.float32(center)] + upper,
        dtype=np.float32,
    )


def _all_f16_values_in_unit_interval() -> np.ndarray:
    # Positive IEEE-754 binary16 encodings are ordered by their bit patterns.
    return np.arange(0x0000, 0x3C01, dtype=np.uint16).view(np.float16)


def _assert_nondecreasing(values: np.ndarray, *, tolerance: float = 0.0) -> None:
    differences = np.diff(values.astype(np.float64))
    assert np.all(differences >= -tolerance), float(differences.min())


def test_scene_curve_is_neutral_at_zero_and_clamps_its_domain() -> None:
    assert scene_curve_decode(0.0) == np.float32(0.0)
    assert scene_curve_encode(0.0) == np.float32(0.0)
    assert scene_curve_decode(-1.0) == np.float32(0.0)
    assert scene_curve_decode(2.0) == np.float32(SCENE_CURVE_MAX)


def test_scene_curve_decode_is_monotone_bounded_and_f16_safe() -> None:
    encoded = np.unique(
        np.concatenate(
            (
                _all_f16_values_in_unit_interval().astype(np.float32),
                _adjacent_f32_values(_SHOULDER_ENCODE_START, 256),
                np.asarray([0.0, 1.0], dtype=np.float32),
            )
        )
    )
    decoded = np.asarray([scene_curve_decode(value) for value in encoded])

    assert np.all(np.isfinite(decoded))
    assert np.all(decoded >= np.float32(0.0))
    assert np.all(decoded <= np.float32(SCENE_CURVE_MAX))
    _assert_nondecreasing(decoded)

    stored = decoded.astype(np.float16)
    assert np.all(np.isfinite(stored))
    assert np.all(stored.astype(np.float64) <= HALF_FLOAT_MAX)
    _assert_nondecreasing(stored)


def test_scene_curve_encode_and_round_trip_are_order_preserving() -> None:
    _, _, _, shoulder_start, _ = _curve_parameters()
    scene_values = np.unique(
        np.concatenate(
            (
                np.linspace(0.0, 1.0, 1025, dtype=np.float32),
                np.geomspace(
                    np.float32(SCENE_MIDDLE_GREY),
                    np.float32(SCENE_CURVE_MAX),
                    4096,
                    dtype=np.float32,
                ),
                _adjacent_f32_values(shoulder_start, 256),
                np.asarray([0.0, SCENE_CURVE_MAX], dtype=np.float32),
            )
        )
    )
    encoded = np.asarray([scene_curve_encode(value) for value in scene_values])
    decoded = np.asarray([scene_curve_decode(value) for value in encoded])

    assert np.all(np.isfinite(encoded))
    assert np.all((encoded >= np.float32(0.0)) & (encoded <= np.float32(1.0)))
    _assert_nondecreasing(encoded)

    assert np.all(np.isfinite(decoded))
    assert np.all((decoded >= np.float32(0.0)) & (decoded <= SCENE_CURVE_MAX))
    _assert_nondecreasing(decoded)


def test_shoulder_join_is_c0_and_c1_continuous() -> None:
    middle, _, shoulder_width, _, _ = _curve_parameters()
    denominator = np.float32(np.float32(1.0) - _SHOULDER_ENCODE_START)

    rational_value = np.float32(
        middle * _SHOULDER_ENCODE_START / denominator
    )
    shoulder_value = _shoulder_decode(np.float32(0.0))
    assert math.isclose(
        float(rational_value), float(shoulder_value), rel_tol=5e-6, abs_tol=1e-3
    )

    rational_derivative = np.float32(middle / np.float32(denominator * denominator))
    shoulder_derivative = np.float32(
        _shoulder_derivative(np.float32(0.0)) / shoulder_width
    )
    assert math.isclose(
        float(rational_derivative),
        float(shoulder_derivative),
        rel_tol=5e-6,
        abs_tol=1.0,
    )


def test_shoulder_reaches_the_ceiling_with_zero_endpoint_slope() -> None:
    _, maximum, shoulder_width, _, _ = _curve_parameters()
    assert math.isclose(
        float(scene_curve_decode(1.0)), float(maximum), rel_tol=0.0, abs_tol=1e-3
    )
    endpoint_slope = float(_shoulder_derivative(1.0) / shoulder_width)
    assert math.isclose(endpoint_slope, 0.0, rel_tol=0.0, abs_tol=1e-3)


def test_scene_curve_work_limiter_is_bounded_and_ratio_preserving() -> None:
    candidates = (
        (0.0, 0.0, 0.0),
        (SCENE_CURVE_WORK_LIMIT / 2.0, -100.0, 25.0),
        (SCENE_CURVE_WORK_LIMIT * 2.0, SCENE_CURVE_WORK_LIMIT / 3.0, -7.0),
    )
    for candidate in candidates:
        limited = limit_scene_curve_rgb_ratio_preserving(candidate)
        assert all(math.isfinite(channel) for channel in limited)
        assert max(abs(channel) for channel in limited) <= SCENE_CURVE_WORK_LIMIT

        peak = max(abs(channel) for channel in candidate)
        if peak > SCENE_CURVE_WORK_LIMIT:
            scale = SCENE_CURVE_WORK_LIMIT / peak
            assert all(
                math.isclose(output, source * scale, rel_tol=1e-12, abs_tol=1e-12)
                for source, output in zip(candidate, limited)
            )
