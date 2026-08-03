from __future__ import annotations

from tests.source_helpers import read_source_tree
from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
DETAIL_SCALE = (ROOT / "src/shaders/detail_scale_space.wgsl").read_text(encoding="utf-8")
TONEMAP = (ROOT / "src/shaders/tonemap.wgsl").read_text(encoding="utf-8")
GPU = read_source_tree(ROOT / "src/pipeline/gpu.rs")
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
APP = read_source_tree(ROOT / "src/app.rs")
SIDEBAR = read_source_tree(ROOT / "src/ui/sidebar.rs")
SETTINGS = (ROOT / "src/ui/settings.rs").read_text(encoding="utf-8")
ALL_SHADERS = "\n".join(
    path.read_text(encoding="utf-8") for path in (ROOT / "src/shaders").glob("*.wgsl")
)


def test_effects_are_a_dedicated_same_stage_pass() -> None:
    assert "fn prepare_scene_node" in ADJUSTMENTS
    assert "fn apply_scene_effects_node" in ADJUSTMENTS
    assert "fn adjustment_base_at" in ADJUSTMENTS
    assert "textureLoad(adjustment_base_tex" in ADJUSTMENTS
    assert "scene_working_at(pos +" not in ADJUSTMENTS

    prepare = GPU.index('"prepare_scene_node"')
    sharpen_tone = GPU.index('"apply_scene_tone_node"', prepare)
    effects = GPU.index('"apply_scene_effects_node"', sharpen_tone)
    creative = GPU.index('"apply_creative_effects"', effects)
    render = GPU.index('"apply_view_node"', creative)
    assert prepare < sharpen_tone < effects < creative < render




def test_saturation_and_vibrance_keep_the_effects_pass_enabled() -> None:
    gate = GPU[
        GPU.index("fn needs_intermediate_adjustment_passes") : GPU.index("struct Pass")
    ]
    assert "self.saturation.abs() > 1e-6" in gate
    assert "self.vibrance.abs() > 1e-6" in gate
    assert "mask_adjust_2" in gate
    assert "params.needs_intermediate_adjustment_passes()" in GPU

def test_texture_and_clarity_are_adjacent_scale_space_bands() -> None:
    assert "texture_band_ev = center_ev - fine_base_ev" in DETAIL_SCALE
    assert "clarity_band_ev = fine_base_ev - broad_base_ev" in DETAIL_SCALE
    assert "creative_fine_base_ev" in DETAIL_SCALE
    assert "creative_broad_base_ev" in DETAIL_SCALE
    assert "atrous_log_luminance" in DETAIL_SCALE
    assert "soft_detail_threshold" in DETAIL_SCALE
    assert "rgb * exp2(delta_ev)" in DETAIL_SCALE
    assert "if abs(texture) < 1e-6 && abs(clarity) < 1e-6" in DETAIL_SCALE
    assert "let positive_texture_strength = 7.50" in DETAIL_SCALE
    assert "texture_ev = -negative_texture * smoothing" in DETAIL_SCALE
    assert "clarity_strength = select(1.55, 5.40" in DETAIL_SCALE
    assert "percentiles.p995 - percentiles.p005" in DETAIL_SCALE
    assert "clarity * mix(-1.25, 0.36, clarity_tone_position) * clarity_scene_gate" in DETAIL_SCALE
    assert "presence_step(1.65, 5)" in DETAIL_SCALE
    assert "presence_step(clarity_reference, 14)" in DETAIL_SCALE


def test_dehaze_uses_airlight_and_transmission() -> None:
    assert "haze_neighborhood" in ADJUSTMENTS
    assert "dark_ratio" in ADJUSTMENTS
    assert "transmission" in ADJUSTMENTS
    assert "airlight" in ADJUSTMENTS
    assert "(rgb - airlight * (1.0 - transmission)) / transmission" in ADJUSTMENTS
    assert "mix(rgb, airlight" in ADJUSTMENTS
    assert "tone_stats.percentiles_1.x + params.exposure" in ADJUSTMENTS
    assert "SCENE_MIDDLE_GREY * exp2(ambient_ev)" in ADJUSTMENTS
    assert "normalized_dark_ratio" in ADJUSTMENTS
    assert "presence_step(2.0, 6)" in ADJUSTMENTS


def test_blacks_has_a_visible_lower_tonal_range() -> None:
    assert "black_fade_end" in TONEMAP
    assert "apply_blacks_toe_v2" in TONEMAP
    assert "gamma = exp2(-1.25 * blacks)" in TONEMAP
    assert "gamma = exp2(1.25 * (-blacks))" in TONEMAP

    def smoothstep(edge0: float, edge1: float, value: float) -> float:
        x = max(0.0, min(1.0, (value - edge0) / max(edge1 - edge0, 1e-4)))
        return x * x * (3.0 - 2.0 * x)

    p005, p05, p50 = -8.0, -5.0, -1.0
    fade_end = min(p50 - 0.35, p05 + 3.00)
    upper = max(fade_end, p05 + 0.45)
    mask_at_fifth_percentile = 1.0 - smoothstep(p005 - 0.55, upper, p05)
    mask_near_median = 1.0 - smoothstep(p005 - 0.55, upper, p50)
    assert mask_at_fifth_percentile > 0.40
    assert mask_near_median < 1e-6


def test_point_curve_starts_with_only_endpoints() -> None:
    linear = re.search(r"pub const fn linear\(\) -> Self \{(.+?)\n    \}", BASIC, re.DOTALL)
    assert linear is not None
    body = linear.group(1)
    assert "len: 2" in body
    assert "[0.0, 0.0]" in body
    assert body.count("[1.0, 1.0]") == 7
    assert "[0.5, 0.5]" not in body


def test_expert_mode_is_disabled_by_default_and_gates_advanced_controls() -> None:
    assert APP.count("expert_mode: false") == 2
    assert 'ui.checkbox(&mut app.expert_mode, "Expert mode")' in SETTINGS
    assert "if !app.expert_mode" in SETTINGS
    assert "if app.expert_mode" in SIDEBAR
    gated = re.search(r"if app\.expert_mode \{(.+?)\n        \}", SIDEBAR, re.DOTALL)
    assert gated is not None
    assert "show_rendering" in gated.group(1)
    assert "show_raw" in gated.group(1)


def test_every_exposed_slider_is_connected_to_gpu_processing() -> None:
    # Standard Basic controls. Contrast is darktable's sigmoid slope.
    expected = {
        "exposure.exposure": ("exposure: exposure.exposure", "params.exposure"),
        "exposure.contrast": ("sigmoid_contrast_from_percent(exposure.contrast)", "params.sigmoid_power.x"),
        "exposure.highlights": ("exposure.highlights", "params.basic_tone.x"),
        "exposure.shadows": ("exposure.shadows", "params.basic_tone.y"),
        "exposure.whites": ("exposure.whites", "params.basic_tone.z"),
        "exposure.blacks": ("exposure.blacks", "params.basic_tone.w"),
        "exposure.vibrance": ("vibrance: exposure.vibrance", "params.vibrance"),
        "exposure.saturation": ("saturation: exposure.saturation", "params.saturation"),
        "exposure.texture": ("exposure.texture", "params.presence.x"),
        "exposure.clarity": ("exposure.clarity", "params.presence.y"),
        "exposure.dehaze": ("exposure.dehaze", "params.presence.z"),
        "exposure.glow_amount": ("exposure.glow_amount.clamp", "params.creative_effects.x"),
        "exposure.glow_radius": ("exposure.glow_radius.clamp", "params.creative_effects.y"),
        "exposure.glow_threshold": ("exposure.glow_threshold.clamp", "params.creative_effects.z"),
        "exposure.vignette_amount": ("exposure.vignette_amount.clamp", "params.vignette.x"),
        "exposure.vignette_midpoint": ("exposure.vignette_midpoint.clamp", "params.vignette.y"),
        "exposure.vignette_roundness": ("exposure.vignette_roundness.clamp", "params.vignette.z"),
        "exposure.vignette_feather": ("exposure.vignette_feather.clamp", "params.vignette.w"),
        "exposure.vignette_highlights": ("exposure.vignette_highlights.clamp", "params.vignette_options.x"),
    }
    for ui_field, (rust_mapping, shader_use) in expected.items():
        assert f"&mut {ui_field}" in SIDEBAR, ui_field
        assert rust_mapping in GPU, ui_field
        assert shader_use in ALL_SHADERS, ui_field

    # Global WB is camera/profile metadata-driven on the CPU; its result reaches
    # shaders through the live camera matrix and DCP interpolation weight.
    compact_gpu = re.sub(r"\s+", "", GPU)
    assert "temperature_offset_from_kelvin" in SIDEBAR
    assert "exposure.temperature=" in re.sub(r"\s+", "", SIDEBAR)
    assert "&mut exposure.tint" in SIDEBAR
    for ui_field in ("exposure.temperature", "exposure.tint"):
        assert f"{ui_field}.clamp" in compact_gpu, ui_field
    assert "raw.adjusted_camera_transform(" in GPU
    assert "cam_to_working(camera_rgb)" in ALL_SHADERS

    # Color Mixer arrays, advanced rendering, and RAW expert controls.
    for name in ("hsl_hue", "hsl_saturation", "hsl_luminance"):
        assert f"&mut exposure.{name}[index]" in SIDEBAR
        assert f"split_eight(exposure.{name})" in GPU
        assert f"split_eight(adjustment.{name})" in GPU
        assert f"params.{name}_0" in ALL_SHADERS
        assert f"params.{name}_1" in ALL_SHADERS

    for name in (
        "black_point",
        "chroma_denoise",
        "dual_threshold",
        "frequency_chroma",
        "ca_red",
        "ca_blue",
        "highlight_clip",
        "highlight_reconstruction",
    ):
        assert f"exposure.{name}" in GPU
        assert f"params.{name}" in ALL_SHADERS


def test_revised_adjustment_formulas_increment_the_process_version() -> None:
    assert "SCENE_DISPLAY_BOUNDARY_PROCESS_VERSION: u32 = 13" in BASIC
    assert "0..=7 =>" in BASIC
    assert "8 | 9 =>" in BASIC
    assert "self.exposure += LEGACY_GLOBAL_EXPOSURE_BACKEND_OFFSET_EV" in BASIC


def test_presence_and_color_controls_use_perceptual_bounded_mappings() -> None:
    assert "fn perceptual_control" in ALL_SHADERS
    assert "muted_weight" in ALL_SHADERS
    assert "neutral_guard" in ALL_SHADERS
    assert "creative_edge_guard" in DETAIL_SCALE
    assert "negative_texture" in DETAIL_SCALE
    assert "dark_ratio" in ADJUSTMENTS
    assert "hue_safe" in ADJUSTMENTS
    assert "chroma_boost" in ADJUSTMENTS


def test_global_temperature_is_presented_as_metadata_aware_kelvin() -> None:
    assert "MIN_TEMPERATURE_KELVIN: f32 = 1_901.0" in BASIC
    assert "MAX_TEMPERATURE_KELVIN: f32 = 25_000.0" in BASIC
    assert "temperature_kelvin_from_offset" in SIDEBAR
    assert "temperature_offset_from_kelvin" in SIDEBAR
    assert '"Temperature (K)"' in SIDEBAR
    assert "GLOBAL_TEMPERATURE_LIMIT" in GPU
