from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
CAPTURE_DETAIL = (ROOT / "src/shaders/detail_capture.wgsl").read_text(encoding="utf-8")
TONEMAP = (ROOT / "src/shaders/tonemap.wgsl").read_text(encoding="utf-8")
DEVELOP = (ROOT / "src/ui/sidebar/develop.rs").read_text(encoding="utf-8")
NAVIGATION = (ROOT / "src/ui/sidebar/navigation.rs").read_text(encoding="utf-8")
PROCESSING = (ROOT / "src/pipeline/processing.rs").read_text(encoding="utf-8")


def test_capture_sharpening_has_lightroom_style_controls_and_defaults() -> None:
    for field in (
        "sharpen_amount",
        "sharpen_radius",
        "sharpen_detail",
        "sharpen_masking",
    ):
        assert f"pub {field}: f32" in BASIC
        assert f"exposure.{field}" in GPU
        assert f"&mut exposure.{field}" in DEVELOP

    assert "default_sharpen_amount()" in BASIC
    assert "40.0" in BASIC
    assert "default_sharpen_radius()" in BASIC
    assert "default_sharpen_detail()" in BASIC
    assert '(AdjustmentSection::Detail, "Detail")' in NAVIGATION


def test_capture_sharpening_is_edge_aware_luminance_preserving_and_scale_aware() -> None:
    assert "fn apply_capture_sharpening" in CAPTURE_DETAIL
    assert "fn capture_sharpen_blur_ev" in CAPTURE_DETAIL
    assert "fn capture_sharpen_edge_strength" in CAPTURE_DETAIL
    assert "capture_detail_scale()" in CAPTURE_DETAIL
    assert "sqrt(presence_reference_scale())" in CAPTURE_DETAIL
    assert "let range = exp(-3.4 * delta * delta)" in CAPTURE_DETAIL
    assert "edge_mask = smoothstep" in CAPTURE_DETAIL
    assert "capture_local_ev_bounds" in CAPTURE_DETAIL
    assert "capture_impulse_coherence" in CAPTURE_DETAIL
    assert "fn capture_noise_ev_sigma" in CAPTURE_DETAIL
    assert "params.noise_read.rgb + params.noise_shot.rgb" in CAPTURE_DETAIL
    assert "let edge_noise_relief = smoothstep" in CAPTURE_DETAIL
    assert "let detail_threshold = max(fixed_threshold, sensor_threshold)" in CAPTURE_DETAIL
    assert "let strength = amount * mix(4.20, 6.00, detail)" in CAPTURE_DETAIL
    assert "return max(rgb * exp2(sharpen_ev)" in CAPTURE_DETAIL
    # Stage ordering is checked from Naga's WGSL call graph in
    # src/pipeline/gpu/tests.rs rather than by slicing source text.
    assert "exposure.sharpen_amount.abs() > 1e-6" in PROCESSING
    optional_gate = GPU[
        GPU.index("fn needs_intermediate_adjustment_passes") : GPU.index("struct Pass")
    ]
    assert "self.creative_effects[3].abs()" not in optional_gate


def test_exposure_partially_retargets_adaptive_tone_without_reanalysis() -> None:
    assert "exposure.exposure.to_bits()" in GPU
    assert "fn adaptive_tone_user_exposure_ev" in TONEMAP
    assert "params.process_info.z" in TONEMAP
    assert "+ adaptive_tone_user_exposure_ev()" in TONEMAP
    assert "adaptive_tone_user_exposure_ev() * 0.35" in TONEMAP
    assert "SCENE_DISPLAY_BOUNDARY_PROCESS_VERSION: u32 = 13" in BASIC
