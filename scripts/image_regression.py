#!/usr/bin/env python3
from __future__ import annotations

from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "regression"))

from iqr.cli import main  # noqa: E402


if __name__ == "__main__":
    raise SystemExit(main())
