from __future__ import annotations

from tests.source_helpers import read_source_tree
from pathlib import Path
import math
import re

ROOT = Path(__file__).resolve().parents[1]
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
COMMON = (ROOT / "src/shaders/common.wgsl").read_text(encoding="utf-8")
GPU = read_source_tree(ROOT / "src/pipeline/gpu.rs")
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
SIDEBAR = read_source_tree(ROOT / "src/ui/sidebar.rs")


def smoothstep(edge0: float, edge1: float, value: float) -> float:
    t = max(0.0, min(1.0, (value - edge0) / max(edge1 - edge0, 1e-9)))
    return t * t * (3.0 - 2.0 * t)


def test_glow_is_highlight_aware_cascaded_and_same_stage() -> None:
    assert "fn glow_emission" in ADJUSTMENTS
    assert "extended_perceptual_luminance" in ADJUSTMENTS
    assert "cutoff_fade" in ADJUSTMENTS
    assert "black_gate" in ADJUSTMENTS
    assert "colour_ratio" in ADJUSTMENTS
    assert "warm_tint" in ADJUSTMENTS
    assert "fn glow_diffuse_at" in ADJUSTMENTS
    for stage in range(5):
        assert f"fn diffuse_glow_{stage}" in ADJUSTMENTS
    assert "core_protection" in ADJUSTMENTS
    assert "fn prepare_glow_source" in ADJUSTMENTS
    assert "glow_emission(local_effects_at(pos), glow_cutoff())" in ADJUSTMENTS
    assert "sum = sum + glow_work_at(sample_pos) * weight" in ADJUSTMENTS
    assert "return mix(center, sum / max(sum_weight, 1e-6), stage_mix)" in ADJUSTMENTS
    assert "scene_working_at(sample_pos)" not in ADJUSTMENTS


def test_creative_pass_ping_pongs_before_color_mixer() -> None:
    local_pass = GPU.index('"apply_scene_effects_node"')
    glow_source = GPU.index('"prepare_glow_source"', local_pass)
    glow_start = GPU.index('"diffuse_glow_0"', glow_source)
    glow_end = GPU.index('"diffuse_glow_4"', glow_start)
    creative_pass = GPU.index('"apply_creative_effects"', glow_end)
    render_pass = GPU.index('"apply_view_node"', creative_pass)
    assert local_pass < glow_source < glow_start < glow_end < creative_pass < render_pass
    assert "binding: 24" in GPU and "TextureView(&tex1_view)" in GPU
    assert "binding: 25" in GPU and "TextureView(&tex2_view)" in GPU
    assert "binding: 26" in GPU and "TextureView(&tex2_view)" in GPU
    assert "binding: 30" in GPU and "TextureView(&display_linear_view)" in GPU
    assert "binding: 31" in GPU
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
    assert "let frame_ellipse = length(p);" in ADJUSTMENTS
    assert "frame_rectangle" in ADJUSTMENTS
    assert "image_circle" in ADJUSTMENTS
    assert "fn lightroom_vignette_opacity" in ADJUSTMENTS


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
    assert "size_of::<super::GpuParams>(), 25136" in GPU


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
    assert "if abs(midpoint - 0.5) >= 1e-6" in ADJUSTMENTS
    assert "let midpoint_power = exp2((midpoint - 0.5) * 1.4);" in ADJUSTMENTS
    assert "if abs(feather - 0.5) < 1e-6" in ADJUSTMENTS
    assert "let feather_power = exp2((0.5 - feather) * 1.3);" in ADJUSTMENTS
    assert "highlight_protection" in ADJUSTMENTS


def test_vignette_distance_is_invariant_across_frame_aspect_ratios() -> None:
    normalized_point = (0.72, 0.48)
    expected = math.hypot(*normalized_point)
    for width, height in ((7008, 4672), (6000, 6000), (6000, 4500), (8192, 3456)):
        pixel_x = normalized_point[0] * width * 0.5
        pixel_y = normalized_point[1] * height * 0.5
        distance = math.hypot(pixel_x / (width * 0.5), pixel_y / (height * 0.5))
        assert math.isclose(distance, expected, abs_tol=1e-12)


def test_vignette_is_a_post_view_black_or_white_edge_treatment() -> None:
    assert "return rgb * (1.0 - opacity);" in ADJUSTMENTS
    assert "return mix(rgb, vec3<f32>(1.0), opacity);" in ADJUSTMENTS
    creative = re.search(
        r"fn apply_creative_effects.+?textureStore\(creative_effects_out",
        ADJUSTMENTS,
        re.DOTALL,
    )
    assert creative is not None
    assert "apply_vignette" not in creative.group(0)
    view = re.search(r"fn apply_view_node.+?textureStore\(out_tex", ADJUSTMENTS, re.DOTALL)
    assert view is not None
    assert view.group(0).index("apply_local_display_blacks") < view.group(0).index(
        "apply_vignette"
    )
