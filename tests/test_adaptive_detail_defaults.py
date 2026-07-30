from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
NOISE = (ROOT / "src/pipeline/noise.rs").read_text(encoding="utf-8")
RAW_LOADER = (ROOT / "src/pipeline/raw_loader.rs").read_text(encoding="utf-8")
LIFECYCLE = (ROOT / "src/app/lifecycle.rs").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
DEVELOP_EXPORT = (ROOT / "src/bin/auraw-develop-export.rs").read_text(encoding="utf-8")
SHADER_NOISE = (ROOT / "src/shaders/noise.wgsl").read_text(encoding="utf-8")
SHADER_COLOR_DENOISE = (ROOT / "src/shaders/color_denoise.wgsl").read_text(encoding="utf-8")
SIDECAR = (ROOT / "src/sidecar.rs").read_text(encoding="utf-8")


def test_noise_estimator_calibrates_the_selected_flat_direction() -> None:
    assert "FLAT_MIN_SECOND_DIFFERENCE_MEDIAN_SQUARED: f32 = 0.163" in NOISE
    assert "median_sq / FLAT_MIN_SECOND_DIFFERENCE_MEDIAN_SQUARED" in NOISE
    assert "median_sq / 0.454_936_4" not in NOISE


def test_new_raws_derive_detail_defaults_from_the_sensor_profile() -> None:
    assert "pub struct AdaptiveDetailDefaults" in NOISE
    assert "pub fn adaptive_detail_defaults" in NOISE
    assert "METADATA_VARIANCE_FLOOR" in NOISE
    assert "smoothstep(0.018, 0.080, relative_signal_sigma)" in NOISE
    assert "smoothstep(0.035, 0.160, relative_opponent_sigma)" in NOISE
    assert "pub fn apply_adaptive_detail_defaults" in RAW_LOADER
    assert "original_raw.apply_adaptive_detail_defaults(&mut rendered_exposure)" in LIFECYCLE
    assert "original_raw.apply_adaptive_detail_defaults(&mut edits.exposure)" in EXPORT


def test_migration_only_replaces_untouched_historical_detail_defaults() -> None:
    assert "has_legacy_default_detail_settings" in LIFECYCLE
    assert "loaded.migrated" in LIFECYCLE
    assert "has_legacy_default_detail_settings" in EXPORT
    assert "0x4155_5241_5700_0004" in SIDECAR


def test_detail_comparison_exporter_exposes_all_relevant_controls() -> None:
    for control in (
        "luminance_denoise",
        "color_denoise",
        "denoise_detail",
        "denoise_quality",
        "sharpen_amount",
        "sharpen_radius",
        "sharpen_detail",
        "sharpen_masking",
    ):
        assert f'"{control}"' in DEVELOP_EXPORT
    assert "--crop" in DEVELOP_EXPORT
    assert "--report-detail-defaults" in DEVELOP_EXPORT


def test_denoise_uses_continuous_perceptual_strength_and_dense_chroma_wavelets() -> None:
    assert "fn nr_perceptual_strength" in SHADER_NOISE
    assert "nr_perceptual_strength(params.noise_options.x, 3.2)" in SHADER_NOISE
    assert "0.5 * (rgb.r - rgb.b)" in SHADER_NOISE
    assert "fn nr_opponent_variance" in SHADER_NOISE
    assert "for (var y = -extent; y <= extent; y = y + 1)" in SHADER_COLOR_DENOISE
    assert "for (var x = -extent; x <= extent; x = x + 1)" in SHADER_COLOR_DENOISE
    assert "let requested = clamp(params.chroma_denoise, 0.0, 1.0)" in SHADER_COLOR_DENOISE
    assert "let filtered_opponents = low_opponents + opponent_detail * retained" in SHADER_COLOR_DENOISE
    assert "nr_from_signal_opponents(center_signal, filtered_opponents)" in SHADER_COLOR_DENOISE
