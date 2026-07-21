from __future__ import annotations

from tests.source_helpers import read_source_tree
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
AI = (ROOT / "src/ai_masks.rs").read_text(encoding="utf-8")
APP = read_source_tree(ROOT / "src/app.rs")
RAW = read_source_tree(ROOT / "src/pipeline/raw_loader.rs")
COLOR_PROFILE = read_source_tree(ROOT / "src/pipeline/color_profile.rs")
CARGO = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
CI = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
ANDROID_ACTIVITY = (ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java").read_text(encoding="utf-8")
LIBRAW_BUILD = (ROOT / "scripts/build-android-libraw.sh").read_text(encoding="utf-8")


def test_native_runtime_is_never_downloaded_or_archive_extracted() -> None:
    assert "onnxruntime-linux" not in AI
    assert "flate2" not in CARGO
    assert "tar =" not in CARGO
    assert "ort::init_from" in AI
    assert "expected_sha256" in AI
    assert "selected ONNX Runtime changed after approval" in AI
    assert "DESKTOP_RUNTIME_IDENTITY" in AI
    assert "builder.with_name(\"AuRaw\").commit()" in AI
    assert "a different ONNX Runtime is already active" in AI
    # Linux must load the user-approved canonical on-disk runtime so ONNX
    # Runtime can discover sibling execution-provider libraries. It must not
    # stage or load the runtime through a memfd pseudo-path.
    assert "Ok((path.to_path_buf(), None, actual_sha256))" in AI
    assert "memfd_create(" not in AI
    assert "F_SEAL_WRITE" not in AI


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
    full_decode = RAW.index("fn load_raw_file_with_selected_profile")
    opened = RAW.index("validate_opened_raw_geometry(&ctx)", full_decode)
    unpack = RAW.index("ffi::libraw_unpack(ctx.raw)", full_decode)
    assert opened < unpack

    thumbnail_decode = RAW.index("fn load_embedded_thumbnail")
    thumbnail_guard = RAW.index("validate_opened_thumbnail_geometry(&ctx)")
    thumbnail_unpack = RAW.index("ffi::libraw_unpack_thumb(ctx.raw)", thumbnail_decode)
    assert thumbnail_guard < thumbnail_unpack
    assert "MAX_RAW_FILE_BYTES" in RAW
    assert "MAX_SENSOR_PIXELS" in RAW
    assert "validate_raw_dimensions" in RAW


def test_ci_runs_full_pytest_suite() -> None:
    assert "python3 -m pytest -q" in CI
    assert "unittest discover" not in CI


def test_android_stream_import_is_bounded_and_makes_progress() -> None:
    assert "MAX_RAW_IMPORT_BYTES" in ANDROID_ACTIVITY
    assert "queryDocumentSize(uri)" in ANDROID_ACTIVITY
    assert "checkedCopyLength" in ANDROID_ACTIVITY
    assert "if (count == 0)" in ANDROID_ACTIVITY
    assert "int value = input.read()" in ANDROID_ACTIVITY
    android_export = APP[APP.index('let export_dir = data_dir.join("cache").join("exports")'):]
    assert "std::fs::read_dir(&export_dir)" not in android_export


def test_android_sidecars_are_complete_discoverable_generations() -> None:
    assert "MAX_SIDECAR_BYTES = 32L * 1024L * 1024L" in ANDROID_ACTIVITY
    assert 'values.put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")' in ANDROID_ACTIVITY
    assert "values.put(MediaStore.Downloads.IS_PENDING, 1)" in ANDROID_ACTIVITY
    assert "sidecarStagePrefix(rawDisplayName)" in ANDROID_ACTIVITY
    assert "contentPublished = true" in ANDROID_ACTIVITY
    assert "removedOldRows &= deleteScopedSidecarGeneration" in ANDROID_ACTIVITY
    assert "queryStoredDisplayName(destination)" in ANDROID_ACTIVITY
    assert "String storedDisplayName = queryStoredDisplayName(destination);" in ANDROID_ACTIVITY
    raw_store = ANDROID_ACTIVITY.index("private StoredRaw storeRawScoped")
    assert ANDROID_ACTIVITY.index(
        "String storedDisplayName = queryStoredDisplayName(destination);", raw_store
    ) < ANDROID_ACTIVITY.index("published = true;", raw_store)
    assert "return new StoredRaw(destination, storedDisplayName)" in ANDROID_ACTIVITY


def test_sidecar_serialization_and_io_stay_off_the_render_thread() -> None:
    sidecar = (ROOT / "src/sidecar.rs").read_text(encoding="utf-8")
    persistence = (ROOT / "src/app/sidecar_persistence.rs").read_text(encoding="utf-8")
    assert "struct CappedVec" in sidecar
    assert "serde_json::to_writer(&mut writer" in sidecar
    assert "preflight_edit_size(&edits)" in sidecar
    assert 'name("auraw-sidecar-save"' in persistence
    assert "save_sidecar_request(" in persistence


def test_downloaded_build_inputs_and_actions_are_immutable() -> None:
    assert "LIBRAW_ARCHIVE_SHA256=" in LIBRAW_BUILD
    assert "LIBRAW_CMAKE_ARCHIVE_SHA256=" in LIBRAW_BUILD
    assert "sha256sum --check --status" in LIBRAW_BUILD
    assert not re.search(r"uses:\s+[^\s]+@v\d+", CI)


def test_profile_parsing_is_bounded_and_raw_is_checked_first() -> None:
    assert "MAX_DCP_FILE_BYTES" in RAW
    load_raw = RAW[RAW.index("pub fn load_raw_file(path"):RAW.index("fn read_optional_profile")]
    assert load_raw.index("validate_input_file(path") < load_raw.index("read_optional_profile(path)")
    assert "MAX_DCP_TAG_BYTES" in COLOR_PROFILE
    assert "MAX_DCP_MAP_ENTRIES" in COLOR_PROFILE
    assert "MAX_DCP_TONE_POINTS" in COLOR_PROFILE
    assert "try_reserve_exact(byte_len as usize)" in COLOR_PROFILE


def test_libraw_preserves_non_utf8_unix_paths() -> None:
    path_conversion = RAW[RAW.index("fn path_to_libraw_cstring"):RAW.index("struct LibRawContext")]
    assert "OsStrExt" in RAW
    assert "path.as_os_str().as_bytes()" in path_conversion
    assert "to_string_lossy" not in path_conversion


def test_global_white_balance_rebuilds_camera_and_dcp_transforms() -> None:
    adjustments = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
    tone = (ROOT / "src/shaders/tone_analysis.wgsl").read_text(encoding="utf-8")
    basic = (ROOT / "src/shaders/basic_adjustments.wgsl").read_text(encoding="utf-8")
    gpu = read_source_tree(ROOT / "src/pipeline/gpu.rs")
    profile = (ROOT / "src/shaders/profile.wgsl").read_text(encoding="utf-8")
    assert "apply_camera_temperature_tint" not in basic
    assert "raw.adjusted_camera_transform(" in gpu
    assert "profile_layout.flags[3] = profile_weight" in gpu
    assert "bitcast<f32>(params.profile_flags.w)" in profile
    matrix = adjustments.index("cam_to_working(camera_rgb)")
    hue_sat = adjustments.index("var rgb = apply_profile_hue_sat(scene_working_at(pos))")
    exposure = adjustments.index("rgb = apply_exposure(rgb)")
    assert matrix < hue_sat < exposure

    matrix = tone.index("cam_to_working(camera_rgb)")
    hue_sat = tone.index("apply_profile_hue_sat(working)")
    assert matrix < hue_sat


def test_rust_compile_surface_regressions_are_absent() -> None:
    gpu_tests = (ROOT / "src/pipeline/gpu/tests.rs").read_text(encoding="utf-8")
    gpu_resources = (ROOT / "src/pipeline/gpu/resources.rs").read_text(encoding="utf-8")
    icc = (ROOT / "src/pipeline/color_profile/icc.rs").read_text(encoding="utf-8")
    raw_loader = (ROOT / "src/pipeline/raw_loader.rs").read_text(encoding="utf-8")
    sidebar = (ROOT / "src/ui/sidebar.rs").read_text(encoding="utf-8")

    assert "&tone_analysis_module" not in gpu_tests
    assert "SHADER_TONE_ANALYSIS" in gpu_tests
    assert "crate::pipeline::raw_loader::validate_raw_dimensions" in gpu_resources
    assert "super::raw_loader::validate_raw_dimensions" not in gpu_resources
    assert "#[derive(Clone, Debug)]\nenum TransferCurve" in icc
    assert '#[cfg(not(libraw_available))]\nuse anyhow::anyhow;' in raw_loader
    assert "use crate::ui::mask_component_color;" not in sidebar
    assert "offset_of!(super::GpuParams, process_info), 880" in gpu_tests
    assert "offset_of!(super::GpuParams, mask_counts), 896" in gpu_tests
