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


def test_process_25_is_the_single_runtime_renderer() -> None:
    assert "pub const BASIC_TONE_RESPONSE_PROCESS_VERSION: u32 = 16;" in BASIC
    assert "pub const PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION: u32 = 17;" in BASIC
    assert "pub const LIGHTROOM_BASIC_MATCH_PROCESS_VERSION: u32 = 18;" in BASIC
    assert "pub const ADAPTIVE_DETAIL_DEFAULTS_PROCESS_VERSION: u32 = 19;" in BASIC
    assert "pub const MULTISCALE_COLOR_DENOISE_PROCESS_VERSION: u32 = 20;" in BASIC
    assert "pub const EDGE_AWARE_COLOR_DENOISE_PROCESS_VERSION: u32 = 21;" in BASIC
    assert "pub const SCALE_AWARE_COLOR_DENOISE_PROCESS_VERSION: u32 = 22;" in BASIC
    assert "pub const LIGHTROOM_VIGNETTE_PROCESS_VERSION: u32 = 23;" in BASIC
    assert "pub const AI_DENOISE_PROCESS_VERSION: u32 = 24;" in BASIC
    assert "pub const AI_DENOISE_REMOSAIC_PROCESS_VERSION: u32 = 25;" in BASIC
    assert "pub const LIGHTROOM_HIGH_QUALITY_PROCESS_VERSION: u32 = 26;" in BASIC
    assert "pub const AI_DENOISE_SEAMLESS_CACHE_PROCESS_VERSION: u32 = 27;" in BASIC
    assert "pub const AI_DENOISE_CFA_CACHE_PROCESS_VERSION: u32 = 28;" in BASIC
    assert "pub const CURRENT_PROCESS_VERSION: u32 = AI_DENOISE_CFA_CACHE_PROCESS_VERSION;" in BASIC
    assert "const BASIC_TONE_RESPONSE_PROCESS_VERSION: u32 = 16u;" in TONEMAP
    assert "const PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION: u32 = 17u;" in TONEMAP
    assert "const LIGHTROOM_BASIC_MATCH_PROCESS_VERSION: u32 = 18u;" in TONEMAP
    assert "10..=CURRENT_PROCESS_VERSION" in BASIC
    assert "self.process_version = CURRENT_PROCESS_VERSION;" in BASIC
    assert "CURRENT_PROCESS_VERSION," in GPU
    assert "render_graph_flags()," in GPU


def test_historical_low_tone_formulas_are_inaccessible_to_normal_edits() -> None:
    assert "if params.process_info.x < PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION" in TONEMAP
    assert "apply_blacks_toe_v2" in TONEMAP
    assert "shadow_mask = shadow_mask * mix(0.28, 1.0, toe_guard)" in TONEMAP
    assert "signed_tone_range(shadows, 2.35, 3.20) * shadow_mask" in TONEMAP
    assert "process_info: [" in GPU
    assert "CURRENT_PROCESS_VERSION," in GPU


def test_global_controls_do_not_switch_process_versions() -> None:
    assert "exposure.process_version =" not in DEVELOP
    assert "exposure.process_version ==" not in DEVELOP


def test_local_controls_do_not_switch_process_versions() -> None:
    assert "let shadows_before = adjustment.shadows;" in MASKS
    assert "let blacks_before = adjustment.blacks;" in MASKS
    assert "adjustment.shadows != shadows_before || adjustment.blacks != blacks_before" in MASKS
    assert "app.exposure.process_version =" not in MASKS
    assert "app.exposure.process_version ==" not in MASKS

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


def test_shadow_bounds_harden_internal_statistics_against_nonfinite_values() -> None:
    assert "select(-8.0, percentiles.p005, finite_scalar(percentiles.p005))" in TONEMAP
    assert "select(-5.0, percentiles.p05, finite_scalar(percentiles.p05))" in TONEMAP
    assert "select(0.0, percentiles.p50, finite_scalar(percentiles.p50))" in TONEMAP


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


def test_blacks_is_view_adjacent_not_scene_masked_in_current_process() -> None:
    assert "blacks = 0.0;" in TONEMAP
    assert "display_linear = apply_display_blacks_toe_value(display_linear, params.basic_tone.w);" in ADJUSTMENTS
    assert "display_linear = apply_local_display_blacks(pos, display_linear);" in ADJUSTMENTS
    assert "name: \"display_black_toe\"" in GPU
    assert GPU.index('name: "view_transform"') < GPU.index('name: "display_black_toe"') < GPU.index('name: "output_encoding"')


def test_display_blacks_math_uses_measured_asymmetric_endpoints() -> None:
    assert "let weight = 0.08 + 0.92 * exp2(-luminance / 0.035);" in TONEMAP
    assert "amount * 1.75 * weight * hdr_guard" in TONEMAP
    assert "tone_smoothstep(0.012, 0.030, luminance)" in TONEMAP
    assert "10.50 * deep + tail" in TONEMAP


def test_local_masks_scale_low_tone_strength_not_fully_adjusted_result() -> None:
    assert "shadows = shadows * clamp(low_tone_strength, 0.0, 1.0);" in TONEMAP
    assert "let amount = basic_low_tone_control(value) * weight;" in ADJUSTMENTS
    assert "apply_local_basic_tone_values_with_low_strength(" in ADJUSTMENTS


def test_highlights_and_whites_use_lightroom_calibrated_ranges() -> None:
    assert "signed_tone_range(highlights, 1.35, 1.00)" in TONEMAP
    assert "percentiles.p50 - 0.35" in TONEMAP
    assert "percentiles.p95 + 0.45" in TONEMAP
    assert "percentiles.p05 - 0.15" in TONEMAP
    assert "percentiles.p50 + 0.55" in TONEMAP
    assert "fn lightroom_positive_whites_offset_ev" in TONEMAP
    assert "return min(whites * 0.95, monotone_limit) * mask;" in TONEMAP


def test_dehaze_endpoint_uses_bounded_ambient_relative_transfer() -> None:
    assert "let shaped_position = pow(ambient_position, 0.33);" in ADJUSTMENTS
    assert "let mid_position_hump = 0.30 * shaped_position * (1.0 - shaped_position);" in ADJUSTMENTS
    assert "mix(0.008, 0.012, haze_likelihood)" in ADJUSTMENTS
    assert "exp2(-amount * 0.90 * tone_mask)" in ADJUSTMENTS
    assert "haze * mix(0.045, 0.23, position_weight)" in ADJUSTMENTS
    assert "haze * mix(0.32, 0.27, haze_likelihood)" in ADJUSTMENTS


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
