from __future__ import annotations

from pathlib import Path
import hashlib
import os
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]


def test_all_workflow_actions_are_immutable_commit_pins() -> None:
    subprocess.run(
        [sys.executable, str(ROOT / "scripts/dev.py"), "check-workflows"],
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


def test_verified_download_reports_expected_and_actual_digest(tmp_path) -> None:
    payload = tmp_path / "payload"
    payload.write_bytes(b"downloaded but not trusted")

    fake_bin = tmp_path / "bin"
    fake_bin.mkdir()
    fake_curl = fake_bin / "curl"
    fake_curl.write_text(
        """#!/bin/sh
set -eu
output=
while [ \"$#\" -gt 0 ]; do
    case \"$1\" in
        -o)
            output=$2
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done
[ -n \"$output\" ]
cp \"$FAKE_CURL_PAYLOAD\" \"$output\"
"""
    )
    fake_curl.chmod(0o755)

    output = tmp_path / "download"
    expected = "0" * 64
    actual = hashlib.sha256(payload.read_bytes()).hexdigest()
    env = os.environ.copy()
    env["PATH"] = f"{fake_bin}:{env['PATH']}"
    env["FAKE_CURL_PAYLOAD"] = str(payload)

    result = subprocess.run(
        [
            "sh",
            str(ROOT / "scripts/verified-download.sh"),
            "https://example.invalid/file",
            str(output),
            expected,
        ],
        cwd=ROOT,
        capture_output=True,
        text=True,
        env=env,
    )

    assert result.returncode == 1
    assert "sha256 checksum mismatch" in result.stderr
    assert f"expected: {expected}" in result.stderr
    assert f"actual:   {actual}" in result.stderr
    assert not output.exists()
