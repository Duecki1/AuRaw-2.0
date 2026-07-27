from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
STORAGE = (
    ROOT / "android/app/src/main/java/de/duecki/auraw/StorageManager.java"
).read_text(encoding="utf-8")
EXPORT = (
    ROOT / "android/app/src/main/java/de/duecki/auraw/ExportPublisher.java"
).read_text(encoding="utf-8")


def method(source: str, start: str, end: str) -> str:
    start_offset = source.index(start)
    return source[start_offset : source.index(end, start_offset)]


def test_android_library_has_one_canonical_hidden_app_media_folder() -> None:
    constants = STORAGE[STORAGE.index("private static final String LEGACY_MEDIASTORE_RAW_RELATIVE_PATH") : STORAGE.index("private static final Set<String> RAW_SUFFIXES")]
    assert 'RAW_LIBRARY_DIRECTORY_NAME = ".library"' in constants
    assert 'Environment.DIRECTORY_PICTURES + "/AuRaw"' in EXPORT

    location = method(STORAGE, "String rawLibraryLocation()", "String listRawLibrary()")
    assert "rawLibraryDirectory().getAbsolutePath()" in location
    assert "Build.VERSION.SDK_INT" not in location

    directory = method(STORAGE, "private File externalMediaRootDirectory()", "void startLegacyRawStorageMigration()")
    assert "getExternalMediaDirs()" in directory
    assert "new File(externalMediaRootDirectory(), RAW_LIBRARY_DIRECTORY_NAME)" in directory
    assert 'new File(directory, ".nomedia")' in directory


def test_all_new_raw_imports_write_directly_to_hidden_library() -> None:
    store = method(STORAGE, "private StoredRaw storeRawInLibrary", "private void deliverLibraryRawFd")
    assert "return storeRawFile(source, requestedName);" in store
    assert "MediaStore.Downloads" not in store
    assert "rawLibraryDirectory()" in store
    assert '".auraw-import-"' in store
    assert "Uri.fromFile(destination)" in store


def test_catalog_prefers_canonical_library_and_keeps_upgrade_fallbacks() -> None:
    combined = method(STORAGE, "private String listCombinedRawLibrary", "private ArrayList<RawLibraryRecord> listLegacyMediaStoreRawLibrary")
    assert "listFileRawLibrary(rawLibraryDirectory())" in combined
    assert "listFileRawLibrary(externalMediaRootDirectory())" in combined
    assert "listLegacyMediaStoreRawLibrary()" in combined
    assert "Long.compare(right.modifiedSeconds, left.modifiedSeconds)" in combined
    assert "Set<String> seenUris" in combined
    assert "added > MAX_RAW_LIBRARY_FILES" in combined


def test_file_identity_accepts_only_canonical_library_or_pre_migration_root() -> None:
    identity = method(STORAGE, "private void verifyFileRawLibraryIdentity", "private void verifyLegacyMediaStoreRawIdentity")
    assert "rawLibraryDirectory().getCanonicalFile()" in identity
    assert "externalMediaRootDirectory().getCanonicalFile()" in identity
    assert "(!library.equals(parent) && !legacyRoot.equals(parent))" in identity


def test_legacy_mediastore_identity_is_read_only_compatibility_path() -> None:
    identity = method(STORAGE, "private void verifyLegacyMediaStoreRawIdentity", "private static String sidecarDisplayName")
    assert "ContentResolver.SCHEME_CONTENT.equals(rawUri.getScheme())" in identity
    assert "ContentUris.parseId(rawUri)" in identity
    assert "MediaStore.Downloads.RELATIVE_PATH" in identity
    assert "MediaStore.Downloads.OWNER_PACKAGE_NAME" in identity
    assert "LEGACY_MEDIASTORE_RAW_RELATIVE_PATH.equals(storedPath)" in identity
    assert "getPackageName().equals(storedOwner)" in identity


def test_sidecars_for_new_library_are_atomic_sibling_files() -> None:
    publish = method(STORAGE, "private String publishRawSidecarFile", "private ArrayList<Uri> legacyMediaStoreSidecarUris")
    assert "File.createTempFile(\".auraw-sidecar-\", \".part\", directory)" in publish
    assert "StandardCopyOption.ATOMIC_MOVE" in publish
    assert "StandardCopyOption.REPLACE_EXISTING" in publish

    public_publish = method(STORAGE, "String publishRawSidecar", "private String publishRawSidecarLegacyMediaStore")
    assert "new File(rawUri.getPath()).getParentFile()" in public_publish
    assert "publishRawSidecarFile" in public_publish


def test_upgrade_migration_moves_old_locations_and_only_deletes_after_copy() -> None:
    migration = method(STORAGE, "void startLegacyRawStorageMigration", "private void deleteStoredRaw")
    assert "migrateLegacyExternalMediaRoot()" in migration
    assert "migrateLegacyMediaStoreRawLibrary()" in migration
    assert "moveOrCopyLegacyFile" in migration
    assert "copy(input, output, MAX_RAW_IMPORT_BYTES)" in migration
    assert "partial.renameTo(destination)" in migration
    assert "getContentResolver().delete(source, null, null) <= 0" in migration
    assert "removeRawSidecarLegacyMediaStore(record.displayName)" in migration


def test_android_export_publisher_targets_gallery_visible_pictures_auraw() -> None:
    pending = method(EXPORT, "String createPendingExport", "void finishPendingExport")
    assert "MediaStore.Images.Media.RELATIVE_PATH" in pending
    assert "EXPORT_RELATIVE_PATH" in pending
    assert 'EXPORT_RELATIVE_PATH + "/" + displayName' in pending

    publish = method(EXPORT, "void publishImage", "private static void copy(InputStream input, OutputStream output, long maximumBytes)")
    assert '"image/png"' in publish
    assert '"image/jpeg"' in publish
    assert "MediaStore.Images.Media.MIME_TYPE" in publish
    assert "MediaStore.Images.Media.RELATIVE_PATH" in publish
    assert "EXPORT_RELATIVE_PATH" in publish
