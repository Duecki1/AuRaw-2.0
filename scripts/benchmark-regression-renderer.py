#!/usr/bin/env python3
"""Repeatable wall-clock benchmark for AuRaw's canonical GPU renderer."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import shlex
import statistics
import subprocess
import time

SCENES = {
    "synthetic-bayer-multitarget": ("synthetic-bayer.dng", 256, 256),
    "synthetic-xtrans-multitarget": ("synthetic-xtrans.dng", 256, 256),
}


def percentile_95(values: list[float]) -> float:
    ordered = sorted(values)
    return ordered[max(0, int(len(ordered) * 0.95) - 1)]


def render_command(renderer: Path, source: Path, target: Path) -> list[str]:
    return [
        str(renderer),
        "--backend",
        "gpu",
        "--input",
        str(source),
        "--output",
        str(target),
    ]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--renderer",
        type=Path,
        default=Path("target/release/auraw-regression-render"),
    )
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument(
        "--output", type=Path, default=Path("target/benchmark-report.json")
    )
    parser.add_argument(
        "--budget-file", type=Path, default=Path("benchmarks/gpu-budget.json")
    )
    parser.add_argument(
        "--enforce-budget",
        action="store_true",
        help="exit non-zero when the versioned startup/throughput guardrails fail",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="validate the committed scenes and print renderer commands without executing them",
    )
    args = parser.parse_args()
    if args.runs < 1:
        parser.error("--runs must be positive")

    root = Path(__file__).resolve().parents[1]
    renderer = args.renderer if args.renderer.is_absolute() else root / args.renderer
    output = args.output if args.output.is_absolute() else root / args.output
    budget_file = (
        args.budget_file if args.budget_file.is_absolute() else root / args.budget_file
    )

    scene_inputs: dict[str, tuple[Path, int, int]] = {}
    for scene, (filename, width, height) in SCENES.items():
        source = root / "regression/raw" / filename
        if not source.is_file():
            parser.error(f"committed benchmark scene is missing: {source}")
        scene_inputs[scene] = (source, width, height)

    if args.dry_run:
        for scene, (source, _, _) in scene_inputs.items():
            target = root / "target/benchmarks" / f"{scene}-1.npz"
            print(shlex.join(render_command(renderer, source, target)))
        return 0

    if not renderer.is_file():
        parser.error(f"renderer does not exist: {renderer}")

    measured: dict[str, list[float]] = {scene: [] for scene in SCENES}
    warmups: dict[str, float] = {}
    for scene, (source, _, _) in scene_inputs.items():
        for run in range(args.runs + 1):
            target = root / "target/benchmarks" / f"{scene}-{run}.npz"
            target.parent.mkdir(parents=True, exist_ok=True)
            started = time.perf_counter()
            subprocess.run(render_command(renderer, source, target), check=True)
            elapsed_ms = (time.perf_counter() - started) * 1000.0
            if run == 0:
                warmups[scene] = elapsed_ms
            else:
                measured[scene].append(elapsed_ms)

    scene_reports: dict[str, dict[str, object]] = {}
    for scene, times in measured.items():
        _, width, height = scene_inputs[scene]
        megapixels = width * height / 1_000_000.0
        median_ms = statistics.median(times)
        scene_reports[scene] = {
            "width": width,
            "height": height,
            "megapixels": megapixels,
            "warmup_ms": warmups[scene],
            "times_ms": times,
            "median_ms": median_ms,
            "p95_ms": percentile_95(times),
            "median_megapixels_per_second": megapixels / (median_ms / 1000.0),
        }

    budget = json.loads(budget_file.read_text(encoding="utf-8"))
    minimum_throughput = float(budget["budgets"]["export_mp_per_second_min"])
    maximum_startup = float(budget["budgets"]["startup_shader_compile_p95_ms"])
    throughput_pass = all(
        float(scene["median_megapixels_per_second"]) >= minimum_throughput
        for scene in scene_reports.values()
    )
    startup_pass = max(warmups.values()) <= maximum_startup
    budget_result = {
        "budget_file": str(budget_file.relative_to(root)),
        "export_throughput_pass": throughput_pass,
        "startup_pass": startup_pass,
        "passed": throughput_pass and startup_pass,
    }

    report = {
        "schema": 2,
        "renderer": str(renderer),
        "runs": args.runs,
        "scenes": scene_reports,
        "budget": budget_result,
        "measurement_scope": (
            "wall-clock process startup plus canonical GPU render/readback; "
            "use native GPU timestamp queries for per-pass diagnosis"
        ),
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(output)
    if args.enforce_budget and not budget_result["passed"]:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
