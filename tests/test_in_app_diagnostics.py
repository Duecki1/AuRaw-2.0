from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_settings_exposes_copyable_diagnostic_log():
    settings = (ROOT / "src/ui/settings.rs").read_text(encoding="utf-8")
    assert 'ui.heading("Diagnostics")' in settings
    assert 'ui.button("Copy log")' in settings
    assert "crate::diagnostics::snapshot()" in settings
    assert "ui.ctx().copy_text" in settings


def test_diagnostics_capture_device_gpu_raw_and_timing_information():
    diagnostics = (ROOT / "src/diagnostics.rs").read_text(encoding="utf-8")
    lib = (ROOT / "src/lib.rs").read_text(encoding="utf-8")
    lifecycle = (ROOT / "src/app/lifecycle.rs").read_text(encoding="utf-8")
    export = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")
    android = (
        ROOT
        / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java"
    ).read_text(encoding="utf-8")

    assert "sampled_fingerprint" in diagnostics
    assert "cam_to_srgb" in diagnostics
    assert "adapter.get_info()" in lib
    assert "driver_info" in lib
    assert "deviceDiagnostics" in android
    assert "RAW decode finished" in lifecycle
    assert "Preview proxy prepared" in lifecycle
    assert "First export tile completed" in export


def test_android_diagnostics_uses_native_clipboard_bridge():
    settings = (ROOT / "src/ui/settings.rs").read_text(encoding="utf-8")
    android = (ROOT / "src/android.rs").read_text(encoding="utf-8")
    activity = (
        ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java"
    ).read_text(encoding="utf-8")

    assert 'app.copy_text_to_clipboard("AuRaw diagnostics", &diagnostic_log)' in settings
    assert 'jni::jni_str!("copyTextToClipboard")' in android
    assert "ClipboardManager" in activity
    assert "setPrimaryClip" in activity
    assert 'Toast.makeText(this, "Diagnostic log copied"' in activity
