"""Executable mathematical references for selected processing policies.

The formulas here are independent Python references, not executions of the Rust
or WGSL implementations. They can catch policy regressions but cannot detect
shader drift; Naga compilation and rendered GPU comparisons remain required.
"""

from __future__ import annotations

import math


def camera_opponent(rgb: tuple[float, float, float]) -> tuple[float, float, float]:
    r, g, b = rgb
    signal = 0.25 * r + 0.50 * g + 0.25 * b
    return signal, r - g, b - g


def camera_opponent_inverse(values: tuple[float, float, float]) -> tuple[float, float, float]:
    signal, rg, bg = values
    g = signal - 0.25 * (rg + bg)
    return g + rg, g, g + bg


def display_black_toe(y: float, amount: float) -> float:
    if y <= 0.0 or abs(amount) < 1e-7:
        return y
    amount = max(-1.0, min(1.0, amount))
    guard_t = max(0.0, min(1.0, (y - 0.35) / 0.65))
    hdr_guard = 1.0 - guard_t * guard_t * (3.0 - 2.0 * guard_t)
    if amount >= 0.0:
        offset = amount * 1.75 * (0.08 + 0.92 * 2.0 ** (-y / 0.035)) * hdr_guard
    else:
        deep_t = max(0.0, min(1.0, (y - 0.012) / 0.018))
        deep = 1.0 - deep_t * deep_t * (3.0 - 2.0 * deep_t)
        tail = 0.10 + 2.35 * 2.0 ** (-y / 0.070)
        offset = -(-amount) * (10.50 * deep + tail) * hdr_guard
    return y * 2.0**offset


def test_camera_space_opponent_basis_is_exactly_reversible() -> None:
    samples = [
        (0.0, 0.0, 0.0),
        (0.1, 0.2, 0.3),
        (-0.05, 0.12, 0.9),
        (1.4, 0.4, -0.2),
    ]
    for sample in samples:
        reconstructed = camera_opponent_inverse(camera_opponent(sample))
        assert all(math.isclose(a, b, abs_tol=1e-12) for a, b in zip(sample, reconstructed))


def test_black_toe_is_continuous_and_monotone_above_zero() -> None:
    for amount in (-1.0, -0.5, 0.5, 1.0):
        previous = 0.0
        for i in range(1, 100_001):
            y = i / 100_000
            mapped = display_black_toe(y, amount)
            assert mapped >= previous - 1e-12
            previous = mapped
        epsilon = 1e-12
        assert abs(display_black_toe(epsilon, amount) - 0.0) < 1e-8
