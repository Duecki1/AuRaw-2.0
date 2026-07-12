#!/usr/bin/env python3
"""Reject stale modules, missing shader tracking, and committed build products."""

from __future__ import annotations

import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
errors: list[str] = []


def normalized_rust_version(version: str) -> tuple[int, int, int]:
    parts = version.split(".")
    if not 2 <= len(parts) <= 3 or any(not part.isdigit() for part in parts):
        raise ValueError(f"invalid Rust version: {version!r}")
    major, minor, *patch = (int(part) for part in parts)
    return major, minor, patch[0] if patch else 0


try:
    with (ROOT / "Cargo.toml").open("rb") as handle:
        manifest = tomllib.load(handle)
    with (ROOT / "rust-toolchain.toml").open("rb") as handle:
        toolchain = tomllib.load(handle)
    manifest_rust = str(manifest["package"]["rust-version"])
    pinned_rust = str(toolchain["toolchain"]["channel"])
    if normalized_rust_version(manifest_rust) != normalized_rust_version(pinned_rust):
        errors.append(
            "Rust version mismatch: Cargo.toml declares "
            f"{manifest_rust}, rust-toolchain.toml pins {pinned_rust}"
        )
except (KeyError, OSError, ValueError, tomllib.TOMLDecodeError) as error:
    errors.append(f"cannot validate the pinned Rust version: {error}")


def declares(module_file: Path, name: str) -> bool:
    if not module_file.is_file():
        return False
    text = module_file.read_text(encoding="utf-8")
    pattern = rf"\b(?:pub(?:\([^)]*\))?\s+)?mod\s+{re.escape(name)}\s*;"
    return re.search(pattern, text) is not None


for module in sorted(SRC.rglob("*.rs")):
    if module.name in {"lib.rs", "main.rs", "mod.rs"}:
        continue
    # Files directly under src/bin are Cargo auto-discovered binary roots,
    # not modules that require a containing mod.rs declaration.
    if module.parent == SRC / "bin":
        continue
    owner = SRC / "lib.rs" if module.parent == SRC else module.parent / "mod.rs"
    if not declares(owner, module.stem):
        errors.append(f"stale Rust module not declared by {owner.relative_to(ROOT)}: {module.relative_to(ROOT)}")

for module_dir in sorted(path.parent for path in SRC.rglob("mod.rs") if path.parent != SRC):
    parent_owner = SRC / "lib.rs" if module_dir.parent == SRC else module_dir.parent / "mod.rs"
    if not declares(parent_owner, module_dir.name):
        errors.append(
            f"stale module directory not declared by {parent_owner.relative_to(ROOT)}: "
            f"{module_dir.relative_to(ROOT)}"
        )

shader_paths = {
    path.relative_to(ROOT).as_posix() for path in (SRC / "shaders").glob("*.wgsl")
}
build_rs = (ROOT / "build.rs").read_text(encoding="utf-8")
watched = set(re.findall(r'"(src/shaders/[^"\\]+\.wgsl)"', build_rs))
if shader_paths != watched:
    for path in sorted(shader_paths - watched):
        errors.append(f"WGSL file is not watched by build.rs: {path}")
    for path in sorted(watched - shader_paths):
        errors.append(f"build.rs watches a missing WGSL file: {path}")

rust_sources = "\n".join(
    path.read_text(encoding="utf-8") for path in SRC.rglob("*.rs")
)
included_names = set(re.findall(r'include_str!\("\.\./shaders/([^"\\]+\.wgsl)"\)', rust_sources))
included = {f"src/shaders/{name}" for name in included_names}
for path in sorted(shader_paths - included):
    errors.append(f"WGSL file is not included by Rust source: {path}")

binary_suffixes = {
    ".a", ".aar", ".apk", ".class", ".dll", ".dylib", ".exe", ".jar",
    ".o", ".obj", ".rlib", ".rmeta", ".so",
}
ignored_roots = {
    ".git",
    ".gradle",
    "dist",
    "target",
    "android/.gradle",
    "android/build",
    "android/app/build",
    "android/native",
}
for path in sorted(item for item in ROOT.rglob("*") if item.is_file()):
    relative = path.relative_to(ROOT).as_posix()
    if any(relative == root or relative.startswith(f"{root}/") for root in ignored_roots):
        continue
    if path.suffix.lower() in binary_suffixes:
        errors.append(f"generated binary is present in the source tree: {relative}")

if errors:
    print("source-tree validation failed:", file=sys.stderr)
    for error in errors:
        print(f"  - {error}", file=sys.stderr)
    raise SystemExit(1)

print("source tree contains only connected modules and tracked shader sources")
