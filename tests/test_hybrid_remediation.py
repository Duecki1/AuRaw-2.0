"""Source-policy checks for the hybrid remediation.

These checks intentionally verify repository wiring and required implementation
patterns. They do not execute Rust, compile WGSL, validate GPU synchronization,
or establish rendered image quality; those suites remain mandatory before merge.
"""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

def text(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def test_camera_space_denoise_and_green_noise_canonicalization() -> None:
    noise = text("src/shaders/noise.wgsl")
    gpu = text("src/pipeline/gpu.rs")
    assert "NR_SIGNAL_WEIGHTS" in noise
    assert "fn nr_opponents" in noise
    assert "NR_LUMA_WEIGHTS" not in noise
    assert "fn canonicalize_green_noise" in gpu
    assert "coefficients[1] = green" in gpu
    assert "coefficients[3] = green" in gpu


def test_color_boundary_policy_preserves_valid_inputs_and_avoids_detail_chroma_loss() -> None:
    profile = text("src/shaders/profile.wgsl")
    detail = text("src/shaders/detail_scale_space.wgsl")
    color = text("src/shaders/color.wgsl")
    assert "if rgb_is_unit(rgb)" in profile
    assert "return clamp(rgb" in profile
    assert "gamut_project_nonnegative_rec2020(rgb * exp2(delta_ev))" in detail
    assert "perceptual_gamut_compress_nonnegative_rec2020(rgb * exp2(delta_ev))" not in detail
    assert "return clamp(" in color


def test_blacks_and_release_validation_fixes_are_present() -> None:
    tone = text("src/shaders/tonemap.wgsl")
    gpu = text("src/pipeline/gpu.rs")
    resources = text("src/pipeline/gpu/resources.rs")
    build = text("build.rs")
    assert "if luminance <= 0.0 || luminance >= pivot" in tone
    assert "GPU render-plan mismatch" in gpu
    assert "pub(super) fn work_shader_source" in resources
    assert "-> Result<Cow<'_, str>>" in resources
    assert "marker_count == 0" in resources
    assert "allow_or_fail_without_desktop_libraw" in build
    assert "AURAW_ALLOW_NO_LIBRAW=1" in build
    assert "fn allow_no_libraw() -> bool" in build
    assert 'matches!(value.as_str(), "1" | "true")' in build
    assert 'var_os("AURAW_ALLOW_NO_LIBRAW").is_some()' not in build


def test_uhhhyea_reliability_infrastructure_is_retained() -> None:
    lifecycle = text("src/app/lifecycle.rs")
    resources = text("src/pipeline/gpu/resources.rs")
    readback = text("src/pipeline/gpu/readback.rs")
    assert "transactional display-profile update failed" in lifecycle
    assert "restore display ICC LUT" in lifecycle
    assert "GpuResourcePlan" in resources
    assert "aggregate" in resources.lower()
    assert "checked_mul" in readback
    assert (ROOT / "scripts/check-gradle-wrapper.py").is_file()
