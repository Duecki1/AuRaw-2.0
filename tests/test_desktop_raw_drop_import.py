from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")


def test_desktop_frame_collects_native_file_drops_and_shows_destination_overlay() -> None:
    assert "input.raw.hovered_files" in EFRAME
    assert ".dropped_files" in EFRAME
    assert ".import_dropped_raws(dropped_paths, ui.ctx())" in EFRAME
    assert "show_raw_drop_overlay(ui, self.library.folder())" in EFRAME
    assert "Drop RAW files to import them into" in EFRAME


def test_dropped_raws_are_filtered_and_copied_off_the_ui_thread() -> None:
    start = LIBRARY.index("pub(crate) fn import_dropped_raws")
    end = LIBRARY.index("pub(crate) fn open_folder", start)
    implementation = LIBRARY[start:end]

    assert 'name("auraw-library-drop-import"' in implementation
    assert "!is_supported_raw_path(&source)" in implementation
    assert "import_raw_into_folder(&source, &folder)" in implementation
    assert "repaint.request_repaint()" in implementation


def test_raw_drop_import_never_overwrites_and_refreshes_the_library() -> None:
    helper_start = LIBRARY.index("fn import_raw_into_folder")
    helper_end = LIBRARY.index("fn unique_library_export_path", helper_start)
    helper = LIBRARY[helper_start:helper_end]

    assert "same_existing_file(source, &destination)" in helper
    assert 'file_name.push(format!(" ({number})"))' in helper
    assert "copy_file_create_new(source, &destination)" in helper
    assert "io::ErrorKind::AlreadyExists" in helper

    poll_start = LIBRARY.index("pub(crate) fn poll_dropped_raw_import")
    poll_end = LIBRARY.index("fn poll(&mut self", poll_start)
    poll = LIBRARY[poll_start:poll_end]
    assert "raw_import_receiver" in poll
    assert "self.refresh(context)" in poll
