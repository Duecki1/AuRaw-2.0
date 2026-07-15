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
