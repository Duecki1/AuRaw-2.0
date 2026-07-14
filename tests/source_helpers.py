"""Helpers for source-oriented tests.

The product source is intentionally split into focused Rust modules. Tests that inspect
source text should follow `mod foo;` and `include!("foo.rs")` boundaries instead of
assuming an implementation remains in one monolithic file.
"""

from __future__ import annotations

from pathlib import Path
import re

_INCLUDE_RE = re.compile(r'\binclude!\(\s*"([^"]+)"\s*\)\s*;')
_MODULE_RE = re.compile(
    r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)


def _module_file(parent: Path, module_name: str) -> Path | None:
    """Resolve a Rust out-of-line module using the standard file layout rules."""
    if parent.name in {"lib.rs", "main.rs", "mod.rs"}:
        module_root = parent.parent
    else:
        module_root = parent.parent / parent.stem

    direct = module_root / f"{module_name}.rs"
    nested = module_root / module_name / "mod.rs"
    if direct.is_file():
        return direct
    if nested.is_file():
        return nested
    return None


def read_source_tree(path: Path) -> str:
    """Return a Rust source file plus recursively referenced local source modules."""
    chunks: list[str] = []
    visited: set[Path] = set()

    def visit(candidate: Path) -> None:
        resolved = candidate.resolve()
        if resolved in visited or not resolved.is_file():
            return
        visited.add(resolved)

        text = resolved.read_text(encoding="utf-8")
        chunks.append(f"\n// SOURCE FILE: {resolved}\n{text}")

        for include_path in _INCLUDE_RE.findall(text):
            visit(resolved.parent / include_path)
        for module_name in _MODULE_RE.findall(text):
            module_path = _module_file(resolved, module_name)
            if module_path is not None:
                visit(module_path)

    visit(path)
    return "\n".join(chunks)
