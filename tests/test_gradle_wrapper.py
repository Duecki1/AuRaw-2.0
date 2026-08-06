from __future__ import annotations

import configparser
import subprocess
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_checked_in_gradle_wrapper_integrity() -> None:
    completed = subprocess.run(
        ["cargo", "xtask", "check-gradle"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr


def test_gradle_wrapper_jar_contains_an_executable_bootstrap() -> None:
    jar = ROOT / "gradle/wrapper/gradle-wrapper.jar"
    with zipfile.ZipFile(jar) as archive:
        names = set(archive.namelist())
        assert "org/gradle/wrapper/GradleWrapperMain.class" in names
        assert "META-INF/MANIFEST.MF" in names
        assert archive.testzip() is None


def test_gradle_wrapper_properties_are_complete_and_pinned() -> None:
    parser = configparser.ConfigParser(interpolation=None)
    parser.read_string(
        "[wrapper]\n" + (ROOT / "gradle/wrapper/gradle-wrapper.properties").read_text(encoding="utf-8")
    )
    wrapper = parser["wrapper"]
    assert wrapper.getboolean("validateDistributionUrl")
    assert len(wrapper["distributionSha256Sum"]) == 64
    assert wrapper["distributionUrl"].endswith("-bin.zip")
    assert int(wrapper["networkTimeout"]) > 0
