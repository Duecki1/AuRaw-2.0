from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import subprocess
from typing import Any

import yaml

from .manifest import file_sha256


@dataclass(frozen=True)
class ReferenceEngine:
    name: str
    version: str
    source_revision: str
    source_sha256: str | None
    version_command: tuple[str, ...]
    version_output_contains: str
    history: Path
    history_sha256: str


@dataclass(frozen=True)
class ReferenceEngines:
    path: Path
    engines: dict[str, ReferenceEngine]


def load_reference_engines(path: Path | str) -> ReferenceEngines:
    config_path = Path(path).resolve()
    data = yaml.safe_load(config_path.read_text(encoding="utf-8"))
    if not isinstance(data, dict) or int(data.get("schema", 0)) != 1:
        raise ValueError("reference-engine file schema must be 1")
    raw_engines = data.get("engines")
    if not isinstance(raw_engines, dict) or not raw_engines:
        raise ValueError("reference-engine file must contain a non-empty engines mapping")

    engines: dict[str, ReferenceEngine] = {}
    for name, raw in raw_engines.items():
        if not isinstance(raw, dict):
            raise ValueError(f"reference engine {name!r} must be a mapping")
        command = raw.get("version_command", [])
        if not isinstance(command, list) or not command or not all(
            isinstance(value, str) and value for value in command
        ):
            raise ValueError(f"reference engine {name!r} version_command must be a string list")
        source_sha256 = raw.get("source_sha256")
        if source_sha256 is not None:
            source_sha256 = str(source_sha256).lower()
        history = config_path.parent / str(raw.get("history", ""))
        engine = ReferenceEngine(
            name=str(name),
            version=str(raw.get("version", "")).strip(),
            source_revision=str(raw.get("source_revision", "")).strip(),
            source_sha256=source_sha256,
            version_command=tuple(command),
            version_output_contains=str(raw.get("version_output_contains", "")).strip(),
            history=history.resolve(),
            history_sha256=str(raw.get("history_sha256", "")).lower(),
        )
        engines[engine.name] = engine
    return ReferenceEngines(config_path, engines)


def validate_reference_engines(
    config: ReferenceEngines,
    *,
    verify_binaries: bool = False,
) -> list[str]:
    errors: list[str] = []
    for name, engine in sorted(config.engines.items()):
        if not engine.version:
            errors.append(f"{name}: version is required")
        if not engine.source_revision:
            errors.append(f"{name}: source_revision is required")
        if engine.source_sha256 is not None and not _is_sha256(engine.source_sha256):
            errors.append(f"{name}: source_sha256 must be null or 64 lowercase hex characters")
        if not _is_sha256(engine.history_sha256):
            errors.append(f"{name}: history_sha256 must be 64 lowercase hex characters")
        if not engine.history.is_file():
            errors.append(f"{name}: processing history missing: {engine.history}")
        elif _is_sha256(engine.history_sha256):
            actual = file_sha256(engine.history)
            if actual != engine.history_sha256:
                errors.append(
                    f"{name}: processing-history SHA-256 mismatch: "
                    f"expected {engine.history_sha256}, got {actual}"
                )
        if not engine.version_output_contains:
            errors.append(f"{name}: version_output_contains is required")
        if verify_binaries:
            try:
                output = run_version_command(engine.version_command)
            except (OSError, subprocess.CalledProcessError) as error:
                errors.append(f"{name}: version command failed: {error}")
            else:
                if engine.version_output_contains not in output:
                    errors.append(
                        f"{name}: version output does not contain "
                        f"{engine.version_output_contains!r}: {output!r}"
                    )
    return errors


def get_reference_engine(config: ReferenceEngines, name: str) -> ReferenceEngine:
    try:
        return config.engines[name]
    except KeyError as error:
        available = ", ".join(sorted(config.engines))
        raise ValueError(f"unknown reference engine {name!r}; available: {available}") from error


def run_version_command(command: tuple[str, ...] | list[str]) -> str:
    completed = subprocess.run(
        list(command),
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return completed.stdout.strip()


def reference_metadata(engine: ReferenceEngine) -> dict[str, Any]:
    return {
        "reference_engine": engine.name,
        "reference_version": engine.version,
        "reference_source_revision": engine.source_revision,
        "reference_source_sha256": engine.source_sha256,
        "processing_history": f"profiles/{engine.history.name}",
        "processing_history_sha256": engine.history_sha256,
    }


def _is_sha256(value: str) -> bool:
    return len(value) == 64 and all(character in "0123456789abcdef" for character in value)
