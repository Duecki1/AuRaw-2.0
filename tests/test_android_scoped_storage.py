from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
ACTIVITY = (
    ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java"
).read_text(encoding="utf-8")


def method(start: str, end: str) -> str:
    start_offset = ACTIVITY.index(start)
    return ACTIVITY[start_offset : ACTIVITY.index(end, start_offset)]


def test_android_10_uses_scoped_storage_for_new_imports_and_location() -> None:
    location = method("public String rawLibraryLocation()", "public String listRawLibrary()")
    assert "Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q" in location
    assert "return RAW_LIBRARY_RELATIVE_PATH" in location
    assert "legacyRawLibraryDirectory().getAbsolutePath()" in location

    store = method(
        "private StoredRaw storeRawInLibrary",
        "private StoredRaw storeRawScoped",
    )
    assert "Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q" in store
    assert store.index("storeRawScoped") < store.index("storeRawLegacy")


def test_android_10_catalog_keeps_legacy_upgrade_items_visible_and_bounded() -> None:
    combined = method(
        "private String listCombinedRawLibrary",
        "private ArrayList<RawLibraryRecord> listScopedRawLibrary",
    )
    assert "records.addAll(listScopedRawLibrary())" in combined
    assert "records.addAll(listLegacyRawLibrary())" in combined
    assert "Long.compare(right.modifiedSeconds, left.modifiedSeconds)" in combined
    assert "Set<String> seenUris" in combined
    assert "added > MAX_RAW_LIBRARY_FILES" in combined


def test_raw_identity_accepts_legacy_items_by_scheme_after_an_os_upgrade() -> None:
    identity = method(
        "private void verifyRawLibraryIdentity",
        "private void verifyLegacyRawLibraryIdentity",
    )
    assert "ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())" in identity
    assert identity.index("verifyLegacyRawLibraryIdentity") < identity.index(
        "Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q"
    )

    for signature, end in (
        ("public void removeRawSidecar", "private void removeRawSidecarScoped"),
        ("public String materializeRawSidecar", "private String materializeRawSidecarScoped"),
        ("public String publishRawSidecar", "private String publishRawSidecarScoped"),
    ):
        dispatch = method(signature, end)
        assert "ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())" in dispatch
        assert "Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q" not in dispatch


def test_scoped_raw_identity_is_bound_to_one_owned_published_download() -> None:
    identity = method(
        "private void verifyScopedRawLibraryIdentity",
        "private static String sidecarDisplayName",
    )
    assert "ContentResolver.SCHEME_CONTENT.equals(rawUri.getScheme())" in identity
    assert "ContentUris.parseId(rawUri)" in identity
    assert "ContentUris.withAppendedId(collection, expectedId).equals(rawUri)" in identity
    for column in (
        "MediaStore.Downloads._ID",
        "MediaStore.Downloads.DISPLAY_NAME",
        "MediaStore.Downloads.RELATIVE_PATH",
        "MediaStore.Downloads.OWNER_PACKAGE_NAME",
        "MediaStore.Downloads.IS_PENDING",
    ):
        assert column in identity
    assert "expectedDisplayName.equals(storedName)" in identity
    assert "RAW_LIBRARY_RELATIVE_PATH.equals(storedPath)" in identity
    assert "getPackageName().equals(storedOwner)" in identity
    assert "pending != 0" in identity


def test_scoped_sidecars_read_newest_complete_generation_and_delete_by_identity() -> None:
    materialize = method(
        "private String materializeRawSidecarScoped",
        "public String createRawSidecarCache",
    )
    assert "scopedSidecarUris(rawDisplayName)" in materialize
    assert "generations.get(0)" in materialize
    assert "openInputStream(newestGeneration)" in materialize

    generations = method(
        "private ArrayList<Uri> scopedSidecarUris",
        "private int deleteScopedSidecarGeneration",
    )
    assert "scopedSidecarSelection()" in generations
    assert 'MediaStore.Downloads._ID + " DESC"' in generations

    selection = method(
        "private static String scopedSidecarSelection",
        "private String[] scopedSidecarSelectionArgs",
    )
    assert "MediaStore.Downloads.RELATIVE_PATH" in selection
    assert "MediaStore.Downloads.OWNER_PACKAGE_NAME" in selection
    assert 'MediaStore.Downloads.IS_PENDING + "=0' in selection
    assert "MediaStore.Downloads.DISPLAY_NAME" in selection

    deletion = method(
        "private int deleteScopedSidecarGeneration",
        "private static String scopedSidecarSelection",
    )
    assert "ContentUris.parseId(generation)" in deletion
    assert 'MediaStore.Downloads._ID + "=?' in deletion
    assert "scopedSidecarSelection()" in deletion
    assert "MediaStore.Downloads.EXTERNAL_CONTENT_URI" in deletion


def test_android_export_publisher_supports_png_and_jpeg() -> None:
    publish = method(
        "public void publishImage",
        "private static void copy(InputStream input, OutputStream output, long maximumBytes)",
    )
    assert '"image/png"' in publish
    assert '"image/jpeg"' in publish
    assert "normalizeExportMimeType" in publish
    assert "MediaStore.Images.Media.MIME_TYPE" in publish
    assert "safeImageName" in publish
    assert 'jpeg ? ".jpg" : ".png"' in publish
