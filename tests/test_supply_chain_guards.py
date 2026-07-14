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
    assert "invalid checksum" in result.stderr


def test_verified_download_accepts_sha512_digest_for_cached_file(tmp_path) -> None:
    import hashlib

    output = tmp_path / "cached-download"
    output.write_bytes(b"verified payload")
    digest = hashlib.sha512(output.read_bytes()).hexdigest()

    result = subprocess.run(
        [
            "sh",
            str(ROOT / "scripts/verified-download.sh"),
            "https://example.invalid/file",
            str(output),
            f"sha512:{digest}",
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0
    assert "verified cached download" in result.stdout
