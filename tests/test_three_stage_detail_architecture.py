from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
CAPTURE = (ROOT / "src/shaders/detail_capture.wgsl").read_text(encoding="utf-8")
SCALE = (ROOT / "src/shaders/detail_scale_space.wgsl").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")
BUILD = (ROOT / "build.rs").read_text(encoding="utf-8")


def test_detail_stages_are_physically_separated() -> None:
    assert 'include_str!("../shaders/detail_capture.wgsl")' in GPU
    assert 'include_str!("../shaders/detail_scale_space.wgsl")' in GPU
    assert '"src/shaders/detail_capture.wgsl"' in BUILD
    assert '"src/shaders/detail_scale_space.wgsl"' in BUILD
    assert "fn apply_capture_sharpening" not in ADJUSTMENTS
    assert "fn apply_texture_and_clarity_values" not in ADJUSTMENTS
    assert "fn apply_capture_sharpening" in CAPTURE
    assert "fn apply_texture_and_clarity_values" in SCALE


def test_capture_precedes_creative_scale_space() -> None:
    tone = ADJUSTMENTS[
        ADJUSTMENTS.index("fn apply_scene_tone_node") :
        ADJUSTMENTS.index("fn apply_scene_effects_node")
    ]
    effects = ADJUSTMENTS[
        ADJUSTMENTS.index("fn apply_scene_effects_node") :
        ADJUSTMENTS.index("fn prepare_glow_source")
    ]
    assert "apply_capture_sharpening" in tone
    assert "apply_texture_and_clarity_values" not in tone
    assert "apply_texture_and_clarity_values" in effects
    assert "apply_capture_sharpening" not in effects


def test_creative_bands_do_not_double_count_fine_residual() -> None:
    assert "texture_band_ev = center_ev - fine_base_ev" in SCALE
    assert "clarity_band_ev = fine_base_ev - broad_base_ev" in SCALE
    assert "center_ev - broad_base_ev" not in SCALE
    assert "creative_edge_guard" in SCALE


def test_output_sharpen_runs_after_final_size_resampling() -> None:
    assert "struct FinalSizeOutputSharpen" in EXPORT
    assert "fn output_sharpen_linear_row" in EXPORT
    assert "after resize/geometry" in EXPORT
    assert "output_sharpen.push_row" in EXPORT
    assert "output_sharpen.finish" in EXPORT
    assert "local_max * 1.015" in EXPORT
    assert "detail_ev.abs() - threshold" in EXPORT
