from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")
MASK_UI = (ROOT / "src/ui/sidebar/masks.rs").read_text(encoding="utf-8")
MASKS = (ROOT / "src/pipeline/masks.rs").read_text(encoding="utf-8")
SIDECAR = (ROOT / "src/sidecar.rs").read_text(encoding="utf-8")


def test_library_cards_offer_destructive_and_duplicate_actions() -> None:
    assert "response.context_menu(|ui|" in LIBRARY
    assert "Duplicate (RAW + sidecar)" in LIBRARY
    assert "Reset all adjustments" in LIBRARY
    assert '"Delete selected"' in LIBRARY
    assert 'egui::Button::new(delete_label)' in LIBRARY
    assert "duplicate_raw_and_sidecar" in LIBRARY
    assert "remove_desktop_edits" in LIBRARY


def test_mask_and_component_context_menus_have_requested_actions() -> None:
    for label in (
        "Duplicate",
        "Invert",
        "Duplicate & Invert",
        "Copy Mask Group",
        "Paste Mask Group",
        "Copy Component",
        "Paste Component",
    ):
        assert label in MASK_UI
    assert "add_enabled(can_paste" in MASK_UI
    assert "Copy a mask group first" in MASK_UI
    assert "Copy a component first" in MASK_UI


def test_duplicate_and_invert_resets_the_new_mask_adjustments() -> None:
    copy = MASK_UI[
        MASK_UI.index("fn insert_mask_group_copy") : MASK_UI.index("fn duplicate_mask_component")
    ]
    invert_branch = copy[copy.index("if invert {") : copy.index("let insert_at")]
    assert "mask.invert = !mask.invert" in invert_branch
    assert "mask.adjustments.reset()" in invert_branch


def test_mask_rename_uses_a_dialog_instead_of_inline_context_text_fields() -> None:
    group_menu = MASK_UI[MASK_UI.index("fn mask_group_context_menu"):MASK_UI.index("fn submask_context_menu")]
    component_menu = MASK_UI[MASK_UI.index("fn submask_context_menu"):MASK_UI.index("fn mask_group_clipboard_id")]
    assert "TextEdit::singleline" not in group_menu
    assert "TextEdit::singleline" not in component_menu
    assert "show_mask_rename_dialog" in MASK_UI
    assert 'egui::Window::new(title)' in MASK_UI
    assert "Rename mask group" in MASK_UI
    assert "Rename sub-mask" in MASK_UI


def test_mask_groups_have_serialized_final_inversion() -> None:
    assert "#[serde(default)]\n    pub invert: bool" in MASKS
    assert "let value = if mask.invert { 1.0 - value } else { value };" in MASKS
    assert "group_invert_is_the_exact_final_mask_complement" in MASKS
    assert "MAX_MASK_COMPONENTS" in MASKS
    assert "MAX_MASK_COMPONENTS" in SIDECAR



def test_desktop_library_selection_context_menu_offers_bulk_actions() -> None:
    # Selection state transitions and labels are native LibraryState tests.
    assert 'response.context_menu(|ui|' in LIBRARY
    assert 'Duplicate selected (RAW + sidecars)' in LIBRARY
    assert 'Reset adjustments for selected' in LIBRARY
    assert 'Delete selected' in LIBRARY
    assert 'LibraryCardAction::Duplicate(context_paths.clone())' in LIBRARY
    assert 'LibraryCardAction::ResetAdjustments(' in LIBRARY
    assert 'LibraryCardAction::Delete(context_paths.clone())' in LIBRARY

def test_android_library_bulk_actions_keep_required_jni_bridge_contracts() -> None:
    android_bridge = (ROOT / "src/android.rs").read_text(encoding="utf-8")
    android_activity = (
        ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java"
    ).read_text(encoding="utf-8")
    sidecar_persistence = (
        ROOT / "src/app/sidecar_persistence.rs"
    ).read_text(encoding="utf-8")

    # Touch-selection state transitions are exercised by native LibraryState tests.
    assert '"Export selected…"' in LIBRARY
    assert '"Duplicate selected (RAW + sidecars)"' in LIBRARY
    assert "reset_android_library_adjustments" in LIBRARY
    assert "delete_android_library_item" in LIBRARY
    assert 'LibraryCardAction::Export(targets())' in LIBRARY
    assert 'LibraryCardAction::Duplicate(targets())' in LIBRARY
    assert 'jni::jni_str!("duplicateRawLibraryDocument")' in android_bridge
    assert 'jni::jni_str!("deleteRawLibraryDocument")' in android_bridge
    assert 'jni::jni_str!("removeRawSidecar")' in android_bridge
    assert "public String duplicateRawLibraryDocument" in android_activity
    assert "public void deleteRawLibraryDocument" in android_activity
    assert "public void removeRawSidecar" in android_activity
    assert "crate::android::remove_raw_sidecar" in sidecar_persistence
    assert "reset_adjustments_preserving_mask_properties" not in sidecar_persistence
    android_reset = android_bridge[
        android_bridge.index("pub fn remove_raw_sidecar") : android_bridge.index(
            "fn clear_developed_thumbnail_cache"
        )
    ]
    assert "clear_developed_thumbnail_cache(app, raw_uri)" in android_reset


def test_desktop_reset_all_deletes_the_sidecar_instead_of_rewriting_masks() -> None:
    sidecar = (ROOT / "src/sidecar.rs").read_text(encoding="utf-8")
    reset = sidecar[
        sidecar.index("pub fn reset_desktop_adjustments") : sidecar.index(
            "pub fn load_android"
        )
    ]
    assert "remove_desktop_edits(raw_path)" in reset
    assert "save_desktop" not in reset
    assert "reset_adjustments_preserving_mask_properties" not in sidecar


def test_android_library_uses_one_shared_selection_overflow_instead_of_card_menus() -> None:
    ui = (ROOT / "src/ui/mod.rs").read_text(encoding="utf-8")
    # Button geometry is covered by native egui Rect invariants.
    assert 'fn android_overflow_menu' in ui
    assert 'Popup::menu(&response).show(add_contents)' in ui
    assert 'android-library-selection-overflow' in LIBRARY
    assert 'android_selection_menu(' in LIBRARY
    assert 'android-library-card-overflow' not in LIBRARY
    assert 'android_library_card_menu(' not in LIBRARY
    assert 'Reset adjustments for selected' in LIBRARY
    assert 'Delete selected' in LIBRARY


def test_android_mask_cards_have_visible_overflow_menu_buttons() -> None:
    assert 'android-mask-group-overflow' in MASK_UI
    assert 'android-submask-overflow' in MASK_UI
    assert MASK_UI.count('crate::ui::android_overflow_menu(') >= 2
    assert MASK_UI.count('&& !overflow_clicked') >= 2


def test_desktop_library_export_context_menu_supports_single_and_batch_destinations() -> None:
    assert '"Export selected…"' in LIBRARY
    assert '"Export…"' in LIBRARY
    assert 'LibraryCardAction::Export(context_paths.clone())' in LIBRARY
    assert 'library-export-dialog' in LIBRARY
    assert 'Export {count} images' in LIBRARY
    assert 'let folder = dialog.pick_folder()?' in LIBRARY
    assert 'dialog = dialog.set_directory(parent)' in LIBRARY
    assert 'rfd::FileDialog::new().set_file_name(default_name)' in LIBRARY
    assert 'start_library_exports(' in LIBRARY



def test_library_batch_export_opens_dedicated_progress_dialog_with_cancel() -> None:
    app = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
    assert 'library-batch-export-progress-dialog' in LIBRARY
    assert '"Exporting images"' in LIBRARY
    assert '"{exported} / {total} exported"' in LIBRARY
    assert 'egui::ProgressBar::new(overall_fraction)' in LIBRARY
    assert 'egui::Button::new("Cancel")' in LIBRARY
    assert 'app.cancel_library_batch_export()' in LIBRARY
    assert 'Cancelling after the current image finishes' in LIBRARY
    assert 'cancel_requested: bool' in (ROOT / "src/app.rs").read_text(encoding="utf-8")
    assert 'pub(crate) fn cancel_library_batch_export' in app
    assert 'batch.pending.clear()' in app
