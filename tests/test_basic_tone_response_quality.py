from __future__ import annotations

import math
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TONEMAP = (ROOT / "src/shaders/tonemap.wgsl").read_text(encoding="utf-8")
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")


def smoothstep(edge0: float, edge1: float, value: float) -> float:
    t = max(0.0, min(1.0, (value - edge0) / max(edge1 - edge0, 1e-6)))
    return t * t * (3.0 - 2.0 * t)


def shaped(value: float) -> float:
    n = max(-1.0, min(1.0, value / 100.0))
    m = abs(n)
    return math.copysign(m * (1.45 - 0.45 * m), n) if m else 0.0


def shadow_mask(ev: float, p05: float = -5.0, p50: float = -1.0) -> float:
    return 1.0 - smoothstep(p05 - 0.90, p50 + 1.35, ev)


def black_toe(luma: float, pivot: float, amount: float) -> float:
    if luma >= pivot:
        return luma
    n = max(luma / pivot, 1e-6)
    a = shaped(amount)
    gamma = 2.0 ** (-1.25 * a) if a >= 0.0 else 2.0 ** (1.25 * (-a))
    mapped = pivot * n**gamma
    feather = 1.0 - smoothstep(0.72, 1.0, n)
    return luma + (mapped - luma) * feather


def test_process_16_is_the_low_tone_response_version() -> None:
    assert "pub const BASIC_TONE_RESPONSE_PROCESS_VERSION: u32 = 16;" in BASIC
    assert "pub const CURRENT_PROCESS_VERSION: u32 = BASIC_TONE_RESPONSE_PROCESS_VERSION;" in BASIC
    assert "const BASIC_TONE_RESPONSE_PROCESS_VERSION: u32 = 16u;" in TONEMAP
    assert "HIGHLIGHT_CONSENSUS_PROCESS_VERSION =>" in BASIC
    assert "self.process_version = BASIC_TONE_RESPONSE_PROCESS_VERSION;" in BASIC


def test_shadow_mid_slider_has_decisive_authority() -> None:
    # Around a representative 5th percentile, +50 should produce comfortably
    # more than one stop of lift before the view transform.
    amount = shaped(50.0)
    mask = shadow_mask(-5.0)
    toe_guard = smoothstep(-8.0 - 0.35, -5.0 + 0.90, -5.0)
    mask *= 0.28 + 0.72 * toe_guard
    lift_ev = 3.20 * amount * mask
    assert amount > 0.60
    assert lift_ev > 1.5


def test_shadow_selector_recovers_small_dark_subjects_from_bright_guide_cells() -> None:
    pixel_ev = -6.0
    guide_ev = -1.0
    mismatch = abs(pixel_ev - guide_ev)
    guide_weight = 0.38 + (0.16 - 0.38) * smoothstep(0.75, 2.75, mismatch)
    selected = pixel_ev + (guide_ev - pixel_ev) * guide_weight
    # Old guide-only selection was -1 EV and barely selected Shadows. The new
    # selector remains strongly in the shadow domain while retaining some
    # bilateral-guide stabilization.
    assert selected < -5.0
    assert shadow_mask(selected) > 0.9


def test_blacks_toe_is_strong_and_monotone() -> None:
    pivot = 0.03
    # A pixel one decade below the pivot should move visibly in both directions.
    source = pivot * 0.1
    lifted = black_toe(source, pivot, 50.0)
    deepened = black_toe(source, pivot, -50.0)
    assert lifted > source * 2.3
    assert deepened < source * 0.25

    for amount in (-100.0, -50.0, 50.0, 100.0):
        previous = -1.0
        for i in range(1, 4001):
            luma = pivot * i / 4000.0
            mapped = black_toe(luma, pivot, amount)
            assert mapped >= previous - 1e-12
            previous = mapped


def test_process_16_keeps_highlight_and_white_ranges_unchanged() -> None:
    assert "signed_tone_range(highlights, 1.90, 1.15) * masks.z" in TONEMAP
    assert "signed_tone_range(whites, 1.25, 1.40) * masks.w" in TONEMAP


def test_positive_shadows_protect_absolute_black_endpoint() -> None:
    assert "shadow_mask = shadow_mask * mix(0.28, 1.0, toe_guard)" in TONEMAP
    assert "apply_blacks_toe_v2" in TONEMAP



def shadow_output_ev(ev: float, amount: float, p005: float, p05: float, p50: float) -> float:
    mask = 1.0 - smoothstep(p05 - 0.90, p50 + 1.35, ev)
    a = shaped(amount)
    if a > 0.0:
        guard = smoothstep(p005 - 0.35, p05 + 0.90, ev)
        mask *= 0.28 + 0.72 * guard
    offset = (3.20 * a if a >= 0.0 else 2.35 * a) * mask
    return ev + max(-6.5, min(6.5, offset))


def test_shadow_curve_remains_monotone_at_extremes() -> None:
    for p005, p05, p50 in [
        (-12.0, -8.0, -4.0),
        (-10.0, -7.0, -2.0),
        (-8.0, -5.0, -1.0),
        (-6.0, -3.0, 0.0),
        (-4.0, -2.0, 1.0),
    ]:
        for amount in (-100.0, -50.0, 50.0, 100.0):
            previous = -1e9
            for i in range(6001):
                ev = (p005 - 2.0) + (p50 - p005 + 5.0) * i / 6000.0
                mapped = shadow_output_ev(ev, amount, p005, p05, p50)
                assert mapped >= previous - 1e-9
                previous = mapped


def test_blacks_toe_preserves_nonpositive_scene_luminance_for_gamut_handling() -> None:
    assert "let luminance = dot(rgb, LUMA);" in TONEMAP
    assert "if luminance <= 1e-8" in TONEMAP
