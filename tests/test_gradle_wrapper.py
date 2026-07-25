from __future__ import annotations

import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_checked_in_gradle_wrapper_integrity() -> None:
    completed = subprocess.run(
        [sys.executable, "scripts/check-gradle-wrapper.py"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert "Gradle wrapper 8.11.1 integrity verified" in completed.stdout


def test_android_ci_uses_checked_in_wrapper() -> None:
    workflow = (ROOT / ".gitea/workflows/build.yml").read_text(encoding="utf-8")
    assert "python3 scripts/check-gradle-wrapper.py" in workflow
    assert "./gradlew --version" in workflow
    assert "./gradlew \\\n            --no-daemon" in workflow
    assert "gradle-${GRADLE_VERSION}" not in workflow
    assert "GRADLE_VERSION=" not in workflow
