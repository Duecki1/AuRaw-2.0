from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
NAVIGATION = (ROOT / "src/ui/sidebar/navigation.rs").read_text(encoding="utf-8")
ICONS = (ROOT / "src/ui/icons.rs").read_text(encoding="utf-8")
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
TOP_BAR = (ROOT / "src/ui/top_bar.rs").read_text(encoding="utf-8")
UI_MOD = (ROOT / "src/ui/mod.rs").read_text(encoding="utf-8")
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")


def test_desktop_tools_use_a_fixed_right_edge_icon_rail() -> None:
    assert 'Panel::right("develop_tool_rail")' in EFRAME
    assert "show_desktop_tool_rail" in EFRAME
    assert "icon_toggle_button" in NAVIGATION
    for icon in ("Adjustments", "Crop", "Mask", "Heal", "Export"):
        assert f"UiIcon::{icon}" in NAVIGATION


def test_desktop_sidebar_width_is_owned_by_app_state() -> None:
    assert "desktop_sidebar_width: Option<f32>" in APP
    assert "self.desktop_sidebar_width.unwrap_or(sidebar_size)" in EFRAME
    assert "self.desktop_sidebar_width = Some(sidebar_response.response.rect.width())" in EFRAME
    assert ".resizable(true)" in EFRAME
    assert ".max_size(" in EFRAME


def test_sidebar_icons_come_from_phosphor_and_have_tooltips() -> None:
    assert "egui_phosphor::regular" in ICONS
    assert ".on_hover_text(tooltip)" in ICONS
    assert "paint_icon" not in ICONS
    assert "Shape::line" not in ICONS


def test_action_icons_do_not_use_custom_painter_geometry() -> None:
    assert "egui_phosphor::regular::ARROW_LEFT" in TOP_BAR
    assert "ui.painter()" not in TOP_BAR
    assert "egui_phosphor::regular::DOTS_THREE_VERTICAL" in UI_MOD
    assert "Draw the overflow mark geometrically" not in UI_MOD
    assert "egui_phosphor::regular::PLUS" in LIBRARY
