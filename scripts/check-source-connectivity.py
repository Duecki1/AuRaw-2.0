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


MODULE_RE = re.compile(
    r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
PATH_MODULE_RE = re.compile(
    r'(?ms)#\s*\[\s*path\s*=\s*"([^"]+)"\s*\]\s*'
    r'(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;'
)
INCLUDE_RE = re.compile(r'\binclude!\(\s*"([^"]+)"\s*\)\s*;')
DERIVE_RE = re.compile(r"#\s*\[\s*derive\b[^]]*\]")


def brace_depth_at(text: str, stop: int) -> int:
    """Return Rust brace depth before ``stop``, ignoring comments and literals."""
    depth = 0
    index = 0
    block_comment_depth = 0
    while index < stop:
        if block_comment_depth:
            if text.startswith("/*", index):
                block_comment_depth += 1
                index += 2
            elif text.startswith("*/", index):
                block_comment_depth -= 1
                index += 2
            else:
                index += 1
            continue

        if text.startswith("//", index):
            newline = text.find("\n", index + 2, stop)
            index = stop if newline < 0 else newline + 1
            continue
        if text.startswith("/*", index):
            block_comment_depth = 1
            index += 2
            continue

        char = text[index]
        if char == '"':
            index += 1
            while index < stop:
                if text[index] == "\\":
                    index += 2
                elif text[index] == '"':
                    index += 1
                    break
                else:
                    index += 1
            continue
        if char == "'" and index + 2 < stop:
            # Lifetimes do not contain a closing quote. Only skip actual char literals.
            closing = index + 2 if text[index + 1] != "\\" else index + 3
            if closing < stop and text[closing] == "'":
                index = closing + 1
                continue
        if char == "r":
            raw = re.match(r'r(#+)?"', text[index:stop])
            if raw:
                hashes = raw.group(1) or ""
                terminator = '"' + hashes
                end = text.find(terminator, index + raw.end(), stop)
                index = stop if end < 0 else end + len(terminator)
                continue

        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        index += 1
    return depth


def next_derived_item(text: str, start: int) -> str:
    """Return the first token after derive and any following attributes."""
    tail = text[start:]
    while True:
        tail = tail.lstrip()
        attribute = re.match(r"#\s*\[[^]]*\]", tail)
        if attribute is None:
            break
        tail = tail[attribute.end():]
    match = re.match(r"(?:pub(?:\([^)]*\))?\s+)?([A-Za-z_][A-Za-z0-9_]*)", tail)
    return match.group(1) if match else ""


def module_candidates(parent: Path, name: str) -> tuple[Path, Path]:
    if parent.name in {"lib.rs", "main.rs", "mod.rs"}:
        module_root = parent.parent
    else:
        module_root = parent.parent / parent.stem
    return module_root / f"{name}.rs", module_root / name / "mod.rs"


roots = [path for path in (SRC / "lib.rs", SRC / "main.rs") if path.is_file()]
roots.extend(sorted((SRC / "bin").glob("*.rs")))
connected: set[Path] = set()


def visit(module: Path) -> None:
    module = module.resolve()
    if module in connected:
        return
    if not module.is_file():
        errors.append(f"referenced Rust source is missing: {module.relative_to(ROOT)}")
        return
    connected.add(module)
    text = module.read_text(encoding="utf-8")

    for include_match in INCLUDE_RE.finditer(text):
        if brace_depth_at(text, include_match.start()) != 0:
            errors.append(
                f"include! must appear at module scope in {module.relative_to(ROOT)}: "
                f"{include_match.group(1)}"
            )

    for derive_match in DERIVE_RE.finditer(text):
        item = next_derived_item(text, derive_match.end())
        if item not in {"struct", "enum", "union"}:
            errors.append(
                f"derive attribute in {module.relative_to(ROOT)} is attached to {item or 'no item'}"
            )

    for include_name in INCLUDE_RE.findall(text):
        included = (module.parent / include_name).resolve()
        if not included.is_file():
            errors.append(
                f"include! in {module.relative_to(ROOT)} references missing file: {include_name}"
            )
        else:
            visit(included)

    path_modules = {name for _, name in PATH_MODULE_RE.findall(text)}
    for relative, name in PATH_MODULE_RE.findall(text):
        target = (module.parent / relative).resolve()
        if target.is_file():
            visit(target)
        else:
            errors.append(
                f"module {name!r} declared by {module.relative_to(ROOT)} "
                f"references missing source: {relative}"
            )

    for name in MODULE_RE.findall(text):
        if name in path_modules:
            continue
        direct, nested = module_candidates(module, name)
        matches = [candidate for candidate in (direct, nested) if candidate.is_file()]
        if len(matches) == 1:
            visit(matches[0])
        elif not matches:
            errors.append(
                f"module {name!r} declared by {module.relative_to(ROOT)} has no source file"
            )
        else:
            errors.append(
                f"module {name!r} declared by {module.relative_to(ROOT)} is ambiguous: "
                f"{direct.relative_to(ROOT)} and {nested.relative_to(ROOT)}"
            )


for root in roots:
    visit(root)

for module in sorted(SRC.rglob("*.rs")):
    if module.resolve() not in connected:
        errors.append(f"stale Rust source is not reachable from a crate root: {module.relative_to(ROOT)}")

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

# Build-time templates are expanded into OUT_DIR and then included through
# concat!(env!("OUT_DIR"), ...), so they cannot appear in the direct
# include_str! scan above. Count both each declared template and its explicit
# sibling fragments as connected shader inputs.
preprocessor = (ROOT / "build_support/shader_preprocessor.rs").read_text(encoding="utf-8")
generated_templates = set(
    re.findall(r'\("([^"\\]+\.wgsl)",\s*"[^"\\]+\.generated\.wgsl"\)', preprocessor)
)
for template_name in generated_templates:
    template = SRC / "shaders" / template_name
    if not template.is_file():
        continue
    included.add(f"src/shaders/{template_name}")
    template_source = template.read_text(encoding="utf-8")
    for fragment in re.findall(r'//\s*@include\s+"([^"\\]+\.wgsl)"', template_source):
        included.add(f"src/shaders/{fragment}")
for path in sorted(shader_paths - included):
    errors.append(f"WGSL file is not included by Rust source: {path}")

binary_suffixes = {
    ".a", ".aar", ".apk", ".class", ".dll", ".dylib", ".exe", ".jar",
    ".o", ".obj", ".rlib", ".rmeta", ".so",
}
allowed_binary_paths = {"gradle/wrapper/gradle-wrapper.jar"}
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
    if path.suffix.lower() in binary_suffixes and relative not in allowed_binary_paths:
        errors.append(f"generated binary is present in the source tree: {relative}")

if errors:
    print("source-tree validation failed:", file=sys.stderr)
    for error in errors:
        print(f"  - {error}", file=sys.stderr)
    raise SystemExit(1)

print("source tree contains only connected modules and tracked shader sources")
