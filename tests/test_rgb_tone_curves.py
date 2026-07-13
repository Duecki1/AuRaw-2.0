from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
SIDEBAR = (ROOT / "src/ui/sidebar.rs").read_text(encoding="utf-8")
EDITOR = (ROOT / "src/ui/components/tone_curve_editor.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
COMMON = (ROOT / "src/shaders/common.wgsl").read_text(encoding="utf-8")
TONEMAP = (ROOT / "src/shaders/tonemap.wgsl").read_text(encoding="utf-8")


def test_curve_tabs_and_white_composite_curve_are_present() -> None:
    assert "pub enum ToneCurveTab" in APP
    for tab in ("Rgb", "Red", "Green", "Blue"):
        assert f"ToneCurveTab::{tab}" in SIDEBAR
    assert '(ToneCurveTab::Rgb, "RGB", egui::Color32::WHITE)' in SIDEBAR
    assert "Stroke::new(2.0, curve_color)" in EDITOR


def test_all_four_curves_have_independent_state_and_neutral_defaults() -> None:
    for field in ("tone_curve", "tone_curve_red", "tone_curve_green", "tone_curve_blue"):
        assert f"pub {field}: PointCurve" in BASIC
        assert f"{field}: PointCurve::linear()" in BASIC
    assert "sanitize_tone_curves" in BASIC


def test_rgb_curve_uniforms_match_between_rust_and_wgsl() -> None:
    for channel in ("red", "green", "blue"):
        for part in ("0", "1", "2", "3", "meta"):
            field = f"tone_curve_{channel}_{part}"
            assert f"{field}: [f32; 4]" in GPU
            assert f"{field}: vec4<f32>" in COMMON
    assert "size_of::<super::GpuParams>(), 1344" in GPU


def test_channel_curves_use_monotone_scene_referred_processing() -> None:
    assert "fn tone_curve_tangent(curve: u32" in TONEMAP
    assert "fn scene_curve_encode" in TONEMAP
    assert "fn scene_curve_decode" in TONEMAP
    assert "fn tone_curve_is_identity" in TONEMAP
    assert ".is_identity()" in GPU
    assert "fn apply_rgb_point_curves" in TONEMAP
    assert "apply_rgb_point_curves(apply_point_tone_curve(basic))" in TONEMAP
    assert "return clamp(hermite, min(p0.y, p1.y), max(p0.y, p1.y));" in TONEMAP
