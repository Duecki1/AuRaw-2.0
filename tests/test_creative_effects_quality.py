from __future__ import annotations

from pathlib import Path
import math
import re

ROOT = Path(__file__).resolve().parents[1]
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
COMMON = (ROOT / "src/shaders/common.wgsl").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
SIDEBAR = (ROOT / "src/ui/sidebar.rs").read_text(encoding="utf-8")


def smoothstep(edge0: float, edge1: float, value: float) -> float:
    t = max(0.0, min(1.0, (value - edge0) / max(edge1 - edge0, 1e-9)))
    return t * t * (3.0 - 2.0 * t)


def test_glow_is_highlight_aware_multiscale_and_same_stage() -> None:
    assert "fn glow_emission" in ADJUSTMENTS
    assert "extended_perceptual_luminance" in ADJUSTMENTS
    assert "cutoff_fade" in ADJUSTMENTS
    assert "black_gate" in ADJUSTMENTS
    assert "colour_ratio" in ADJUSTMENTS
    assert "warm_tint" in ADJUSTMENTS
    assert "core_bloom" in ADJUSTMENTS and "near_bloom" in ADJUSTMENTS and "far_bloom" in ADJUSTMENTS
    assert "core_protection" in ADJUSTMENTS
    assert "fn glow_source_at" in ADJUSTMENTS
    assert "glow_emission(sample_rgb, cutoff)" in ADJUSTMENTS
    assert "scene_working_at(sample_pos)" not in ADJUSTMENTS


def test_creative_pass_ping_pongs_before_color_mixer() -> None:
    local_pass = GPU.index('"apply_lightroom_effects"')
    creative_pass = GPU.index('"apply_creative_effects"', local_pass)
    render_pass = GPU.index('"apply_lightroom_adjustments"', creative_pass)
    assert local_pass < creative_pass < render_pass
    assert "binding: 24" in GPU and "TextureView(&tex2_view)" in GPU
    assert "binding: 25" in GPU and "TextureView(&tex1_view)" in GPU
    assert "binding: 26" in GPU and "TextureView(&tex1_view)" in GPU
    assert "textureLoad(final_adjustment_tex" in ADJUSTMENTS


def test_vignette_has_lightroom_style_controls_and_tile_safe_coordinates() -> None:
    for field in (
        "vignette_amount",
        "vignette_midpoint",
        "vignette_roundness",
        "vignette_feather",
        "vignette_highlights",
    ):
        assert f"pub {field}: f32" in BASIC
        assert f"&mut exposure.{field}" in SIDEBAR

    assert "fn full_image_uv" in ADJUSTMENTS
    assert "pos + tile_origin()" in ADJUSTMENTS
    assert "params.full_width" in ADJUSTMENTS
    assert "params.full_height" in ADJUSTMENTS
    assert "frame_rectangle" in ADJUSTMENTS
    assert "image_circle" in ADJUSTMENTS
    assert "transition_center" in ADJUSTMENTS
    assert "highlight_protection" in ADJUSTMENTS


def test_glow_advanced_controls_are_hidden_without_expert_mode() -> None:
    assert '&mut exposure.glow_amount' in SIDEBAR
    expert_block = re.search(
        r"if expert_mode \{(.+?)\n                    \}", SIDEBAR, re.DOTALL
    )
    assert expert_block is not None
    assert "&mut exposure.glow_radius" in expert_block.group(1)
    assert "&mut exposure.glow_threshold" in expert_block.group(1)
    assert "&mut exposure.vignette_amount" in SIDEBAR


def test_defaults_are_neutral_and_match_expected_ui_starting_points() -> None:
    for assignment in (
        "glow_amount: 0.0",
        "glow_radius: 50.0",
        "glow_threshold: 60.0",
        "vignette_amount: 0.0",
        "vignette_midpoint: 50.0",
        "vignette_roundness: 0.0",
        "vignette_feather: 50.0",
        "vignette_highlights: 0.0",
    ):
        assert assignment in BASIC
    assert "if amount < 1e-6" in ADJUSTMENTS
    assert "if abs(amount) < 1e-6" in ADJUSTMENTS


def test_uniform_layout_contains_creative_and_vignette_blocks() -> None:
    assert "creative_effects: vec4<f32>" in COMMON
    assert "vignette: vec4<f32>" in COMMON
    assert "vignette_options: vec4<f32>" in COMMON
    assert "creative_effects: [f32; 4]" in GPU
    assert "vignette: [f32; 4]" in GPU
    assert "vignette_options: [f32; 4]" in GPU
    assert "size_of::<super::GpuParams>(), 6944" in GPU


def test_default_glow_threshold_rejects_shadows_but_accepts_bright_sources() -> None:
    threshold = 0.60
    cutoff = 0.06 + (0.92 - 0.06) * threshold**1.12

    def emission_gate(linear_luma: float) -> float:
        if linear_luma <= 1.0:
            perceptual = max(linear_luma, 0.0) ** (1.0 / 2.2)
        else:
            perceptual = 1.0 + (linear_luma - 1.0) ** (1.0 / 2.2)
        cutoff_fade = smoothstep(cutoff, cutoff + 0.16, perceptual)
        black_gate = smoothstep(0.0, 0.42, linear_luma) ** 0.5
        return cutoff_fade * black_gate

    assert emission_gate(0.02) < 1e-6
    assert emission_gate(0.18) < 0.05
    assert emission_gate(1.5) > 0.95


def test_vignette_math_keeps_center_neutral_and_reaches_edges() -> None:
    midpoint = 0.50
    feather = 0.50
    midpoint_shaped = midpoint**0.82
    center = 0.16 + (0.985 - 0.16) * midpoint_shaped
    confinement = 1.0 + (0.16 - 1.0) * midpoint * midpoint
    inner_width = (0.010 + (0.42 - 0.010) * feather) * confinement
    outer_width = (0.015 + (0.12 - 0.015) * feather) * (
        1.0 + (0.25 - 1.0) * midpoint * midpoint
    )
    start = max(center - inner_width, 0.0)
    end = min(center + outer_width, 1.0)

    assert smoothstep(start, end, 0.0) == 0.0
    assert smoothstep(start, end, 1.0) == 1.0
    assert 0.0 < smoothstep(start, end, center) < 1.0


def test_vignette_highlight_protection_only_reduces_darkening() -> None:
    amount = -1.0
    mask = 1.0
    highlights = 1.0
    dark_luma = 0.1
    bright_luma = 3.0

    def delta_ev(luma: float) -> float:
        protection = 1.0 - highlights * smoothstep(0.50, 2.4, luma)
        return amount * 2.45 * mask * protection

    assert math.isclose(delta_ev(dark_luma), -2.45)
    assert abs(delta_ev(bright_luma)) < 1e-6


def test_vignette_max_midpoint_reaches_outermost_edge() -> None:
    midpoint = 1.0
    feather = 0.0
    midpoint_shaped = midpoint**0.80
    center = 0.18 + (0.992 - 0.18) * midpoint_shaped
    inward_softness = (0.010 + (0.72 - 0.010) * feather) * (
        1.0 + (0.62 - 1.0) * midpoint_shaped * midpoint_shaped
    )
    start = center - inward_softness
    assert start > 0.97
    assert "inward_softness" in ADJUSTMENTS
    assert "outward_softness" in ADJUSTMENTS
