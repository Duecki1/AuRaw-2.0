from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")


def test_folder_tree_exposes_expected_management_actions() -> None:
    for label in (
        "New Folder…",
        "Paste Folder",
        "Copy Folder",
        "Cut Folder",
        "Rename Folder…",
        "Delete Folder…",
        "Refresh Folders",
    ):
        assert label in LIBRARY
    assert "library-folder-name-dialog" in LIBRARY
    assert "library-folder-delete-confirmation" in LIBRARY
    assert "This cannot be undone." in LIBRARY


def test_folder_operations_are_root_confined_and_non_overwriting() -> None:
    assert "canonical_library_directory" in LIBRARY
    assert "resolved.starts_with(&root)" in LIBRARY
    assert "The top-level library folder cannot be moved, renamed, or deleted" in LIBRARY
    assert "create_new(true)" in LIBRARY
    assert "fs::create_dir(destination)" in LIBRARY
    assert "Refusing to follow symbolic link" in LIBRARY
    assert "Refusing to copy special filesystem entry" in LIBRARY
    assert 'name("auraw-library-folder-operation"' in LIBRARY


def test_folder_tree_and_native_drops_support_folders() -> None:
    assert "Sense::click_and_drag()" in LIBRARY
    assert "dnd_set_drag_payload" in LIBRARY
    assert "dnd_hover_payload::<LibraryFolderDrag>" in LIBRARY
    assert "dnd_release_payload::<LibraryFolderDrag>" in LIBRARY
    assert "A folder cannot be moved into itself or one of its subfolders" in LIBRARY
    assert "if source.is_dir()" in LIBRARY
    assert "import_folder_into_library(&source, &folder)" in LIBRARY
    assert "Folders are copied here too" in EFRAME
