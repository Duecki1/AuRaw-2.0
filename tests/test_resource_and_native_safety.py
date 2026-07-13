from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
AI = (ROOT / "src/ai_masks.rs").read_text(encoding="utf-8")
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
RAW = (ROOT / "src/pipeline/raw_loader.rs").read_text(encoding="utf-8")
COLOR_PROFILE = (ROOT / "src/pipeline/color_profile.rs").read_text(encoding="utf-8")
CARGO = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
CI = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")


def test_native_runtime_is_never_downloaded_or_archive_extracted() -> None:
    assert "onnxruntime-linux" not in AI
    assert "libonnxruntime.so" not in AI
    assert "flate2" not in CARGO
    assert "tar =" not in CARGO
    assert "ort::init_from" in AI
    assert "expected_sha256" in AI
    assert "selected ONNX Runtime changed after approval" in AI
    assert "DESKTOP_RUNTIME_IDENTITY" in AI
    assert "builder.with_name(\"AuRaw\").commit()" in AI
    assert "a different ONNX Runtime is already active" in AI


def test_downloaded_model_is_size_and_sha256_pinned() -> None:
    assert "BIREFNET_MODEL_SHA256" in AI
    assert "downloaded <= BIREFNET_MODEL_BYTES" in AI
    assert "create_new(true)" in AI
    assert "file.sync_all()" in AI
    assert "verify_model(path)" in AI


def test_desktop_requires_runtime_before_model_download() -> None:
    assert '#[cfg(not(target_os = "android"))]\n        if self.onnx_runtime_path.is_none()' in APP
    request = APP[APP.index("pub(crate) fn request_subject_mask"):APP.index("fn start_subject_worker")]
    assert request.index("onnx_runtime_path.is_none()") < request.index("subject_consent_open = true")


def test_raw_geometry_is_rejected_before_unpack() -> None:
    opened = RAW.index("validate_opened_raw_geometry(&ctx)")
    unpack = RAW.index("libraw_unpack")
    assert opened < unpack
    assert "MAX_RAW_FILE_BYTES" in RAW
    assert "MAX_SENSOR_PIXELS" in RAW
    assert "validate_raw_dimensions" in RAW


def test_ci_runs_full_pytest_suite() -> None:
    assert "python3 -m pytest -q" in CI
    assert "unittest discover" not in CI


def test_profile_parsing_is_bounded_and_raw_is_checked_first() -> None:
    assert "MAX_DCP_FILE_BYTES" in RAW
    load_raw = RAW[RAW.index("pub fn load_raw_file(path"):RAW.index("fn read_optional_profile")]
    assert load_raw.index("validate_input_file(path") < load_raw.index("read_optional_profile(path)")
    assert "MAX_DCP_TAG_BYTES" in COLOR_PROFILE
    assert "MAX_DCP_MAP_ENTRIES" in COLOR_PROFILE
    assert "MAX_DCP_TONE_POINTS" in COLOR_PROFILE
    assert "try_reserve_exact(byte_len as usize)" in COLOR_PROFILE
