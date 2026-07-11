from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time
from typing import Sequence

import numpy as np

from .io import LinearImage, load_linear_image, save_linear_image
from .manifest import (
    file_sha256,
    load_manifest,
    load_thresholds,
    thresholds_for_scene,
    validate_manifest,
)
from .metrics import compare_images
from .report import SceneResult, evaluate_thresholds, write_html, write_json, write_junit


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="AuRaw image-quality regression framework")
    sub = parser.add_subparsers(dest="command", required=True)

    validate = sub.add_parser("validate-corpus", help="validate corpus metadata and coverage")
    validate.add_argument("--manifest", type=Path, required=True)
    validate.add_argument("--verify-files", action="store_true")
    validate.add_argument("--allow-incomplete-coverage", action="store_true")

    normalize = sub.add_parser("normalize", help="convert an export/readback into canonical NPZ")
    normalize.add_argument("input", type=Path)
    normalize.add_argument("output", type=Path)
    normalize.add_argument(
        "--color-space",
        choices=["linear-srgb-d65", "linear-rec2020-d65", "camera-rgb"],
        required=True,
    )
    normalize.add_argument("--transfer", choices=["linear", "srgb"], default="linear")
    normalize.add_argument("--metadata", action="append", default=[], metavar="KEY=VALUE")

    normalize_corpus = sub.add_parser(
        "normalize-corpus", help="normalize one export per enabled corpus scene"
    )
    normalize_corpus.add_argument("--manifest", type=Path, required=True)
    normalize_corpus.add_argument("--input-root", type=Path, required=True)
    normalize_corpus.add_argument("--output-root", type=Path, required=True)
    normalize_corpus.add_argument("--extension", default=".tif")
    normalize_corpus.add_argument("--transfer", choices=["linear", "srgb"], default="linear")
    normalize_corpus.add_argument("--metadata", action="append", default=[], metavar="KEY=VALUE")
    normalize_corpus.add_argument("--scene", action="append", default=[])

    compare = sub.add_parser("compare", help="compare a backend against pinned references")
    compare.add_argument("--manifest", type=Path, required=True)
    compare.add_argument("--thresholds", type=Path, required=True)
    compare.add_argument("--reference-root", type=Path, required=True)
    compare.add_argument("--candidate-root", type=Path, required=True)
    compare.add_argument("--backend", choices=["cpu", "gpu"], required=True)
    compare.add_argument("--reference-engine", default="darktable")
    compare.add_argument("--border", type=int, default=18)
    compare.add_argument("--report-dir", type=Path, required=True)
    compare.add_argument("--scene", action="append", default=[])

    deterministic = sub.add_parser("determinism", help="compare two renders of the same backend")
    deterministic.add_argument("--manifest", type=Path, required=True)
    deterministic.add_argument("--run-a", type=Path, required=True)
    deterministic.add_argument("--run-b", type=Path, required=True)
    deterministic.add_argument("--backend", choices=["cpu", "gpu"], required=True)
    deterministic.add_argument("--max-abs", type=float, required=True)
    deterministic.add_argument("--report", type=Path, required=True)

    backend_pair = sub.add_parser("cpu-gpu", help="compare CPU and GPU outputs directly")
    backend_pair.add_argument("--manifest", type=Path, required=True)
    backend_pair.add_argument("--cpu-root", type=Path, required=True)
    backend_pair.add_argument("--gpu-root", type=Path, required=True)
    backend_pair.add_argument("--thresholds", type=Path, required=True)
    backend_pair.add_argument("--border", type=int, default=18)
    backend_pair.add_argument("--report-dir", type=Path, required=True)

    render = sub.add_parser("render", help="run a deterministic renderer command for each RAW")
    render.add_argument("--manifest", type=Path, required=True)
    render.add_argument("--backend", choices=["cpu", "gpu", "darktable", "ansel"], required=True)
    render.add_argument("--command-template", required=True)
    render.add_argument("--output-root", type=Path, required=True)
    render.add_argument("--extension", default=".npz")
    render.add_argument("--repeat", type=int, default=1)
    render.add_argument("--version-command")
    render.add_argument("--scene", action="append", default=[])

    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "validate-corpus":
            return _validate_corpus(args)
        if args.command == "normalize":
            return _normalize(args)
        if args.command == "normalize-corpus":
            return _normalize_corpus(args)
        if args.command == "compare":
            return _compare(args)
        if args.command == "determinism":
            return _determinism(args)
        if args.command == "cpu-gpu":
            return _cpu_gpu(args)
        if args.command == "render":
            return _render(args)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    raise AssertionError(args.command)


def _validate_corpus(args: argparse.Namespace) -> int:
    manifest = load_manifest(args.manifest)
    errors = validate_manifest(
        manifest,
        verify_files=args.verify_files,
        require_coverage=not args.allow_incomplete_coverage,
    )
    if errors:
        for error in errors:
            print(f"FAIL: {error}")
        return 1
    enabled = sum(scene.enabled for scene in manifest.scenes)
    print(f"PASS: {enabled} enabled scenes; color space {manifest.color_space}")
    return 0


def _normalize(args: argparse.Namespace) -> int:
    metadata = _parse_metadata(args.metadata)
    loaded = load_linear_image(
        args.input, color_space=args.color_space, transfer=args.transfer
    )
    image = LinearImage(
        loaded.rgb,
        loaded.color_space,
        loaded.metadata | metadata | {"source_sha256": file_sha256(args.input)},
        loaded.valid_mask,
    )
    save_linear_image(args.output, image)
    print(f"wrote {args.output} ({image.rgb.shape[1]}x{image.rgb.shape[0]})")
    return 0


def _normalize_corpus(args: argparse.Namespace) -> int:
    manifest = load_manifest(args.manifest)
    selected = set(args.scene)
    metadata = _parse_metadata(args.metadata)
    extension = args.extension if args.extension.startswith(".") else "." + args.extension
    count = 0
    for scene in manifest.scenes:
        if not scene.enabled or selected and scene.scene_id not in selected:
            continue
        source = args.input_root / f"{scene.scene_id}{extension}"
        target = args.output_root / f"{scene.scene_id}.npz"
        loaded = load_linear_image(
            source, color_space=manifest.color_space, transfer=args.transfer
        )
        image = LinearImage(
            loaded.rgb,
            loaded.color_space,
            loaded.metadata
            | metadata
            | {
                "scene_id": scene.scene_id,
                "source_sha256": file_sha256(source),
                "raw_sha256": scene.sha256,
            },
            loaded.valid_mask,
        )
        save_linear_image(target, image)
        count += 1
        print(f"normalized {scene.scene_id} -> {target}")
    if count == 0:
        raise ValueError("no enabled scenes selected")
    return 0


def _compare(args: argparse.Namespace) -> int:
    manifest = load_manifest(args.manifest)
    errors = validate_manifest(manifest, require_coverage=False)
    if errors:
        raise ValueError("invalid manifest: " + "; ".join(errors))
    threshold_config = load_thresholds(args.thresholds)
    selected = set(args.scene)
    results: list[SceneResult] = []
    for scene in manifest.scenes:
        if not scene.enabled or selected and scene.scene_id not in selected:
            continue
        reference_path = args.reference_root / f"{scene.scene_id}.npz"
        candidate_path = args.candidate_root / f"{scene.scene_id}.npz"
        reference = load_linear_image(reference_path, color_space=manifest.color_space)
        candidate = load_linear_image(candidate_path, color_space=manifest.color_space)
        metrics = compare_images(reference, candidate, rois=scene.rois, border=args.border)
        thresholds = thresholds_for_scene(scene, threshold_config, args.backend)
        failures = evaluate_thresholds(metrics, thresholds)
        result = SceneResult(
            scene.scene_id,
            args.backend,
            str(reference_path),
            str(candidate_path),
            metrics,
            thresholds,
            failures,
        )
        results.append(result)
        print(f"{'PASS' if result.passed else 'FAIL'} {scene.scene_id}")
        for failure in failures:
            print(f"  {failure}")
    if not results:
        raise ValueError("no enabled scenes selected")
    metadata = {
        "backend": args.backend,
        "reference_engine": args.reference_engine,
        "manifest": str(args.manifest),
        "thresholds": str(args.thresholds),
        "border": args.border,
    }
    _write_reports(args.report_dir, results, metadata)
    return 0 if all(result.passed for result in results) else 1


def _determinism(args: argparse.Namespace) -> int:
    manifest = load_manifest(args.manifest)
    failures: list[dict[str, object]] = []
    rows: list[dict[str, object]] = []
    for scene in manifest.scenes:
        if not scene.enabled:
            continue
        path_a = args.run_a / f"{scene.scene_id}.npz"
        path_b = args.run_b / f"{scene.scene_id}.npz"
        a = load_linear_image(path_a, color_space=manifest.color_space)
        b = load_linear_image(path_b, color_space=manifest.color_space)
        if a.rgb.shape != b.rgb.shape:
            row = {"scene": scene.scene_id, "failure": "shape mismatch"}
            failures.append(row)
            rows.append(row)
            continue
        diff = np.abs(np.asarray(a.rgb, dtype=np.float64) - np.asarray(b.rgb, dtype=np.float64))
        max_abs = float(np.max(diff))
        bit_exact = bool(np.array_equal(a.rgb, b.rgb))
        row = {"scene": scene.scene_id, "max_abs": max_abs, "bit_exact": bit_exact}
        rows.append(row)
        if max_abs > args.max_abs:
            failures.append(row)
            print(f"FAIL {scene.scene_id}: max_abs={max_abs:.8g} > {args.max_abs:.8g}")
        else:
            print(f"PASS {scene.scene_id}: max_abs={max_abs:.8g}; bit_exact={bit_exact}")
    payload = {
        "schema": 1,
        "backend": args.backend,
        "max_abs_limit": args.max_abs,
        "passed": not failures,
        "results": rows,
    }
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0 if not failures else 1


def _cpu_gpu(args: argparse.Namespace) -> int:
    manifest = load_manifest(args.manifest)
    threshold_config = load_thresholds(args.thresholds)
    results: list[SceneResult] = []
    for scene in manifest.scenes:
        if not scene.enabled:
            continue
        cpu_path = args.cpu_root / f"{scene.scene_id}.npz"
        gpu_path = args.gpu_root / f"{scene.scene_id}.npz"
        cpu = load_linear_image(cpu_path, color_space=manifest.color_space)
        gpu = load_linear_image(gpu_path, color_space=manifest.color_space)
        metrics = compare_images(cpu, gpu, rois=scene.rois, border=args.border)
        thresholds = {
            str(k): float(v)
            for k, v in threshold_config.get("cpu_gpu", {}).items()
        }
        failures = evaluate_thresholds(metrics, thresholds)
        results.append(
            SceneResult(
                scene.scene_id,
                "cpu-gpu",
                str(cpu_path),
                str(gpu_path),
                metrics,
                thresholds,
                failures,
            )
        )
        print(f"{'PASS' if not failures else 'FAIL'} {scene.scene_id}")
    if not results:
        raise ValueError("no enabled scenes")
    _write_reports(
        args.report_dir,
        results,
        {"backend": "cpu-gpu", "manifest": str(args.manifest), "border": args.border},
    )
    return 0 if all(result.passed for result in results) else 1


def _render(args: argparse.Namespace) -> int:
    manifest = load_manifest(args.manifest)
    selected = set(args.scene)
    version = ""
    if args.version_command:
        completed = subprocess.run(
            shlex.split(args.version_command),
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        version = completed.stdout.strip()
    environment = os.environ.copy()
    environment.setdefault("LC_ALL", "C")
    environment.setdefault("TZ", "UTC")
    environment.setdefault("RAYON_NUM_THREADS", "1")
    environment.setdefault("OMP_NUM_THREADS", "1")
    environment.setdefault("OPENBLAS_NUM_THREADS", "1")
    environment["AURAW_REGRESSION_BACKEND"] = args.backend
    environment["AURAW_REGRESSION_SEED"] = "0"

    metadata_rows: list[dict[str, object]] = []
    for repeat in range(args.repeat):
        root = args.output_root if args.repeat == 1 else args.output_root / f"run-{repeat + 1}"
        root.mkdir(parents=True, exist_ok=True)
        for scene in manifest.scenes:
            if not scene.enabled or selected and scene.scene_id not in selected:
                continue
            output = root / f"{scene.scene_id}{args.extension}"
            command = args.command_template.format(
                raw=shlex.quote(str(scene.raw)),
                output=shlex.quote(str(output)),
                scene=shlex.quote(scene.scene_id),
                backend=shlex.quote(args.backend),
                repeat=repeat + 1,
            )
            started = time.monotonic()
            subprocess.run(command, shell=True, check=True, env=environment)
            elapsed = time.monotonic() - started
            if not output.is_file():
                raise ValueError(f"renderer did not create {output}")
            metadata_rows.append(
                {
                    "scene": scene.scene_id,
                    "repeat": repeat + 1,
                    "output": str(output),
                    "sha256": file_sha256(output),
                    "elapsed_seconds": elapsed,
                }
            )
            print(f"rendered {scene.scene_id} repeat {repeat + 1} -> {output}")
    manifest_out = args.output_root / "render-manifest.json"
    manifest_out.parent.mkdir(parents=True, exist_ok=True)
    manifest_out.write_text(
        json.dumps(
            {
                "schema": 1,
                "backend": args.backend,
                "renderer_version": version,
                "command_template": args.command_template,
                "environment": {
                    key: environment[key]
                    for key in (
                        "LC_ALL",
                        "TZ",
                        "RAYON_NUM_THREADS",
                        "OMP_NUM_THREADS",
                        "OPENBLAS_NUM_THREADS",
                        "AURAW_REGRESSION_BACKEND",
                        "AURAW_REGRESSION_SEED",
                    )
                },
                "outputs": metadata_rows,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )
    return 0


def _write_reports(report_dir: Path, results: list[SceneResult], metadata: dict[str, object]) -> None:
    report_dir.mkdir(parents=True, exist_ok=True)
    write_json(report_dir / "report.json", results, metadata)
    write_junit(report_dir / "junit.xml", results)
    write_html(report_dir / "report.html", results, metadata)


def _parse_metadata(values: list[str]) -> dict[str, str]:
    result: dict[str, str] = {}
    for value in values:
        if "=" not in value:
            raise ValueError(f"metadata must be KEY=VALUE, got {value!r}")
        key, item = value.split("=", 1)
        if not key:
            raise ValueError("metadata key cannot be empty")
        result[key] = item
    return result


if __name__ == "__main__":
    raise SystemExit(main())
