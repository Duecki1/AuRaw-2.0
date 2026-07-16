from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
SWIPE = (ROOT / "src/app/android_tab_swipe.rs").read_text(encoding="utf-8")
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
TOP_BAR = (ROOT / "src/ui/top_bar.rs").read_text(encoding="utf-8")
PREVIEW = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")


def test_android_tabs_are_equal_width_page_tabs() -> None:
    assert 'let tab_width = (ui.available_width() / 3.0).max(1.0);' in TOP_BAR
    for tab in ("Library", "Develop", "Settings"):
        assert f'(AppTab::{tab}, "{tab}")' in TOP_BAR
    assert 'ui.add_sized(egui::vec2(tab_width, 42.0), button)' in TOP_BAR
    assert 'app.activate_tab(tab);' in TOP_BAR
    assert 'fn activate_tab(&mut self, tab: AppTab)' in APP


def test_android_page_swipe_is_guarded_from_editing_gestures() -> None:
    assert 'HORIZONTAL_DOMINANCE' in SWIPE
    assert 'VIEWPORT_SWIPE_FRACTION' in SWIPE
    assert 'input.multi_touch().is_some()' in SWIPE
    assert 'slider_scroll_locked(ctx)' in SWIPE
    assert 'self.mask_drag.is_some()' in SWIPE
    assert 'self.active_mask_tool.is_some()' in SWIPE
    assert 'captured_by_control' in SWIPE
    assert 'ctx.any_popup_open()' in SWIPE
    assert 'next_tab(swipe.starting_tab)' in SWIPE
    assert 'previous_tab(swipe.starting_tab)' in SWIPE
    assert 'self.preview_zoom = swipe.preview_zoom;' in SWIPE
    assert 'self.preview_center = swipe.preview_center;' in SWIPE
    assert 'self.prepare_android_tab_swipe_frame();' in EFRAME
    assert 'self.finish_android_tab_swipe_frame(ui.ctx(), _central.response.rect);' in EFRAME
    assert 'app.note_tab_swipe_surface(response.id);' in PREVIEW
    assert 'app.note_tab_swipe_surface(scroll.id);' in LIBRARY
