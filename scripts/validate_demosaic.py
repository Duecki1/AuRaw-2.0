#!/usr/bin/env python3
"""Run compiler-backed and numerical demosaic validation through Rust tests."""
from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TEST_FILTERS = (
    "compute_shaders_parse_and_validate",
    "demosaic_contracts_are_compiler_validated",
    "demosaic_shaders_expose_every_dispatched_entry_point",
    "inpaint_opposed",
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
        parser.error("cargo is required because demosaic validation parses and validates WGSL with Naga")

    failed = [name for name in TEST_FILTERS if not run_test(name, release=args.release)]
    if failed:
        print("Failed demosaic test filters:", ", ".join(failed), file=sys.stderr)
        return 1
    print(f"All {len(TEST_FILTERS)} compiler-backed demosaic test groups passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
