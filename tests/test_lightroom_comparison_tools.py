from __future__ import annotations

import ast
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
COMPARATOR = ROOT / "scripts/compare_lightroom_adjustments.py"


def test_lightroom_comparator_is_valid_python() -> None:
    ast.parse(COMPARATOR.read_text(encoding="utf-8"))




def test_comparison_is_relative_to_each_renderers_own_baseline() -> None:
    source = COMPARATOR.read_text(encoding="utf-8")
    assert "args.auraw_dir / args.auraw_baseline" in source
    assert "args.lightroom_dir / args.lightroom_baseline" in source
    assert "luma_delta_ev(auraw_base, auraw_edit" in source
    assert "luma_delta_ev(lightroom_base, lightroom_edit" in source
    assert "baseline_luma_quantiles(auraw_base, SRGB_LUMA)" in source
    assert "baseline_luma_quantiles(lightroom_base, ADOBE_RGB_LUMA)" in source


def test_comparison_preserves_tiff_precision_and_uses_the_supplied_profiles() -> None:
    source = COMPARATOR.read_text(encoding="utf-8")
    assert 'dtype="<u2"' in source
    assert "65535.0" in source
    assert "ADOBE_RGB_GAMMA" in source
    assert "ADOBE_RGB_LUMA" in source
    assert '"Camera NT.tif"' in source
    assert '"Vibration -100.tif"' in source
    assert "--lightroom-crop" in source
    assert "--auraw-crop" in source


