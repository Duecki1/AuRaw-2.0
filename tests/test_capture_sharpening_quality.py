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
    assert "return max(rgb * exp2(sharpen_ev)" in CAPTURE_DETAIL
    assert "fn apply_scene_tone_node" in ADJUSTMENTS
    sharpen_stage = ADJUSTMENTS[
        ADJUSTMENTS.index("fn apply_scene_tone_node") :
        ADJUSTMENTS.index("fn apply_scene_effects_node")
    ]
    sharpen = sharpen_stage.index("rgb = apply_capture_sharpening(pos, rgb);")
    profile_tone = sharpen_stage.index("rgb = apply_profile_view_tone(rgb);")
    adaptive_tone = sharpen_stage.index("rgb = apply_lightroom_tone(rgb, pos);")
    local_tone = sharpen_stage.index("rgb = apply_local_scene_tone_nodes(pos, rgb);")
    assert sharpen < profile_tone < adaptive_tone < local_tone
    assert "rgb = apply_capture_sharpening(pos, rgb);" not in ADJUSTMENTS[
        ADJUSTMENTS.index("fn apply_scene_effects_node") :
        ADJUSTMENTS.index("fn prepare_glow_source")
    ]
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
