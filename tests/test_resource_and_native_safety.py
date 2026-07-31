from __future__ import annotations

from tests.source_helpers import read_source_tree
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]
AI = (ROOT / "src/ai_masks.rs").read_text(encoding="utf-8")
RAWNIND = (ROOT / "src/ai_denoise.rs").read_text(encoding="utf-8")
APP_RAWNIND = (ROOT / "src/app/ai_denoise.rs").read_text(encoding="utf-8")
APP = read_source_tree(ROOT / "src/app.rs")
RAW = read_source_tree(ROOT / "src/pipeline/raw_loader.rs")
COLOR_PROFILE = read_source_tree(ROOT / "src/pipeline/color_profile.rs")
CARGO = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
CI = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")
ANDROID_JAVA_ROOT = ROOT / "android/app/src/main/java/de/duecki/auraw"
ANDROID_ACTIVITY = "\n".join(
    path.read_text(encoding="utf-8") for path in sorted(ANDROID_JAVA_ROOT.glob("*.java"))
)
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
    assert "VITMATTE_MODEL_SHA256_HEX" in AI
    assert "downloaded <= VITMATTE_MODEL_BYTES" in AI
    assert "verify_vitmatte_model(path)" in AI
    assert "LANDSCAPE_MODEL_SHA256_HEX" in AI
    assert "downloaded <= LANDSCAPE_MODEL_BYTES" in AI
    assert "verify_landscape_model(path)" in AI
    assert ".https_only(true)" in AI
    assert "if !allow_download" in AI
    assert "consent to its download again" in AI


def test_large_ai_model_hashing_stays_off_the_ui_thread() -> None:
    subject_request = APP[
        APP.index("pub(crate) fn request_subject_mask"):
        APP.index("fn start_subject_worker")
    ]
    object_request = APP[
        APP.index("pub(crate) fn request_object_mask"):
        APP.index("fn start_object_worker")
    ]
    runtime = (ROOT / "src/app/background_task_runtime.rs").read_text(encoding="utf-8")
    subject_start = runtime[
        runtime.index("fn start_subject_mask_task"):
        runtime.index("fn start_object_mask_task")
    ]
    object_start = runtime[
        runtime.index("fn start_object_mask_task"):
        runtime.index("fn start_landscape_mask_task")
    ]

    assert "subject_models_are_verified" not in subject_request
    assert "object_models_are_verified" not in object_request
    assert "subject_models_are_verified" not in subject_start
    assert "object_models_are_verified" not in object_start
    assert ".is_file()" in subject_request
    assert ".is_file()" in object_request
    assert ".is_file()" in subject_start
    assert ".is_file()" in object_start

    # Exact size/SHA verification remains mandatory in the worker before use.
    subject_worker = AI[AI.index("pub fn spawn_subject_mask"):AI.index("pub fn spawn_object_mask")]
    object_worker = AI[AI.index("pub fn spawn_object_mask"):AI.index("fn infer_object_mask")]
    assert "ensure_model(" in subject_worker
    assert "ensure_vitmatte_model(" in subject_worker
    assert "ensure_sam_model(" in object_worker
    assert "ensure_vitmatte_model(" in object_worker


def test_desktop_requires_runtime_before_model_download() -> None:
    assert "validate_onnx_runtime_for_ai" in APP
    request = APP[APP.index("pub(crate) fn request_subject_mask"):APP.index("fn start_subject_worker")]
    assert request.index("validate_onnx_runtime_for_ai()") < request.index("subject_consent_open = true")
    landscape = APP[
        APP.index("pub(crate) fn request_landscape_mask"):
        APP.index("fn start_landscape_worker")
    ]
    assert landscape.index("validate_onnx_runtime_for_ai()") < landscape.index(
        "landscape_consent_open = true"
    )
    assert "landscape_model_is_verified" in landscape
    consent = APP[APP.index("if self.landscape_consent_open"):]
    assert '"Consent, download and continue"' in consent
    assert "self.start_landscape_worker(" in consent


def test_android_rawnind_starts_visibly_and_releases_preview_gpu_memory() -> None:
    start = APP_RAWNIND[
        APP_RAWNIND.index("fn start_ai_denoise"):
        APP_RAWNIND.index("pub(crate) fn poll_ai_denoise_worker")
    ]
    assert "crate::ai_masks::initialize_runtime(None, None)" in start
    assert "take_preview_pipeline_and_release_textures" in start
    assert "self.ai_denoise_receiver = Some(receiver)" in start
    assert "self.ai_denoise_apply_progress = Some" in start
    worker = RAWNIND[
        RAWNIND.index("pub fn spawn_rawnind_denoise"):
        RAWNIND.index("fn ensure_not_cancelled")
    ]
    assert 'phase: "Checking RawNIND models"' in worker
    assert 'phase: "Starting AI runtime"' in worker


def test_rawnind_restores_preview_after_every_terminal_result() -> None:
    poll = APP_RAWNIND[
        APP_RAWNIND.index("pub(crate) fn poll_ai_denoise_worker"):
        APP_RAWNIND.index("pub(crate) fn abandon_ai_denoise_worker")
    ]
    assert "self.preview_quality_dirty = true" in poll
    assert poll.index("self.preview_quality_dirty = true") < poll.index("match result")
    assert "self.target_exposure.ai_denoise_enabled = false" in poll


def test_windows_runtime_is_isolated_and_uses_safe_cpu_fallback() -> None:
    assert "--auraw-onnx-runtime-probe" in AI or "--auraw-onnx-runtime-probe" in APP
    assert "probe_runtime_subprocess" in AI
    windows = AI[AI.index('#[cfg(target_os = "windows")]\nfn create_accelerated_session'):AI.index('#[cfg(target_os = "macos")]')]
    assert "Ok(None)" in windows
    assert "ort::ep::TensorRT" not in windows
    assert "ort::ep::CUDA" not in windows
    assert "ort::ep::DirectML" not in windows


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


def test_android_sidecars_are_complete_atomic_sibling_files() -> None:
    assert "MAX_SIDECAR_BYTES = 32L * 1024L * 1024L" in ANDROID_ACTIVITY
    assert 'RAW_LIBRARY_DIRECTORY_NAME = ".library"' in ANDROID_ACTIVITY
    assert 'File.createTempFile(".auraw-sidecar-", ".part", directory)' in ANDROID_ACTIVITY
    assert "StandardCopyOption.ATOMIC_MOVE" in ANDROID_ACTIVITY
    assert "StandardCopyOption.REPLACE_EXISTING" in ANDROID_ACTIVITY
    assert "publishRawSidecarFile" in ANDROID_ACTIVITY
    raw_store = ANDROID_ACTIVITY.index("private StoredRaw storeRawFile")
    assert ANDROID_ACTIVITY.index("rawLibraryDirectory()", raw_store) < ANDROID_ACTIVITY.index(
        "Uri.fromFile(destination)", raw_store
    )

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
    hue_sat = adjustments.index("var rgb = apply_camera_characterization(scene_working_at(pos))")
    exposure = adjustments.index("rgb = apply_exposure(rgb)")
    assert matrix < hue_sat < exposure

    matrix = tone.index("cam_to_working(camera_rgb)")
    hue_sat = tone.index("apply_camera_characterization(working)")
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
    assert "offset_of!(super::GpuParams, process_info), 928" in gpu_tests
    assert "offset_of!(super::GpuParams, mask_counts), 944" in gpu_tests


def test_large_object_mask_models_resume_after_transient_download_failures() -> None:
    sam_download = AI[AI.index("fn download_sam_model("):AI.index("fn infer_object_mask(")]
    vitmatte_download = AI[AI.index("fn download_vitmatte_model<F>"):AI.index("struct MatteCrop")]
    for downloader in (sam_download, vitmatte_download):
        assert 'header("Range", range.as_str())' in downloader
        assert "const MAX_ATTEMPTS: usize = 5" in downloader
        assert "file.sync_data()" in downloader
        assert "timeout_recv_body(Some(Duration::from_secs(30 * 60)))" in downloader


def test_linux_appimage_ai_uses_stable_cpu_and_nonpersistent_object_sessions() -> None:
    assert 'fn running_from_appimage() -> bool' in AI
    assert 'std::env::var_os("APPIMAGE").is_some()' in AI
    linux = AI[AI.index('#[cfg(target_os = "linux")]\nfn create_accelerated_session'):AI.index('#[cfg(target_os = "windows")]')]
    assert 'if running_from_appimage()' in linux
    assert 'return Ok(None);' in linux
    assert 'fn cache_object_ai_sessions() -> bool' in AI
    assert '!running_from_appimage()' in AI


def test_windows_sam_encoder_uses_conservative_numeric_session() -> None:
    assert '#[cfg(windows)]\nfn create_windows_sam_encoder_session' in AI
    block = AI[
        AI.index('#[cfg(windows)]\nfn create_windows_sam_encoder_session'):
        AI.index('#[cfg(target_os = "linux")]\nfn running_from_appimage')
    ]
    assert 'with_parallel_execution(false)' in block
    assert 'with_intra_threads(1)' in block
    assert 'GraphOptimizationLevel::Disable' in block
    assert 'with_arena_allocator(false)' in block
    assert 'extract_sam_encoder_output' in AI
    assert 'non_finite <= repair_limit' in AI
    assert 'numerically corrupted' in AI
