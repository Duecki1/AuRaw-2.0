#!/usr/bin/env python3
"""Pre-Rust HTTPS download helper for CI bootstrap only."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import shutil
import ssl
import stat
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from typing import NoReturn


class HttpsOnlyRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Reject redirects that downgrade a verified download to HTTP."""

    def redirect_request(self, request, file_pointer, code, message, headers, new_url):
        if not new_url.startswith("https://"):
            raise urllib.error.URLError(f"refusing non-HTTPS redirect: {new_url}")
        return super().redirect_request(
            request, file_pointer, code, message, headers, new_url
        )


def fail(message: str, exit_code: int = 1) -> NoReturn:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(exit_code)


def download_https(
    url: str,
    destination: Path,
    *,
    attempts: int,
    timeout: float,
    retry_delay: float = 3.0,
) -> None:
    if not url.startswith("https://"):
        fail(f"refusing non-HTTPS download: {url}", 2)

    context = ssl.create_default_context()
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    opener = urllib.request.build_opener(
        HttpsOnlyRedirectHandler(),
        urllib.request.HTTPSHandler(context=context),
    )
    request = urllib.request.Request(url, headers={"User-Agent": "CalibRaw-bootstrap/1"})

    last_error: Exception | None = None
    curl = shutil.which("curl")
    urllib_attempts = 1 if curl is not None else attempts
    urllib_timeout = min(timeout, 5.0) if curl is not None else timeout
    for attempt in range(urllib_attempts):
        try:
            with (
                opener.open(request, timeout=min(urllib_timeout, 30.0)) as response,
                destination.open("wb") as output,
            ):
                if not response.geturl().startswith("https://"):
                    fail(f"refusing non-HTTPS redirect: {response.geturl()}", 2)
                shutil.copyfileobj(response, output)
            return
        except (OSError, urllib.error.URLError) as error:
            last_error = error
            destination.unlink(missing_ok=True)
            if attempt + 1 < urllib_attempts:
                time.sleep(retry_delay)

    if curl is not None:
        completed = subprocess.run(
            [
                curl,
                "--proto",
                "=https",
                "--tlsv1.2",
                "--http1.1",
                "--fail",
                "--location",
                "--show-error",
                "--retry",
                str(max(attempts - 1, 0)),
                "--retry-all-errors",
                "--retry-delay",
                str(int(retry_delay)),
                "--connect-timeout",
                "30",
                "--max-time",
                str(max(1, int(timeout))),
                url,
                "-o",
                destination,
            ],
            check=False,
        )
        if completed.returncode == 0:
            return
        destination.unlink(missing_ok=True)

    fail(f"download failed for {url}: {last_error or 'unknown error'}")


def parse_expected_digest(source: str) -> tuple[str, str]:
    if source.startswith("https://"):
        with tempfile.TemporaryDirectory(prefix="calibraw-checksum-") as temporary:
            checksum_file = Path(temporary) / "checksum.txt"
            download_https(source, checksum_file, attempts=9, timeout=300)
            try:
                checksum_text = checksum_file.read_text(encoding="utf-8")
            except (OSError, UnicodeError) as error:
                fail(f"cannot read checksum response from {source}: {error}")
        match = re.search(r"[0-9a-fA-F]{64}", checksum_text)
        algorithm = "sha256"
        expected = match.group(0) if match else ""
    elif source.startswith("sha256:"):
        algorithm = "sha256"
        expected = source.removeprefix("sha256:")
    elif source.startswith("sha512:"):
        algorithm = "sha512"
        expected = source.removeprefix("sha512:")
    else:
        algorithm = "sha256"
        expected = source

    if not expected or not re.fullmatch(r"[0-9a-fA-F]+", expected):
        fail("invalid checksum value", 2)
    required_length = 64 if algorithm == "sha256" else 128
    if len(expected) != required_length:
        label = "SHA-256" if algorithm == "sha256" else "SHA-512"
        fail(f"{label} must contain {required_length} hex digits", 2)
    return algorithm, expected.lower()


def digest(path: Path, algorithm: str) -> str:
    value = hashlib.new(algorithm)
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        fail("usage: bootstrap_download.py URL OUTPUT EXPECTED-DIGEST", 2)
    url, output_arg, expected_source = argv
    if not url.startswith("https://"):
        fail(f"refusing non-HTTPS download: {url}", 2)
    algorithm, expected = parse_expected_digest(expected_source)
    output = Path(output_arg)
    if output.is_file() and digest(output, algorithm) == expected:
        print(f"verified cached download: {output}")
        return 0

    temporary = output.with_name(f"{output.name}.download.{os.getpid()}")
    previous_mode: int | None = None
    try:
        if output.exists():
            previous_mode = stat.S_IMODE(output.stat().st_mode)
        temporary.unlink(missing_ok=True)
        download_https(url, temporary, attempts=9, timeout=900)
        actual = digest(temporary, algorithm)
        if actual != expected:
            print(f"{algorithm} checksum mismatch for {temporary}", file=sys.stderr)
            print(f"expected: {expected}", file=sys.stderr)
            print(f"actual:   {actual}", file=sys.stderr)
            return 1
        if previous_mode is not None:
            temporary.chmod(previous_mode)
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
