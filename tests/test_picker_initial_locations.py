from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LIFECYCLE = (ROOT / "src/app/lifecycle.rs").read_text(encoding="utf-8")
MASKS_AI = (ROOT / "src/app/masks_ai.rs").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
EXPORT_UI = (ROOT / "src/ui/sidebar/export.rs").read_text(encoding="utf-8")
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")
ANDROID_STORAGE = (
    ROOT / "android/app/src/main/java/de/duecki/auraw/StorageManager.java"
).read_text(encoding="utf-8")
ANDROID_PROFILES = (
    ROOT / "android/app/src/main/java/de/duecki/auraw/ProfileImporter.java"
).read_text(encoding="utf-8")
ANDROID_ACTIVITY = (
    ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java"
).read_text(encoding="utf-8")
ANDROID_BRIDGE = (ROOT / "src/android.rs").read_text(encoding="utf-8")


def method(source: str, start: str, end: str) -> str:
    start_offset = source.index(start)
    return source[start_offset : source.index(end, start_offset)]


def test_desktop_open_pickers_start_from_the_active_selection() -> None:
    raw_picker = method(
        LIFECYCLE,
        "pub fn open_file_dialog(&mut self, _frame: &eframe::Frame)",
        "pub fn open_library_folder_dialog(&mut self)",
    )
    assert ".current_path" in raw_picker
    assert ".and_then(selected_picker_directory)" in raw_picker
    assert "self.library.folder()" in raw_picker
    assert "dialog = dialog.set_directory(directory)" in raw_picker

    library_picker = method(
        LIFECYCLE,
        "pub fn open_library_folder_dialog(&mut self)",
        "pub(crate) fn choose_camera_profile_folder(&mut self)",
    )
    assert "if let Some(folder) = self.library.folder()" in library_picker
    assert "dialog = dialog.set_directory(folder)" in library_picker

    camera_profiles = method(
        LIFECYCLE,
        "pub(crate) fn choose_camera_profile_folder(&mut self)",
        "fn apply_camera_profile_folder(&mut self, folder: PathBuf)",
    )
    assert "if let Some(folder) = &self.camera_profile_folder" in camera_profiles
    assert "dialog = dialog.set_directory(folder)" in camera_profiles

    display_profile = method(
        LIFECYCLE,
        "pub(crate) fn choose_display_profile_override(&mut self)",
        "fn apply_display_profile_override(&mut self, path: PathBuf)",
    )
    assert "self.display_profile_override.as_deref()" in display_profile
    assert "dialog = dialog.set_directory(parent)" in display_profile

    onnx_picker = method(
        MASKS_AI,
        "pub(crate) fn choose_onnx_runtime(&mut self)",
        "fn validate_and_persist_onnx_runtime(path: PathBuf)",
    )
    assert ".onnx_runtime_path" in onnx_picker
    assert "dialog = dialog.set_directory(parent)" in onnx_picker


def test_desktop_export_pickers_start_from_the_source_or_selected_profile() -> None:
    for picker in ("export_png", "export_jpeg", "export_tiff"):
        start = f"pub(crate) fn {picker}(&mut self, frame: &eframe::Frame)"
        start_offset = EXPORT.index(start)
        next_offset = EXPORT.find("pub(crate) fn ", start_offset + len(start))
        body = EXPORT[start_offset : next_offset if next_offset >= 0 else None]
        assert ".current_path" in body
        assert "dialog = dialog.set_directory(parent)" in body
        assert "dialog.save_file()" in body

    assert ".custom_icc_path" in EXPORT_UI
    assert ".or(fallback_picker_directory)" in EXPORT_UI
    assert "dialog = dialog.set_directory(directory)" in EXPORT_UI

    export_jobs = method(
        LIBRARY,
        "fn library_export_jobs(paths: &[PathBuf], format: ExportFormat)",
        "fn apply_library_adjustment_paste(",
    )
    assert ".parent()" in export_jobs
    assert ".first()" in export_jobs
    assert export_jobs.count("dialog = dialog.set_directory(parent)") >= 2


def test_android_saf_pickers_remember_and_restore_the_last_successful_uri() -> None:
    assert "DocumentsContract.EXTRA_INITIAL_URI" in ANDROID_STORAGE
    assert "rememberPickerUri(RAW_PICKER_URI_KEY, uri)" in ANDROID_STORAGE
    assert ANDROID_STORAGE.index("rememberPickerUri(RAW_PICKER_URI_KEY, uri)") > ANDROID_STORAGE.index("importDocumentIntoLibrary(uri, displayName)")
    assert 'PICKER_PREFERENCES = "auraw-picker-locations"' in ANDROID_STORAGE

    assert "DocumentsContract.EXTRA_INITIAL_URI" in ANDROID_PROFILES
    assert "rememberProfileFolderUri(treeUri)" in ANDROID_PROFILES
    assert "void clearFolderPickerLocation()" in ANDROID_PROFILES
    assert ".remove(CAMERA_PROFILE_PICKER_URI_KEY)" in ANDROID_PROFILES

    assert "clearCameraProfileFolderPickerLocation()" in ANDROID_ACTIVITY
    assert "clear_camera_profile_folder_picker_location" in ANDROID_BRIDGE
    assert "clear_camera_profile_folder_picker_location" in LIFECYCLE
