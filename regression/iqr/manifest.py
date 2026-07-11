from __future__ import annotations

from dataclasses import dataclass, field
import hashlib
from pathlib import Path
from typing import Any

import yaml

from .metrics import Roi


REQUIRED_COVERAGE = {
    "bayer",
    "xtrans",
    "high-iso",
    "underexposed",
    "saturated-highlight",
    "difficult-frequency",
}


@dataclass(frozen=True)
class Scene:
    scene_id: str
    raw: Path
    sha256: str
    cfa: str
    tags: tuple[str, ...]
    license: str
    source: str
    redistributable: bool
    rois: tuple[Roi, ...] = ()
    thresholds: dict[str, float] = field(default_factory=dict)
    enabled: bool = True


@dataclass(frozen=True)
class Manifest:
    path: Path
    color_space: str
    scenes: tuple[Scene, ...]
    raw_root: Path


def load_manifest(path: Path | str) -> Manifest:
    manifest_path = Path(path).resolve()
    data = yaml.safe_load(manifest_path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError("manifest root must be a mapping")
    if int(data.get("schema", 0)) != 1:
        raise ValueError("manifest schema must be 1")
    color_space = str(data.get("color_space", ""))
    raw_root = manifest_path.parent / str(data.get("raw_root", "raw"))
    raw_scenes = data.get("scenes", [])
    if not isinstance(raw_scenes, list):
        raise ValueError("scenes must be a list")
    scenes: list[Scene] = []
    for entry in raw_scenes:
        if not isinstance(entry, dict):
            raise ValueError("every scene must be a mapping")
        scene_id = str(entry.get("id", "")).strip()
        if not scene_id:
            raise ValueError("scene id cannot be empty")
        tags = tuple(sorted({str(tag) for tag in entry.get("tags", [])}))
        rois = tuple(Roi.from_mapping(roi) for roi in entry.get("rois", []))
        scenes.append(
            Scene(
                scene_id=scene_id,
                raw=raw_root / str(entry.get("raw", "")),
                sha256=str(entry.get("sha256", "")).lower(),
                cfa=str(entry.get("cfa", "")),
                tags=tags,
                license=str(entry.get("license", "")),
                source=str(entry.get("source", "")),
                redistributable=bool(entry.get("redistributable", False)),
                rois=rois,
                thresholds={str(k): float(v) for k, v in entry.get("thresholds", {}).items()},
                enabled=bool(entry.get("enabled", True)),
            )
        )
    return Manifest(manifest_path, color_space, tuple(scenes), raw_root)


def validate_manifest(
    manifest: Manifest,
    *,
    verify_files: bool = False,
    require_coverage: bool = True,
) -> list[str]:
    errors: list[str] = []
    ids: set[str] = set()
    coverage: set[str] = set()
    for scene in manifest.scenes:
        if not scene.enabled:
            continue
        if scene.scene_id in ids:
            errors.append(f"duplicate scene id: {scene.scene_id}")
        ids.add(scene.scene_id)
        if scene.cfa not in {"bayer", "xtrans"}:
            errors.append(f"{scene.scene_id}: cfa must be 'bayer' or 'xtrans'")
        coverage.add(scene.cfa)
        coverage.update(scene.tags)
        if len(scene.sha256) != 64 or any(ch not in "0123456789abcdef" for ch in scene.sha256):
            errors.append(f"{scene.scene_id}: sha256 must be 64 lowercase hex characters")
        if not scene.license:
            errors.append(f"{scene.scene_id}: license is required")
        if not scene.source:
            errors.append(f"{scene.scene_id}: source is required")
        if not scene.raw.name:
            errors.append(f"{scene.scene_id}: raw path is required")
        if verify_files:
            if not scene.raw.is_file():
                errors.append(f"{scene.scene_id}: RAW file missing: {scene.raw}")
            elif file_sha256(scene.raw) != scene.sha256:
                errors.append(f"{scene.scene_id}: RAW SHA-256 mismatch: {scene.raw}")
    if require_coverage:
        missing = sorted(REQUIRED_COVERAGE - coverage)
        if missing:
            errors.append("corpus is missing required coverage: " + ", ".join(missing))
    return errors


def file_sha256(path: Path | str) -> str:
    digest = hashlib.sha256()
    with Path(path).open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_thresholds(path: Path | str) -> dict[str, Any]:
    data = yaml.safe_load(Path(path).read_text(encoding="utf-8"))
    if not isinstance(data, dict) or int(data.get("schema", 0)) != 1:
        raise ValueError("threshold file schema must be 1")
    return data


def thresholds_for_scene(
    scene: Scene,
    config: dict[str, Any],
    backend: str,
) -> dict[str, float]:
    result: dict[str, float] = {
        str(k): float(v) for k, v in config.get("defaults", {}).items()
    }
    for tag in (scene.cfa,) + scene.tags:
        result.update(
            {str(k): float(v) for k, v in config.get("tags", {}).get(tag, {}).items()}
        )
    result.update(
        {str(k): float(v) for k, v in config.get("backends", {}).get(backend, {}).items()}
    )
    result.update(scene.thresholds)
    return result
