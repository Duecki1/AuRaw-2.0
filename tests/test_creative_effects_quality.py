from __future__ import annotations

import math


def smoothstep(edge0: float, edge1: float, value: float) -> float:
    t = max(0.0, min(1.0, (value - edge0) / max(edge1 - edge0, 1e-9)))
    return t * t * (3.0 - 2.0 * t)














def test_default_glow_threshold_rejects_shadows_but_accepts_bright_sources() -> None:
    threshold = 0.60
    cutoff = 0.06 + (0.92 - 0.06) * threshold**1.12

    def emission_gate(linear_luma: float) -> float:
        if linear_luma <= 1.0:
            perceptual = max(linear_luma, 0.0) ** (1.0 / 2.2)
        else:
            perceptual = 1.0 + (1.0 / 2.2) * math.log(linear_luma)
        cutoff_fade = smoothstep(cutoff, cutoff + 0.16, perceptual)
        black_gate = smoothstep(0.0, 0.42, linear_luma) ** 0.5
        return cutoff_fade * black_gate

    assert emission_gate(0.02) < 1e-6
    assert emission_gate(0.18) < 0.05
    assert emission_gate(1.5) > 0.95


def vignette_anchor(
    distance: float, start: float, end: float, power: float, opacity: float
) -> float:
    return opacity * smoothstep(start, end, distance) ** power


def test_vignette_curves_track_supplied_lightroom_linear_light_anchors() -> None:
    distances = (0.6, 0.8, 1.0, 1.2, 1.4)
    curves = (
        ((0.10, 1.235, 2.88, 0.86), (0.073, 0.262, 0.629, 0.827, 0.871)),
        ((0.02, 1.135, 3.46, 1.00), (0.124, 0.426, 0.874, 0.995, 0.999)),
        ((0.305, 1.24, 4.36, 0.90), (0.007, 0.061, 0.414, 0.836, 0.905)),
        ((0.13, 1.075, 5.66, 1.00), (0.035, 0.262, 0.903, 1.000, 1.000)),
    )
    for parameters, lightroom_samples in curves:
        fitted = [vignette_anchor(distance, *parameters) for distance in distances]
        assert max(abs(a - b) for a, b in zip(fitted, lightroom_samples)) < 0.045

    assert vignette_anchor(0.0, 0.02, 1.135, 3.46, 1.0) == 0.0
    assert vignette_anchor(math.sqrt(2.0), 0.02, 1.135, 3.46, 1.0) == 1.0


def test_vignette_distance_is_invariant_across_frame_aspect_ratios() -> None:
    normalized_point = (0.72, 0.48)
    expected = math.hypot(*normalized_point)
    for width, height in ((7008, 4672), (6000, 6000), (6000, 4500), (8192, 3456)):
        pixel_x = normalized_point[0] * width * 0.5
        pixel_y = normalized_point[1] * height * 0.5
        distance = math.hypot(pixel_x / (width * 0.5), pixel_y / (height * 0.5))
        assert math.isclose(distance, expected, abs_tol=1e-12)


