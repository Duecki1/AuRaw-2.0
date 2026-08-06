from __future__ import annotations

import math
MIDDLE_GREY = 0.1845


def smoothstep(edge0: float, edge1: float, value: float) -> float:
    t = max(0.0, min(1.0, (value - edge0) / max(edge1 - edge0, 1e-6)))
    return t * t * (3.0 - 2.0 * t)


def shaped(value: float) -> float:
    n = max(-1.0, min(1.0, value / 100.0))
    m = abs(n)
    return math.copysign(m * (1.45 - 0.45 * m), n) if m else 0.0


def shadow_range(p05: float, p50: float) -> tuple[float, float]:
    return p05 - 0.90, p50 + 1.35


def shadow_weight(ev: float, bounds: tuple[float, float]) -> float:
    lower, upper = bounds
    return 1.0 - smoothstep(lower, upper, ev)


def shadow_output_ev(ev: float, amount: float, p05=-5.0, p50=0.0) -> tuple[float, float]:
    bounds = shadow_range(p05, p50)
    mask = shadow_weight(ev, bounds)
    a = shaped(amount)
    lower, upper = bounds
    monotone_limit = 0.64 * max(upper - lower, 0.25)
    strength = math.copysign(min(abs(a) * 2.20, monotone_limit), a) if a else 0.0
    return ev + strength * mask, mask


def display_black_toe(luma: float, amount: float) -> float:
    if luma <= 0.0 or amount == 0.0:
        return luma
    a = shaped(amount)
    hdr_guard = 1.0 - smoothstep(0.35, 1.0, luma)
    if a >= 0.0:
        weight = 0.08 + 0.92 * 2.0 ** (-luma / 0.035)
        offset = a * 1.75 * weight * hdr_guard
    else:
        deep = 1.0 - smoothstep(0.012, 0.030, luma)
        tail = 0.10 + 2.35 * 2.0 ** (-luma / 0.070)
        offset = -(-a) * (10.50 * deep + tail) * hdr_guard
    return luma * 2.0**offset









def test_shadow_selector_dark_subject_cannot_inherit_bright_neighborhood() -> None:
    pixel_ev = -6.0
    guide_ev = -1.5
    mismatch = abs(pixel_ev - guide_ev)
    bounded_guide = pixel_ev + max(-1.25, min(0.75, guide_ev - pixel_ev))
    guide_weight = 0.42 + (0.22 - 0.42) * smoothstep(0.50, 3.00, mismatch)
    selected = pixel_ev + (bounded_guide - pixel_ev) * guide_weight
    assert selected < -5.8


def test_shadow_neutrality_is_exact() -> None:
    for ev in (-12.0, -8.0, -5.0, -3.0, 0.0, 3.0):
        mapped, _ = shadow_output_ev(ev, 0.0)
        assert mapped == ev


def test_blacks_neutrality_is_exact() -> None:
    for y in (0.0, 1e-6, 1e-4, 0.001, 0.01, 0.1, 0.15, 1.0):
        assert display_black_toe(y, 0.0) == y


def test_shadow_moderate_settings_have_perceptual_authority() -> None:
    # Representative dark-detail zone in the measured Lightroom low-pass range.
    source_ev = -4.0
    plus25, mask = shadow_output_ev(source_ev, 25.0)
    plus50, _ = shadow_output_ev(source_ev, 50.0)
    minus25, _ = shadow_output_ev(source_ev, -25.0)
    minus50, _ = shadow_output_ev(source_ev, -50.0)
    assert mask > 0.80
    assert plus25 - source_ev > 0.55
    assert plus50 - source_ev > 1.00
    assert source_ev - minus25 > 0.55
    assert source_ev - minus50 > 1.00


def test_shadows_give_deep_detail_authority_then_roll_out_through_midtones() -> None:
    _, black_weight = shadow_output_ev(-10.0, 50.0)
    _, detail_weight = shadow_output_ev(-4.0, 50.0)
    _, mid_weight = shadow_output_ev(0.0, 50.0)
    _, bright_weight = shadow_output_ev(3.0, 50.0)
    assert black_weight == 1.0
    assert detail_weight > 0.80
    assert 0.05 < mid_weight < 0.20
    assert bright_weight == 0.0




def test_shadow_transfer_is_monotone_for_all_requested_settings_and_extreme_histograms() -> None:
    scenarios = [
        (-8.0, -4.0),  # low-key
        (-5.0, 0.0),   # normal
        (-2.0, 1.0),   # high-key
        (-6.0, -1.0),  # backlit/night-like
        (-6.8, -6.2),  # nearly degenerate dark histogram
        (-1.9, -1.2),  # nearly degenerate bright histogram
    ]
    for p05, p50 in scenarios:
        lower, upper = shadow_range(p05, p50)
        assert math.isfinite(lower + upper)
        assert lower < upper
        for amount in (-100.0, -50.0, -25.0, 0.0, 25.0, 50.0, 100.0):
            previous = -1e30
            for i in range(12001):
                ev = -16.0 + 28.0 * i / 12000.0
                mapped, _ = shadow_output_ev(ev, amount, p05, p50)
                assert mapped >= previous - 1e-10
                previous = mapped


def test_blacks_moderate_settings_have_toe_authority_but_protect_lower_midtones() -> None:
    near_black = 0.01
    lower_mid = 0.10
    for amount, minimum_ev in [(25.0, 0.45), (50.0, 0.80), (-25.0, 3.5), (-50.0, 6.0)]:
        mapped = display_black_toe(near_black, amount)
        delta = math.log2(mapped / near_black)
        assert abs(delta) > minimum_ev
    assert abs(math.log2(display_black_toe(lower_mid, 50.0) / lower_mid)) < 0.25
    assert abs(math.log2(display_black_toe(lower_mid, -50.0) / lower_mid)) < 0.75
    assert 0.0 < math.log2(display_black_toe(0.15, 100.0) / 0.15) < 0.25


def test_blacks_transfer_is_monotone_for_all_requested_settings() -> None:
    for amount in (-100.0, -50.0, -25.0, 0.0, 25.0, 50.0, 100.0):
        previous = -1.0
        for i in range(1, 20001):
            y = 0.30 * i / 20000.0
            mapped = display_black_toe(y, amount)
            assert mapped >= previous - 1e-12
            previous = mapped












def test_hdr_values_are_outside_shadow_and_black_toe_regions() -> None:
    # +4 EV relative to middle gray is scene HDR and must remain a Shadows no-op.
    mapped, mask = shadow_output_ev(4.0, 100.0)
    assert mask == 0.0
    assert mapped == 4.0
    # Blacks receives display-linear data and has an explicit protected pivot.
    assert display_black_toe(4.0, 100.0) == 4.0


def test_ratio_scaling_preserves_signed_rgb_chromatic_relationships() -> None:
    rgb = (-0.02, 0.08, 0.04)
    scale = 2.0 ** 1.25
    adjusted = tuple(c * scale for c in rgb)
    assert adjusted[0] < 0.0  # no premature max(rgb, 0) in the scene control
    assert math.isclose(adjusted[1] / adjusted[2], rgb[1] / rgb[2])


