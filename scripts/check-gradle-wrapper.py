#!/usr/bin/env python3
"""Verify AuRaw's checked-in Gradle wrapper before executing it."""

from __future__ import annotations

import hashlib
from pathlib import Path
import re
import stat

ROOT = Path(__file__).resolve().parents[1]
PROPERTIES = ROOT / "gradle/wrapper/gradle-wrapper.properties"
WRAPPER_JAR = ROOT / "gradle/wrapper/gradle-wrapper.jar"
GRADLEW = ROOT / "gradlew"
GRADLEW_BAT = ROOT / "gradlew.bat"
EXPECTED_VERSION = "8.11.1"
EXPECTED_DISTRIBUTION_SHA256 = (
    "f397b287023acdba1e9f6fc5ea72d22dd63669d59ed4a289a29b1a76eee151c6"
)
EXPECTED_WRAPPER_JAR_SHA256 = (
    "2db75c40782f5e8ba1fc278a5574bab070adccb2d21ca5a6e5ed840888448046"
)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_properties(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for line_number, raw_line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        line = raw_line.strip()
        if not line or line.startswith(("#", "!")):
            continue
        if "=" not in line:
            raise ValueError(f"{path}:{line_number}: expected key=value")
        key, value = line.split("=", 1)
        values[key.strip()] = value.strip()
    return values


def validate() -> list[str]:
    errors: list[str] = []
    required_files = (PROPERTIES, WRAPPER_JAR, GRADLEW, GRADLEW_BAT)
    for path in required_files:
        if not path.is_file():
            errors.append(f"missing wrapper file: {path.relative_to(ROOT)}")
    if errors:
        return errors

    try:
        properties = parse_properties(PROPERTIES)
    except (OSError, UnicodeError, ValueError) as error:
        return [str(error)]

    distribution_url = properties.get("distributionUrl", "").replace("\\:", ":")
    expected_suffix = f"/gradle-{EXPECTED_VERSION}-bin.zip"
    if not distribution_url.startswith("https://services.gradle.org/distributions/"):
        errors.append("distributionUrl must use the official HTTPS Gradle distribution host")
    if not distribution_url.endswith(expected_suffix):
        errors.append(
            f"distributionUrl must select Gradle {EXPECTED_VERSION}; found {distribution_url or '<missing>'}"
        )
    if properties.get("distributionSha256Sum") != EXPECTED_DISTRIBUTION_SHA256:
        errors.append("distributionSha256Sum does not match the pinned Gradle distribution")
    if properties.get("validateDistributionUrl", "").lower() != "true":
        errors.append("validateDistributionUrl must remain enabled")

    actual_jar_sha256 = sha256(WRAPPER_JAR)
    if actual_jar_sha256 != EXPECTED_WRAPPER_JAR_SHA256:
        errors.append(
            "gradle-wrapper.jar checksum mismatch: "
            f"expected {EXPECTED_WRAPPER_JAR_SHA256}, found {actual_jar_sha256}"
        )

    if not GRADLEW.stat().st_mode & stat.S_IXUSR:
        errors.append("gradlew must be executable")
    shell_script = GRADLEW.read_text(encoding="utf-8")
    batch_script = GRADLEW_BAT.read_text(encoding="utf-8")
    wrapper_path = "gradle/wrapper/gradle-wrapper.jar"
    if wrapper_path not in shell_script.replace("$APP_HOME/", ""):
        errors.append("gradlew does not reference the checked-in wrapper JAR")
    if not re.search(r"gradle[\\/]wrapper[\\/]gradle-wrapper\.jar", batch_script, re.I):
        errors.append("gradlew.bat does not reference the checked-in wrapper JAR")
    return errors


def main() -> int:
    errors = validate()
    if errors:
        print("Gradle wrapper integrity validation failed:")
        for error in errors:
            print(f"  - {error}")
        return 1
    print(
        f"Gradle wrapper {EXPECTED_VERSION} integrity verified "
        f"({EXPECTED_WRAPPER_JAR_SHA256})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
