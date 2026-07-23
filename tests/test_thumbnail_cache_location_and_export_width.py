from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
THUMBNAIL_CACHE = (ROOT / "src/thumbnail_cache.rs").read_text(encoding="utf-8")
SIDECAR = (ROOT / "src/sidecar.rs").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/ui/sidebar/export.rs").read_text(encoding="utf-8")


def test_desktop_thumbnails_use_private_os_cache_locations() -> None:
    assert 'std::env::var_os("LOCALAPPDATA")' in THUMBNAIL_CACHE
    assert 'home.join("Library").join("Caches")' in THUMBNAIL_CACHE
    assert 'std::env::var_os("XDG_CACHE_HOME")' in THUMBNAIL_CACHE
    assert 'join("auraw")' in THUMBNAIL_CACHE
    assert 'DESKTOP_THUMBNAIL_CACHE_DIR: &str = "library-thumbnails"' in THUMBNAIL_CACHE
    assert "desktop_cache_path_for_raw" in THUMBNAIL_CACHE
    assert "desktop_cache_path_for_raw(raw_path, DEVELOPED_THUMBNAIL_SUFFIX)" in SIDECAR


def test_legacy_sibling_thumbnail_caches_are_migrated_not_recreated() -> None:
    assert 'LEGACY_THUMBNAIL_CACHE_DIR: &str = ".auraw-cache"' in THUMBNAIL_CACHE
    assert "migrate_legacy_desktop_raw_thumbnail" in THUMBNAIL_CACHE
    assert "migrate_legacy_developed_thumbnail_cache" in SIDECAR
    assert "remove_legacy_cache_file" in THUMBNAIL_CACHE
    assert "parent.join(LEGACY_THUMBNAIL_CACHE_DIR)" in THUMBNAIL_CACHE
    assert "parent.join(DEVELOPED_THUMBNAIL_CACHE_DIR)" not in SIDECAR


def test_export_sidebar_reserves_the_scrollbar_gutter_for_all_controls() -> None:
    show_export = EXPORT[EXPORT.index("fn show_export"):]
    assert "ui.available_width() - Self::SCROLLBAR_GUTTER" in show_export
    assert show_export.index("ui.set_width(content_width)") < show_export.index(
        "export_settings_controls"
    )
    assert show_export.index("ui.set_max_width(content_width)") < show_export.index(
        'Button::new("Export PNG…")'
    )
