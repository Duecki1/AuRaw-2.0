from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")
SIDECAR = (ROOT / "src/sidecar.rs").read_text(encoding="utf-8")
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
PERSISTENCE = (ROOT / "src/app/sidecar_persistence.rs").read_text(encoding="utf-8")
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")


def test_desktop_library_scan_is_strictly_non_recursive() -> None:
    scan = LIBRARY[LIBRARY.index("fn scan_folder_with_limit"):]
    assert "std::fs::read_dir(folder)" in scan
    assert "stack.push" not in scan
    assert "MAX_DIRECTORY_DEPTH" not in LIBRARY
    assert "Only direct children of the selected folder" in scan
    assert "directly inside the selected folder" in LIBRARY


def test_developed_thumbnails_are_cached_and_sidecar_invalidated() -> None:
    assert 'DEVELOPED_THUMBNAIL_SUFFIX: &str = ".auraw-thumb.png"' in SIDECAR
    assert "developed_thumbnail_cache_is_fresh" in SIDECAR
    assert "desktop_sidecar_fingerprint" in SIDECAR
    assert "save_developed_thumbnail_cache" in SIDECAR
    assert "atomic_write(&cache_path" in SIDECAR
    assert "sidecar changed while its thumbnail" in SIDECAR


def test_saved_gpu_preview_refreshes_the_visible_library_entry() -> None:
    assert "pub struct GpuOutputSnapshot" in GPU
    assert "read_thumbnail_blocking" in GPU
    assert "DevelopedThumbnailJob" in APP
    assert 'name("auraw-developed-thumbnail"' in PERSISTENCE
    assert "output_snapshot" in PERSISTENCE
    assert "install_developed_thumbnail" in LIBRARY
    assert "self.poll_developed_thumbnail(frame);" in EFRAME


def test_library_prefers_cached_developed_thumbnail_over_embedded_raw_preview() -> None:
    cache = LIBRARY.index("load_developed_thumbnail_cache")
    raw = LIBRARY.index("load_raw_embedded_thumbnail(path, THUMBNAIL_EDGE)", cache)
    assert cache < raw
    assert "developed_thumbnail && !loaded.developed" in LIBRARY


def test_uncached_edited_library_cards_render_raw_plus_sidecar_before_fallbacks() -> None:
    render = LIBRARY.index("render_uncached_developed_thumbnail")
    raw_cache = LIBRARY.index("load_desktop_raw_thumbnail", render)
    embedded = LIBRARY.index("load_raw_embedded_thumbnail(path, THUMBNAIL_EDGE)", raw_cache)
    assert render < raw_cache < embedded
    assert "load_raw_file_with_profile_selection" in LIBRARY
    assert "RawGpuPipeline::new_headless_with_quality" in LIBRARY
    assert "save_developed_thumbnail_cache" in LIBRARY


def test_invalid_sidecar_falls_back_to_the_normal_raw_thumbnail() -> None:
    render = LIBRARY[LIBRARY.index("fn render_uncached_developed_thumbnail") :]
    render = render[: render.index("fn load_desktop_library_thumbnail")]
    assert "Err(crate::sidecar::SidecarError::Invalid(error))" in render
    assert "return Ok(None);" in render


def test_library_sort_dropdown_supports_date_name_and_size_orders() -> None:
    assert 'ComboBox::from_id_salt("library-sort-order")' in LIBRARY
    for label in (
        "Newest first",
        "Oldest first",
        "Name A–Z",
        "Name Z–A",
        "Largest first",
        "Smallest first",
    ):
        assert label in LIBRARY
    assert "self.sort_entries();" in LIBRARY
    assert "self.rebuild_entry_indices();" in LIBRARY
