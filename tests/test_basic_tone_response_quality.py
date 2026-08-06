"""Functional invariants for the canonical Shadows and Blacks response models."""
from __future__ import annotations

import math

import numpy as np

from tests.auraw_math_eval import evaluate_math

# Display-domain region boundaries are semantic joins used by the production
# shader. The response itself is evaluated by the Rust CLI, never reimplemented
# in Python.
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


def _all_f16_values_in_unit_interval() -> np.ndarray:
    return np.arange(0x0000, 0x3C01, dtype=np.uint16).view(np.float16)


def _assert_nondecreasing(values: np.ndarray, *, tolerance: float = 0.0) -> None:
    differences = np.diff(values.astype(np.float64))
    assert np.all(differences >= -tolerance), float(differences.min())


def _black_toe(luma: np.ndarray, amount: float) -> np.ndarray:
    samples = np.column_stack(
        (luma.astype(np.float32), np.full(luma.shape, amount, dtype=np.float32))
    )
    return evaluate_math("display-black-toe-value", samples)[:, 0]


def _shadows(
    ev: np.ndarray,
    amount: float,
    p05: float,
    p50: float,
) -> np.ndarray:
    samples = np.column_stack(
        (
            ev.astype(np.float32),
            np.full(ev.shape, amount, dtype=np.float32),
            np.full(ev.shape, p05, dtype=np.float32),
            np.full(ev.shape, p50, dtype=np.float32),
        )
    )
    return evaluate_math("shadows-scene", samples)


def test_zero_input_and_neutral_controls_are_exactly_neutral() -> None:
    zero_samples = [[0.0, amount] for amount in CONTROL_AMOUNTS]
    zero_outputs = evaluate_math("display-black-toe-value", zero_samples)[:, 0]
    assert np.array_equal(zero_outputs, np.zeros_like(zero_outputs))

    luma = _all_f16_values_in_unit_interval().astype(np.float32)
    neutral_black = _black_toe(luma, 0.0)
    assert np.array_equal(neutral_black, luma)

    ev = np.asarray((-16.0, -8.0, -4.0, 0.0, 4.0, 12.0), dtype=np.float32)
    neutral_shadows = _shadows(ev, 0.0, -5.0, 0.0)[:, 0]
    assert np.array_equal(neutral_shadows, ev)


def test_shadow_transfer_is_finite_and_monotone_for_extreme_histograms() -> None:
    ev_values = np.linspace(-16.0, 12.0, 4097, dtype=np.float32)
    cases = [
        (p05, p50, amount)
        for p05, p50 in SHADOW_HISTOGRAMS
        for amount in CONTROL_AMOUNTS
    ]
    samples = np.asarray(
        [
            (ev, amount, p05, p50)
            for p05, p50, amount in cases
            for ev in ev_values
        ],
        dtype=np.float32,
    )
    evaluated = evaluate_math("shadows-scene", samples).reshape(
        len(cases), ev_values.size, 4
    )
    for result, case in zip(evaluated, cases):
        mapped = result[:, 0]
        lower, upper = (float(result[0, 2]), float(result[0, 3]))
        assert math.isfinite(lower) and math.isfinite(upper), case
        assert lower < upper, case
        assert np.all(np.isfinite(mapped)), case
        _assert_nondecreasing(mapped, tolerance=2e-6)


def test_shadow_mask_joins_are_c0_and_c1_continuous() -> None:
    step = 1e-4
    controls = (-100.0, -50.0, 50.0, 100.0)
    metadata_rows = [
        [0.0, amount, p05, p50]
        for p05, p50 in SHADOW_HISTOGRAMS
        for amount in controls
    ]
    metadata = evaluate_math("shadows-scene", metadata_rows)

    rows: list[list[float]] = []
    cases: list[tuple[float, float, float]] = []
    for (p05, p50, amount), probe in zip(
        (
            (p05, p50, amount)
            for p05, p50 in SHADOW_HISTOGRAMS
            for amount in controls
        ),
        metadata,
    ):
        for join in (float(probe[2]), float(probe[3])):
            rows.extend(
                [
                    [join - step, amount, p05, p50],
                    [join, amount, p05, p50],
                    [join + step, amount, p05, p50],
                ]
            )
            cases.append((join, p05, p50))

    evaluated = evaluate_math("shadows-scene", rows)[:, 0].reshape(-1, 3)
    for (left_value, center, right_value), case in zip(evaluated, cases):
        assert abs(float(right_value - left_value)) < 3e-4 * max(1.0, abs(float(center))), case
        left_derivative = float((center - left_value) / step)
        right_derivative = float((right_value - center) / step)
        assert math.isclose(
            left_derivative,
            right_derivative,
            rel_tol=8e-3,
            abs_tol=3e-3,
        ), (case, left_derivative, right_derivative)


def test_black_toe_is_monotone_bounded_and_f16_safe() -> None:
    inputs = _all_f16_values_in_unit_interval().astype(np.float32)
    minimum, maximum = DISPLAY_RANGE

    samples = np.column_stack(
        (
            np.tile(inputs, len(CONTROL_AMOUNTS)),
            np.repeat(np.asarray(CONTROL_AMOUNTS, dtype=np.float32), inputs.size),
        )
    )
    evaluated = evaluate_math("display-black-toe-value", samples)[:, 0].reshape(
        len(CONTROL_AMOUNTS), inputs.size
    )
    for mapped, amount in zip(evaluated, CONTROL_AMOUNTS):
        assert np.all(np.isfinite(mapped)), amount
        assert np.all((mapped >= minimum) & (mapped <= maximum)), amount
        _assert_nondecreasing(mapped, tolerance=2e-7)

        stored = mapped.astype(np.float16)
        assert np.all(np.isfinite(stored)), amount
        assert np.all(stored.astype(np.float64) <= HALF_FLOAT_MAX), amount
        _assert_nondecreasing(stored)


def test_black_toe_region_joins_are_c0_and_c1_continuous() -> None:
    step = 1e-5
    joins = (*BLACK_TOE_DEEP_REGION, *BLACK_TOE_HDR_GUARD)
    rows: list[list[float]] = []
    cases: list[tuple[float, float]] = []
    for amount in (-100.0, -50.0, -25.0, 25.0, 50.0, 100.0):
        for join in joins:
            rows.extend(
                [
                    [join - step, amount],
                    [join, amount],
                    [join + step, amount],
                ]
            )
            cases.append((amount, join))

    evaluated = evaluate_math("display-black-toe-value", rows)[:, 0].reshape(-1, 3)
    for (left_value, center, right_value), case in zip(evaluated, cases):
        assert abs(float(right_value - left_value)) < 1e-3 * max(1.0, abs(float(center))), case
        left_derivative = float((center - left_value) / step)
        right_derivative = float((right_value - center) / step)
        assert math.isclose(
            left_derivative,
            right_derivative,
            rel_tol=2e-2,
            abs_tol=6e-3,
        ), (case, left_derivative, right_derivative)


def test_black_toe_is_identity_above_the_hdr_guard() -> None:
    rows = [
        [luma, amount]
        for amount in CONTROL_AMOUNTS
        for luma in (1.0, 2.0, 4.0, 16.0)
    ]
    outputs = evaluate_math("display-black-toe-value", rows)[:, 0]
    expected = np.asarray([row[0] for row in rows], dtype=np.float32)
    assert np.array_equal(outputs, expected)
