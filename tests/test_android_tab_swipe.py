from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
LIB = (ROOT / "src/lib.rs").read_text(encoding="utf-8")
BASE_STYLE = (ROOT / "android/app/src/main/res/values/styles.xml").read_text(encoding="utf-8")
V35_STYLE = (ROOT / "android/app/src/main/res/values-v35/styles.xml").read_text(encoding="utf-8")


def test_android_tab_swipe_navigation_is_removed() -> None:
    assert not (ROOT / "src/app/android_tab_swipe.rs").exists()
    assert "android_tab_swipe" not in APP
    assert "finish_android_tab_swipe_frame" not in EFRAME
    assert "prepare_android_tab_swipe_frame" not in EFRAME


def test_android_runs_with_system_bars_visible() -> None:
    assert ".with_fullscreen(true)" not in LIB
    assert 'android:windowFullscreen">false' in BASE_STYLE
    assert 'android:windowOptOutEdgeToEdgeEnforcement">true' in V35_STYLE
