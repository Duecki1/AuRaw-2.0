from __future__ import annotations

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_macos_libraw_uses_libcxx_instead_of_removed_libstdcxx() -> None:
    build_rs = (ROOT / "build.rs").read_text(encoding="utf-8")

    assert 'Ok("macos")' in build_rs
    assert "config.cargo_metadata(!target_is_macos)" in build_rs
    assert 'if library == "stdc++"' in build_rs
    assert '"c++"' in build_rs
