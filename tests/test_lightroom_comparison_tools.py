from __future__ import annotations

import ast
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
COMPARATOR = ROOT / "scripts/compare_lightroom_adjustments.py"
EXPORTER = ROOT / "src/bin/auraw-develop-export.rs"


def test_lightroom_comparator_is_valid_python() -> None:
    ast.parse(COMPARATOR.read_text(encoding="utf-8"))


def test_headless_exporter_and_comparator_cover_the_same_isolated_endpoints() -> None:
    comparator = COMPARATOR.read_text(encoding="utf-8")
    exporter = EXPORTER.read_text(encoding="utf-8")
    for control in (
        "exposure",
        "contrast",
        "highlights",
        "shadows",
        "whites",
        "blacks",
        "texture",
        "clarity",
        "dehaze",
        "vibrance",
        "saturation",
    ):
        assert f'"{control}_plus' in exporter
        assert f'"{control}_minus' in exporter
        assert f'"{control}_plus' in comparator
        assert f'"{control}_minus' in comparator


def test_comparison_is_relative_to_each_renderers_own_baseline() -> None:
    source = COMPARATOR.read_text(encoding="utf-8")
    assert 'args.auraw_dir / "baseline.png"' in source
    assert 'args.lightroom_dir / "Adobe Color.jpg"' in source
    assert "auraw_delta = luma_delta_ev(auraw_base" in source
    assert "lightroom_delta = luma_delta_ev(lightroom_base" in source
