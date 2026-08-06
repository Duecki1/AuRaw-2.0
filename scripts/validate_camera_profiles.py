#!/usr/bin/env python3
"""Run compiler-backed and numerical camera-profile validation through Rust tests."""
from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TEST_FILTERS = (
    "pipeline::color_profile::tests",
    "pipeline::color_profile::dcp::tests",
    "pipeline::color_profile::icc::tests",
    "pipeline::sigmoid::tests",
    "gpu_params_follow_the_wgsl_uniform_layout",
    "profile_shader_parses_with_the_profile_storage_contract",
    "adjustments_shader_exposes_darktable_sigmoid_paths",
    "scene_graph_preserves_native_call_order_and_stage_ownership",
    "global_wb_changes_raw_multipliers_without_changing_the_camera_transform",
)


def run_test(test_filter: str, *, release: bool) -> bool:
    command = ["cargo", "test", "--locked", "--lib"]
    if release:
        command.append("--release")
    command += [test_filter, "--", "--nocapture"]
    completed = subprocess.run(command, cwd=ROOT, check=False)
    return completed.returncode == 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release", action="store_true", help="run the selected tests in release mode")
    args = parser.parse_args()
    if shutil.which("cargo") is None:
        parser.error("cargo is required because profile validation compiles Rust and validates WGSL with Naga")

    failed = [name for name in TEST_FILTERS if not run_test(name, release=args.release)]
    if failed:
        print("Failed camera-profile test filters:", ", ".join(failed), file=sys.stderr)
        return 1
    print(f"All {len(TEST_FILTERS)} compiler-backed camera-profile test groups passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
