from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "build-windows.yml"


def test_windows_release_does_not_ship_regression_renderer():
    text = WORKFLOW.read_text(encoding="utf-8")
    assert 'cp "$release_dir/auraw-regression-render.exe"' not in text
    assert '--bin auraw' in text
    assert '--bins' not in text
    assert 'test ! -e "$package_dir/auraw-regression-render.exe"' in text


def test_windows_artifact_is_not_prezipped():
    text = WORKFLOW.read_text(encoding="utf-8")
    assert 'path: dist/AuRaw-windows-x86_64/' in text
    assert 'path: dist/AuRaw-windows-x86_64.zip' not in text
    assert 'zip -9 -r "$package_name.zip"' not in text


def test_windows_artifact_includes_hash_manifest():
    text = WORKFLOW.read_text(encoding="utf-8")
    assert 'SHA256SUMS.txt' in text
    assert 'sha256sum auraw.exe' in text


def test_windows_executable_embeds_the_shared_application_icon():
    text = WORKFLOW.read_text(encoding="utf-8")
    build = (ROOT / "build.rs").read_text(encoding="utf-8")
    resource = (ROOT / "packaging" / "windows" / "auraw.rc").read_text(encoding="utf-8")

    assert "embed_windows_application_icon();" in build
    assert 'Command::new("windres")' in build
    assert "cargo:rustc-link-arg-bin=auraw=" in build
    assert '"../icons/auraw.ico"' in resource
    assert "objdump" in text and r"grep -q '\.rsrc'" in text


def test_windows_executable_uses_gui_subsystem():
    text = WORKFLOW.read_text(encoding="utf-8")
    main = (ROOT / "src" / "main.rs").read_text(encoding="utf-8")

    assert '#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]' in main
    assert 'objdump -p "$release_dir/auraw.exe"' in text
    assert "Subsystem[[:space:]]+00000002" in text

