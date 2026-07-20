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
    assert 'egui::Button::new("Delete")' in LIBRARY
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


def test_android_thumbnail_long_press_opens_context_menu_without_opening_photo() -> None:
    android_bridge = (ROOT / "src/android.rs").read_text(encoding="utf-8")
    android_activity = (
        ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java"
    ).read_text(encoding="utf-8")

    assert "response.clicked()" in LIBRARY
    assert "!response.secondary_clicked()" in LIBRARY
    assert "&& !overflow_clicked" in LIBRARY
    assert 'let LibrarySource::Android {' in LIBRARY
    assert 'ui.button("Open")' in LIBRARY
    assert "reset_android_library_adjustments" in LIBRARY
    assert "delete_android_library_item" in LIBRARY
    assert 'jni::jni_str!("removeRawSidecar")' in android_bridge
    assert 'jni::jni_str!("deleteRawLibraryDocument")' in android_bridge
    assert "public void removeRawSidecar" in android_activity
    assert "public void deleteRawLibraryDocument" in android_activity


def test_android_library_cards_have_visible_overflow_menu_buttons() -> None:
    ui = (ROOT / "src/ui/mod.rs").read_text(encoding="utf-8")
    assert 'fn android_overflow_menu' in ui
    assert 'painter.circle_filled(' in ui
    assert 'ui.interact(button_rect, id, Sense::click())' in ui
    assert 'Popup::menu(&response).show(add_contents)' in ui
    assert 'Do not use `Ui::menu_button` here' in ui
    assert 'RichText::new("⋮")' not in ui
    assert 'android-library-card-overflow' in LIBRARY
    assert 'android_library_card_menu(' in LIBRARY
    assert '&& !overflow_clicked' in LIBRARY


def test_android_mask_cards_have_visible_overflow_menu_buttons() -> None:
    assert 'android-mask-group-overflow' in MASK_UI
    assert 'android-submask-overflow' in MASK_UI
    assert MASK_UI.count('crate::ui::android_overflow_menu(') >= 2
    assert MASK_UI.count('&& !overflow_clicked') >= 2
    assert 'response.rect,\n                            menu_id,\n                            22.0,' in MASK_UI
    assert 'response.rect,\n                                    menu_id,\n                                    20.0,' in MASK_UI
