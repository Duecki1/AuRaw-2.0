from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
AI = (ROOT / "src/ai_masks.rs").read_text(encoding="utf-8")
MASKS = (ROOT / "src/app/masks_ai.rs").read_text(encoding="utf-8")
RUNTIME = (ROOT / "src/app/background_task_runtime.rs").read_text(encoding="utf-8")


def _function(source: str, name: str, next_name: str) -> str:
    start = source.index(name)
    end = source.index(next_name, start)
    return source[start:end]


def test_runtime_probe_caches_only_successful_results() -> None:
    assert "type RuntimeProbeResult = (PathBuf, String);" in AI
    probe = _function(
        AI,
        "pub(crate) fn probe_runtime_subprocess",
        "pub(crate) fn run_runtime_probe_process",
    )
    failure = probe.index("if !status.success()")
    cache_write = probe.index("*cached = Some((runtime_path, expected_sha256.to_owned()));")
    assert failure < cache_write
    assert "cached_error" not in probe
    assert "Option<String>" not in probe


def test_ai_mask_requests_retry_runtime_validation_without_restart() -> None:
    for function_name, next_name in [
        ("pub(crate) fn request_subject_mask", "fn start_subject_worker"),
        ("pub(crate) fn request_landscape_mask", "fn start_landscape_worker"),
        ("pub(crate) fn request_object_mask", "fn start_object_worker"),
    ]:
        body = _function(MASKS, function_name, next_name)
        assert "validate_onnx_runtime_for_ai()" in body
        assert "recover_terminal_ai_mask_task_owners();" in body


def test_consent_buttons_validate_runtime_without_losing_pending_request() -> None:
    dialogs = MASKS[MASKS.index("fn show_subject_dialogs") :]
    assert dialogs.count('ui.button("Consent, download and continue").clicked()') == 3
    assert dialogs.count("&& self.ai_runtime_ready()") == 3
    assert dialogs.index("&& self.ai_runtime_ready()") < dialogs.index(
        "self.subject_consent_open = false;"
    )


def test_failed_ai_task_startup_releases_task_owner() -> None:
    for function_name, next_name in [
        ("fn start_subject_mask_task", "fn start_object_mask_task"),
        ("fn start_object_mask_task", "fn start_landscape_mask_task"),
        ("fn start_landscape_mask_task", "fn start_inpaint_task"),
    ]:
        body = _function(RUNTIME, function_name, next_name)
        assert body.index("self.clear_ai_mask_task_owner(id);") < body.index(
            "self.fail_background_task"
        )
