"""Thin client for AuRaw's Rust color-science evaluator."""
from __future__ import annotations

from functools import lru_cache
import os
from pathlib import Path
import subprocess
from typing import Iterable

import numpy as np

ROOT = Path(__file__).resolve().parents[1]


@lru_cache(maxsize=1)
def _evaluator_binary() -> Path:
    override = os.environ.get("AURAW_REGRESSION_RENDER")
    if override:
        binary = Path(override)
        if not binary.is_absolute():
            binary = ROOT / binary
    else:
        subprocess.run(
            [
                "cargo",
                "build",
                "--quiet",
                "--locked",
                "-p",
                "auraw-cli",
                "--bin",
                "auraw-regression-render",
            ],
            cwd=ROOT,
            check=True,
        )
        target_root = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
        if not target_root.is_absolute():
            target_root = ROOT / target_root
        suffix = ".exe" if os.name == "nt" else ""
        binary = target_root / "debug" / f"auraw-regression-render{suffix}"

    if not binary.is_file():
        raise FileNotFoundError(f"AuRaw math evaluator not found: {binary}")
    return binary


def evaluate_math(
    operation: str,
    samples: Iterable[Iterable[float]] | np.ndarray,
) -> np.ndarray:
    """Evaluate f32x4 sample rows through ``auraw-regression-render math-eval``."""
    values = np.asarray(samples, dtype=np.float32)
    if values.ndim == 1:
        values = values.reshape(1, -1)
    if values.ndim != 2 or values.shape[1] > 4:
        raise ValueError("math-eval samples must be a two-dimensional array with <= 4 columns")
    if values.shape[1] < 4:
        values = np.pad(values, ((0, 0), (0, 4 - values.shape[1])))

    packed = np.asarray(values, dtype="<f4", order="C").tobytes()
    completed = subprocess.run(
        [
            str(_evaluator_binary()),
            "math-eval",
            "--operation",
            operation,
        ],
        cwd=ROOT,
        input=packed,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            "AuRaw math evaluator failed\n"
            f"stderr:\n{completed.stderr.decode('utf-8', errors='replace')}"
        )
    outputs = np.frombuffer(completed.stdout, dtype="<f4")
    if outputs.size != values.size:
        raise RuntimeError(
            f"math evaluator returned {outputs.size} floats, expected {values.size}"
        )
    return outputs.reshape(values.shape).astype(np.float32, copy=False)
