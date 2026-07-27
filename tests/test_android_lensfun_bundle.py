from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def test_android_builds_and_links_lensfun_with_its_database() -> None:
    build = read("build.rs")
    native = read("scripts/build-android-lensfun.sh")
    packaging = read("scripts/build-android.sh")

    assert "configure_android_lensfun();" in build
    assert 'cargo:rustc-link-lib=static={library}' in build
    assert "LENSFUN_VERSION=0.3.4" in native
    assert "ICONV_VERSION=1.17" in native
    assert "GLIB_VERSION=2.78.6" in native
    assert "--wrap-mode=forcefallback" in native
    assert "-DCMAKE_INSTALL_DATAROOTDIR=apk-assets" in native
    assert 'sh "$ROOT/scripts/build-android-lensfun.sh" "$ABI"' in packaging
    assert 'find "$AURAW_LENSFUN_ROOT/apk-assets/lensfun"' in packaging
    assert "main.assets.srcDirs = [lensfunAssets]" in read("android/app/build.gradle")
    assert '"intl",' in build
    assert 'lib/libintl.a' in native


def test_android_materializes_lensfun_assets_for_the_native_database_loader() -> None:
    activity = read("android/app/src/main/java/de/duecki/auraw/AuRawActivity.java")
    bridge = read("src/android.rs")
    lifecycle = read("src/app/lifecycle.rs")

    assert "String lensfunDatabaseDir()" in activity
    assert 'copyAssetTree("lensfun", destination);' in activity
    assert 'jni_str!("lensfunDatabaseDir")' in bridge
    assert "AURAW_LENSFUN_DB" in lifecycle
