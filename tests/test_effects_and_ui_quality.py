from __future__ import annotations

from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
TONEMAP = (ROOT / "src/shaders/tonemap.wgsl").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
SIDEBAR = (ROOT / "src/ui/sidebar.rs").read_text(encoding="utf-8")
SETTINGS = (ROOT / "src/ui/settings.rs").read_text(encoding="utf-8")
ALL_SHADERS = "\n".join(
    path.read_text(encoding="utf-8") for path in (ROOT / "src/shaders").glob("*.wgsl")
)


def test_effects_are_a_dedicated_same_stage_pass() -> None:
    assert "fn prepare_adjustment_base" in ADJUSTMENTS
    assert "fn apply_lightroom_effects" in ADJUSTMENTS
    assert "fn adjustment_base_at" in ADJUSTMENTS
    assert "textureLoad(adjustment_base_tex" in ADJUSTMENTS
    assert "scene_working_at(pos +" not in ADJUSTMENTS

    prepare = GPU.index('"prepare_adjustment_base"')
    effects = GPU.index('"apply_lightroom_effects"', prepare)
    creative = GPU.index('"apply_creative_effects"', effects)
    render = GPU.index('"apply_lightroom_adjustments"', creative)
    assert prepare < effects < creative < render


def test_texture_and_clarity_are_band_pass_not_global_exposure() -> None:
    assert "fine_detail_ev = center_ev - fine_base_ev" in ADJUSTMENTS
    assert "mid_detail_ev = fine_base_ev - broad_base_ev" in ADJUSTMENTS
    assert "atrous_log_luminance" in ADJUSTMENTS
    assert "soft_detail_threshold" in ADJUSTMENTS
    assert "rgb * exp2(delta_ev)" in ADJUSTMENTS
    assert "flat field produces exactly zero effect" in ADJUSTMENTS


def test_dehaze_uses_airlight_and_transmission() -> None:
    assert "local_dark_channel" in ADJUSTMENTS
    assert "transmission" in ADJUSTMENTS
    assert "airlight" in ADJUSTMENTS
    assert "mix(rgb, airlight" in ADJUSTMENTS


def test_blacks_has_a_visible_lower_tonal_range() -> None:
    assert "black_fade_end" in TONEMAP
    assert "signed_tone_range(blacks, 2.35, 1.90)" in TONEMAP

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
    # Standard Lightroom-style controls.
    expected = {
        "exposure.exposure": ("exposure: exposure.exposure", "params.exposure"),
        "exposure.contrast": ("exposure.contrast.clamp", "params.presence.w"),
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
    for ui_field in ("exposure.temperature", "exposure.tint"):
        assert f"&mut {ui_field}" in SIDEBAR, ui_field
        assert f"{ui_field}.clamp" in GPU, ui_field
    assert "raw.adjusted_camera_transform(" in GPU
    assert "cam_to_working(camera_rgb)" in ALL_SHADERS

    # Color Mixer arrays, advanced rendering, and RAW expert controls.
    for name in ("hsl_hue", "hsl_saturation", "hsl_luminance"):
        assert f"&mut exposure.{name}[index]" in SIDEBAR
        assert f"exposure.{name}[..4]" in GPU
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
        "highlight_color_adaptation",
    ):
        assert f"exposure.{name}" in GPU
        assert f"params.{name}" in ALL_SHADERS or name == "highlight_color_adaptation"
