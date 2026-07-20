#!/usr/bin/env python3
"""Fail when a workflow references a mutable third-party action tag."""

from __future__ import annotations

from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW_ROOTS = (ROOT / ".github/workflows", ROOT / ".gitea/workflows")
USES_RE = re.compile(r"\buses:\s*([^\s#]+)")
SHA_RE = re.compile(r"[0-9a-f]{40}")


def main() -> int:
    errors: list[str] = []
    for workflow_root in WORKFLOW_ROOTS:
        if not workflow_root.is_dir():
            continue
        for path in sorted([*workflow_root.glob("*.yml"), *workflow_root.glob("*.yaml")]):
            for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
                match = USES_RE.search(line)
                if match is None:
                    continue
                action = match.group(1)
                if action.startswith(("./", "docker://")):
                    continue
                if "@" not in action or SHA_RE.fullmatch(action.rsplit("@", 1)[1]) is None:
                    errors.append(
                        f"{path.relative_to(ROOT)}:{line_number}: mutable action reference {action}"
                    )
    if errors:
        print("workflow action pin validation failed:")
        for error in errors:
            print(f"  - {error}")
        return 1
    print("all third-party workflow actions are pinned to full commit SHAs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
