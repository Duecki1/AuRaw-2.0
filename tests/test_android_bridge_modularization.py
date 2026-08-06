from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
JAVA_ROOT = ROOT / "android/app/src/main/java/de/duecki/auraw"
ACTIVITY = (JAVA_ROOT / "AuRawActivity.java").read_text(encoding="utf-8")
STORAGE = (JAVA_ROOT / "StorageManager.java").read_text(encoding="utf-8")
PROFILES = (JAVA_ROOT / "ProfileImporter.java").read_text(encoding="utf-8")
EXPORTS = (JAVA_ROOT / "ExportPublisher.java").read_text(encoding="utf-8")
ANDROID_RS = (ROOT / "src/android.rs").read_text(encoding="utf-8")


def test_native_activity_is_a_thin_jni_facade() -> None:
    assert len(ACTIVITY.splitlines()) < 350
    assert "private StorageManager storageManager;" in ACTIVITY
    assert "private ProfileImporter profileImporter;" in ACTIVITY
    assert "private ExportPublisher exportPublisher;" in ACTIVITY
    assert "storageManager.handleRawDocumentResult(resultCode, data);" in ACTIVITY
    assert "profileImporter.handleFolderPickerResult(resultCode, data);" in ACTIVITY
    assert "exportPublisher.onRequestPermissionsResult" in ACTIVITY

    # Lifecycle callbacks stay on the Activity, while Rust reflects delegates directly.
    for method in (
        "nativeOnFilePicked",
        "nativeOnFilePickedFd",
        "nativeOnImportBatchFinished",
        "nativeOnCameraProfileFolderPicked",
        "nativeOnExportPublished",
        "private StorageManager storageManager;",
        "private ProfileImporter profileImporter;",
        "private ExportPublisher exportPublisher;",
    ):
        assert method in ACTIVITY

    for passthrough in (
        "public String listRawLibrary()",
        "public String publishRawSidecar(",
        "public String createPendingExport(",
        "public void publishImage(",
        "public void removeCameraProfileMirror(",
    ):
        assert passthrough not in ACTIVITY

    for field, delegate_type in (
        ("storageManager", "de.duecki.auraw.StorageManager"),
        ("profileImporter", "de.duecki.auraw.ProfileImporter"),
        ("exportPublisher", "de.duecki.auraw.ExportPublisher"),
    ):
        assert f'jni::jni_str!("{field}")' in ANDROID_RS
        assert f'jni::jni_sig!({delegate_type})' in ANDROID_RS


def test_storage_delegate_owns_raw_library_and_sidecar_logic() -> None:
    assert "final class StorageManager" in STORAGE
    assert "MAX_RAW_IMPORT_BYTES" in STORAGE
    assert "MAX_SIDECAR_BYTES" in STORAGE
    assert "listCombinedRawLibrary" in STORAGE
    assert "publishRawSidecarFile" in STORAGE
    assert "startLegacyRawStorageMigration" in STORAGE
    assert "callbacks.onFilePicked" in STORAGE
    assert "callbacks.onImportBatchFinished" in STORAGE


def test_profile_and_export_delegates_own_their_platform_workflows() -> None:
    assert "final class ProfileImporter" in PROFILES
    assert "Intent.ACTION_OPEN_DOCUMENT_TREE" in PROFILES
    assert "copyCameraProfileTree" in PROFILES
    assert "removeOwnedCameraProfileMirror" in PROFILES
    assert "callbacks.onFolderPicked" in PROFILES

    assert "final class ExportPublisher" in EXPORTS
    assert "MediaStore.Images.Media.RELATIVE_PATH" in EXPORTS
    assert "WRITE_EXPORT_PERMISSION" in EXPORTS
    assert "publishImageScoped" in EXPORTS
    assert "publishImageLegacy" in EXPORTS
    assert "callbacks.onExportPublished" in EXPORTS
