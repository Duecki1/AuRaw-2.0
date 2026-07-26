from pathlib import Path

from tests.source_helpers import read_source_tree

ROOT = Path(__file__).resolve().parents[1]
APP = read_source_tree(ROOT / "src/app.rs")
TOP_BAR = (ROOT / "src/ui/top_bar.rs").read_text(encoding="utf-8")
LIBRARY = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
ANDROID_RS = (ROOT / "src/android.rs").read_text(encoding="utf-8")
ACTIVITY = (ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java").read_text(
    encoding="utf-8"
)


def android_top_bar_source() -> str:
    start = TOP_BAR.index('#[cfg(target_os = "android")]\n    fn show_android')
    end = TOP_BAR.index('#[cfg(not(target_os = "android"))]', start)
    return TOP_BAR[start:end]


def test_android_removes_persistent_page_tab_buttons() -> None:
    android_bar = android_top_bar_source()
    assert "tab_width" not in android_bar
    assert 'Button::new("Library")' not in android_bar
    assert 'Button::new("Develop")' not in android_bar
    assert 'Button::new("Settings")' not in android_bar
    assert 'if self.active_tab == AppTab::Develop' in EFRAME
    assert 'Panel::top("top_bar")' in EFRAME


def test_android_library_has_count_refresh_and_settings_without_path() -> None:
    assert '"{count} RAW {}"' in LIBRARY
    assert '"Refresh library"' in LIBRARY
    assert "egui_phosphor::regular::ARROW_CLOCKWISE" in LIBRARY
    assert 'if ui.button("Settings").clicked()' in LIBRARY
    assert 'app.activate_tab(AppTab::Settings);' in LIBRARY
    assert '#[cfg(not(target_os = "android"))]\n        if let Some(location)' in LIBRARY


def test_android_system_back_returns_develop_or_settings_to_library() -> None:
    assert "OnBackInvokedDispatcher.PRIORITY_DEFAULT" in ACTIVITY
    assert "nativeOnBackRequested()" in ACTIVITY
    assert "nativeOnBackRequested" in ANDROID_RS
    assert "BACK_NAVIGATION_ACTIVE" in ANDROID_RS
    assert "BACK_REQUESTED" in ANDROID_RS
    assert "if crate::android::take_back_request()" in EFRAME
    assert "self.activate_tab(AppTab::Library);" in EFRAME
    assert "self.active_tab != AppTab::Library" in EFRAME
