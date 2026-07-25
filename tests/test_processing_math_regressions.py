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
    if y <= 0.0 or y >= 0.15 or abs(amount) < 1e-7:
        return y
    x = y / 0.15
    toe = (1.0 - x) ** 2
    endpoint = 2.60 if amount >= 0.0 else 3.10
    return y * 2.0 ** (max(-1.0, min(1.0, amount)) * endpoint * toe)


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
            y = 0.15 * i / 100_000
            mapped = display_black_toe(y, amount)
            assert mapped >= previous - 1e-12
            previous = mapped
        epsilon = 1e-12
        assert abs(display_black_toe(epsilon, amount) - 0.0) < 1e-8
