from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LIBRARY = (ROOT / "src/ui/library.rs").read_text()
APP = (ROOT / "src/app.rs").read_text() + (ROOT / "src/app/lifecycle.rs").read_text()
SETTINGS = (ROOT / "src/ui/settings.rs").read_text()
THUMBNAIL_CACHE = (ROOT / "src/thumbnail_cache.rs").read_text()
ANDROID = (ROOT / "src/android.rs").read_text()
ACTIVITY = (ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java").read_text()
RAW = (ROOT / "src/pipeline/raw_loader/libraw_loader.rs").read_text()


def test_unedited_desktop_thumbnails_are_persisted() -> None:
    assert ".auraw-raw-thumb.png" in THUMBNAIL_CACHE
    assert "load_desktop_raw_thumbnail" in LIBRARY
    assert "save_desktop_raw_thumbnail" in LIBRARY
    assert LIBRARY.index("load_desktop_raw_thumbnail") < LIBRARY.index("load_raw_thumbnail(path")


def test_android_thumbnail_cache_survives_library_refresh() -> None:
    assert "rawThumbnailCachePath" in ACTIVITY
    assert 'new File(getCacheDir(), "library-thumbnails")' in ACTIVITY
    assert "load_png(&cache_path" in ANDROID
    assert "save_png(&cache_path" in ANDROID
    assert "materializeRawLibraryThumbnail" in ACTIVITY


def test_android_embedded_previews_are_not_blocked_by_full_sensor_pixel_budget() -> None:
    thumbnail_guard = RAW[RAW.index("unsafe fn validate_opened_thumbnail_geometry"):RAW.index("unsafe fn validate_opened_raw_geometry")]
    assert "MAX_SENSOR_PIXELS" not in thumbnail_guard
    assert "checked_mul" in thumbnail_guard
    assert "MAX_ANDROID_THUMBNAIL_FALLBACK_SENSOR_PIXELS" in RAW


def test_performance_defaults_and_controls_are_exposed() -> None:
    assert 'if cfg!(target_os = "android") { 1 } else { 2 }' in APP
    assert "Decoded RAW cache" in SETTINGS
    assert "Thumbnail workers" in SETTINGS
    assert "default_thumbnail_worker_count" in LIBRARY
    assert "run_thumbnail_workers" in LIBRARY
    assert ".read()" in LIBRARY and "decode_gate" in LIBRARY
    assert ".write()" in APP and "decode_gate" in APP


def test_performance_settings_are_persistent_on_both_platforms() -> None:
    assert "performance_settings_path" in APP
    assert "performanceSettingsPath" in ACTIVITY
    assert "performance.json" in (ROOT / "src/performance_settings.rs").read_text()


def test_reopening_the_same_folder_keeps_live_thumbnail_textures() -> None:
    refresh = LIBRARY[LIBRARY.index("pub(crate) fn refresh"):LIBRARY.index("fn poll(&mut self")]
    assert "Keep already decoded GPU textures visible" in refresh
    assert "std::mem::take(&mut self.entries)" in LIBRARY
    assert "same_library_file_identity" in LIBRARY
