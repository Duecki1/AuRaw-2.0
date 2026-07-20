from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ANDROID_RS = (ROOT / "src/android.rs").read_text()
ACTIVITY = (ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java").read_text()
LIFECYCLE = (ROOT / "src/app/lifecycle.rs").read_text()
SETTINGS_UI = (ROOT / "src/ui/settings.rs").read_text()
SIDEBAR = (ROOT / "src/ui/sidebar/navigation.rs").read_text()
PERFORMANCE = (ROOT / "src/performance_settings.rs").read_text()


def test_android_uses_tree_picker_and_persistent_private_dcp_mirror() -> None:
    assert "Intent.ACTION_OPEN_DOCUMENT_TREE" in ACTIVITY
    assert "Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION" in ACTIVITY
    assert "takePersistableUriPermission" in ACTIVITY
    assert 'name.toLowerCase(Locale.ROOT).endsWith(".dcp")' in ACTIVITY
    assert "copyCameraProfileTree" in ACTIVITY
    assert 'getFilesDir(),' in ACTIVITY
    assert '"camera-profiles-"' in ACTIVITY
    assert "MAX_DCP_FILES" in ACTIVITY
    assert "MAX_DCP_FILE_BYTES" in ACTIVITY
    assert "MAX_DCP_TREE_DEPTH" in ACTIVITY


def test_android_folder_picker_is_bridged_back_to_rust_settings() -> None:
    assert "open_camera_profile_folder" in ANDROID_RS
    assert "nativeOnCameraProfileFolderPicked" in ANDROID_RS
    assert "CameraProfileFolderResult" in ANDROID_RS
    assert "take_camera_profile_folder_result" in LIFECYCLE
    assert "camera_profile_folder_label" in LIFECYCLE
    assert "camera_profile_auto_detect = false" in LIFECYCLE
    assert "Camera profile folder" in SETTINGS_UI
    assert "Android's system folder picker" in SETTINGS_UI


def test_camera_profile_dropdown_and_reload_are_available_on_android() -> None:
    assert '#[cfg(not(target_os = "android"))]\n    fn show_camera_profile_selector' not in SIDEBAR
    assert "Self::show_camera_profile_selector(ui, app, frame);" in SIDEBAR
    assert "pending_android_profile_reload" in LIFECYCLE
    assert "open_library_document" in LIFECYCLE
    assert "open_path_labeled_with_options" in LIFECYCLE


def test_desktop_auto_detects_documented_adobe_camera_profile_roots() -> None:
    assert 'std::env::var_os("ProgramData")' in PERFORMANCE
    assert '.join("Adobe")' in PERFORMANCE
    assert '.join("CameraRaw")' in PERFORMANCE
    assert '.join("CameraProfiles")' in PERFORMANCE
    assert '"Library"' in PERFORMANCE
    assert '"Application Support"' in PERFORMANCE
    assert '"/Library/Application Support/Adobe/CameraRaw/CameraProfiles"' in PERFORMANCE
    assert "detected_adobe_camera_profile_folder" in LIFECYCLE
    assert "Auto-detect Adobe" in SETTINGS_UI


def test_android_folder_import_updates_live_ui_without_restart() -> None:
    assert "nativeOnCameraProfileFolderImportStarted" in ACTIVITY
    assert "nativeOnCameraProfileFolderImportStarted" in ANDROID_RS
    assert "CameraProfileFolderResult::ImportStarted" in LIFECYCLE
    assert "camera_profile_folder_importing_label" in LIFECYCLE
    assert "Importing {label}" in SETTINGS_UI
    # Keep the NativeActivity/egui loop alive while the Java worker mirrors a
    # large SAF tree, even on devices where a JNI request_repaint does not wake it.
    eframe_impl = (ROOT / "src/app/eframe_impl.rs").read_text()
    assert "if self.picker_pending" in eframe_impl
    assert "request_repaint_after(Duration::from_millis(120))" in eframe_impl
    # Deliver the terminal JNI callback on Android's UI thread after import.
    assert "runOnUiThread(() -> nativeOnCameraProfileFolderPicked" in ACTIVITY


def test_android_profile_picker_buttons_reflect_pending_import_state() -> None:
    assert '"Importing…"' in SETTINGS_UI
    assert "let choose_enabled = !app.picker_pending" in SETTINGS_UI
    assert "let can_clear = can_clear && !app.picker_pending" in SETTINGS_UI
