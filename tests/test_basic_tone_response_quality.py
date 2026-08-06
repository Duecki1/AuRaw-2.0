"""Functional invariants for the basic Shadows and Blacks response models."""
from __future__ import annotations

import math
from collections.abc import Callable

import numpy as np

# Display-domain region boundaries. These are semantic joins, not fitted
# response coefficients copied for exact-output comparisons.
BLACK_TOE_DEEP_REGION = (0.012, 0.030)
BLACK_TOE_HDR_GUARD = (0.35, 1.0)
DISPLAY_RANGE = (0.0, 1.0)
HALF_FLOAT_MAX = float(np.finfo(np.float16).max)

CONTROL_AMOUNTS = (-100.0, -50.0, -25.0, 0.0, 25.0, 50.0, 100.0)
SHADOW_HISTOGRAMS = (
    (-8.0, -4.0),  # low-key
    (-5.0, 0.0),   # normal
    (-2.0, 1.0),   # high-key
    (-6.0, -1.0),  # backlit/night-like
    (-6.8, -6.2),  # compressed dark histogram
    (-1.9, -1.2),  # compressed bright histogram
)


def smoothstep(edge0: float, edge1: float, value: float) -> float:
    t = max(0.0, min(1.0, (value - edge0) / max(edge1 - edge0, 1e-6)))
    return t * t * (3.0 - 2.0 * t)


def shaped(value: float) -> float:
    normalized = max(-1.0, min(1.0, value / 100.0))
    magnitude = abs(normalized)
    if magnitude == 0.0:
        return 0.0
    return math.copysign(
        magnitude * (1.45 - 0.45 * magnitude), normalized
    )


def shadow_range(p05: float, p50: float) -> tuple[float, float]:
    return p05 - 0.90, p50 + 1.35


def shadow_weight(ev: float, bounds: tuple[float, float]) -> float:
    lower, upper = bounds
    return 1.0 - smoothstep(lower, upper, ev)


def shadow_output_ev(
    ev: float,
    amount: float,
    p05: float = -5.0,
    p50: float = 0.0,
) -> tuple[float, float]:
    bounds = shadow_range(p05, p50)
    mask = shadow_weight(ev, bounds)
    control = shaped(amount)
    lower, upper = bounds
    monotone_limit = 0.64 * max(upper - lower, 0.25)
    strength = (
        math.copysign(min(abs(control) * 2.20, monotone_limit), control)
        if control
        else 0.0
    )
    return ev + strength * mask, mask


def display_black_toe(luma: float, amount: float) -> float:
    if luma <= 0.0 or amount == 0.0:
        return luma

    control = shaped(amount)
    hdr_guard = 1.0 - smoothstep(*BLACK_TOE_HDR_GUARD, luma)
    if control >= 0.0:
        weight = 0.08 + 0.92 * 2.0 ** (-luma / 0.035)
        offset_ev = control * 1.75 * weight * hdr_guard
    else:
        deep = 1.0 - smoothstep(*BLACK_TOE_DEEP_REGION, luma)
        tail = 0.10 + 2.35 * 2.0 ** (-luma / 0.070)
        offset_ev = -(-control) * (10.50 * deep + tail) * hdr_guard
    return luma * 2.0**offset_ev


def _all_f16_values_in_unit_interval() -> np.ndarray:
    return np.arange(0x0000, 0x3C01, dtype=np.uint16).view(np.float16)


def _assert_nondecreasing(values: np.ndarray, *, tolerance: float = 0.0) -> None:
    differences = np.diff(values.astype(np.float64))
    assert np.all(differences >= -tolerance), float(differences.min())


def _one_sided_derivatives(
    function: Callable[[float], float],
    join: float,
    step: float,
) -> tuple[float, float]:
    value = function(join)
    left = (value - function(join - step)) / step
    right = (function(join + step) - value) / step
    return left, right


def test_zero_input_and_neutral_controls_are_exactly_neutral() -> None:
    for amount in CONTROL_AMOUNTS:
        assert display_black_toe(0.0, amount) == 0.0

    for luma in _all_f16_values_in_unit_interval():
        assert display_black_toe(float(luma), 0.0) == float(luma)

    for ev in (-16.0, -8.0, -4.0, 0.0, 4.0, 12.0):
        mapped, _ = shadow_output_ev(ev, 0.0)
        assert mapped == ev


def test_shadow_transfer_is_finite_and_monotone_for_extreme_histograms() -> None:
    ev_values = np.linspace(-16.0, 12.0, 4097)
    for p05, p50 in SHADOW_HISTOGRAMS:
        lower, upper = shadow_range(p05, p50)
        assert math.isfinite(lower) and math.isfinite(upper)
        assert lower < upper

        for amount in CONTROL_AMOUNTS:
            mapped = np.asarray(
                [shadow_output_ev(float(ev), amount, p05, p50)[0] for ev in ev_values]
            )
            assert np.all(np.isfinite(mapped))
            _assert_nondecreasing(mapped, tolerance=1e-12)


def test_shadow_mask_joins_are_c0_and_c1_continuous() -> None:
    step = 1e-5
    for p05, p50 in SHADOW_HISTOGRAMS:
        for amount in (-100.0, -50.0, 50.0, 100.0):
            function = lambda ev: shadow_output_ev(ev, amount, p05, p50)[0]
            for join in shadow_range(p05, p50):
                center = function(join)
                left_value = function(join - step)
                right_value = function(join + step)
                assert abs(right_value - left_value) < 3e-5 * max(1.0, abs(center))

                left_derivative, right_derivative = _one_sided_derivatives(
                    function, join, step
                )
                assert math.isclose(
                    left_derivative,
                    right_derivative,
                    rel_tol=1e-5,
                    abs_tol=1e-5,
                )


def test_black_toe_is_monotone_bounded_and_f16_safe() -> None:
    inputs = _all_f16_values_in_unit_interval().astype(np.float64)
    minimum, maximum = DISPLAY_RANGE

    for amount in CONTROL_AMOUNTS:
        mapped = np.asarray([display_black_toe(float(y), amount) for y in inputs])
        assert np.all(np.isfinite(mapped))
        assert np.all((mapped >= minimum) & (mapped <= maximum))
        _assert_nondecreasing(mapped)

        stored = mapped.astype(np.float16)
        assert np.all(np.isfinite(stored))
        assert np.all(stored.astype(np.float64) <= HALF_FLOAT_MAX)
        _assert_nondecreasing(stored)


def test_black_toe_region_joins_are_c0_and_c1_continuous() -> None:
    step = 1e-7
    joins = (*BLACK_TOE_DEEP_REGION, *BLACK_TOE_HDR_GUARD)

    for amount in (-100.0, -50.0, -25.0, 25.0, 50.0, 100.0):
        function = lambda luma: display_black_toe(luma, amount)
        for join in joins:
            center = function(join)
            left_value = function(join - step)
            right_value = function(join + step)
            assert abs(right_value - left_value) < 1e-5 * max(1.0, abs(center))

            left_derivative, right_derivative = _one_sided_derivatives(
                function, join, step
            )
            assert math.isclose(
                left_derivative,
                right_derivative,
                rel_tol=2e-4,
                abs_tol=1e-5,
            )


def test_black_toe_is_identity_above_the_hdr_guard() -> None:
    for amount in CONTROL_AMOUNTS:
        for luma in (1.0, 2.0, 4.0, 16.0):
            assert display_black_toe(luma, amount) == luma
