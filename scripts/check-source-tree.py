#!/usr/bin/env python3
"""Compatibility wrapper; use check-source-connectivity.py for new automation."""
from pathlib import Path
import runpy

runpy.run_path(str(Path(__file__).with_name("check-source-connectivity.py")), run_name="__main__")
