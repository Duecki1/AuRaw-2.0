from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
GRADLEW = ROOT / ("gradlew.bat" if os.name == "nt" else "gradlew")
TEST_CLASS = "de.duecki.auraw.AndroidStorageContractTest"


def test_android_storage_contract_behaves_correctly() -> None:
    if shutil.which("java") is None:
        pytest.skip("a JDK is required for Android storage contract tests")
    if not GRADLEW.is_file():
        pytest.fail(f"Gradle wrapper is missing: {GRADLEW}")

    completed = subprocess.run(
        [
            str(GRADLEW),
            ":app:testDebugUnitTest",
            "--tests",
            TEST_CLASS,
            "-PaurawBuildRust=false",
            "--no-daemon",
            "--console=plain",
        ],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
