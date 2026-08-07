from __future__ import annotations


def smoothstep(edge0: float, edge1: float, value: float) -> float:
    x = max(0.0, min(1.0, (value - edge0) / max(edge1 - edge0, 1e-4)))
    return x * x * (3.0 - 2.0 * x)


def test_blacks_has_a_visible_lower_tonal_range() -> None:
    p005, p05, p50 = -8.0, -5.0, -1.0
    fade_end = min(p50 - 0.35, p05 + 3.00)
    upper = max(fade_end, p05 + 0.45)
    mask_at_fifth_percentile = 1.0 - smoothstep(p005 - 0.55, upper, p05)
    mask_near_median = 1.0 - smoothstep(p005 - 0.55, upper, p50)

    assert mask_at_fifth_percentile > 0.40
    assert mask_near_median < 1e-6
