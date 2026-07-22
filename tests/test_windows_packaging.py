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
