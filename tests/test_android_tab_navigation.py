from tests.source_helpers import read_source_tree
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = read_source_tree(ROOT / "src/app.rs")
TOP_BAR = (ROOT / "src/ui/top_bar.rs").read_text(encoding="utf-8")


def test_android_tabs_fill_the_available_width() -> None:
    assert '#[cfg(target_os = "android")]' in TOP_BAR
    assert 'let tab_width = ((ui.available_width() - tab_spacing * 2.0) / 3.0)' in TOP_BAR
    assert '[tab_width, tab_height]' in TOP_BAR
    assert 'egui::Button::new(label).selected(app.active_tab == tab)' in TOP_BAR
    for label in ("Library", "Develop", "Settings"):
        assert f'(AppTab::{label}, "{label}")' in TOP_BAR


def test_android_content_swipes_move_between_adjacent_tabs() -> None:
    assert "struct AndroidTabSwipeState" in APP
    assert "handle_android_tab_swipe" in APP
    assert "central_panel.response.rect" in APP
    assert "self.active_tab.next()" in APP
    assert "self.active_tab.previous()" in APP
    assert "HORIZONTAL_DOMINANCE" in APP
    assert "swipe_distance" in APP


def test_android_swipes_yield_to_editing_gestures() -> None:
    assert "slider_scroll_locked(ctx)" in APP
    assert "input.multi_touch().is_some()" in APP
    assert "self.preview_touch_navigation_active" in APP
    assert "self.sidebar_tab == SidebarTab::Masks" in APP
    assert "self.preview_zoom > 1.01" in APP
    assert "VERTICAL_CANCEL_POINTS" in APP
