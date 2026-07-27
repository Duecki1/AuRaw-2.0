from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BUILD = (ROOT / "build.rs").read_text(encoding="utf-8")
PREPROCESSOR = (ROOT / "build_support/shader_preprocessor.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
BAYER = (ROOT / "src/shaders/pass4.wgsl").read_text(encoding="utf-8")
XTRANS = (ROOT / "src/shaders/xtrans_pass7.wgsl").read_text(encoding="utf-8")
SHARED = (ROOT / "src/shaders/noise_ca_finish.wgsl").read_text(encoding="utf-8")


def test_noise_and_ca_finish_logic_has_one_shared_source() -> None:
    include = '// @include "noise_ca_finish.wgsl"'
    assert include in BAYER
    assert include in XTRANS
    for routine in (
        "fn finish_warped_pos",
        "fn finish_reference_bilinear",
        "fn finish_apply_ca",
        "fn finish_apply_legacy_chroma_denoise",
        "fn finish_apply_sensor_denoise",
    ):
        assert routine in SHARED
        assert routine not in BAYER
        assert routine not in XTRANS


def test_finish_templates_supply_cfa_specific_reference_adapters() -> None:
    assert "return rcd_reference_at(clamp_pos(pos));" in BAYER
    assert "return xt_high(pos);" in XTRANS
    assert "finish_reference_at(pos + vec2<i32>(dx, dy))" in SHARED
    assert "finish_reference_at(pos + direction * radius)" in SHARED


def test_build_script_expands_templates_into_out_dir() -> None:
    assert '#[path = "build_support/shader_preprocessor.rs"]' in BUILD
    assert "shader_preprocessor::generate_shader_sources" in BUILD
    assert 'pub const INCLUDE_DIRECTIVE: &str = "// @include ";' in PREPROCESSOR
    assert "pub fn generate_shader_sources(" in PREPROCESSOR
    assert "pub fn preprocess_shader(" in PREPROCESSOR
    assert '("pass4.wgsl", "pass4.generated.wgsl")' in PREPROCESSOR
    assert '("xtrans_pass7.wgsl", "xtrans_pass7.generated.wgsl")' in PREPROCESSOR
    assert '"src/shaders/noise_ca_finish.wgsl"' in BUILD
    assert 'env!("OUT_DIR"), "/pass4.generated.wgsl"' in GPU
    assert 'env!("OUT_DIR"), "/xtrans_pass7.generated.wgsl"' in GPU
