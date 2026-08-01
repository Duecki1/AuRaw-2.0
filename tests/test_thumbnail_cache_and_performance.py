import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LIBRARY = (ROOT / "src/ui/library.rs").read_text()
APP = (ROOT / "src/app.rs").read_text() + (ROOT / "src/app/lifecycle.rs").read_text()
SETTINGS = (ROOT / "src/ui/settings.rs").read_text()
THUMBNAIL_CACHE = (ROOT / "src/thumbnail_cache.rs").read_text()
ANDROID = (ROOT / "src/android.rs").read_text()
ACTIVITY = (ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java").read_text()
STORAGE = (ROOT / "android/app/src/main/java/de/duecki/auraw/StorageManager.java").read_text()
RAW = (ROOT / "src/pipeline/raw_loader/libraw_loader.rs").read_text()


def test_unedited_desktop_thumbnails_are_persisted() -> None:
    assert ".auraw-raw-thumb.png" in THUMBNAIL_CACHE
    assert "load_desktop_raw_thumbnail" in LIBRARY
    assert "save_desktop_raw_thumbnail" in LIBRARY
    assert LIBRARY.index("load_desktop_raw_thumbnail") < LIBRARY.index("load_raw_thumbnail(path")


def test_android_thumbnail_cache_survives_refresh_restart_and_cache_scavenging() -> None:
    assert "rawThumbnailCachePath" in STORAGE
    assert 'new File(activity.getNoBackupFilesDir(), "library-thumbnails")' in STORAGE
    assert 'new File(activity.getCacheDir(), "library-thumbnails")' in STORAGE
    assert "migrateLegacyThumbnailCacheEntry" in STORAGE
    assert "MAX_THUMBNAIL_CACHE_ENTRIES" in STORAGE
    assert "deleteThumbnailCacheEntry" in STORAGE
    assert "load_png(&cache_path" in ANDROID
    assert "save_png(&cache_path" in ANDROID
    assert "materializeRawLibraryThumbnail" in STORAGE


def test_library_unedited_thumbnails_prefer_embedded_preview_then_render_fallback() -> None:
    assert "load_raw_thumbnail(path, THUMBNAIL_EDGE)" in LIBRARY
    desktop_loader = LIBRARY[
        LIBRARY.index("fn load_desktop_library_thumbnail"):
        LIBRARY.index("fn load_android_library_thumbnail")
    ]
    assert "load_raw_thumbnail(path, THUMBNAIL_EDGE)" in desktop_loader
    assert "load_raw_thumbnail(&path, maximum_edge)" in ANDROID
    assert "load_raw_thumbnail(&temporary, maximum_edge)" in ANDROID
    fallback = RAW[RAW.index("pub fn load_raw_thumbnail"):RAW.index("fn open_libraw")]
    assert fallback.index("load_embedded_thumbnail") < fallback.index("load_processed_thumbnail")
    assert "acquire_rendered_thumbnail_worker" in RAW


def test_android_embedded_previews_are_not_blocked_by_full_sensor_pixel_budget() -> None:
    thumbnail_guard = RAW[RAW.index("unsafe fn validate_opened_thumbnail_geometry"):RAW.index("unsafe fn validate_opened_raw_geometry")]
    assert "MAX_SENSOR_PIXELS" not in thumbnail_guard
    assert "checked_mul" in thumbnail_guard
    assert "MAX_ANDROID_THUMBNAIL_FALLBACK_SENSOR_PIXELS" in RAW


def test_performance_defaults_and_controls_are_exposed() -> None:
    assert re.search(
        r"default_raw_cache_limit\(\).*?cfg!\(target_os = \"android\"\).*?1.*?else.*?2",
        APP,
        re.DOTALL,
    )
    assert "Decoded RAW cache" in SETTINGS
    assert SETTINGS.count('.text("Thumbnail workers")') == 1
    assert "Rendered thumbnail workers" not in SETTINGS
    assert "default_thumbnail_worker_count" in LIBRARY
    assert "run_thumbnail_workers" in LIBRARY
    assert "set_rendered_thumbnail_worker_limit(workers)" in LIBRARY
    assert ".read()" in LIBRARY and "decode_gate" in LIBRARY
    assert ".write()" in APP and "decode_gate" in APP


def test_performance_settings_are_persistent_on_both_platforms() -> None:
    assert "performance_settings_path" in APP
    assert "performanceSettingsPath" in ACTIVITY
    assert "performance.json" in (ROOT / "src/performance_settings.rs").read_text()
    assert "thumbnail_workers" in APP
    assert "thumbnail_workers" in (ROOT / "src/performance_settings.rs").read_text()
    assert "rendered_thumbnail_workers" not in APP
    assert "rendered_thumbnail_workers" not in (ROOT / "src/performance_settings.rs").read_text()


def test_desktop_last_library_folder_is_restored() -> None:
    lifecycle = (ROOT / "src/app/lifecycle.rs").read_text()
    performance = (ROOT / "src/performance_settings.rs").read_text()
    assert "last_library_folder" in performance
    assert 'cfg(not(target_os = "android"))' in performance
    assert "last_library_folder.filter(|folder| folder.is_dir())" in lifecycle
    assert "app.library.open_folder(folder, ctx)" in lifecycle
    # LibraryState has a native test proving the selected folder is recorded synchronously.


def test_reopening_the_same_folder_keeps_live_thumbnail_textures() -> None:
    refresh = LIBRARY[LIBRARY.index("pub(crate) fn refresh"):LIBRARY.index("fn poll(&mut self")]
    assert "Keep already decoded GPU textures visible" in refresh
    assert "std::mem::take(&mut self.entries)" in LIBRARY
    assert "same_library_file_identity" in LIBRARY


def test_android_import_picker_supports_single_and_batch_selection() -> None:
    lifecycle = (ROOT / "src/app/lifecycle.rs").read_text()
    top_bar = (ROOT / "src/ui/top_bar.rs").read_text()
    assert "Intent.EXTRA_ALLOW_MULTIPLE" in STORAGE
    assert "selectedDocumentUris" in STORAGE
    assert "importSingleDocument" in STORAGE
    assert "nativeOnImportBatchFinished" in ACTIVITY
    assert "Java_de_duecki_auraw_AuRawActivity_nativeOnImportBatchFinished" in ANDROID
    assert "BatchImported" in ANDROID
    assert "BatchImported" in lifecycle
    assert "self.active_tab = AppTab::Library" in lifecycle
    assert 'button("Open RAW…")' not in top_bar
    batch = STORAGE[STORAGE.index("private void importDocuments"):STORAGE.index("private void importSingleDocument")]
    single = STORAGE[STORAGE.index("private void importSingleDocument"):STORAGE.index("private StoredRaw importDocumentIntoLibrary")]
    assert "materializeLibraryRaw" not in batch
    assert "callbacks.onImportBatchFinished" in batch
    assert "deliverLibraryRawFd" in single
    assert "materializeLibraryRaw" not in STORAGE


def test_previewless_and_edited_raws_share_the_thumbnail_worker_limit() -> None:
    assert "acquire_rendered_thumbnail_worker" in RAW
    assert "acquire_rendered_thumbnail_worker" in LIBRARY
    assert "set_rendered_thumbnail_worker_limit" in LIBRARY
    assert "MAX_ANDROID_THUMBNAIL_FALLBACK_SENSOR_PIXELS: u64 = MAX_SENSOR_PIXELS" in RAW
    assert "MAX_EMBEDDED_THUMBNAIL_BYTES: usize = 64 * 1024 * 1024" in RAW
    fallback = RAW[RAW.index("fn load_processed_thumbnail"):RAW.index("fn embedded_thumbnail_orientation")]
    assert fallback.index("acquire_rendered_thumbnail_worker") < fallback.index("open_libraw(path)")


def test_failed_thumbnail_attempts_retry_and_cache_can_be_cleared() -> None:
    assert '"Preview unavailable"' not in LIBRARY
    assert "thumbnail_retry_after" in LIBRARY
    assert "request_repaint_after(delay)" in LIBRARY
    assert "Clear thumbnail cache" in SETTINGS
    assert "clear_desktop_thumbnail_cache" in THUMBNAIL_CACHE
    assert "prepare_for_thumbnail_cache_clear" in LIBRARY
    assert "clear_thumbnail_cache" in APP
    assert "clearThumbnailCache" in ANDROID
    assert "clearThumbnailCache" in ACTIVITY
    assert "clearThumbnailCacheDirectory" in STORAGE
