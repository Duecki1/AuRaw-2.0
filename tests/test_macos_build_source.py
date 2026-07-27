from __future__ import annotations

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_macos_libraw_uses_libcxx_instead_of_removed_libstdcxx() -> None:
    build_rs = (ROOT / "build.rs").read_text(encoding="utf-8")

    assert 'Ok("macos")' in build_rs
    assert "config.cargo_metadata(!target_is_macos)" in build_rs
    assert 'if library == "stdc++"' in build_rs
    assert '"c++"' in build_rs


def test_macos_bundle_builds_and_declares_the_shared_application_icon() -> None:
    workflow = (ROOT / ".github/workflows/build-macos.yml").read_text(encoding="utf-8")

    assert 'icon_source="packaging/icons/auraw-1024.png"' in workflow
    assert 'iconutil -c icns "$iconset"' in workflow
    assert "<key>CFBundleIconFile</key>" in workflow
    assert "<string>AuRaw.icns</string>" in workflow
