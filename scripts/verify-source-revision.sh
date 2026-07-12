#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

if ! command -v git >/dev/null 2>&1; then
    echo "git is required to verify the source revision" >&2
    exit 1
fi
if ! git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "release builds must run from a Git checkout" >&2
    exit 1
fi

STATUS=$(git -C "$ROOT" status --porcelain=v1 --untracked-files=all)
if [ -n "$STATUS" ]; then
    echo "release builds require a clean source tree:" >&2
    printf '%s\n' "$STATUS" >&2
    exit 1
fi

git -C "$ROOT" rev-parse --verify HEAD
