from __future__ import annotations

from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def test_all_workflow_actions_are_immutable_commit_pins() -> None:
    subprocess.run(
        [sys.executable, str(ROOT / "scripts/check-workflow-pins.py")],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )


def test_verified_download_rejects_non_hex_digest_before_network_access() -> None:
    result = subprocess.run(
        [
            "sh",
            str(ROOT / "scripts/verified-download.sh"),
            "https://example.invalid/file",
            str(ROOT / "target/invalid-download"),
            "0" * 63 + "z",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 2
    assert "invalid SHA-256" in result.stderr
