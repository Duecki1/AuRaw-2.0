from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def test_benchmark_dry_run_covers_the_committed_scenes() -> None:
    result = subprocess.run(
        [
            sys.executable,
            "scripts/dev.py",
            "bench",
            "--dry-run",
        ],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    assert "synthetic-bayer.dng" in result.stdout
    assert "synthetic-xtrans.dng" in result.stdout
    assert result.stdout.count("--backend gpu") == 6
    assert "--workgroup-size 8x8" in result.stdout
    assert "--workgroup-size 16x8" in result.stdout
    assert "--workgroup-size 16x16" in result.stdout


def test_versioned_gpu_budget_names_the_same_scenes() -> None:
    budget = json.loads((ROOT / "benchmarks/gpu-budget.json").read_text(encoding="utf-8"))
    assert budget["schema"] == 2
    assert budget["scenes"] == [
        "synthetic-bayer-multitarget",
        "synthetic-xtrans-multitarget",
    ]
    assert budget["budgets"]["export_mp_per_second_min"] > 0
    assert budget["workgroup_sizes"] == ["8x8", "16x8", "16x16"]
    assert budget["budgets"]["pipeline_create_p95_ms"] > 0
    assert budget["budgets"]["render_p95_ms"] > 0
