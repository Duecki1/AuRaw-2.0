from __future__ import annotations

import math
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TONEMAP = (ROOT / "src/shaders/tonemap.wgsl").read_text(encoding="utf-8")
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
DEVELOP = (ROOT / "src/ui/sidebar/develop.rs").read_text(encoding="utf-8")
MASKS = (ROOT / "src/ui/sidebar/masks.rs").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")

MIDDLE_GREY = 0.1845


def smoothstep(edge0: float, edge1: float, value: float) -> float:
    t = max(0.0, min(1.0, (value - edge0) / max(edge1 - edge0, 1e-6)))
    return t * t * (3.0 - 2.0 * t)


def shaped(value: float) -> float:
    n = max(-1.0, min(1.0, value / 100.0))
    m = abs(n)
    return math.copysign(m * (1.45 - 0.45 * m), n) if m else 0.0


def shadow_bounds(p005: float, p05: float, p50: float) -> tuple[float, float, float]:
    p005 = max(-15.5, min(11.0, p005))
    p05 = max(max(-15.25, min(11.25, p05)), p005 + 0.25)
    p50 = max(max(-15.0, min(11.5, p50)), p05 + 0.50)
    lower = max(-13.0, min(-6.0, min(p005 - 0.50, p05 - 2.50)))
    peak = max(max(-6.0, min(-2.0, p05 + 1.25)), lower + 2.50)
    upper = min(max(p50 + 0.50, peak + 3.50), 0.75)
    upper = max(upper, peak + 2.50)
    return lower, peak, upper


def shadow_weight(ev: float, bounds: tuple[float, float, float]) -> float:
    lower, peak, upper = bounds
    if ev <= peak:
        return smoothstep(lower, peak, ev)
    return 1.0 - smoothstep(peak, upper, ev)


def shadow_output_ev(ev: float, amount: float, p005=-8.0, p05=-5.0, p50=0.0) -> tuple[float, float]:
    bounds = shadow_bounds(p005, p05, p50)
    mask = shadow_weight(ev, bounds)
    a = shaped(amount)
    lower, peak, upper = bounds
    if a >= 0.0:
        strength = min(a * 3.40, 0.64 * max(upper - peak, 0.25))
    else:
        strength = -min((-a) * 3.00, 0.64 * max(peak - lower, 0.25))
    return ev + strength * mask, mask


def display_black_toe(luma: float, amount: float, pivot: float = 0.15) -> float:
    if luma <= 0.0 or luma >= pivot or amount == 0.0:
        return luma
    x = max(0.0, min(1.0, luma / pivot))
    toe = (1.0 - x) ** 2
    a = shaped(amount)
    endpoint = 2.60 if a >= 0.0 else 3.10
    return luma * 2.0 ** (a * endpoint * toe)


def test_process_17_is_current_but_process_16_remains_compatible() -> None:
    assert "pub const BASIC_TONE_RESPONSE_PROCESS_VERSION: u32 = 16;" in BASIC
    assert "pub const PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION: u32 = 17;" in BASIC
    assert "pub const CURRENT_PROCESS_VERSION: u32 = PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION;" in BASIC
    assert "const BASIC_TONE_RESPONSE_PROCESS_VERSION: u32 = 16u;" in TONEMAP
    assert "const PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION: u32 = 17u;" in TONEMAP
    assert "BASIC_TONE_RESPONSE_PROCESS_VERSION | CURRENT_PROCESS_VERSION => {}" in BASIC
    assert "HIGHLIGHT_CONSENSUS_PROCESS_VERSION =>" in BASIC
    assert "self.process_version = BASIC_TONE_RESPONSE_PROCESS_VERSION;" in BASIC


def test_process_16_low_tone_formulas_are_preserved_behind_version_gate() -> None:
    assert "if params.process_info.x < PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION" in TONEMAP
    assert "apply_blacks_toe_v2" in TONEMAP
    assert "shadow_mask = shadow_mask * mix(0.28, 1.0, toe_guard)" in TONEMAP
    assert "signed_tone_range(shadows, 2.35, 3.20) * shadow_mask" in TONEMAP


def test_unrelated_noise_change_does_not_upgrade_process_16() -> None:
    assert "exposure.process_version = CURRENT_PROCESS_VERSION;" not in DEVELOP
    assert "exposure.process_version == BASIC_TONE_RESPONSE_PROCESS_VERSION" in DEVELOP
    assert "shadows_changed || blacks_changed" in DEVELOP
    assert "exposure.process_version = PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION;" in DEVELOP


def test_local_low_tone_edit_is_the_only_mask_action_that_opts_16_into_17() -> None:
    assert "let shadows_before = adjustment.shadows;" in MASKS
    assert "let blacks_before = adjustment.blacks;" in MASKS
    assert "adjustment.shadows != shadows_before || adjustment.blacks != blacks_before" in MASKS
    assert "app.exposure.process_version == BASIC_TONE_RESPONSE_PROCESS_VERSION" in MASKS
    assert "app.exposure.process_version = PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION;" in MASKS


def test_shadow_selector_dark_subject_cannot_inherit_bright_neighborhood() -> None:
    pixel_ev = -6.0
    guide_ev = -1.5
    mismatch = abs(pixel_ev - guide_ev)
    bounded_guide = pixel_ev + max(-1.25, min(0.75, guide_ev - pixel_ev))
    guide_weight = 0.42 + (0.22 - 0.42) * smoothstep(0.50, 3.00, mismatch)
    selected = pixel_ev + (bounded_guide - pixel_ev) * guide_weight
    assert selected < -5.8
    assert "clamp(guide_ev - pixel_ev, -1.25, 0.75)" in TONEMAP


def test_shadow_neutrality_is_exact() -> None:
    for ev in (-12.0, -8.0, -5.0, -3.0, 0.0, 3.0):
        mapped, _ = shadow_output_ev(ev, 0.0)
        assert mapped == ev


def test_blacks_neutrality_is_exact() -> None:
    for y in (0.0, 1e-6, 1e-4, 0.001, 0.01, 0.1, 0.15, 1.0):
        assert display_black_toe(y, 0.0) == y


def test_shadow_moderate_settings_have_perceptual_authority() -> None:
    # Representative dark-detail zone near the adaptive peak.
    source_ev = -4.0
    plus25, mask = shadow_output_ev(source_ev, 25.0)
    plus50, _ = shadow_output_ev(source_ev, 50.0)
    minus25, _ = shadow_output_ev(source_ev, -25.0)
    minus50, _ = shadow_output_ev(source_ev, -50.0)
    assert mask > 0.95
    assert plus25 - source_ev > 1.0
    assert plus50 - source_ev > 1.8
    assert source_ev - minus25 > 0.9
    assert source_ev - minus50 > 1.7


def test_shadows_separate_dark_detail_from_absolute_black_and_midtones() -> None:
    _, black_weight = shadow_output_ev(-10.0, 50.0)
    _, detail_weight = shadow_output_ev(-4.0, 50.0)
    _, mid_weight = shadow_output_ev(0.0, 50.0)
    assert black_weight < 0.05
    assert detail_weight > 0.95
    assert mid_weight < 0.05


def test_shadow_bounds_harden_internal_statistics_against_nonfinite_values() -> None:
    assert "select(-8.0, percentiles.p005, finite_scalar(percentiles.p005))" in TONEMAP
    assert "select(-5.0, percentiles.p05, finite_scalar(percentiles.p05))" in TONEMAP
    assert "select(0.0, percentiles.p50, finite_scalar(percentiles.p50))" in TONEMAP


def test_shadow_transfer_is_monotone_for_all_requested_settings_and_extreme_histograms() -> None:
    scenarios = [
        (-12.0, -8.0, -4.0),  # low-key
        (-8.0, -5.0, 0.0),    # normal
        (-5.0, -2.0, 1.0),    # high-key
        (-10.0, -6.0, -1.0),  # backlit/night-like
        (-7.0, -6.8, -6.2),   # nearly degenerate dark histogram
        (-2.0, -1.9, -1.2),   # nearly degenerate bright histogram
    ]
    for p005, p05, p50 in scenarios:
        lower, peak, upper = shadow_bounds(p005, p05, p50)
        assert math.isfinite(lower + peak + upper)
        assert lower < peak < upper
        for amount in (-100.0, -50.0, -25.0, 0.0, 25.0, 50.0, 100.0):
            previous = -1e30
            for i in range(12001):
                ev = -16.0 + 28.0 * i / 12000.0
                mapped, _ = shadow_output_ev(ev, amount, p005, p05, p50)
                assert mapped >= previous - 1e-10
                previous = mapped


def test_blacks_moderate_settings_have_toe_authority_but_protect_lower_midtones() -> None:
    near_black = 0.01
    lower_mid = 0.10
    for amount, minimum_ev in [(25.0, 0.70), (50.0, 1.20), (-25.0, 0.80), (-50.0, 1.45)]:
        mapped = display_black_toe(near_black, amount)
        delta = math.log2(mapped / near_black)
        assert abs(delta) > minimum_ev
    assert abs(math.log2(display_black_toe(lower_mid, 50.0) / lower_mid)) < 0.25
    assert abs(math.log2(display_black_toe(lower_mid, -50.0) / lower_mid)) < 0.30
    assert display_black_toe(0.15, 100.0) == 0.15


def test_blacks_transfer_is_monotone_for_all_requested_settings() -> None:
    for amount in (-100.0, -50.0, -25.0, 0.0, 25.0, 50.0, 100.0):
        previous = -1.0
        for i in range(1, 20001):
            y = 0.30 * i / 20000.0
            mapped = display_black_toe(y, amount)
            assert mapped >= previous - 1e-12
            previous = mapped


def test_blacks_is_view_adjacent_not_scene_masked_in_process_17() -> None:
    assert "blacks = 0.0;" in TONEMAP
    assert "display_linear = apply_display_blacks_toe_value(display_linear, params.basic_tone.w);" in ADJUSTMENTS
    assert "display_linear = apply_local_display_blacks(pos, display_linear);" in ADJUSTMENTS
    assert "name: \"display_black_toe\"" in GPU
    assert GPU.index('name: "view_transform"') < GPU.index('name: "display_black_toe"') < GPU.index('name: "output_encoding"')


def test_display_blacks_math_has_analytic_monotonicity_margin() -> None:
    assert "A < 2/ln(2)=2.885 EV" in TONEMAP
    assert "select(3.10, 2.60, amount >= 0.0)" in TONEMAP
    assert 2.60 < 2.0 / math.log(2.0)


def test_local_masks_scale_low_tone_strength_not_fully_adjusted_result() -> None:
    assert "shadows = shadows * clamp(low_tone_strength, 0.0, 1.0);" in TONEMAP
    assert "let amount = basic_low_tone_control(value) * weight;" in ADJUSTMENTS
    assert "apply_local_basic_tone_values_with_low_strength(" in ADJUSTMENTS


def test_highlights_and_whites_keep_existing_ranges() -> None:
    assert "signed_tone_range(highlights, 1.90, 1.15) * masks.z" in TONEMAP
    assert "signed_tone_range(whites, 1.25, 1.40) * masks.w" in TONEMAP


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
    assert "return rgb * exp2" in TONEMAP


def test_preview_detail_and_tiled_export_share_tone_statistics_and_output_stage() -> None:
    assert "pub fn inherit_tone_statistics" in GPU
    assert EXPORT.count("inherit_tone_statistics") >= 2
    assert '"apply_view_node"' in GPU
    assert "tone_stats_buffer" in GPU
