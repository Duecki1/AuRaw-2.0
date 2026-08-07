"""Executable regression gates for AuRaw-owned color-science math.

All values are evaluated by ``auraw-regression-render math-eval``. The tests
therefore exercise the canonical Rust math shared with the pipeline instead of
maintaining independent Python translations of shader formulas.
"""

from __future__ import annotations

import numpy as np

from tests.auraw_math_eval import evaluate_math


def test_camera_space_opponent_basis_is_exactly_reversible() -> None:
    samples = np.asarray(
        [
            (0.0, 0.0, 0.0),
            (0.1, 0.2, 0.3),
            (-0.05, 0.12, 0.9),
            (1.4, 0.4, -0.2),
        ],
        dtype=np.float32,
    )
    reconstructed = evaluate_math("camera-opponent-roundtrip", samples)[:, :3]
    assert np.allclose(reconstructed, samples, rtol=0.0, atol=2e-7)


def test_rec2020_oklab_basis_is_reversible() -> None:
    samples = np.asarray(
        [
            (0.0, 0.0, 0.0),
            (0.1, 0.2, 0.3),
            (-0.05, 0.12, 0.9),
            (1.4, 0.4, -0.2),
        ],
        dtype=np.float32,
    )
    lab = evaluate_math("rec2020-to-oklab", samples)[:, :3]
    reconstructed = evaluate_math("oklab-to-rec2020", lab)[:, :3]
    assert np.allclose(reconstructed, samples, rtol=2e-5, atol=2e-5)


def test_black_toe_is_continuous_and_monotone_above_zero() -> None:
    y = np.arange(1, 100_001, dtype=np.float32) / np.float32(100_000.0)
    amounts = np.asarray((-1.0, -0.5, 0.5, 1.0), dtype=np.float32)
    samples = np.column_stack(
        (
            np.tile(y, amounts.size),
            np.repeat(amounts, y.size),
        )
    )
    mapped = evaluate_math("display-black-toe-amount", samples)[:, 0].reshape(
        amounts.size, y.size
    )
    for curve in mapped:
        assert np.all(np.diff(curve.astype(np.float64)) >= 0.0)

    epsilon_samples = np.column_stack(
        (np.full(amounts.shape, 1e-12, dtype=np.float32), amounts)
    )
    epsilon_output = evaluate_math("display-black-toe-amount", epsilon_samples)[:, 0]
    assert np.all(np.abs(epsilon_output.astype(np.float64)) < 1e-8)
