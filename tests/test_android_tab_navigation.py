from tests.source_helpers import read_source_tree
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = read_source_tree(ROOT / "src/app.rs")
TOP_BAR = (ROOT / "src/ui/top_bar.rs").read_text(encoding="utf-8")


def test_android_tabs_fill_the_available_width() -> None:
    assert '#[cfg(target_os = "android")]' in TOP_BAR
    assert 'let tab_width = (ui.available_width() / 3.0).max(1.0);' in TOP_BAR
    assert 'egui::vec2(tab_width, 42.0)' in TOP_BAR
    assert 'egui::Button::new(label)' in TOP_BAR
    assert '.selected(app.active_tab == tab)' in TOP_BAR
    for label in ("Library", "Develop", "Settings"):
        assert f'(AppTab::{label}, "{label}")' in TOP_BAR


def test_android_content_swipes_move_between_adjacent_tabs() -> None:
    assert "struct AndroidTabSwipe" in APP
    assert "finish_android_tab_swipe_frame" in APP
    assert "_central.response.rect" in APP
    assert "next_tab(swipe.starting_tab)" in APP
    assert "previous_tab(swipe.starting_tab)" in APP
    assert "HORIZONTAL_DOMINANCE" in APP
    assert "VIEWPORT_SWIPE_FRACTION" in APP


def test_android_swipes_yield_to_editing_gestures() -> None:
    assert "slider_scroll_locked(ctx)" in APP
    assert "input.multi_touch().is_some()" in APP
    assert "self.preview_touch_navigation_active" in APP
    assert "self.sidebar_tab == SidebarTab::Masks" in APP
    assert "preview_zoom > 1.0" in APP
    assert "HORIZONTAL_INTENT_POINTS" in APP
